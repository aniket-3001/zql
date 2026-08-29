# zql — the frozen SQL subset

> **This is a specification, not code.** Grammar, semantics, and an explicit
> not-supported list. Written before kickoff so that on day one the parser has a fixed
> target instead of a growing one.
>
> **The rule this document exists to enforce:** scope creep in a SQL parser is the single
> most reliable way to lose a hackathon. Every "while I'm here, let me also support…" is
> a subquery away from a rewrite. **If it is not in §2, it returns `0A000` and that is a
> feature.**

---

## 1. The one-sentence contract

> zql speaks enough SQL to *inspect* data: select, filter, group, sort, limit, and join
> two sources. It does not modify anything, and it says so clearly when asked for
> something it does not have.

Read-only is not a limitation to apologise for — it is the product boundary. It removes
transactions, locking, constraint checking, and write-ahead logging from scope in one
stroke, and none of them appear in the demo.

---

## 2. The grammar

```ebnf
statement   = select | show | explain ;

select      = "SELECT" [ "DISTINCT" ] projection
              [ "FROM" from_item ]
              [ "WHERE" expr ]
              [ "GROUP" "BY" expr { "," expr } ]
              [ "HAVING" expr ]
              [ "ORDER" "BY" order_key { "," order_key } ]
              [ "LIMIT" integer ] [ "OFFSET" integer ] ;

projection  = "*" | proj_item { "," proj_item } ;
proj_item   = expr [ [ "AS" ] identifier ] ;

from_item   = source [ [ "AS" ] identifier ]
              [ join_kind "JOIN" source [ [ "AS" ] identifier ] "ON" expr ] ;

join_kind   = "INNER" | "LEFT" [ "OUTER" ] | ;      (* omitted = INNER *)

source      = identifier                            (* files, env *)
            | identifier "(" [ arg { "," arg } ] ")" ;  (* sqlite('x.db','t'), csv('y.csv') *)

order_key   = expr [ "ASC" | "DESC" ] [ "NULLS" ( "FIRST" | "LAST" ) ] ;

expr        = or_expr ;
or_expr     = and_expr { "OR" and_expr } ;
and_expr    = not_expr { "AND" not_expr } ;
not_expr    = [ "NOT" ] predicate ;
predicate   = sum [ ( "=" | "!=" | "<>" | "<" | "<=" | ">" | ">=" ) sum
                  | [ "NOT" ] "LIKE" sum
                  | [ "NOT" ] "IN" "(" literal { "," literal } ")"
                  | [ "NOT" ] "BETWEEN" sum "AND" sum
                  | "IS" [ "NOT" ] "NULL" ] ;
sum         = product { ( "+" | "-" | "||" ) product } ;
product     = unary { ( "*" | "/" | "%" ) unary } ;
unary       = [ "-" | "+" ] atom ;
atom        = literal
            | column_ref
            | function "(" [ "DISTINCT" ] ( "*" | expr { "," expr } ) ")"
            | "CAST" "(" expr "AS" type_name ")"
            | "CASE" { "WHEN" expr "THEN" expr } [ "ELSE" expr ] "END"
            | "(" expr ")" ;

column_ref  = identifier [ "." identifier ] ;
literal     = string | number | "TRUE" | "FALSE" | "NULL" ;
```

**Precedence, lowest to highest:** `OR` → `AND` → `NOT` → comparison / `LIKE` / `IN` /
`BETWEEN` / `IS` → `||` → `+` `-` → `*` `/` `%` → unary minus.

A Pratt (precedence-climbing) expression parser handles all of it in one function with a
binding-power table. That is why the parser is 800 lines and not 2,500 — the table is the
complexity, not the code.

---

## 3. Lexical rules

| Element | Rule |
| --- | --- |
| Identifiers | `[A-Za-z_][A-Za-z0-9_]*`, case-insensitive, folded to lowercase |
| Quoted identifiers | `"like this"` — case **preserved**, `""` escapes a quote |
| Strings | `'single quotes'`, `''` escapes a quote. No backslash escapes — Postgres-correct |
| Numbers | `123`, `1.5`, `1e10`, `-` is a unary operator, never part of the literal |
| Comments | `-- to end of line` and `/* block */`, non-nesting |
| Statement end | `;` optional; **only one statement per `Query` message** — a second returns `0A000` |
| Keywords | reserved; a quoted identifier can shadow one |
| Operators | no `::` — casts are spelled `CAST(x AS type)`, and `::` is refused by name |
| Nesting | expressions nest at most **50** levels; deeper is `54001`, not a crash |

