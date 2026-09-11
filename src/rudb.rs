//! rudb, as an engine the harness can point at.
//!
//! This used to answer only one question, whether a piece of text is SQL, because that was the
//! only one rudb could answer. It runs queries now, so this drives [`rudb::Database`] and hands
//! back real rows, and the parse only path stays because the two questions are still different.
//! A statement that fails to parse and a statement that returns the wrong answer are different
//! bugs, and one number covering both hides both.
//!
//! The database is held across statements. A corpus that creates a table and then selects from it
//! needs that, and it is also what makes [`Engine::reset`] necessary: a `.test` file expects to
//! start from an empty database, and one file leaving a table behind for the next one is a pass
//! that means nothing.
//!
//! Everything here goes through the `rudb` crate and nothing else. That is a real constraint and
//! not tidiness: this harness is the closest thing the project has to somebody embedding rudb, so
//! the moment it reaches past the embedding API for something, the embedding API is missing
//! something. It used to reach into `rudb-parse` for a tokenizer and into `rudb-common` for the
//! error type, and both of those reaches were holes in `rudb` rather than conveniences here.

use rudb::{Database, Error, ErrorCode, LogicalType, RowOrder, Value};

use crate::compare::Ordering;
use crate::engine::{Acceptance, Cell, Column, Engine, EngineError, HarnessError, Outcome, Table};

/// The rudb build this harness was linked against, and the database it is talking to.
#[derive(Debug)]
pub struct Rudb {
    version: String,
    database: Database,
}

impl Default for Rudb {
    fn default() -> Self {
        Self::new()
    }
}

impl Rudb {
    /// The rudb this crate is built against, with an empty database.
    #[must_use]
    pub fn new() -> Self {
        Self { version: format!("rudb {}", rudb_version()), database: Database::new() }
    }

    /// The database, for a caller that wants to look at the catalog after a run.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.database
    }
}

/// What version of rudb we linked, which is the version being tested.
fn rudb_version() -> &'static str {
    // Set by CI from the commit that was built. A local run says `from git`, which is honest: the
    // lock file says which commit and the report says to go and look at it.
    option_env!("RUDB_VERSION").unwrap_or("from git")
}

impl Engine for Rudb {
    fn name(&self) -> &str {
        "rudb"
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
        match self.database.execute(sql) {
            Ok(result) => Ok(Outcome::Rows(table(&result))),
            Err(e) => Ok(Outcome::Error(engine_error(&e))),
        }
    }

    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError> {
        // The grammar and not the AST, because acceptance is a question about the grammar. A
        // statement the AST has not reached yet still parses, and counting it as a rejection would
        // make the harness report a hole in the dialect where there is a hole in the AST.
        Ok(match rudb::accepts(sql) {
            Ok(_) => Acceptance::Accepted,
            Err(e) => Acceptance::Rejected(engine_error(&e)),
        })
    }

    fn reset(&mut self) -> Result<(), HarnessError> {
        self.database = Database::new();
        Ok(())
    }
}

/// Turn a rudb result into the table the comparison reads.
///
/// Values go across as the text rudb printed, for the reason [`Cell`] gives: the printed form is
/// itself part of what has to match DuckDB, so comparing text catches a printing difference that
/// comparing decoded values would let through.
fn table(result: &rudb::QueryResult) -> Table {
    let columns = result
        .names()
        .iter()
        .zip(result.types())
        .map(|(name, ty)| Column { name: name.clone(), ty: type_name(ty) })
        .collect();
    let rows = result
        .rows()
        .map(|row| {
            row.into_iter()
                .map(|value| match value {
                    Value::Null => Cell::Null,
                    other => Cell::Text(other.to_string()),
                })
                .collect()
        })
        .collect();
    Table { columns, rows }
}

/// A logical type, spelled the way DuckDB spells it in `DESCRIBE`.
fn type_name(ty: &LogicalType) -> String {
    ty.to_string()
}

/// Turn a rudb error into the form the comparison reads.
///
/// The code comes across as itself rather than through its printed form, because `ErrorCode`
/// already carries DuckDB's exact spelling including the parts that look like typos, and going
/// through text would mean parsing back out something we have in hand.
fn engine_error(error: &Error) -> EngineError {
    EngineError { kind: error.code().duckdb_name().to_owned(), message: error.message().to_owned() }
}

/// The error a statement rudb cannot run yet produces, for a caller that wants to recognize one.
///
/// Worth having a name for. A run against the corpus is mostly this today, and telling it apart
/// from a wrong answer is the difference between a list of things to build and a list of bugs.
#[must_use]
pub fn is_not_implemented(error: &EngineError) -> bool {
    error.kind == ErrorCode::NotImplemented.duckdb_name()
}

