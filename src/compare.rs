//! Comparing two outcomes, and saying exactly where they stopped agreeing.
//!
//! The rules are `spec/14-rudb-compat.md` section 14.2 in the rudb repository. The whole result
//! set, not a hash and not a row count. Values, types and column names. Order when the query says
//! what the order is, sorted when it does not. Errors are results, so an error on one side and
//! rows on the other is a failure whichever side is which.
//!
//! Every difference carries enough to reproduce it by hand. A report that says two results are not
//! equal is a report that costs an afternoon to act on, and there are going to be thousands of
//! them.

use std::fmt;

use crate::engine::{Acceptance, Cell, Column, EngineError, Outcome, Table};

/// Whether row order is part of the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ordering {
    /// The query fixes the order, so the rows are compared where they are.
    AsWritten,
    /// The query does not fix the order, so both sides are sorted before comparison.
    Sorted,
}

/// How closely two error messages have to agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageMatch {
    /// Only the kind, which is the part before the colon. This is the floor and it applies to
    /// every error, because a program that branches on the kind is a program that breaks when the
    /// kind is wrong.
    Kind,
    /// The kind and the first line of the message. Section 12.5 requires this for the errors a
    /// user is expected to read and act on, and requiring it everywhere would freeze wording that
    /// upstream changes freely.
    Headline,
}

/// What a comparison was told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Whether to sort before comparing.
    pub ordering: Ordering,
    /// How closely errors have to match.
    pub messages: MessageMatch,
}

impl Default for Rules {
    fn default() -> Self {
        Self { ordering: Ordering::Sorted, messages: MessageMatch::Kind }
    }
}

/// One way in which two outcomes disagreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// One side ran and the other did not.
    OneErrored {
        /// Which side errored.
        side: Side,
        /// What it said.
        error: EngineError,
    },
    /// One side parsed the text and the other did not.
    ///
    /// This is the difference that matters most and it is the only one the harness can find today.
    /// A statement rudb rejects and DuckDB accepts is a query a user cannot run at all, and one
    /// rudb accepts and DuckDB rejects is a dialect we invented.
    OneRejected {
        /// Which side rejected it.
        side: Side,
        /// What it said.
        error: EngineError,
    },
    /// Both errored, with different kinds.
    ErrorKind {
        /// The left engine's kind.
        left: String,
        /// The right engine's kind.
        right: String,
    },
    /// Both errored with the same kind and different messages.
    ErrorMessage {
        /// The left engine's first line.
        left: String,
        /// The right engine's first line.
        right: String,
    },
    /// The results are different widths.
    Width {
        /// How many columns the left engine returned.
        left: usize,
        /// How many the right engine returned.
        right: usize,
    },
    /// A column has a different name.
    ColumnName {
        /// Which column, counting from zero.
        at: usize,
        /// The left engine's name.
        left: String,
        /// The right engine's name.
        right: String,
    },
    /// A column has a different type.
    ColumnType {
        /// Which column, counting from zero.
        at: usize,
        /// The left engine's type.
        left: String,
        /// The right engine's type.
        right: String,
    },
    /// The results are different heights.
    Height {
        /// How many rows the left engine returned.
        left: usize,
        /// How many the right engine returned.
        right: usize,
    },
    /// One side did not come back with anything, because it came apart.
    ///
    /// [`compare`] never produces this one, because a comparison needs two outcomes and this is
    /// what is left when there is only one. It is here rather than in the caller so that a crash
    /// reads as a difference everywhere a difference is read, and so that it can never match
    /// anything: no engine reports an error of this kind, so two engines cannot both crash into
    /// agreement.
    Panicked {
        /// Which side came apart.
        side: Side,
        /// What it said on the way down.
        message: String,
    },
    /// A value differs.
    Value {
        /// Which row, counting from zero, after sorting when the rules said to sort.
        row: usize,
        /// The column's name, because a column number sends the reader back to the query.
        column: String,
        /// The left engine's value.
        left: Cell,
        /// The right engine's value.
        right: Cell,
    },
}

