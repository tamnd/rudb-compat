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

/// Put a `DESCRIBE` and a result set together into one table.
///
/// Both arrive as records with the header first, whichever way the engine was made to write them,
/// so this is shared between the driver that reads a `COPY` and the one that reads a shell.
///
/// The column names come from the `DESCRIBE` rather than from the row header, because they are the
/// same names and the `DESCRIBE` is the one that also carries the types. They are checked against
/// each other anyway, since a disagreement means the two runs did not see the same query and every
/// value below is then lined up against the wrong column.
///
/// They are the same names with one exception, and it is why [`uniquely`] exists. A header is one
/// row of a CSV file and two columns in it cannot be told apart by name, so a writer that has to
/// produce one makes the names unique. `DESCRIBE` is a result set rather than a header and says
/// what the query really called them. So the check is against the names made unique the way the
/// writer makes them, and the names kept are the ones `DESCRIBE` gave.
///
/// # Errors
///
/// When the `DESCRIBE` is malformed, when the two disagree about how many columns there are, or
/// when they disagree about a name.
pub(crate) fn assemble(types: &[Vec<Cell>], rows: &[Vec<Cell>]) -> Result<Table, HarnessError> {
    let mut columns = Vec::with_capacity(types.len().saturating_sub(1));
    for record in types.iter().skip(1) {
        let name = match record.first() {
            Some(Cell::Text(t)) => t.clone(),
            _ => return Err(HarnessError::new("DESCRIBE returned a column with no name")),
        };
        let ty = match record.get(1) {
            Some(Cell::Text(t)) => t.clone(),
            _ => return Err(HarnessError::new(format!("DESCRIBE gave {name} no type"))),
        };
        columns.push(Column { name, ty });
    }

    let header = rows.first().map_or(&[][..], Vec::as_slice);
    if header.len() != columns.len() {
        return Err(HarnessError::new(format!(
            "the query returned {} columns and DESCRIBE named {}",
            header.len(),
            columns.len()
        )));
    }
    for (at, name) in uniquely(&columns).iter().enumerate() {
        if header[at] != Cell::Text(name.clone()) {
            return Err(HarnessError::new(format!(
                "column {at} is {} in the result and {name} in the DESCRIBE",
                header[at]
            )));
        }
    }

    Ok(Table { columns, rows: rows.iter().skip(1).cloned().collect() })
}

