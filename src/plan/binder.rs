//! The binder: AST plus catalogue in, plan plus schema out.
//!
//! This is the only place in the program that knows both what the user wrote
//! and what actually exists. Everything upstream deals in names; everything
//! downstream deals in indices.
//!
//! The phase exists because of one protocol rule: `RowDescription` is written
//! before the first `DataRow`, so the complete output schema — every column
//! name and every type — has to be settled before a single row is read.
//!
//! # The plan it builds
//!
//! ```text
//! Scan/Join → Filter(WHERE) → Aggregate → Filter(HAVING)
//!           → Sort → Project → Distinct → Limit
//! ```
//!
//! Two orderings in there are worth explaining.
//!
//! **`Sort` sits below `Project`.** `ORDER BY size` must work even when `size`
//! is not selected, so sort keys are resolved against the *input* to the
//! projection. An `ORDER BY` naming an output alias is handled by substituting
//! that alias's already-bound expression.
//!
//! **`Distinct` sits above `Project`.** `SELECT DISTINCT` is distinct over the
//! output row, not the input one, and keeping first occurrences means it does
//! not disturb the sort beneath it.
//!
//! # Aggregation changes what names mean
//!
//! After a `GROUP BY`, the rows flowing upward are no longer input rows: they
//! are group keys followed by aggregate results. So `SELECT ext, COUNT(*)`
//! binds `ext` to the *key* column and `COUNT(*)` to the *aggregate* column,
//! and a bare `name` — which belongs to no group — is an error rather than an
//! arbitrary row's value.

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::sort::SortKey;
use crate::plan::expr::{AggFn, AggSpec, CompiledExpr, ScalarFn};
use crate::plan::plan::Plan;
use crate::plan::schema::{Column, Schema};
use crate::sources::{self, SourceConfig};
use crate::sql::ast::*;
use crate::value::{Row, Type, Value};

pub fn bind(select: &Select, config: &SourceConfig) -> Result<Plan> {
    Binder { config }.bind_select(select)
}

struct Binder<'a> {
    config: &'a SourceConfig,
}

/// What an expression may refer to once a `GROUP BY` is in play.
///
/// Holds the fingerprints of the grouping expressions and of every aggregate
/// call in the query, each paired with its column index in the aggregate's
/// output. Binding in this context is a *rewrite*: a matching expression
/// becomes a column reference into that output.
struct Grouping {
    keys: Vec<(String, usize)>,
    aggregates: Vec<(String, usize)>,
}

impl Grouping {
    fn lookup(&self, fingerprint: &str) -> Option<usize> {
        self.keys
            .iter()
            .chain(&self.aggregates)
            .find(|(candidate, _)| candidate == fingerprint)
            .map(|(_, index)| *index)
    }
}