impl Difference {
    /// What kind of difference this is, in a form two of them can be compared by.
    ///
    /// The reducer keeps a step when the difference survives it, and surviving has to mean the same
    /// difference rather than any difference at all. A cut that turns a wrong answer into a parse
    /// error has not reduced anything, it has thrown one bug away and found another, and a reducer
    /// that accepts it walks off the bug it was pointed at and reports something nobody asked about.
    ///
    /// So this keeps what makes one of these the bug it is and drops what makes it this instance of
    /// it. The side, the error kind and the panic message stay, because those are the bug. The
    /// values, the row and the column number go, because those are exactly what shrinking moves.
    #[must_use]
    pub fn signature(&self) -> String {
        match self {
            Self::OneErrored { side, error } => format!("only {side} errored, {}", error.kind),
            Self::OneRejected { side, error } => format!("only {side} rejected it, {}", error.kind),
            Self::ErrorKind { left, right } => format!("error kind {left} against {right}"),
            Self::ErrorMessage { .. } => "error text".to_owned(),
            Self::Width { .. } => "width".to_owned(),
            Self::ColumnName { .. } => "column name".to_owned(),
            Self::ColumnType { .. } => "column type".to_owned(),
            Self::Height { .. } => "height".to_owned(),
            Self::Panicked { side, message } => format!("{side} panicked, {message}"),
            Self::Value { .. } => "value".to_owned(),
        }
    }
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OneErrored { side, error } => write!(f, "only {side} errored, with {error}"),
            Self::OneRejected { side, error } => {
                write!(f, "only {side} says this is not SQL, with {error}")
            }
            Self::ErrorKind { left, right } => write!(f, "error kind {left} against {right}"),
            Self::ErrorMessage { left, right } => {
                write!(f, "error text {left:?} against {right:?}")
            }
            Self::Width { left, right } => write!(f, "{left} columns against {right}"),
            Self::ColumnName { at, left, right } => {
                write!(f, "column {at} is named {left:?} against {right:?}")
            }
            Self::ColumnType { at, left, right } => {
                write!(f, "column {at} has type {left} against {right}")
            }
            Self::Height { left, right } => write!(f, "{left} rows against {right}"),
            Self::Panicked { side, message } => write!(f, "{side} panicked, with {message}"),
            Self::Value { row, column, left, right } => {
                write!(f, "row {row} column {column} is {left} against {right}")
            }
        }
    }
}

/// Which engine a difference is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The first engine passed to `compare`, which is DuckDB by convention.
    Left,
    /// The second, which is rudb by convention.
    Right,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Left => "the left engine",
            Self::Right => "the right engine",
        })
    }
}

/// Compare two outcomes under the given rules.
///
/// An empty result means they agreed. The list is not exhaustive in one respect that is on
/// purpose: once the widths differ there is no useful way to line the columns up, so the width
/// difference is reported alone rather than followed by one value difference per cell.
#[must_use]
pub fn compare(left: &Outcome, right: &Outcome, rules: Rules) -> Vec<Difference> {
    match (left, right) {
        (Outcome::Error(a), Outcome::Error(b)) => errors(a, b, rules.messages),
        (Outcome::Error(a), Outcome::Rows(_)) => {
            vec![Difference::OneErrored { side: Side::Left, error: a.clone() }]
        }
        (Outcome::Rows(_), Outcome::Error(b)) => {
            vec![Difference::OneErrored { side: Side::Right, error: b.clone() }]
        }
        (Outcome::Rows(a), Outcome::Rows(b)) => tables(a, b, rules.ordering),
    }
}

/// Compare what two engines think of a piece of text, without running it.
///
/// The message rule applies here the same way it applies to a run. Two engines that both reject a
/// statement agree about the dialect whatever they say about it, and whether they have to say the
/// same thing is a separate and much stricter question.
#[must_use]
pub fn compare_acceptance(
    left: &Acceptance,
    right: &Acceptance,
    messages: MessageMatch,
) -> Vec<Difference> {
    match (left, right) {
        (Acceptance::Accepted, Acceptance::Accepted) => Vec::new(),
        (Acceptance::Rejected(a), Acceptance::Accepted) => {
            vec![Difference::OneRejected { side: Side::Left, error: a.clone() }]
        }
        (Acceptance::Accepted, Acceptance::Rejected(b)) => {
            vec![Difference::OneRejected { side: Side::Right, error: b.clone() }]
        }
        (Acceptance::Rejected(a), Acceptance::Rejected(b)) => errors(a, b, messages),
    }
}