/// Whether the query fixes its own row order.
///
/// Section 14.2 says results are compared in order when the query has an `ORDER BY` and sorted
/// when it does not. Deciding which needs a parser, and rudb has one, so [`rudb::row_order`]
/// answers it: a query orders itself when its top level has an order clause, and an `ORDER BY`
/// inside a subquery does not count because it does not survive into the outer result.
///
/// The third answer, the one that says rudb could not read the text, is a policy decision and it
/// belongs here rather than in the engine. This harness says `AsWritten`, so an order it cannot
/// check shows up as a difference rather than disappearing into a sort. A false failure costs
/// someone a look at a report. A false pass costs a user their data coming back in an order the
/// query said it would not.
#[must_use]
pub fn ordering_of(sql: &str) -> Ordering {
    match rudb::row_order(sql) {
        RowOrder::Declared | RowOrder::Unknown => Ordering::AsWritten,
        RowOrder::Unspecified => Ordering::Sorted,
    }
}

#[cfg(test)]
mod tests {
    use super::{Rudb, ordering_of};
    use crate::compare::Ordering;
    use crate::engine::{Cell, Engine, Outcome};

    #[test]
    fn text_that_is_not_sql_gets_duckdbs_own_error_kind() {
        let mut rudb = Rudb::new();
        let Outcome::Error(e) = rudb.run("SELECT FROM WHERE").unwrap() else {
            panic!("that is not valid SQL");
        };
        assert_eq!(e.kind, "Parser Error");
    }

    #[test]
    fn a_query_comes_back_as_rows_with_the_names_and_types_rudb_gave_them() {
        let mut rudb = Rudb::new();
        let Outcome::Rows(table) = rudb.run("SELECT 1 AS a, 'x' AS b").unwrap() else {
            panic!("that is a query");
        };
        assert_eq!(table.width(), 2);
        assert_eq!(table.height(), 1);
        assert_eq!(table.columns[0].name, "a");
        assert_eq!(table.columns[0].ty, "INTEGER");
        assert_eq!(table.rows[0][1], Cell::Text("x".to_owned()));
    }

    #[test]
    fn a_null_comes_back_as_a_null_and_not_as_the_text_of_one() {
        let mut rudb = Rudb::new();
        let Outcome::Rows(table) = rudb.run("SELECT NULL").unwrap() else {
            panic!("that is a query");
        };
        assert_eq!(table.rows[0][0], Cell::Null);
    }

    #[test]
    fn the_database_is_held_across_statements_so_a_table_survives_until_it_is_reset() {
        let mut rudb = Rudb::new();
        assert!(rudb.run("CREATE TABLE t (a INTEGER)").unwrap().is_rows());
        assert!(rudb.run("INSERT INTO t VALUES (1)").unwrap().is_rows());
        let Outcome::Rows(table) = rudb.run("SELECT a FROM t").unwrap() else {
            panic!("that is a query");
        };
        assert_eq!(table.height(), 1);

        rudb.reset().unwrap();
        let Outcome::Error(e) = rudb.run("SELECT a FROM t").unwrap() else {
            panic!("the table is gone");
        };
        assert_eq!(e.kind, "Catalog Error");
    }

    #[test]
    fn a_statement_that_writes_returns_no_columns_rather_than_a_count() {
        let mut rudb = Rudb::new();
        rudb.run("CREATE TABLE t (a INTEGER)").unwrap();
        let Outcome::Rows(table) = rudb.run("INSERT INTO t VALUES (1)").unwrap() else {
            panic!("an insert is not an error");
        };
        assert_eq!(table.width(), 0);
    }

    #[test]
    fn a_top_level_order_by_makes_the_order_part_of_the_answer() {
        assert_eq!(ordering_of("SELECT x FROM t ORDER BY x"), Ordering::AsWritten);
        assert_eq!(ordering_of("SELECT x FROM t"), Ordering::Sorted);
    }

    #[test]
    fn an_order_by_inside_a_subquery_does_not_order_the_outer_result() {
        let sql = "SELECT x FROM (SELECT x FROM t ORDER BY x)";
        assert_eq!(ordering_of(sql), Ordering::Sorted);
    }

    #[test]
    fn the_word_order_in_a_string_is_not_an_order_by() {
        assert_eq!(ordering_of("SELECT 'order by x'"), Ordering::Sorted);
    }

    #[test]
    fn text_rudb_cannot_read_is_treated_as_ordered() {
        // The engine says it does not know, and the harness turns that into the cautious answer.
        assert_eq!(ordering_of("SELECT 'unterminated"), Ordering::AsWritten);
        assert_eq!(ordering_of("SELECT FROM WHERE"), Ordering::AsWritten);
    }
}