impl Binder<'_> {
    fn bind_select(&self, select: &Select) -> Result<Plan> {
        // FROM first: every later clause resolves its names against this.
        let (mut plan, input_schema) = match &select.from {
            Some(from) => self.bind_from(from)?,
            None => (
                Plan::Values {
                    schema: Schema::default(),
                    rows: vec![Row::new(Vec::new())],
                },
                Schema::default(),
            ),
        };

        if let Some(filter) = &select.filter {
            let predicate = self.bind_expr(filter, &input_schema, None)?;
            self.require_condition(&predicate, &input_schema, filter, "WHERE")?;
            plan = Plan::Filter {
                input: Box::new(plan),
                predicate,
            };
        }

        // An aggregate query is one with a GROUP BY, a HAVING, or an aggregate
        // call anywhere in its projection, HAVING or ORDER BY.
        let mut aggregates = Vec::new();
        for expr in self.aggregate_bearing_exprs(select) {
            collect_aggregates(expr, &mut aggregates)?;
        }
        let grouped =
            !select.group_by.is_empty() || select.having.is_some() || !aggregates.is_empty();

        let mut schema = input_schema.clone();
        let mut grouping = None;

        if grouped {
            let (aggregate_plan, aggregate_schema, context) =
                self.bind_aggregate(plan, &input_schema, select, aggregates)?;
            plan = aggregate_plan;
            schema = aggregate_schema;
            grouping = Some(context);
        }

        if let Some(having) = &select.having {
            let predicate = self.bind_expr(having, &schema, grouping.as_ref())?;
            self.require_condition(&predicate, &schema, having, "HAVING")?;
            plan = Plan::Filter {
                input: Box::new(plan),
                predicate,
            };
        }

        let (exprs, output_schema) =
            self.bind_projection(select, &schema, grouping.as_ref())?;

        // Below the projection, so an ORDER BY may name a column that is not
        // selected. Output aliases are handled inside.
        if !select.order_by.is_empty() {
            let keys =
                self.bind_order_by(select, &schema, grouping.as_ref(), &exprs, &output_schema)?;
            plan = Plan::Sort {
                input: Box::new(plan),
                keys,
            };
        }

        plan = Plan::Project {
            input: Box::new(plan),
            exprs,
            schema: output_schema,
        };

        if select.distinct {
            plan = Plan::Distinct {
                input: Box::new(plan),
            };
        }

        if select.limit.is_some() || select.offset.is_some() {
            plan = Plan::Limit {
                input: Box::new(plan),
                limit: select.limit,
                offset: select.offset.unwrap_or(0),
            };
        }

        Ok(plan)
    }

    /// Every expression that may contain an aggregate call.
    fn aggregate_bearing_exprs<'s>(&self, select: &'s Select) -> Vec<&'s Expr> {
        let mut exprs = Vec::new();
        if let Projection::Items(items) = &select.projection {
            exprs.extend(items.iter().map(|item| &item.expr));
        }
        if let Some(having) = &select.having {
            exprs.push(having);
        }
        exprs.extend(select.order_by.iter().map(|key| &key.expr));
        exprs
    }

    /// Builds the `Aggregate` node and the context that rewrites references to
    /// its output.
    fn bind_aggregate(
        &self,
        input: Plan,
        input_schema: &Schema,
        select: &Select,
        aggregates: Vec<(String, AggregateCall)>,
    ) -> Result<(Plan, Schema, Grouping)> {
        let mut keys = Vec::with_capacity(select.group_by.len());
        let mut columns = Vec::new();
        let mut key_fingerprints = Vec::new();

        for (index, expr) in select.group_by.iter().enumerate() {
            let bound = self.bind_expr(expr, input_schema, None)?;
            columns.push(Column::new(
                default_column_name(expr),
                self.type_of(&bound, input_schema),
            ));
            key_fingerprints.push((fingerprint(expr), index));
            keys.push(bound);
        }

        let mut specs = Vec::with_capacity(aggregates.len());
        let mut aggregate_fingerprints = Vec::new();

        for (offset, (print, call)) in aggregates.into_iter().enumerate() {
            let argument = match &call.argument {
                Some(expr) => Some(self.bind_expr(expr, input_schema, None)?),
                None => None,
            };
            let argument_type = argument
                .as_ref()
                .map(|expr| self.type_of(expr, input_schema))
                .unwrap_or(Type::Int);
            let result_type = call.function.result_type(argument_type);

            columns.push(Column::new(call.function.name(), result_type));
            aggregate_fingerprints.push((print, keys.len() + offset));
            specs.push(AggSpec {
                function: call.function,
                argument,
                distinct: call.distinct,
                name: call.function.name().to_string(),
                result_type,
            });
        }

        let schema = Schema::new(columns);
        let plan = Plan::Aggregate {
            input: Box::new(input),
            keys,
            aggregates: specs,
            schema: schema.clone(),
        };

        Ok((
            plan,
            schema,
            Grouping {
                keys: key_fingerprints,
                aggregates: aggregate_fingerprints,
            },
        ))
    }

    fn bind_from(&self, from: &FromItem) -> Result<(Plan, Schema)> {
        let (left_plan, left_schema) = self.bind_source(&from.source, from.alias.as_deref())?;

        let Some(join) = &from.join else {
            return Ok((left_plan, left_schema));
        };

        let (right_plan, right_schema) =
            self.bind_source(&join.source, join.alias.as_deref())?;

        // Left columns then right columns — the layout the join operator
        // produces and every index below is resolved against.
        let mut columns = left_schema.columns.clone();
        columns.extend(right_schema.columns.clone());
        let combined = Schema::new(columns);

        let condition = self.bind_expr(&join.on, &combined, None)?;
        self.require_condition(&condition, &combined, &join.on, "ON")?;

        let plan = Plan::Join {
            left: Box::new(left_plan),
            right: Box::new(right_plan),
            condition,
            kind: join.kind,
            schema: combined.clone(),
        };

        Ok((plan, combined))
    }

    fn bind_source(&self, source: &Source, alias: Option<&str>) -> Result<(Plan, Schema)> {
        let table = sources::resolve(source, self.config)?;

        // An alias renames the source for qualified references: `files AS f`
        // makes `f.size` resolvable and, per SQL, `files.size` no longer so.
        let qualifier = alias.unwrap_or(&source.name);
        let schema = Schema::new(
            table
                .schema()
                .columns
                .iter()
                .map(|column| Column::qualified(&column.name, column.ty, qualifier))
                .collect(),
        );

        Ok((
            Plan::Scan {
                source: table,
                schema: schema.clone(),
            },
            schema,
        ))
    }

    fn bind_projection(
        &self,
        select: &Select,
        schema: &Schema,
        grouping: Option<&Grouping>,
    ) -> Result<(Vec<CompiledExpr>, Schema)> {
        match &select.projection {
            Projection::Wildcard => {
                if select.from.is_none() {
                    return Err(ZqlError::syntax("SELECT * needs a FROM clause")
                        .with_hint("try `SELECT * FROM files`"));
                }
                if grouping.is_some() {
                    return Err(ZqlError::new(
                        SqlState::UndefinedColumn,
                        "SELECT * cannot be combined with GROUP BY",
                    )
                    .with_hint("name the grouped columns and the aggregates explicitly"));
                }

                // `*` is every input column in order, which after binding is
                // just the identity projection.
                let exprs = (0..schema.len()).map(CompiledExpr::Column).collect();
                let columns = schema
                    .columns
                    .iter()
                    .map(|column| Column::new(&column.name, column.ty))
                    .collect();
                Ok((exprs, Schema::new(columns)))
            }

            Projection::Items(items) => {
                let mut exprs = Vec::with_capacity(items.len());
                let mut columns = Vec::with_capacity(items.len());

                for item in items {
                    let expr = self.bind_expr(&item.expr, schema, grouping)?;
                    let name = match &item.alias {
                        Some(alias) => alias.clone(),
                        // A bare column reports the name the *source* declared,
                        // not the folded text the user typed. The two differ
                        // only for `sqlite()`, whose columns keep their original
                        // case — and there the alternative is that `SELECT *`
                        // and `SELECT visitcount` disagree about what the column
                        // is called.
                        None => match &expr {
                            CompiledExpr::Column(index) => schema
                                .columns
                                .get(*index)
                                .map(|column| column.name.clone())
                                .unwrap_or_else(|| default_column_name(&item.expr)),
                            _ => default_column_name(&item.expr),
                        },
                    };
                    columns.push(Column::new(name, self.type_of(&expr, schema)));
                    exprs.push(expr);
                }

                Ok((exprs, Schema::new(columns)))
            }
        }
    }

    /// Binds `ORDER BY`, resolving output aliases against the projection.
    ///
    /// `ORDER BY bytes DESC` where `bytes` is `SUM(size) AS bytes` has to mean
    /// the sum, not a column called `bytes` — and after aggregation there is no
    /// such column to find. So an alias match short-circuits to the expression
    /// the projection already bound.
    fn bind_order_by(
        &self,
        select: &Select,
        schema: &Schema,
        grouping: Option<&Grouping>,
        projection: &[CompiledExpr],
        output: &Schema,
    ) -> Result<Vec<SortKey>> {
        let mut keys = Vec::with_capacity(select.order_by.len());

        for order in &select.order_by {
            let expr = match self.alias_index(&order.expr, output) {
                Some(index) => projection
                    .get(index)
                    .cloned()
                    .ok_or_else(|| ZqlError::internal("projection index out of range"))?,
                None => self.bind_expr(&order.expr, schema, grouping)?,
            };

            keys.push(SortKey {
                expr,
                descending: order.descending,
                // Postgres puts NULLs at the "largest" end: last when
                // ascending, first when descending.
                nulls_first: match order.nulls {
                    Some(NullsOrder::First) => true,
                    Some(NullsOrder::Last) => false,
                    None => order.descending,
                },
            });
        }

        Ok(keys)
    }

    /// The output column index a bare name refers to, if any.
    fn alias_index(&self, expr: &Expr, output: &Schema) -> Option<usize> {
        let ExprKind::Column {
            qualifier: None,
            name,
        } = &expr.kind
        else {
            return None;
        };
        output
            .columns
            .iter()
            .position(|column| &column.name == name)
    }

    /// A predicate must be a condition, not a value.
    ///
    /// Checked at bind time so `WHERE size` fails before the first row is read
    /// and can carry a caret — an error raised mid-scan has no position.
    fn require_condition(
        &self,
        expr: &CompiledExpr,
        schema: &Schema,
        source: &Expr,
        clause: &str,
    ) -> Result<()> {
        if matches!(self.type_of(expr, schema), Type::Bool | Type::Unknown) {
            return Ok(());
        }
        Err(ZqlError::new(
            SqlState::DatatypeMismatch,
            format!("the {clause} clause must be a condition, not a value"),
        )
        .at(source.position)
        .with_hint("compare it against something, for example `size > 0`"))
    }

    /// The type of a bound expression.
    ///
    /// `CompiledExpr::result_type` cannot answer for a column reference — it
    /// holds an index, not a type — so the schema fills that in here. Keeping
    /// the lookup out of `CompiledExpr` is what lets the executor run without
    /// carrying a schema at all.
    fn type_of(&self, expr: &CompiledExpr, schema: &Schema) -> Type {
        match expr {
            CompiledExpr::Column(index) => schema
                .columns
                .get(*index)
                .map(|column| column.ty)
                .unwrap_or(Type::Unknown),
            other => other.result_type(),
        }
    }

    fn bind_expr(
        &self,
        expr: &Expr,
        schema: &Schema,
        grouping: Option<&Grouping>,
    ) -> Result<CompiledExpr> {
        // Under a GROUP BY, an expression that *is* a grouping key or an
        // aggregate becomes a reference into the aggregate's output. This is
        // checked before descending, because `lower(ext)` as a whole may be a
        // group key even though `ext` alone is not available.
        if let Some(grouping) = grouping {
            if let Some(index) = grouping.lookup(&fingerprint(expr)) {
                return Ok(CompiledExpr::Column(index));
            }
        }

        Ok(match &expr.kind {
            ExprKind::Literal(literal) => CompiledExpr::Literal(literal_value(literal)),

            ExprKind::Column { qualifier, name } => {
                if grouping.is_some() {
                    // The column survived the lookup above, so it is neither a
                    // group key nor inside an aggregate.
                    return Err(ZqlError::new(
                        SqlState::UndefinedColumn,
                        format!(
                            "column \"{name}\" must appear in GROUP BY or be used in an aggregate"
                        ),
                    )
                    .at(expr.position)
                    .with_hint("add it to GROUP BY, or wrap it in MIN(), MAX() or COUNT()"));
                }
                CompiledExpr::Column(self.resolve_column(
                    qualifier.as_deref(),
                    name,
                    schema,
                    expr,
                )?)
            }

            ExprKind::Unary { op, expr: inner } => CompiledExpr::Unary {
                op: *op,
                expr: Box::new(self.bind_expr(inner, schema, grouping)?),
            },

            ExprKind::Binary { op, left, right } => CompiledExpr::Binary {
                op: *op,
                left: Box::new(self.bind_expr(left, schema, grouping)?),
                right: Box::new(self.bind_expr(right, schema, grouping)?),
            },

            ExprKind::IsNull {
                expr: inner,
                negated,
            } => CompiledExpr::IsNull {
                expr: Box::new(self.bind_expr(inner, schema, grouping)?),
                negated: *negated,
            },

            ExprKind::Like {
                expr: inner,
                pattern,
                negated,
            } => CompiledExpr::Like {
                expr: Box::new(self.bind_expr(inner, schema, grouping)?),
                pattern: Box::new(self.bind_expr(pattern, schema, grouping)?),
                negated: *negated,
            },

            ExprKind::InList {
                expr: inner,
                list,
                negated,
            } => CompiledExpr::InList {
                expr: Box::new(self.bind_expr(inner, schema, grouping)?),
                list: list.iter().map(literal_value).collect(),
                negated: *negated,
            },

            ExprKind::Between {
                expr: inner,
                low,
                high,
                negated,
            } => CompiledExpr::Between {
                expr: Box::new(self.bind_expr(inner, schema, grouping)?),
                low: Box::new(self.bind_expr(low, schema, grouping)?),
                high: Box::new(self.bind_expr(high, schema, grouping)?),
                negated: *negated,
            },

            ExprKind::Case {
                branches,
                else_result,
            } => {
                let mut bound = Vec::with_capacity(branches.len());
                for branch in branches {
                    bound.push((
                        self.bind_expr(&branch.condition, schema, grouping)?,
                        self.bind_expr(&branch.result, schema, grouping)?,
                    ));
                }
                CompiledExpr::Case {
                    branches: bound,
                    else_result: match else_result {
                        Some(inner) => Some(Box::new(self.bind_expr(inner, schema, grouping)?)),
                        None => None,
                    },
                }
            }

            ExprKind::Cast { expr: inner, ty } => CompiledExpr::Cast {
                expr: Box::new(self.bind_expr(inner, schema, grouping)?),
                ty: type_named(ty, expr.position)?,
            },

            ExprKind::Function(call) => self.bind_function(call, expr, schema, grouping)?,
        })
    }

    fn bind_function(
        &self,
        call: &FunctionCall,
        expr: &Expr,
        schema: &Schema,
        grouping: Option<&Grouping>,
    ) -> Result<CompiledExpr> {
        // An aggregate reaching here was not matched by the grouping lookup,
        // which means there is no aggregation in this query at all.
        if AggFn::lookup(&call.name).is_some() {
            return Err(ZqlError::new(
                SqlState::UndefinedFunction,
                format!("{}() cannot be used here", call.name),
            )
            .at(expr.position)
            .with_hint("aggregates belong in the SELECT list, HAVING or ORDER BY"));
        }

        let Some(function) = ScalarFn::lookup(&call.name) else {
            let mut error = ZqlError::new(
                SqlState::UndefinedFunction,
                format!("no function named {}()", call.name),
            )
            .at(expr.position);
            if let Some(suggestion) = closest_function(&call.name) {
                error = error.with_hint(format!("did you mean {suggestion}()?"));
            }
            return Err(error);
        };

        if call.star {
            return Err(ZqlError::syntax(format!(
                "{}() does not take `*`",
                function.name()
            ))
            .at(expr.position));
        }
        if call.distinct {
            return Err(ZqlError::syntax(format!(
                "{}() does not take DISTINCT",
                function.name()
            ))
            .at(expr.position));
        }

        // Arity is checked here, so a miscounted argument list fails before
        // any row is read rather than on the first one.
        let (minimum, maximum) = function.arity();
        let given = call.args.len();
        if given < minimum || maximum.is_some_and(|max| given > max) {
            let expected = match maximum {
                Some(max) if max == minimum => format!("{minimum}"),
                Some(max) => format!("{minimum} to {max}"),
                None => format!("at least {minimum}"),
            };
            return Err(ZqlError::syntax(format!(
                "{}() takes {expected} arguments, not {given}",
                function.name()
            ))
            .at(expr.position));
        }

        let mut args = Vec::with_capacity(given);
        for arg in &call.args {
            args.push(self.bind_expr(arg, schema, grouping)?);
        }
        Ok(CompiledExpr::Function { function, args })
    }

    /// Name to index — the substitution the whole phase exists for.
    fn resolve_column(
        &self,
        qualifier: Option<&str>,
        name: &str,
        schema: &Schema,
        expr: &Expr,
    ) -> Result<usize> {
        if let Some(qualifier) = qualifier {
            return schema
                .index_of_qualified(qualifier, name)
                // A `sqlite()` column keeps the case it was declared with, so an
                // unquoted reference — which the lexer has folded — needs a
                // second look before this is really an error. See
                // `Schema::index_of_ignoring_case`.
                .or_else(|| schema.index_of_qualified_ignoring_case(qualifier, name))
                .ok_or_else(|| self.no_such_column(&format!("{qualifier}.{name}"), schema, expr));
        }

        match schema.count_named(name) {
            1 => schema
                .index_of(name)
                .ok_or_else(|| ZqlError::internal("column count disagreed with lookup")),
            0 => match schema.count_named_ignoring_case(name) {
                // Only reachable for a case-preserved column, and only once an
                // exact match has already failed — so no query that worked
                // before can change meaning here.
                1 => schema
                    .index_of_ignoring_case(name)
                    .ok_or_else(|| ZqlError::internal("column count disagreed with lookup")),
                0 => Err(self.no_such_column(name, schema, expr)),
                _ => Err(self.ambiguous(name, expr)),
            },
            _ => Err(self.ambiguous(name, expr)),
        }
    }

    fn ambiguous(&self, name: &str, expr: &Expr) -> ZqlError {
        ZqlError::new(
            SqlState::UndefinedColumn,
            format!("column \"{name}\" is ambiguous"),
        )
        .at(expr.position)
        .with_hint("qualify it with the source alias, for example `f.name`")
    }

    fn no_such_column(&self, name: &str, schema: &Schema, expr: &Expr) -> ZqlError {
        let error = ZqlError::new(
            SqlState::UndefinedColumn,
            format!("no column named \"{name}\""),
        )
        .at(expr.position);

        if schema.is_empty() {
            return error.with_hint("this query has no FROM clause, so there are no columns");
        }

        let available: Vec<&str> = schema
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        error.with_hint(format!("available columns: {}", available.join(", ")))
    }
}

