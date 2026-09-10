//! Reading back what DuckDB's CSV writer wrote.
//!
//! This is not a general CSV reader and it should not become one. It reads exactly the dialect
//! DuckDB's `COPY ... TO` produces under the options `crate::duckdb` passes, which is comma
//! separated, quoted with `"`, embedded quotes doubled, and `FORCE_QUOTE *` so that every value
//! that is not NULL arrives quoted.
//!
//! `FORCE_QUOTE *` is the whole reason this file exists rather than a dependency. It makes an
//! unquoted empty field mean NULL and a quoted empty field mean the empty string, which is the one
//! distinction a CSV round trip normally destroys and the one the harness cannot afford to lose.

use crate::engine::{Cell, HarnessError};

/// Parse the output of one `COPY ... TO ... (FORMAT csv, HEADER, FORCE_QUOTE *)`.
///
/// The first record is the header and comes back as the first element. A file with no records at
/// all is an error rather than an empty table, because `HEADER` means there is always at least
/// one, so no records means the write did not happen.
///
/// # Errors
///
/// When a quoted field never closes, when a record is a different width from the header, or when
/// the file is empty.
pub fn read(text: &str) -> Result<Vec<Vec<Cell>>, HarnessError> {
    let bytes = text.as_bytes();
    let mut records: Vec<Vec<Cell>> = Vec::new();
    let mut record: Vec<Cell> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let (cell, next) = field(bytes, i)?;
        record.push(cell);
        i = next;
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b'\n') => {
                i += 1;
                records.push(std::mem::take(&mut record));
            }
            Some(b'\r') if bytes.get(i + 1) == Some(&b'\n') => {
                i += 2;
                records.push(std::mem::take(&mut record));
            }
            None => records.push(std::mem::take(&mut record)),
            Some(other) => {
                return Err(HarnessError::new(format!(
                    "the CSV DuckDB wrote has a {} where a separator belongs, at byte {i}",
                    char::from(*other)
                )));
            }
        }
    }

    let Some(header) = records.first() else {
        return Err(HarnessError::new(
            "DuckDB wrote no CSV at all, and HEADER means there is always at least one record",
        ));
    };
    let width = header.len();
    for (n, record) in records.iter().enumerate().skip(1) {
        if record.len() != width {
            return Err(HarnessError::new(format!(
                "record {n} of the CSV DuckDB wrote is {} wide and the header is {width} wide",
                record.len()
            )));
        }
    }
    Ok(records)
}

/// One field, starting at `at`, returning it and the offset of the separator after it.
fn field(bytes: &[u8], at: usize) -> Result<(Cell, usize), HarnessError> {
    if bytes.get(at) != Some(&b'"') {
        let mut end = at;
        while end < bytes.len() && !matches!(bytes[end], b',' | b'\n' | b'\r') {
            end += 1;
        }
        if end == at {
            return Ok((Cell::Null, end));
        }
        // Nothing unquoted but NULL should ever reach here, so anything that does is worth saying
        // out loud rather than accepting as text. It means the writer options changed under us.
        let text = String::from_utf8_lossy(&bytes[at..end]).into_owned();
        return Err(HarnessError::new(format!(
            "DuckDB wrote the unquoted field {text:?}, and under FORCE_QUOTE the only unquoted field is NULL"
        )));
    }

    let mut out = String::new();
    let mut i = at + 1;
    loop {
        match bytes.get(i) {
            None => {
                return Err(HarnessError::new(
                    "a quoted field in the CSV DuckDB wrote never closes",
                ));
            }
            Some(b'"') if bytes.get(i + 1) == Some(&b'"') => {
                out.push('"');
                i += 2;
            }
            Some(b'"') => return Ok((Cell::Text(out), i + 1)),
            Some(_) => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                out.push_str(&String::from_utf8_lossy(&bytes[start..i]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read;
    use crate::engine::Cell;

    fn text(s: &str) -> Cell {
        Cell::Text(s.to_owned())
    }

    #[test]
    fn a_bare_field_is_null_and_a_quoted_empty_one_is_the_empty_string() {
        let got = read("\"a\",\"b\"\n,\"\"\n").unwrap();
        assert_eq!(got[0], vec![text("a"), text("b")]);
        assert_eq!(got[1], vec![Cell::Null, text("")]);
    }

    #[test]
    fn a_doubled_quote_is_one_quote_and_a_comma_inside_quotes_is_not_a_separator() {
        let got = read("\"c\"\n\"has,comma\"\"q\"\n").unwrap();
        assert_eq!(got[1], vec![text("has,comma\"q")]);
    }

    #[test]
    fn a_newline_inside_a_quoted_field_stays_in_the_value() {
        let got = read("\"c\"\n\"two\nlines\"\n").unwrap();
        assert_eq!(got[1], vec![text("two\nlines")]);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn a_record_that_is_the_wrong_width_is_an_error_and_not_a_short_row() {
        let err = read("\"a\",\"b\"\n\"1\"\n").unwrap_err();
        assert!(err.0.contains("wide"), "{err}");
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        assert!(read("\"a\"\n\"oops\n").is_err());
    }

    #[test]
    fn a_last_record_with_no_trailing_newline_still_arrives() {
        let got = read("\"a\"\n\"1\"").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], vec![text("1")]);
    }
}
