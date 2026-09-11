//! Running a list of statements through two engines and writing down what happened.
//!
//! This is the smallest thing that is still the real shape. There is no reducer, no bisector and
//! no generator, and all three are named in `spec/14-rudb-compat.md` as the parts that make the
//! output usable at volume. What is here is the loop they all hang off, and the report they all
//! write into, because getting those two wrong is expensive later and cheap now.

use crate::compare::{Difference, MessageMatch, Rules, compare, compare_acceptance};
use crate::engine::{Engine, HarnessError};
use crate::rudb::ordering_of;

/// One statement's worth of comparison.
#[derive(Debug, Clone)]
pub struct Case {
    /// The statement, as written.
    pub sql: String,
    /// Everything the two engines disagreed about. Empty means they agreed.
    pub differences: Vec<Difference>,
}

impl Case {
    /// True when the two engines agreed on everything the rules asked about.
    #[must_use]
    pub fn agreed(&self) -> bool {
        self.differences.is_empty()
    }
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct Report {
    /// The left engine's name and version, for the header.
    pub left: String,
    /// The right engine's name and version.
    pub right: String,
    /// Every case, in the order they ran.
    pub cases: Vec<Case>,
}

impl Report {
    /// How many cases agreed.
    #[must_use]
    pub fn agreed(&self) -> usize {
        self.cases.iter().filter(|c| c.agreed()).count()
    }

    /// The share of cases that agreed, between zero and one.
    ///
    /// This is not the compatibility percentage in `spec/10-sql-and-types.md` section 10.7 and it
    /// must not be presented as one. That number is weighted by how much each construct is
    /// actually used, and this one weights `SELECT 1` and a seven way join the same. It is the
    /// number a developer watches while working, not a number that gets published.
    #[must_use]
    pub fn share(&self) -> f64 {
        if self.cases.is_empty() {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a corpus large enough for this to matter would not fit on the machine"
        )]
        {
            self.agreed() as f64 / self.cases.len() as f64
        }
    }
}

/// Run every statement through both engines.
///
/// The ordering rule is decided per statement rather than for the run, because it depends on
/// whether that statement said what its order is. The message rule is the run's, because how
/// closely error text has to match is a policy and not a property of the query.
///
/// # Errors
///
/// When either engine could not be run at all, which is a broken harness and not a failing case.
pub fn run(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    statements: &[String],
    messages: MessageMatch,
) -> Result<Report, HarnessError> {
    let mut cases = Vec::with_capacity(statements.len());
    for sql in statements {
        let rules = Rules { ordering: ordering_of(sql), messages };
        let a = left.run(sql)?;
        let b = right.run(sql)?;
        cases.push(Case { sql: sql.clone(), differences: compare(&a, &b, rules) });
    }
    Ok(Report {
        left: format!("{} {}", left.name(), left.version()),
        right: format!("{} {}", right.name(), right.version()),
        cases,
    })
}

/// Ask both engines about every statement without running any of them.
///
/// This is the mode that works today and it is not a placeholder for the one that does not. The
/// dialect is the compatibility claim, the grammar is vendored from DuckDB precisely so that the
/// dialect cannot drift, and this is the check that the vendoring worked. It stays useful after
/// there is an executor, because a query that fails to parse and a query that returns the wrong
/// answer are different bugs and mixing them in one number hides both.
///
/// # Errors
///
/// When either engine could not be run at all.
pub fn run_parse(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    statements: &[String],
    messages: MessageMatch,
) -> Result<Report, HarnessError> {
    let mut cases = Vec::with_capacity(statements.len());
    for sql in statements {
        let a = left.accepts(sql)?;
        let b = right.accepts(sql)?;
        cases.push(Case { sql: sql.clone(), differences: compare_acceptance(&a, &b, messages) });
    }
    Ok(Report {
        left: format!("{} {}", left.name(), left.version()),
        right: format!("{} {}", right.name(), right.version()),
        cases,
    })
}

/// Split a file of SQL into statements.
///
/// The split is on the tokenizer's statement terminator rather than on a `;` in the text, so a
/// semicolon inside a string literal, a dollar quoted block or a comment does not end a statement.
/// That distinction is not hypothetical for a corpus that contains any string at all, and the
/// tokenizer that answers it is the same one the parser uses, so the corpus is split the way the
/// engine would split it.
///
/// [`rudb::split`] is that same tokenizer, reached through the embedding API rather than through
/// `rudb-parse`, and it keeps the two things this harness needs that a plain `;` split does not:
/// text that does not tokenize comes back as one statement, because the harness's job is to hand
/// it to both engines and see what they say rather than to decide in advance that it is not SQL,
/// and a trailing block of comments is not a statement, because handing that to an engine gets an
/// error that is about the harness rather than about the corpus.
#[must_use]
pub fn statements(text: &str) -> Vec<String> {
    rudb::split(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Report, statements};
    use crate::compare::Difference;
    use crate::suite::Case;

    fn case(sql: &str, agreed: bool) -> Case {
        Case {
            sql: sql.to_owned(),
            differences: if agreed {
                Vec::new()
            } else {
                vec![Difference::Height { left: 1, right: 0 }]
            },
        }
    }

    #[test]
    fn a_semicolon_ends_a_statement_and_the_last_one_needs_none() {
        let got = statements("SELECT 1; SELECT 2");
        assert_eq!(got, vec!["SELECT 1".to_owned(), "SELECT 2".to_owned()]);
    }

    #[test]
    fn a_semicolon_inside_a_string_does_not_end_a_statement() {
        let got = statements("SELECT ';'; SELECT 2");
        assert_eq!(got, vec!["SELECT ';'".to_owned(), "SELECT 2".to_owned()]);
    }

    #[test]
    fn a_semicolon_inside_a_comment_does_not_end_a_statement() {
        let got = statements("SELECT 1 -- one; two\n; SELECT 2");
        assert_eq!(got.len(), 2);
        assert!(got[0].starts_with("SELECT 1"));
    }

    #[test]
    fn a_trailing_semicolon_does_not_produce_an_empty_statement() {
        assert_eq!(statements("SELECT 1;\n\n"), vec!["SELECT 1".to_owned()]);
    }

    #[test]
    fn a_file_of_only_comments_has_no_statements_in_it() {
        assert!(statements("-- nothing here\n").is_empty());
    }

    #[test]
    fn the_share_is_the_cases_that_agreed_and_nothing_cleverer() {
        let report = Report {
            left: "a".to_owned(),
            right: "b".to_owned(),
            cases: vec![case("SELECT 1", true), case("SELECT 2", false)],
        };
        assert_eq!(report.agreed(), 1);
        assert!((report.share() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_report_with_no_cases_is_zero_and_not_a_division_by_zero() {
        let report = Report { left: "a".to_owned(), right: "b".to_owned(), cases: Vec::new() };
        assert!(report.share().abs() < f64::EPSILON);
    }
}