/// One aggregate call found in the query.
struct AggregateCall {
    function: AggFn,
    argument: Option<Expr>,
    distinct: bool,
}

/// Finds every aggregate call in an expression, de-duplicated by fingerprint.
///
/// De-duplication is what makes `HAVING COUNT(*) > 5 ORDER BY COUNT(*)`
/// compute one count rather than two, and it is why the projection and the
/// `HAVING` can both refer to the same aggregate column.
fn collect_aggregates(expr: &Expr, found: &mut Vec<(String, AggregateCall)>) -> Result<()> {
    if let ExprKind::Function(call) = &expr.kind {
        if let Some(function) = AggFn::lookup(&call.name) {
            // `COUNT(*)` is the only aggregate that takes a star, and every
            // aggregate but `COUNT` needs exactly one argument.
            if call.star && function != AggFn::Count {
                return Err(ZqlError::syntax(format!(
                    "{}() does not take `*`",
                    call.name
                ))
                .at(expr.position));
            }
            if !call.star && call.args.len() != 1 {
                return Err(ZqlError::syntax(format!(
                    "{}() takes exactly one argument, not {}",
                    call.name,
                    call.args.len()
                ))
                .at(expr.position)
                .with_hint("count rows with COUNT(*)"));
            }

            for argument in &call.args {
                // `SUM(COUNT(x))` needs two aggregation passes, which zql does
                // not do. Naming it beats a confusing index error later.
                if contains_aggregate(argument) {
                    return Err(ZqlError::unsupported("aggregates inside aggregates")
                        .at(expr.position));
                }
            }

            let print = fingerprint(expr);
            if !found.iter().any(|(existing, _)| *existing == print) {
                found.push((
                    print,
                    AggregateCall {
                        function,
                        argument: call.args.first().cloned(),
                        distinct: call.distinct,
                    },
                ));
            }
            return Ok(());
        }
    }

    for child in children(expr) {
        collect_aggregates(child, found)?;
    }
    Ok(())
}

