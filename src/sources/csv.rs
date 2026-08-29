//! `csv('file.csv')` — a CSV file as a table.
//!
//! Stands in for the `csv` crate. The parser follows RFC 4180 where that
//! matters and is forgiving where being strict would only annoy:
//!
//! - fields are separated by commas, and a field may be double-quoted,
//! - inside quotes, `""` is one literal quote,
//! - **a quoted field may contain newlines**, which is the rule that makes
//!   line-by-line splitting wrong and is the reason this is a state machine
//!   rather than a `split(',')`,
//! - either `\n` or `\r\n` ends a record.
//!
//! # An unquoted empty field is NULL; a quoted one is the empty string
//!
//! `a,,b` has a NULL in the middle and `a,"",b` has an empty string. That is
//! the distinction Postgres's `COPY` makes, it is the only way for a CSV to
//! express "missing" at all, and it matters as soon as the column is compared
//! or aggregated.
//!
//! # Types are sniffed, not declared
//!
//! A CSV carries no type information, and rendering every column as text makes
//! numbers sort lexicographically — `10` before `9` — which is wrong in a way
//! that looks like a bug in the engine. So the first 100 data rows decide:
//! all-integers gives an integer column, all-numbers gives a real one, anything
//! else is text. NULLs abstain.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::values::ValuesIter;
use crate::exec::RowIter;
use crate::plan::schema::{Column, Schema};
use crate::server::cancel::CancelFlag;
use crate::sources::TableSource;
use crate::value::{Row, Type, Value};

/// How many data rows are examined before the column types are fixed.
const SNIFF_ROWS: usize = 100;

/// A ceiling on file size, so a mis-typed path at a multi-gigabyte file is an
/// error rather than an out-of-memory kill.
const MAX_BYTES: u64 = 512 * 1024 * 1024;

pub struct CsvSource {
    schema: Schema,
    rows: Vec<Row>,
}

impl CsvSource {
    /// Reads and parses the file.
    ///
    /// All of it happens at bind time. Sniffing needs the first hundred rows
    /// before the schema can be stated, and `RowDescription` needs the schema
    /// before any row goes out — so there is nothing to be gained by deferring.
    pub fn open(path: &Path) -> Result<CsvSource> {
        let size = fs::metadata(path)
            .map_err(|err| io_error(path, &err))?
            .len();
        if size > MAX_BYTES {
            return Err(ZqlError::new(
                SqlState::ProgramLimitExceeded,
                format!("{} is larger than {MAX_BYTES} bytes", path.display()),
            ));
        }

        let bytes = fs::read(path).map_err(|err| io_error(path, &err))?;
        // Lossy rather than strict: a stray byte in one field should cost that
        // character, not the whole query.
        let text = String::from_utf8_lossy(&bytes);
        // Excel writes a UTF-8 BOM by default. Left in place it becomes part of
        // the first header name, so `SELECT name` fails against a column that
        // prints as exactly `name` in the error message — realistic, and
        // invisible until you hexdump the file.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

        let records = parse(text);
        let mut records = records.into_iter();

        let Some(header) = records.next() else {
            return Err(ZqlError::new(
                SqlState::IoError,
                format!("{} is empty; a CSV needs a header row", path.display()),
            ));
        };

        let names = column_names(header);
        let data: Vec<Vec<Field>> = records.collect();
        let types = sniff(&data, names.len());

        let schema = Schema::new(
            names
                .iter()
                .zip(&types)
                .map(|(name, ty)| Column::new(name, *ty))
                .collect(),
        );

        let rows = data
            .into_iter()
            .map(|record| {
                let mut values = Vec::with_capacity(names.len());
                for (index, ty) in types.iter().enumerate() {
                    // A short record is padded rather than rejected: a trailing
                    // comma missing from one line should not fail the file.
                    values.push(match record.get(index) {
                        Some(field) => convert(field, *ty),
                        None => Value::Null,
                    });
                }
                Row::new(values)
            })
            .collect();

        Ok(CsvSource { schema, rows })
    }
}

impl TableSource for CsvSource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn scan(&self, _cancel: &CancelFlag) -> Result<Box<dyn RowIter>> {
        Ok(Box::new(ValuesIter::new(self.rows.clone())))
    }
}