**Case folding is the subtle one.** Unquoted identifiers fold to lowercase (Postgres
behaviour), but SQLite table names are case-sensitive as stored. So `sqlite('x.db', 'Users')`
takes its table name from a **string literal**, not an identifier — which sidesteps the
whole problem. That is why source arguments are strings.

*Column* names cannot use that escape — they are identifiers by nature, and the SQLite DDL
parser deliberately preserves the case each was declared with so `visitCount` displays as
written. A folded reference would therefore never match it. So column lookup falls back to a
case-insensitive match **after** an exact one fails, which is what SQLite itself does and
cannot change the meaning of a query that already resolved.

**The nesting limit is not a style rule, it is the one input that could end the process.**
Every phase after parsing walks an expression by recursion — binding, the `GROUP BY`
fingerprint, evaluation, and `Box<Expr>`'s own drop glue — so tree depth is stack depth. A
stack overflow aborts rather than unwinding, which puts it outside the `catch_unwind` net
around each connection: one client could take down every other one and the listener with it.
Capping depth where the tree is *built* closes all four at once. Measured before the cap: 
`SELECT 1+1+…+1` killed the server at 1,750 terms.

---

## 4. Functions

Aggregates and scalars are the entire function set. Both lists are closed.

### Aggregates

`COUNT(*)` · `COUNT(expr)` · `COUNT(DISTINCT expr)` · `SUM` · `AVG` · `MIN` · `MAX`

`COUNT(*)` counts rows; `COUNT(expr)` counts non-NULL values. Every other aggregate
**skips NULLs**, and returns NULL over an empty or all-NULL input — except `COUNT`, which
returns 0. This asymmetry is standard, frequently got wrong, and gets a test.

### Scalars

| Group | Functions |
| --- | --- |
| String | `LOWER` `UPPER` `LENGTH` `SUBSTR(s,start[,len])` `TRIM` `REPLACE` |
| Numeric | `ABS` `ROUND(x[,digits])` |
| Null | `COALESCE(a,b,…)` `NULLIF(a,b)` |
| Type | `TYPEOF(x)` |
| Time | `DATE(unix)` `DATETIME(unix)` |

`SUBSTR` is **1-indexed**, matching SQL rather than Rust. Note it in the doc comment; it is
exactly the sort of off-by-one that survives to the demo.

`DATE`/`DATETIME` use the days-from-civil algorithm — no `chrono`. UTC only, stated in the
README rather than silently assumed.

---

## 5. Sources

A source is either a bare name (a virtual table) or a function call.

| Source | Signature | Notes |
| --- | --- | --- |
| `files` | `files` or `files('path')` | defaults to cwd; recursive; **cached per session** |
| `env` | `env` | name, value |
| `sqlite` | `sqlite('file.db', 'table')` | read-only; refuses un-checkpointed WAL |
| `csv` | `csv('file.csv')` | header row required; types sniffed from first 100 rows |
| `json` | `json('file.json')` | array of flat objects only — **first cut candidate** |
| `tail` | `tail('file.log')` | streaming, never ends; needs cancellation to stop |

`files` columns: `path` `name` `ext` `size` `modified` `is_dir` `depth`.

Sources are resolved at **bind** time, so a bad path is `42P01` before execution starts —
not a half-streamed result that dies on row zero.

---

## 6. Semantics that must be right

### 6.1 Three-valued logic

`WHERE` admits a row **only** when the predicate evaluates to exactly `TRUE`. `NULL`
excludes, the same as `FALSE`.

| Expression | Result |
| --- | --- |
| `NULL = NULL` | `NULL` |
| `NULL <> NULL` | `NULL` |
| `NULL IS NULL` | `TRUE` |
| `TRUE OR NULL` | `TRUE` |
| `FALSE OR NULL` | `NULL` |
| `FALSE AND NULL` | `FALSE` |
| `TRUE AND NULL` | `NULL` |
| `NOT NULL` | `NULL` |

`OR` and `AND` short-circuit **only** where the answer is determined regardless of the
NULL — the table above is the specification, not the implementation shortcut.

### 6.2 Ordering and NULLs

Default sort order is ascending with **NULLs last**, matching Postgres. `NULLS FIRST`/
`NULLS LAST` override. Cross-type ordering follows: `NULL` < `Bool` < numbers < `Text` <
`Blob`, so a mixed column never panics on comparison.

### 6.3 `LIKE`

