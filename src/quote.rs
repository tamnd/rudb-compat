//! Reading back what a DuckDB shell wrote in `.mode quote`.
//!
//! This is the shell's answer to the problem [`crate::csv`] solves with `FORCE_QUOTE *`. The
//! harness has to be able to tell a NULL from the empty string and from the four letter string
//! `NULL`, and most of the shell's output modes cannot: `.mode csv` writes all three as nothing,
//! nothing and `NULL`, and `.mode list` writes the last two the same way.
//!
//! `.mode quote` is the one mode that keeps them apart, because it quotes by type rather than by
//! need. A string is always wrapped in single quotes with embedded quotes doubled, a number and a
//! boolean are always bare, and a NULL is always the bare word `NULL`. So `NULL`, `''` and
//! `'NULL'` are three different sequences of bytes and the ambiguity is gone without asking the
//! shell to write to a file.
//!
//! It is not a general reader and should not become one. It reads what the two shells write, and
//! both of them were checked byte for byte before this was written.

use crate::engine::{Cell, HarnessError};

/// Parse the output of one statement run under `.mode quote`.
///
/// The first record is the header and comes back as the first element, because a result set always
/// has one even when it has no rows. Empty text is not an error here and comes back as no records
/// at all, which is what a statement with no result set produces and is a thing the caller has to
/// be able to tell apart from a statement that returned nothing.
///
/// # Errors
///
/// When a quoted field never closes, when a record is a different width from the header, or when
/// something other than a separator follows a field.
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
                    "the shell wrote a {} where a separator belongs, at byte {i}",
                    char::from(*other)
                )));
            }
        }
    }

    let width = records.first().map_or(0, Vec::len);
    for (n, record) in records.iter().enumerate().skip(1) {
        if record.len() != width {
            return Err(HarnessError::new(format!(
                "record {n} of what the shell wrote is {} wide and the header is {width} wide",
                record.len()
            )));
        }
    }
    Ok(records)
}

/// One field, starting at `at`, returning it and the offset of the separator after it.
///
/// A bare field is a number, a boolean, or the word `NULL`. Only the last of those becomes a null,
/// and the others come across as the text they were printed as, for the reason [`Cell`] gives:
/// the printed form is itself part of what has to match, so decoding it here would let a printing
/// difference through.
fn field(bytes: &[u8], at: usize) -> Result<(Cell, usize), HarnessError> {
    if bytes.get(at) != Some(&b'\'') {
        let mut end = at;
        while end < bytes.len() && !matches!(bytes[end], b',' | b'\n' | b'\r') {
            end += 1;
        }
        let text = String::from_utf8_lossy(&bytes[at..end]).into_owned();
        let cell = if text == "NULL" { Cell::Null } else { Cell::Text(text) };
        return Ok((cell, end));
    }

    let mut out = String::new();
    let mut i = at + 1;
    loop {
        match bytes.get(i) {
            None => {
                return Err(HarnessError::new(
                    "a quoted field in what the shell wrote never closes",
                ));
            }
            Some(b'\'') if bytes.get(i + 1) == Some(&b'\'') => {
                out.push('\'');
                i += 2;
            }
            Some(b'\'') => return Ok((Cell::Text(out), i + 1)),
            Some(_) => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\'' {
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

    /// The exact bytes both shells write for the row that carries every case at once.
    ///
    /// Taken off `v2.0.0-dev84237 (Development Version) cc7e7bac7f` and off the rudb shell built
    /// at the pinned commit, which printed the same thing character for character. That agreement
    /// is the reason this reader can be this small.
    const EVERYTHING: &str = "'n','e','q','nl','c','i','d','b','word'\nNULL,'','it''s','two\nlines','has,comma',1,1.5,true,'NULL'\n";

    #[test]
    fn a_null_an_empty_string_and_the_word_null_are_three_different_things() {
        let got = read(EVERYTHING).unwrap();
        assert_eq!(got[1][0], Cell::Null);
        assert_eq!(got[1][1], text(""));
        assert_eq!(got[1][8], text("NULL"));
    }

    #[test]
    fn a_doubled_quote_is_one_quote_and_a_comma_inside_quotes_is_not_a_separator() {
        let got = read(EVERYTHING).unwrap();
        assert_eq!(got[1][2], text("it's"));
        assert_eq!(got[1][4], text("has,comma"));
    }

    #[test]
    fn a_newline_inside_a_quoted_field_stays_in_the_value() {
        let got = read(EVERYTHING).unwrap();
        assert_eq!(got[1][3], text("two\nlines"));
        assert_eq!(got.len(), 2, "the newline inside the value did not start a record");
    }

    #[test]
    fn a_number_and_a_boolean_arrive_as_the_text_they_were_printed_as() {
        let got = read(EVERYTHING).unwrap();
        assert_eq!(got[1][5], text("1"));
        assert_eq!(got[1][6], text("1.5"));
        assert_eq!(got[1][7], text("true"));
    }

    #[test]
    fn a_result_with_no_rows_is_a_header_and_nothing_else() {
        let got = read("'a'\n").unwrap();
        assert_eq!(got, vec![vec![text("a")]]);
    }

    #[test]
    fn a_statement_with_no_result_set_writes_nothing_and_that_is_not_an_error() {
        assert_eq!(read("").unwrap(), Vec::<Vec<Cell>>::new());
    }

    #[test]
    fn a_record_that_is_the_wrong_width_is_an_error_and_not_a_short_row() {
        let err = read("'a','b'\n1\n").unwrap_err();
        assert!(err.0.contains("wide"), "{err}");
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        assert!(read("'a'\n'oops\n").is_err());
    }

    #[test]
    fn a_last_record_with_no_trailing_newline_still_arrives() {
        let got = read("'a'\n1").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], vec![text("1")]);
    }
}