/// One parsed field, remembering whether it was quoted.
///
/// The flag is not decoration: it is the whole of the difference between a
/// missing value and an empty one.
#[derive(Debug, Clone, PartialEq)]
struct Field {
    text: String,
    quoted: bool,
}

/// Splits the whole file into records of fields.
///
/// A single pass with two pieces of state — whether we are inside quotes, and
/// whether the current field began with a quote. Everything awkward about CSV
/// falls out of getting those two right.
fn parse(text: &str) -> Vec<Vec<Field>> {
    let mut records = Vec::new();
    let mut record: Vec<Field> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    // Distinguishes a genuinely empty final record from a trailing newline.
    let mut started = false;

    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        started = true;
        if in_quotes {
            if character == '"' {
                // A doubled quote is one literal quote; a lone one ends the
                // quoted section.
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(character);
            }
            continue;
        }

        match character {
            '"' if field.is_empty() => {
                in_quotes = true;
                quoted = true;
            }
            ',' => {
                record.push(Field {
                    text: std::mem::take(&mut field),
                    quoted,
                });
                quoted = false;
            }
            '\n' => {
                record.push(Field {
                    text: std::mem::take(&mut field),
                    quoted,
                });
                quoted = false;
                records.push(std::mem::take(&mut record));
                started = false;
            }
            // Swallowed only when it precedes a newline, so a lone carriage
            // return inside data survives.
            '\r' if chars.peek() == Some(&'\n') => {}
            other => field.push(other),
        }
    }

    // A file not ending in a newline still has a last record.
    if started || !field.is_empty() || !record.is_empty() {
        record.push(Field { text: field, quoted });
        records.push(record);
    }

    records
}

/// Header names, with blanks filled in so every column can be referred to.
fn column_names(header: Vec<Field>) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(header.len());

    for (index, field) in header.into_iter().enumerate() {
        // zql folds unquoted identifiers to lower case, so a header of `Name`
        // would otherwise be unreachable without quoting it in every query.
        let mut name = field.text.trim().to_lowercase();
        if name.is_empty() {
            name = format!("column{}", index + 1);
        }
        // Duplicate headers are common in exported spreadsheets, and two
        // identically-named columns would be permanently ambiguous.
        if names.contains(&name) {
            name = format!("{name}_{}", index + 1);
        }
        names.push(name);
    }

    names
}

/// Decides each column's type from the first [`SNIFF_ROWS`] data rows.
fn sniff(rows: &[Vec<Field>], width: usize) -> Vec<Type> {
    let mut types = Vec::with_capacity(width);

    for index in 0..width {
        let mut any = false;
        let mut all_integers = true;
        let mut all_numbers = true;

        for record in rows.iter().take(SNIFF_ROWS) {
            let Some(field) = record.get(index) else {
                continue;
            };
            // A NULL abstains: a column of numbers with one gap is still a
            // column of numbers.
            if is_null(field) {
                continue;
            }
            // A quoted field is text by the author's own declaration, so
            // quoted digits — a zip code, a phone number — stay text and keep
            // their leading zeros.
            if field.quoted {
                all_integers = false;
                all_numbers = false;
                break;
            }

            any = true;
            let trimmed = field.text.trim();
            if trimmed.parse::<i64>().is_err() {
                all_integers = false;
            }
            if trimmed.parse::<f64>().is_err() {
                all_numbers = false;
            }
            if !all_numbers {
                break;
            }
        }

        types.push(if !any {
            Type::Text
        } else if all_integers {
            Type::Int
        } else if all_numbers {
            Type::Real
        } else {
            Type::Text
        });
    }

    types
}

fn is_null(field: &Field) -> bool {
    !field.quoted && field.text.trim().is_empty()
}

fn convert(field: &Field, ty: Type) -> Value {
    if is_null(field) {
        return Value::Null;
    }
    let trimmed = field.text.trim();
    match ty {
        // A value that does not fit the sniffed type falls back to text rather
        // than becoming NULL: losing data is worse than an untidy column.
        Type::Int => trimmed
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Text(field.text.clone())),
        Type::Real => trimmed
            .parse::<f64>()
            .map(Value::Real)
            .unwrap_or_else(|_| Value::Text(field.text.clone())),
        _ => Value::Text(field.text.clone()),
    }
}