/// Both sides errored, so the question is whether they errored the same way.
fn errors(left: &EngineError, right: &EngineError, how: MessageMatch) -> Vec<Difference> {
    if left.kind != right.kind {
        return vec![Difference::ErrorKind { left: left.kind.clone(), right: right.kind.clone() }];
    }
    if how == MessageMatch::Headline && left.headline() != right.headline() {
        return vec![Difference::ErrorMessage {
            left: left.headline().to_owned(),
            right: right.headline().to_owned(),
        }];
    }
    Vec::new()
}

/// Both sides returned rows.
fn tables(left: &Table, right: &Table, ordering: Ordering) -> Vec<Difference> {
    if left.width() != right.width() {
        return vec![Difference::Width { left: left.width(), right: right.width() }];
    }

    let mut found = Vec::new();
    for (at, (a, b)) in left.columns.iter().zip(&right.columns).enumerate() {
        found.extend(column(at, a, b));
    }

    if left.height() != right.height() {
        found.push(Difference::Height { left: left.height(), right: right.height() });
        return found;
    }

    let (a, b) = match ordering {
        Ordering::AsWritten => (left.rows.clone(), right.rows.clone()),
        Ordering::Sorted => {
            let mut a = left.rows.clone();
            let mut b = right.rows.clone();
            a.sort();
            b.sort();
            (a, b)
        }
    };

    for (row, (x, y)) in a.iter().zip(&b).enumerate() {
        for (at, (p, q)) in x.iter().zip(y).enumerate() {
            if p != q {
                found.push(Difference::Value {
                    row,
                    column: left.columns[at].name.clone(),
                    left: p.clone(),
                    right: q.clone(),
                });
            }
        }
    }
    found
}

