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
//! There are two of them, and the difference is [`Optimizer`]. [`Rudb::new`] is the engine somebody
//! embedding rudb gets and [`Rudb::unoptimized`] is the same engine with every rewrite turned off,
//! which the corpus runs as well so the two can be required to agree.
//!
//! Everything here goes through the `rudb` crate and nothing else. That is a real constraint and
//! not tidiness: this harness is the closest thing the project has to somebody embedding rudb, so
//! the moment it reaches past the embedding API for something, the embedding API is missing
//! something. It used to reach into `rudb-parse` for a tokenizer and into `rudb-common` for the
//! error type, and both of those reaches were holes in `rudb` rather than conveniences here.

use std::time::Duration;

use rudb::{Config, Database, Error, ErrorCode, LogicalType, RowOrder, Value};

use crate::compare::Ordering;
use crate::engine::{Acceptance, Cell, Column, Engine, EngineError, HarnessError, Outcome, Table};

/// The rudb build this harness was linked against, and the database it is talking to.
#[derive(Debug)]
pub struct Rudb {
    version: String,
    config: Config,
    optimizer: Optimizer,
    database: Database,
}

/// Whether the optimizer passes run at all.
///
/// Off is the interesting one. The plan the binder produced is the right answer by construction,
/// since every pass is a rewrite that is supposed to keep the answer and only the passes can break
/// that, so a corpus file that answers one way with them off and another way with them on has found
/// a pass that changed an answer. That is the strongest property the harness can check without
/// anybody writing down a new expected output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Optimizer {
    /// Every pass runs, which is what a real query gets.
    On,
    /// No pass runs, which is the bound plan going straight to the executor.
    Off,
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
        Self::with_config(Config::new())
    }

    /// A database that stops a statement itself, rather than waiting to be killed from outside.
    ///
    /// This is what the isolating runner opens, and it is the difference between a file that has
    /// an outcome and a file that has nothing. A corpus file that asks for a hundred million rows
    /// runs until something stops it, and something stopping it from outside means the process is
    /// gone along with every record in it, including the ones that had already passed. A timeout
    /// and a budget the engine enforces turn the same file into an error per record, which is a
    /// result, and `Reason::Stopped` is where those land.
    #[must_use]
    pub fn limited(timeout: Duration, memory: u64) -> Self {
        Self::with_config(Config::new().with_query_timeout(timeout).with_memory_limit(memory))
    }

    /// A database opened with a configuration of the caller's own.
    #[must_use]
    pub fn with_config(config: Config) -> Self {
        Self::open(config, Optimizer::On)
    }

    /// The same rudb with every optimizer pass turned off.
    ///
    /// Run the corpus through this as well as through [`Rudb::new`] and the two runs have to agree,
    /// which is what `spec/09-optimizer.md` section 9.1 asks for. It costs no new expected outputs,
    /// because the file already says what the answer is and both runs are checked against it.
    ///
    /// The way it is done is `SET disabled_optimizers`, with every name [`rudb::optimizers`]
    /// publishes joined by commas, which is the setting DuckDB has and the spelling DuckDB uses. It
    /// is reapplied on every [`Engine::reset`], because a reset opens a new database and a new
    /// database has the passes back on.
    #[must_use]
    pub fn unoptimized() -> Self {
        Self::open(Config::new(), Optimizer::Off)
    }

    /// The database, for a caller that wants to look at the catalog after a run.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Builds one, with the passes on or off as asked.
    fn open(config: Config, optimizer: Optimizer) -> Self {
        let suffix = match optimizer {
            Optimizer::On => "",
            Optimizer::Off => " (optimizer off)",
        };
        Self {
            version: format!("rudb {}{suffix}", rudb_version()),
            config,
            optimizer,
            database: opened(config, optimizer),
        }
    }
}

/// A fresh database, with the passes turned off if that is the engine being built.
///
/// # Panics
///
/// If rudb refuses one of the pass names rudb itself published. That is the two halves of
/// `SET disabled_optimizers` disagreeing rather than anything a corpus file did, and the engine has
/// a test of its own that says they agree.
fn opened(config: Config, optimizer: Optimizer) -> Database {
    let database = Database::with_config(config);
    if optimizer == Optimizer::Off {
        let names = rudb::optimizers().join(",");
        database
            .execute(&format!("SET disabled_optimizers = '{names}'"))
            .expect("rudb takes the pass names rudb published");
    }
    database
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
        // The configuration comes with it, and so does the optimizer being off. A reset is the next
        // file starting, not the limits being handed back or the passes coming back on.
        self.database = opened(self.config, self.optimizer);
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
                    other => Cell::Text(result.value_text(&other)),
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
    fn the_unoptimized_engine_really_has_the_passes_off() {
        // The plan, rather than the setting, because the setting being accepted and the setting
        // being obeyed are two different claims and this harness cares about the second one. A
        // constant folded addition would print as its value, and this one still prints as a call.
        let rudb = Rudb::unoptimized();
        let plan = rudb.database().plan("SELECT 1 + 2").expect("that plans");
        assert!(plan.contains("\"+\""), "{plan}");
        let optimized = Rudb::new().database().plan("SELECT 1 + 2").expect("that plans");
        assert!(!optimized.contains("\"+\""), "{optimized}");
    }

    #[test]
    fn the_passes_are_still_off_after_the_next_file_resets_the_database() {
        // A reset opens a new database, and a new database has the passes back on unless somebody
        // turns them off again. Getting this wrong would make the second corpus run quietly
        // optimized from the first reset onwards, which is the whole check evaporating.
        let mut rudb = Rudb::unoptimized();
        rudb.reset().unwrap();
        let plan = rudb.database().plan("SELECT 1 + 2").expect("that plans");
        assert!(plan.contains("\"+\""), "{plan}");
    }

    #[test]
    fn the_unoptimized_engine_says_so_in_its_version() {
        // It goes in a report next to the other one, so the two have to be tellable apart.
        assert!(Rudb::unoptimized().version().contains("optimizer off"));
        assert!(!Rudb::new().version().contains("optimizer off"));
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