fn contains_aggregate(expr: &Expr) -> bool {
    if let ExprKind::Function(call) = &expr.kind {
        if AggFn::lookup(&call.name).is_some() {
            return true;
        }
    }
    children(expr).into_iter().any(contains_aggregate)
}

/// Every sub-expression of an expression.
fn children(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Column { .. } => Vec::new(),
        ExprKind::Unary { expr, .. }
        | ExprKind::IsNull { expr, .. }
        | ExprKind::Cast { expr, .. } => vec![expr],
        ExprKind::Binary { left, right, .. } => vec![left, right],
        ExprKind::Like { expr, pattern, .. } => vec![expr, pattern],
        ExprKind::InList { expr, .. } => vec![expr],
        ExprKind::Between {
            expr, low, high, ..
        } => vec![expr, low, high],
        ExprKind::Function(call) => call.args.iter().collect(),
        ExprKind::Case {
            branches,
            else_result,
        } => {
            let mut all: Vec<&Expr> = Vec::new();
            for branch in branches {
                all.push(&branch.condition);
                all.push(&branch.result);
            }
            all.extend(else_result.as_deref());
            all
        }
    }
}

/// A canonical string for an expression, ignoring source positions.
///
/// `GROUP BY ext` and `SELECT ext` are the same expression written at two
/// different offsets, so the derived `PartialEq` — which compares positions —
/// would say they differ. Comparing canonical forms is what lets a grouping key
/// be recognised wherever it reappears.
fn fingerprint(expr: &Expr) -> String {
    let mut out = String::new();
    write_fingerprint(expr, &mut out);
    out
}

