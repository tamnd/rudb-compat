//! What an engine is, from the harness's point of view.
//!
//! Two things implement this: a real DuckDB, which is a binary of a named version on the machine,
//! and rudb, which is a library this crate links. The harness never knows which is which. That
//! matters because a comparison written against one specific engine ends up encoding that engine's
//! quirks in the comparison, and then the comparison stops being able to see them.
//!
//! `spec/14-rudb-compat.md` section 14.2 in the rudb repository is the design.

use std::fmt;

/// What running one statement produced.
///
/// An error is a result and not an absence of one, per section 14.2. A query that errors on DuckDB
/// has to error here too, so the comparison in `crate::compare` treats a pair of errors as
/// something to check rather than as something to skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The statement produced a result set. Zero rows is still this and not an error.
    Rows(Table),
    /// The statement failed and the engine said why.
    Error(EngineError),
}

impl Outcome {
    /// True when the statement produced rows rather than an error.
    #[must_use]
    pub const fn is_rows(&self) -> bool {
        matches!(self, Self::Rows(_))
    }
}

/// An engine's error, split the way DuckDB spells it.
///
/// DuckDB writes `Parser Error: syntax error at or near "foo"`, and the part before the first
/// colon is a closed set of about twenty kinds that `rudb-common` already reproduces exactly.
/// Splitting them is what makes the weaker comparison possible: section 12.5 requires the message
/// to match only for some kinds, and the kind to match for all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError {
    /// The prefix, such as `Parser Error` or `Binder Error`, without the colon.
    pub kind: String,
    /// Everything after the colon, with the leading space and the trailing newline removed.
    pub message: String,
}

impl EngineError {
    /// Split an engine's error text into a kind and a message.
    ///
    /// Anything that does not have a recognizable prefix gets the kind `Error`, which is what
    /// DuckDB itself falls back to and is a kind two engines can still disagree about.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        match text.split_once(": ") {
            Some((kind, rest)) if kind.ends_with("Error") && !kind.contains('\n') => {
                Self { kind: kind.to_owned(), message: rest.trim().to_owned() }
            }
            _ => Self { kind: "Error".to_owned(), message: text.to_owned() },
        }
    }

    /// The first line of the message, which is the part without DuckDB's source context.
    ///
    /// DuckDB appends a blank line, the offending line of SQL and a caret to most errors. That
    /// part is generated from the byte offset and comparing it compares the offset twice, so the
    /// message comparison uses this and the offset gets compared on its own when we carry one.
    #[must_use]
    pub fn headline(&self) -> &str {
        self.message.split('\n').next().unwrap_or("").trim()
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

/// A result set, in full.
///
/// Section 14.2 says the comparison is on the full result set and not on a hash and not on a row
/// count, so this holds every value. A hash would make the harness cheap and the failure reports
/// useless, and the failure report is the entire product.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    /// One per column, in the order the engine returned them.
    pub columns: Vec<Column>,
    /// One per row, each the same length as `columns`.
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    /// How many columns wide the result is.
    #[must_use]
    pub fn width(&self) -> usize {
        self.columns.len()
    }

    /// How many rows the result has.
    #[must_use]
    pub fn height(&self) -> usize {
        self.rows.len()
    }
}

/// A column's name and the type the engine gave it.
///
/// Both are compared. A query that returns the right numbers under the wrong column name breaks
/// every tool that reads results by name, and one that returns them as `DOUBLE` where DuckDB says
/// `DECIMAL(18,3)` breaks every tool that reads them by type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The name as the engine reports it, including whatever it decided an unaliased expression
    /// is called.
    pub name: String,
    /// The logical type, spelled the way DuckDB spells it in `DESCRIBE`.
    pub ty: String,
}

/// One value.
///
/// Values are carried as the text the engine printed rather than as a decoded number, and that is
/// a decision rather than laziness. `spec/12-duckdb-compat.md` requires that a value prints the
/// way DuckDB prints it, so the printed form is already something that has to match exactly, and
/// comparing decoded values would let a printing difference through while comparing text catches
/// both. It also keeps the harness out of the business of having its own decimal and float
/// parsers, which would be two more places for the comparison to be wrong in a way that looks
/// like the engine being wrong.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cell {
    /// SQL NULL, which is not the empty string and is never compared equal to it.
    Null,
    /// The value as the engine printed it.
    Text(String),
}

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Text(t) => write!(f, "{t:?}"),
        }
    }
}

/// Something that can run a statement and say what happened.
pub trait Engine {
    /// What to call this engine in a report.
    fn name(&self) -> &str;

    /// The exact version, which goes in the report next to every number that came out of it.
    ///
    /// Section 14.1: a percentage without a version attached to it does not mean anything, because
    /// the surface moves between releases.
    fn version(&self) -> &str;

    /// Run one statement.
    ///
    /// The error type is for the harness failing, not for the statement failing. A statement that
    /// fails is an `Outcome::Error` and is a normal result. A missing binary, a crashed process or
    /// unreadable output is a `HarnessError`, because that means the harness did not learn
    /// anything about the statement and reporting a difference would be a lie.
    ///
    /// # Errors
    ///
    /// When the engine could not be run at all.
    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError>;

    /// Decide whether the text is SQL, without running it.
    ///
    /// This is separate from `run` because the two questions have different answers and only one
    /// of them can be asked today. `SELECT * FROM nosuchtable` is SQL and it does not run, and an
    /// engine that reported the catalog error here would be answering the wrong question. It is
    /// also the only question rudb can answer at all until there is an executor, which is why the
    /// harness has a mode that asks nothing else.
    ///
    /// # Errors
    ///
    /// When the engine could not be run at all.
    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError>;
}

/// Whether an engine thinks a piece of text is SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acceptance {
    /// It parses. Nothing is claimed about whether it would run.
    Accepted,
    /// It does not parse, and this is what the engine said about it.
    Rejected(EngineError),
}

impl Acceptance {
    /// True when the engine parsed the text.
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// The harness itself failed, which is different from the statement failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessError(pub String);

impl HarnessError {
    /// Build one from anything printable.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HarnessError {}

#[cfg(test)]
mod tests {
    use super::{Cell, EngineError};

    #[test]
    fn an_error_splits_into_the_kind_duckdb_spells_and_the_rest() {
        let e = EngineError::parse("Parser Error: syntax error at or near \"foo\"");
        assert_eq!(e.kind, "Parser Error");
        assert_eq!(e.message, "syntax error at or near \"foo\"");
    }

    #[test]
    fn the_source_context_duckdb_appends_is_not_part_of_the_headline() {
        let e = EngineError::parse(
            "Catalog Error: Scalar Function with name frobnicate does not exist!\n\nLINE 1: SELECT frobnicate(1)\n               ^",
        );
        assert_eq!(e.kind, "Catalog Error");
        assert_eq!(e.headline(), "Scalar Function with name frobnicate does not exist!");
    }

    #[test]
    fn text_with_a_colon_in_it_does_not_become_a_kind() {
        let e = EngineError::parse("something went wrong: badly");
        assert_eq!(e.kind, "Error");
        assert_eq!(e.message, "something went wrong: badly");
    }

    #[test]
    fn a_null_is_not_an_empty_string() {
        assert_ne!(Cell::Null, Cell::Text(String::new()));
    }
}