/// The column names as a CSV header has to spell them, which is with no two the same.
///
/// A name that is already taken gets `_1` after it, and if that is taken too then `_2`, and so on
/// until one is free. The suffix is tried against everything written so far and not only against
/// the name it came from, which is why a query with three columns called `a` and a fourth called
/// `a_1` gets `a`, `a_1`, `a_2`, `a_1_1`. That was read off the pinned DuckDB rather than guessed,
/// because the obvious rule and the real one disagree on exactly that case.
///
/// `SELECT {'a': 1}, struct_pack(a := 1)` is where this came up. Both of those are the same call
/// written two ways, so the query has two columns called `struct_pack(a := 1)`, and the harness was
/// reading the second one as the two engines having seen different queries.
fn uniquely(columns: &[Column]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::with_capacity(columns.len());
    for column in columns {
        let mut name = column.name.clone();
        let mut at = 1;
        while taken.contains(&name) {
            name = format!("{}_{at}", column.name);
            at += 1;
        }
        taken.push(name);
    }
    taken
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

    /// Throw away everything the engine has been told and start again.
    ///
    /// A sqllogictest file creates its own tables and expects them not to be there when it starts,
    /// so the runner in `crate::conform` calls this between files. The default does nothing, which
    /// is right for an engine that keeps no state between statements, and wrong for one that does,
    /// which is why it is on the trait rather than being something the runner does by rebuilding
    /// whichever engine it happens to know about.
    ///
    /// # Errors
    ///
    /// When the engine could not be restarted.
    fn reset(&mut self) -> Result<(), HarnessError> {
        Ok(())
    }

    /// What the last statement cost, when this engine is in a position to know.
    ///
    /// The goal is a tenth of DuckDB's time and a tenth of its memory, so the harness records the
    /// cost of every record beside the answer to it, per `spec/sql/duckdb/09-the-harness.md`
    /// section 9.7. The default is nothing, and nothing is the right answer for more engines than
    /// it looks like. `crate::rudb` links the engine as a library, so there is no child process to
    /// ask about and no honest way to separate one statement's peak from the process it shares
    /// with the harness. `crate::shell` runs both engines as processes through one driver that
    /// differs only in the path of the binary, which is the only place in this crate where the two
    /// sides are measured the same way, so it is the only place that answers this.
    ///
    /// An engine that cannot measure says so rather than estimating. A record with no number on
    /// one side is a record that is not in the denominator, which is the same rule as a record
    /// that failed or a record that was too fast.
    fn usage(&self) -> Option<crate::resource::Usage> {
        None
    }

    /// Whether this engine can hold more than one connection to the same database.
    ///
    /// A file that names a second connection, or says `reconnect`, is asking what one connection
    /// sees of another. An engine driven through one shell has one connection and nothing else, so
    /// the default is no and the runner ends the file there, which is the only honest thing to do
    /// with a record about isolation run on the connection that made the change.
    fn connections(&self) -> bool {
        false
    }

    /// Run one statement on the named connection, opening it the first time it is named, the way
    /// upstream's runner does.
    ///
    /// # Errors
    ///
    /// When the engine could not be run at all, and always for an engine where
    /// [`Engine::connections`] is false.
    fn run_on(&mut self, connection: &str, sql: &str) -> Result<Outcome, HarnessError> {
        let _ = sql;
        Err(HarnessError::new(format!("{} has no connection {connection}", self.name())))
    }

    /// Close the connection the file has been using and open a new one to the same database.
    ///
    /// # Errors
    ///
    /// When the engine could not reconnect, and always for an engine where
    /// [`Engine::connections`] is false.
    fn reconnect(&mut self) -> Result<(), HarnessError> {
        Err(HarnessError::new(format!("{} cannot open a second connection", self.name())))
    }
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
    use super::{Cell, Column, EngineError, assemble, uniquely};

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

    fn named(names: &[&str]) -> Vec<Column> {
        names.iter().map(|n| Column { name: (*n).to_owned(), ty: "INTEGER".to_owned() }).collect()
    }

    #[test]
    fn a_header_cannot_say_the_same_name_twice_so_the_second_one_is_numbered() {
        assert_eq!(uniquely(&named(&["a", "b"])), ["a", "b"]);
        assert_eq!(uniquely(&named(&["a", "a"])), ["a", "a_1"]);
        assert_eq!(uniquely(&named(&["a", "a", "a"])), ["a", "a_1", "a_2"]);
    }

    #[test]
    fn a_numbered_name_that_is_already_taken_is_numbered_again() {
        // Measured on the pinned DuckDB, because the obvious rule says a_1 here and the real one
        // does not: the suffix is tried against every name written so far and not only against the
        // one it came from. `SELECT 1 AS a, 2 AS a, 3 AS a, 4 AS a_1, 5 AS a` through COPY.
        assert_eq!(
            uniquely(&named(&["a", "a", "a", "a_1", "a"])),
            ["a", "a_1", "a_2", "a_1_1", "a_3"]
        );
    }

    #[test]
    fn the_names_that_are_kept_are_the_ones_describe_gave_and_not_the_numbered_ones() {
        // The whole point of the numbering is to check the header against. A query that asks for
        // the same expression twice really does have two columns of the same name, and a report
        // that renamed one of them would be reporting on a query nobody wrote.
        let types = vec![
            vec![Cell::Text("column_name".into()), Cell::Text("column_type".into())],
            vec![Cell::Text("s".into()), Cell::Text("INTEGER".into())],
            vec![Cell::Text("s".into()), Cell::Text("INTEGER".into())],
        ];
        let rows = vec![
            vec![Cell::Text("s".into()), Cell::Text("s_1".into())],
            vec![Cell::Text("1".into()), Cell::Text("1".into())],
        ];
        let table = assemble(&types, &rows).expect("the header is the names made unique");
        assert_eq!(table.columns[0].name, "s");
        assert_eq!(table.columns[1].name, "s");
        assert_eq!(table.rows.len(), 1);
    }
}