fn write_fingerprint(expr: &Expr, out: &mut String) {
    match &expr.kind {
        ExprKind::Literal(literal) => {
            out.push_str(&format!("lit({literal:?})"));
        }
        ExprKind::Column { qualifier, name } => {
            // An unqualified reference and a qualified one to the same column
            // are deliberately *not* the same fingerprint: whether they resolve
            // alike depends on the schema, which this does not consult.
            out.push_str("col(");
            if let Some(qualifier) = qualifier {
                out.push_str(qualifier);
                out.push('.');
            }
            out.push_str(name);
            out.push(')');
        }
        ExprKind::Unary { op, expr } => {
            out.push_str(&format!("un({op:?},"));
            write_fingerprint(expr, out);
            out.push(')');
        }
        ExprKind::Binary { op, left, right } => {
            out.push_str(&format!("bin({op:?},"));
            write_fingerprint(left, out);
            out.push(',');
            write_fingerprint(right, out);
            out.push(')');
        }
        ExprKind::IsNull { expr, negated } => {
            out.push_str(&format!("isnull({negated},"));
            write_fingerprint(expr, out);
            out.push(')');
        }
        ExprKind::Like {
            expr,
            pattern,
            negated,
        } => {
            out.push_str(&format!("like({negated},"));
            write_fingerprint(expr, out);
            out.push(',');
            write_fingerprint(pattern, out);
            out.push(')');
        }
        ExprKind::InList {
            expr,
            list,
            negated,
        } => {
            out.push_str(&format!("in({negated},{list:?},"));
            write_fingerprint(expr, out);
            out.push(')');
        }
        ExprKind::Between {
            expr,
            low,
            high,
            negated,
        } => {
            out.push_str(&format!("between({negated},"));
            for part in [expr, low, high] {
                write_fingerprint(part, out);
                out.push(',');
            }
            out.push(')');
        }
        ExprKind::Cast { expr, ty } => {
            out.push_str(&format!("cast({ty},"));
            write_fingerprint(expr, out);
            out.push(')');
        }
        ExprKind::Function(call) => {
            out.push_str(&format!(
                "fn({},{},{},",
                call.name, call.distinct, call.star
            ));
            for argument in &call.args {
                write_fingerprint(argument, out);
                out.push(',');
            }
            out.push(')');
        }
        ExprKind::Case {
            branches,
            else_result,
        } => {
            out.push_str("case(");
            for branch in branches {
                write_fingerprint(&branch.condition, out);
                out.push(',');
                write_fingerprint(&branch.result, out);
                out.push(',');
            }
            if let Some(result) = else_result {
                write_fingerprint(result, out);
            }
            out.push(')');
        }
    }
}