/// One column's name and type.
fn column(at: usize, left: &Column, right: &Column) -> Vec<Difference> {
    let mut found = Vec::new();
    if left.name != right.name {
        found.push(Difference::ColumnName {
            at,
            left: left.name.clone(),
            right: right.name.clone(),
        });
    }
    if left.ty != right.ty {
        found.push(Difference::ColumnType { at, left: left.ty.clone(), right: right.ty.clone() });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{Difference, MessageMatch, Ordering, Rules, Side, compare, compare_acceptance};
    use crate::engine::{Acceptance, Cell, Column, EngineError, Outcome, Table};

    fn table(names: &[&str], rows: &[&[&str]]) -> Outcome {
        Outcome::Rows(Table {
            columns: names
                .iter()
                .map(|n| Column { name: (*n).to_owned(), ty: "INTEGER".to_owned() })
                .collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|c| Cell::Text((*c).to_owned())).collect())
                .collect(),
        })
    }

    fn error(kind: &str, message: &str) -> Outcome {
        Outcome::Error(EngineError { kind: kind.to_owned(), message: message.to_owned() })
    }

    #[test]
    fn two_engines_that_both_parse_something_have_nothing_to_say_about_it() {
        let found =
            compare_acceptance(&Acceptance::Accepted, &Acceptance::Accepted, MessageMatch::Kind);
        assert!(found.is_empty());
    }

    #[test]
    fn a_statement_only_one_engine_parses_names_which_one_and_what_it_said() {
        let rejected = Acceptance::Rejected(EngineError {
            kind: "Parser Error".to_owned(),
            message: "syntax error at or near \"ASCENDING\"".to_owned(),
        });
        let found = compare_acceptance(&rejected, &Acceptance::Accepted, MessageMatch::Kind);
        assert!(matches!(found.as_slice(), [Difference::OneRejected { side: Side::Left, .. }]));
        let found = compare_acceptance(&Acceptance::Accepted, &rejected, MessageMatch::Kind);
        assert!(matches!(found.as_slice(), [Difference::OneRejected { side: Side::Right, .. }]));
    }

    #[test]
    fn two_engines_that_both_reject_something_agree_about_the_dialect() {
        let a = Acceptance::Rejected(EngineError {
            kind: "Parser Error".to_owned(),
            message: "one thing".to_owned(),
        });
        let b = Acceptance::Rejected(EngineError {
            kind: "Parser Error".to_owned(),
            message: "another thing".to_owned(),
        });
        assert!(compare_acceptance(&a, &b, MessageMatch::Kind).is_empty());
        assert!(!compare_acceptance(&a, &b, MessageMatch::Headline).is_empty());
    }

    #[test]
    fn two_identical_results_have_nothing_to_say() {
        let a = table(&["x"], &[&["1"], &["2"]]);
        assert!(compare(&a, &a, Rules::default()).is_empty());
    }

    #[test]
    fn without_an_order_by_the_rows_are_sorted_first() {
        let a = table(&["x"], &[&["1"], &["2"]]);
        let b = table(&["x"], &[&["2"], &["1"]]);
        assert!(compare(&a, &b, Rules::default()).is_empty());
        let strict = Rules { ordering: Ordering::AsWritten, ..Rules::default() };
        assert_eq!(compare(&a, &b, strict).len(), 2);
    }

    #[test]
    fn a_column_name_is_part_of_the_answer() {
        let a = table(&["x"], &[&["1"]]);
        let b = table(&["y"], &[&["1"]]);
        assert!(matches!(
            compare(&a, &b, Rules::default()).as_slice(),
            [Difference::ColumnName { at: 0, .. }]
        ));
    }

    #[test]
    fn a_type_difference_is_reported_even_when_every_value_agrees() {
        let a = table(&["x"], &[&["1"]]);
        let Outcome::Rows(mut wrong) = table(&["x"], &[&["1"]]) else { unreachable!() };
        wrong.columns[0].ty = "BIGINT".to_owned();
        let found = compare(&a, &Outcome::Rows(wrong), Rules::default());
        assert!(matches!(found.as_slice(), [Difference::ColumnType { .. }]));
    }

    #[test]
    fn different_widths_stop_the_comparison_rather_than_producing_noise() {
        let a = table(&["x", "y"], &[&["1", "2"]]);
        let b = table(&["x"], &[&["1"]]);
        assert_eq!(
            compare(&a, &b, Rules::default()),
            vec![Difference::Width { left: 2, right: 1 }]
        );
    }

    #[test]
    fn a_row_count_difference_does_not_also_report_every_value() {
        let a = table(&["x"], &[&["1"], &["2"]]);
        let b = table(&["x"], &[&["1"]]);
        assert_eq!(
            compare(&a, &b, Rules::default()),
            vec![Difference::Height { left: 2, right: 1 }]
        );
    }

    #[test]
    fn succeeding_where_the_other_side_failed_is_a_failure_both_ways_round() {
        let rows = table(&["x"], &[&["1"]]);
        let err = error("Binder Error", "no");
        assert!(matches!(
            compare(&err, &rows, Rules::default()).as_slice(),
            [Difference::OneErrored { side: Side::Left, .. }]
        ));
        assert!(matches!(
            compare(&rows, &err, Rules::default()).as_slice(),
            [Difference::OneErrored { side: Side::Right, .. }]
        ));
    }

    #[test]
    fn two_errors_of_the_same_kind_agree_until_the_rules_ask_about_the_text() {
        let a = error("Parser Error", "syntax error at or near \"a\"");
        let b = error("Parser Error", "syntax error at or near \"b\"");
        assert!(compare(&a, &b, Rules::default()).is_empty());
        let strict = Rules { messages: MessageMatch::Headline, ..Rules::default() };
        assert!(matches!(compare(&a, &b, strict).as_slice(), [Difference::ErrorMessage { .. }]));
    }

    #[test]
    fn a_different_kind_of_error_is_a_difference_whatever_the_text_says() {
        let a = error("Parser Error", "same");
        let b = error("Binder Error", "same");
        assert!(matches!(
            compare(&a, &b, Rules::default()).as_slice(),
            [Difference::ErrorKind { .. }]
        ));
    }

    #[test]
    fn a_null_and_an_empty_string_are_a_difference() {
        let a = Outcome::Rows(Table {
            columns: vec![Column { name: "x".to_owned(), ty: "VARCHAR".to_owned() }],
            rows: vec![vec![Cell::Null]],
        });
        let b = table(&["x"], &[&[""]]);
        let Outcome::Rows(mut b) = b else { unreachable!() };
        b.columns[0].ty = "VARCHAR".to_owned();
        assert!(matches!(
            compare(&a, &Outcome::Rows(b), Rules::default()).as_slice(),
            [Difference::Value { .. }]
        ));
    }
}