fn io_error(path: &Path, err: &std::io::Error) -> ZqlError {
    ZqlError::new(
        SqlState::IoError,
        format!("cannot read {}: {err}", path.display()),
    )
}

/// Used by the source registry to report the path in errors.
pub fn path_of(argument: &str) -> PathBuf {
    PathBuf::from(argument)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(record: &[Field]) -> Vec<&str> {
        record.iter().map(|field| field.text.as_str()).collect()
    }

    #[test]
    fn plain_records_split_on_commas_and_newlines() {
        let parsed = parse("a,b,c\n1,2,3\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(fields(&parsed[0]), vec!["a", "b", "c"]);
        assert_eq!(fields(&parsed[1]), vec!["1", "2", "3"]);
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_last_record() {
        let parsed = parse("a,b\n1,2");
        assert_eq!(parsed.len(), 2);
        assert_eq!(fields(&parsed[1]), vec!["1", "2"]);
    }

    #[test]
    fn crlf_endings_do_not_leave_carriage_returns_in_the_data() {
        let parsed = parse("a,b\r\n1,2\r\n");
        assert_eq!(fields(&parsed[1]), vec!["1", "2"]);
    }

    #[test]
    fn quoted_fields_may_contain_commas_and_newlines() {
        // The rule that makes line-splitting wrong.
        let parsed = parse("a,b\n\"x,y\",\"line1\nline2\"\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(fields(&parsed[1]), vec!["x,y", "line1\nline2"]);
    }

    #[test]
    fn a_doubled_quote_is_one_literal_quote() {
        let parsed = parse("a\n\"she said \"\"hi\"\"\"\n");
        assert_eq!(fields(&parsed[1]), vec!["she said \"hi\""]);
    }

    #[test]
    fn an_unquoted_empty_field_is_null_and_a_quoted_one_is_not() {
        let parsed = parse("a,b,c\n1,,3\n4,\"\",6\n");
        assert!(is_null(&parsed[1][1]), "a,, should be NULL");
        assert!(!is_null(&parsed[2][1]), "\"\" should be an empty string");
    }

    #[test]
    fn types_are_sniffed_per_column() {
        let records = parse("n,x,s\n1,1.5,a\n2,2.5,b\n");
        let types = sniff(&records[1..], 3);
        assert_eq!(types, vec![Type::Int, Type::Real, Type::Text]);
    }

    #[test]
    fn a_null_does_not_stop_a_column_being_numeric() {
        let records = parse("n\n1\n\n3\n");
        assert_eq!(sniff(&records[1..], 1), vec![Type::Int]);
    }

    #[test]
    fn quoted_digits_stay_text_so_leading_zeros_survive() {
        // A zip code is not a number, and the author said so by quoting it.
        let records = parse("zip\n\"01234\"\n\"05678\"\n");
        assert_eq!(sniff(&records[1..], 1), vec![Type::Text]);
    }

    #[test]
    fn an_integer_column_with_one_decimal_becomes_real() {
        let records = parse("n\n1\n2.5\n");
        assert_eq!(sniff(&records[1..], 1), vec![Type::Real]);
    }

    #[test]
    fn header_names_are_folded_deduplicated_and_never_blank() {
        let header = parse("Name,,Name\n").remove(0);
        assert_eq!(column_names(header), vec!["name", "column2", "name_3"]);
    }

    #[test]
    fn a_value_that_contradicts_the_sniffed_type_stays_as_text() {
        // Row 101 onward was never sniffed, so this is genuinely reachable.
        let field = Field {
            text: "not a number".to_string(),
            quoted: false,
        };
        assert!(matches!(convert(&field, Type::Int), Value::Text(_)));
    }

    #[test]
    fn a_utf8_bom_does_not_become_part_of_the_first_column_name() {
        // What Excel writes by default. Unstripped, the first header is
        // BOM+"name" and nothing can reach that column.
        let raw = "\u{feff}Name,Qty\r\nwidget,3\r\n";
        let stripped = raw.strip_prefix('\u{feff}').unwrap_or(raw);
        let header = parse(stripped).remove(0);
        assert_eq!(column_names(header), vec!["name", "qty"]);
    }

    #[test]
    fn an_empty_input_produces_no_records() {
        assert!(parse("").is_empty());
    }
}