/// The name a column gets when the user did not alias it.
///
/// A bare column keeps its own name; anything computed becomes `?column?`,
/// which is what Postgres calls an unnamed expression.
fn default_column_name(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Column { name, .. } => name.clone(),
        ExprKind::Function(call) => call.name.clone(),
        _ => "?column?".to_string(),
    }
}

fn literal_value(literal: &Literal) -> Value {
    match literal {
        Literal::Null => Value::Null,
        Literal::Bool(value) => Value::Bool(*value),
        Literal::Int(value) => Value::Int(*value),
        Literal::Real(value) => Value::Real(*value),
        Literal::String(value) => Value::Text(value.clone()),
    }
}

/// The type names accepted by `CAST`.
///
/// Both the SQL spellings and the Postgres ones, because a user who knows
/// Postgres will type `int8` and a user who knows SQLite will type `integer`,
/// and neither is wrong.
fn type_named(name: &str, position: u32) -> Result<Type> {
    Ok(match name {
        "text" | "varchar" | "char" | "string" => Type::Text,
        "int" | "integer" | "int4" | "int8" | "bigint" | "smallint" => Type::Int,
        "real" | "float" | "float8" | "double" | "numeric" | "decimal" => Type::Real,
        "bool" | "boolean" => Type::Bool,
        "timestamp" | "datetime" => Type::Timestamp,
        unknown => {
            return Err(ZqlError::new(
                SqlState::DatatypeMismatch,
                format!("unknown type \"{unknown}\""),
            )
            .at(position)
            .with_hint("zql casts to text, integer, real, boolean or timestamp"))
        }
    })
}

fn closest_function(name: &str) -> Option<&'static str> {
    let candidates = [
        "lower", "upper", "length", "substr", "trim", "replace", "abs", "round", "coalesce",
        "nullif", "typeof", "date", "datetime",
    ];
    candidates
        .into_iter()
        .find(|candidate| candidate.starts_with(name.get(..2).unwrap_or(name)))
}