`%` matches any run, `_` matches one character, case-**sensitive** (Postgres semantics;
SQLite differs and the README says which we chose). Implemented as a two-pointer backtracking
matcher — not a regex engine, and not a recursive one that a pathological pattern can blow
the stack with.

### 6.4 Type coercion

Numeric comparison between `Int` and `Real` promotes to `Real`. **Text is never implicitly
compared to a number** — that is `42804`, not a silent zero. `CAST` is the explicit escape
hatch. Being strict here is both more correct and less code.

### 6.5 `||`

String concatenation, per SQL. Not logical-or. `NULL || 'x'` is `NULL`.

---

## 7. Explicitly not supported

Each returns `0A000` with a message naming the feature. **This list is a feature of the
project, and the README states it plainly rather than hiding it.**

| Not supported | Why |
| --- | --- |
| `INSERT` `UPDATE` `DELETE` `CREATE` `DROP` | read-only by design |
| Subqueries, CTEs, `WITH` | needs a correlated-execution model — days of work |
| Window functions | same |
| `UNION` `INTERSECT` `EXCEPT` | cheap-looking, but schema unification is not |
| `RIGHT`/`FULL OUTER JOIN` | `LEFT` covers the demo; swap the operands |
| Three-or-more-way joins in one `FROM` | grammar allows one join; nest via… nothing. Stated. |
| Extended protocol (`Parse`/`Bind`/`Execute`) | simple `Query` only — **this is why GUI clients are a stretch goal, never a promise** |
| Prepared statements, cursors, `COPY` | out of scope |
| Transactions | nothing to transact |
| Multiple statements per query | ambiguous result framing |

**On the extended protocol:** `psql` and node-postgres both work over simple `Query`.
DBeaver and TablePlus generally do not. Promise `psql`; treat anything else as a bonus
discovered on camera, never a rehearsed claim.

---

## 8. `SHOW` and `EXPLAIN`

Two non-standard conveniences, both cheap and both good demo material:

- `SHOW SOURCES` — lists available sources and their columns. This is the discoverability
  answer to "what can I even query?", which is the first thing any viewer thinks.
- `EXPLAIN <select>` — prints the plan tree as rows. ~40 lines because `Plan` is already an
  enum, and it is a strong Code Quality signal: it shows the engine has a real plan
  representation rather than an interpreter smeared through the parser.

Both are parsed as statements, not functions, so they cannot appear inside an expression.

---

## 9. Error message style

The parser reports **position and expectation**, not just "syntax error":

```
ERROR:  syntax error at or near "form"
LINE 1: SELECT * form files
                 ^
HINT:  did you mean FROM?
```

The caret line is what makes a hand-written parser look professional instead of homemade,
and `psql` renders `ErrorResponse` position fields natively — so the polish costs one
field, not a rendering engine. The `HINT` fires only on an edit-distance-1 match against
the keyword table.

---

## 10. Parser build gate

The subset above is the **whole** target for gate G2 (H+4–10). The gate is met when:

1. every example in §11 parses to the expected AST,
2. every construct in §7 returns `0A000` with a feature name,
3. a fuzz sweep of truncated and mangled query strings produces **zero panics**.

If G2 runs long, the cut order inside the subset is: `CASE` → `BETWEEN` → `IN` →
`DISTINCT` → `HAVING`. `JOIN` is **not** on this list; it is the utility feature.

---

## 11. Canonical examples

These double as the golden-query test file.

```sql
-- the cold open: a file the judge also has and cannot open
SELECT title, url FROM sqlite('places.sqlite', 'moz_places') ORDER BY visit_count DESC LIMIT 10;

-- what the tool is actually for: one query language over unlike things
SELECT f.name, f.size, m.owner
FROM files('src') AS f
JOIN csv('owners.csv') AS m ON f.name = m.filename
WHERE f.ext = 'rs' AND f.size > 1000
ORDER BY f.size DESC;

-- aggregation over the filesystem
SELECT ext, COUNT(*) AS n, SUM(size) AS bytes
FROM files
WHERE NOT is_dir
GROUP BY ext
HAVING COUNT(*) > 5
ORDER BY bytes DESC
LIMIT 10;

-- three-valued logic, on purpose
SELECT COUNT(*) FROM sqlite('app.db','t') WHERE note IS NULL;

-- streaming; Ctrl-C must stop this
SELECT line FROM tail('server.log') WHERE line LIKE '%ERROR%';

-- discoverability
SHOW SOURCES;
EXPLAIN SELECT ext, COUNT(*) FROM files GROUP BY ext;
```
