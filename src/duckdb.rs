//! A real DuckDB, run as a subprocess.
//!
//! Section 14.1 says the comparison is always against a real DuckDB binary of a named version, and
//! that is not a preference. Comparing against a reimplementation of DuckDB, or against a memory
//! of what DuckDB does, is how a compatibility suite ends up agreeing with the bug it was written
//! to find. So this drives the shipped CLI.
//!
//! ## Why the results come back through a CSV file
//!
//! The CLI has a JSON mode, and JSON is the wrong shape for this. It renders a BIGINT as a JSON
//! number, which any reader that goes through a float loses the low bits of, and it has no way to
//! say what a column's type was. It also has a `-list` mode, where a NULL and the string `NULL`
//! are the same three bytes.
//!
//! `COPY (query) TO 'file' (FORMAT csv, HEADER, FORCE_QUOTE *)` has neither problem. Every value
//! that is not NULL comes back quoted, so an unquoted empty field is unambiguously NULL and a
//! quoted empty field is unambiguously the empty string. Values arrive as the exact text DuckDB
//! rendered, which is the form that has to match anyway, per `crate::engine::Cell`. And writing to
//! a file rather than to `/dev/stdout` keeps DuckDB's own buffered output and the COPY's direct
//! write from interleaving, which they do, unpredictably, when both go to the same descriptor.
//!
//! The types come from a second COPY over `DESCRIBE`, in the same process and the same session, so
//! a temporary view or a setting made by the setup statements is visible to both.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::csv;
use crate::engine::{Acceptance, Cell, Column, Engine, EngineError, HarnessError, Outcome, Table};

/// The DuckDB this project tracks, which is the same ref the grammar is vendored from.
///
/// `crates/rudb-parse/grammar/VENDOR` in the rudb repository pins the commit. If the binary on the
/// machine is not this version then the harness still runs, and every report says loudly which
/// version it actually ran against, because a coverage number attached to the wrong version is
/// worse than no number.
pub const PINNED: &str = "v2.0";

/// How many statements this process has run, which is what keeps two runs from sharing a file.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A DuckDB binary and the session state to put in front of every statement.
#[derive(Debug, Clone)]
pub struct Duckdb {
    binary: PathBuf,
    version: String,
    database: String,
    setup: Vec<String>,
    scratch: PathBuf,
}

impl Duckdb {
    /// Find a DuckDB and ask it what version it is.
    ///
    /// `RUDB_COMPAT_DUCKDB` names the binary when it is set, and otherwise `duckdb` is looked up
    /// the way any other command would be. The version is read once here rather than per query,
    /// because it goes in the report and a report whose version changed halfway through is not a
    /// report.
    ///
    /// # Errors
    ///
    /// When the binary is missing, is not executable, or does not answer `--version`.
    pub fn discover() -> Result<Self, HarnessError> {
        let binary =
            std::env::var_os("RUDB_COMPAT_DUCKDB").map_or_else(|| on_path("duckdb"), PathBuf::from);
        let out = Command::new(&binary).arg("--version").output().map_err(|e| {
            HarnessError::new(format!(
                "cannot run {}: {e}. Set RUDB_COMPAT_DUCKDB to a DuckDB binary, or put one on PATH",
                binary.display()
            ))
        })?;
        if !out.status.success() {
            return Err(HarnessError::new(format!(
                "{} --version exited {}",
                binary.display(),
                out.status
            )));
        }
        let version = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok(Self {
            binary,
            version,
            database: ":memory:".to_owned(),
            setup: Vec::new(),
            scratch: std::env::temp_dir().join(format!("rudb-compat-{}", std::process::id())),
        })
    }

    /// Point it at a database file instead of an in-memory one.
    #[must_use]
    pub fn on(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    /// Statements to run before every query, in order.
    ///
    /// Each query gets its own process, so a table created by one query is not visible to the
    /// next. That is deliberate for now: it makes every comparison independent of the order the
    /// harness happened to run things in, which is the property a shrinker needs. Setup that has
    /// to be there goes here and is paid for again on every query.
    #[must_use]
    pub fn with_setup(mut self, statements: Vec<String>) -> Self {
        self.setup = statements;
        self
    }

    /// True when the binary is the version this project pins.
    #[must_use]
    pub fn is_pinned_version(&self) -> bool {
        self.version.starts_with(&format!("v{}", PINNED.trim_start_matches('v')))
    }

    /// The binary being driven, for the report.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl Engine for Duckdb {
    fn name(&self) -> &str {
        "duckdb"
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
        std::fs::create_dir_all(&self.scratch).map_err(|e| {
            HarnessError::new(format!("cannot make {}: {e}", self.scratch.display()))
        })?;
        // The number makes the two file names unique to this call. Fixed names would be a race
        // between any two runs sharing a process, which is what `cargo test` is, and the failure
        // it produces is one query reading another query's result set.
        let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let types_at = self.scratch.join(format!("types-{n}.csv"));
        let rows_at = self.scratch.join(format!("rows-{n}.csv"));

        let statement = sql.trim().trim_end_matches(';');
        let mut command = Command::new(&self.binary);
        command.arg("-batch").arg(&self.database);
        for setup in &self.setup {
            command.arg("-c").arg(setup);
        }
        command
            .arg("-c")
            .arg(copy_of(
                &format!("SELECT column_name, column_type FROM (DESCRIBE {statement})"),
                &types_at,
            ))
            .arg("-c")
            .arg(copy_of(statement, &rows_at));

        let out = command
            .output()
            .map_err(|e| HarnessError::new(format!("cannot run {}: {e}", self.binary.display())))?;
        if !out.status.success() {
            let text = String::from_utf8_lossy(&out.stderr);
            return Ok(Outcome::Error(EngineError::parse(&text)));
        }

        let types = std::fs::read_to_string(&types_at)
            .map_err(|e| HarnessError::new(format!("cannot read {}: {e}", types_at.display())))?;
        let rows = std::fs::read_to_string(&rows_at)
            .map_err(|e| HarnessError::new(format!("cannot read {}: {e}", rows_at.display())))?;
        let _ = std::fs::remove_file(&types_at);
        let _ = std::fs::remove_file(&rows_at);
        Ok(Outcome::Rows(assemble(&csv::read(&types)?, &csv::read(&rows)?)?))
    }

    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError> {
        let outcome = self.run(&serialize(sql))?;
        let Outcome::Rows(table) = outcome else {
            return Err(HarnessError::new(
                "asking DuckDB to serialize a statement should not itself fail",
            ));
        };
        let row = table
            .rows
            .first()
            .ok_or_else(|| HarnessError::new("the serializer returned no row"))?;
        let field = |at: usize| match row.get(at) {
            Some(Cell::Text(t)) => t.as_str(),
            _ => "",
        };
        if field(0) != "true" {
            return Ok(Acceptance::Accepted);
        }
        // Anything other than a parser error means the text parsed and the serializer declined it
        // afterwards, which is what happens for every statement that is not a SELECT. That is an
        // acceptance and not a rejection, and reading it the other way would have the harness
        // reporting that DuckDB cannot parse its own CREATE TABLE.
        if field(1) != "parser" {
            return Ok(Acceptance::Accepted);
        }
        Ok(Acceptance::Rejected(EngineError {
            kind: "Parser Error".to_owned(),
            message: field(2).to_owned(),
        }))
    }
}

/// The query that asks DuckDB whether some text is SQL.
///
/// `json_serialize_sql` parses the text and hands back either the parse tree or a structured
/// error, which makes it the one entry point in the CLI that separates parsing from binding and
/// running. The four fields are pulled out by DuckDB rather than by a JSON reader here, because
/// the CSV path already exists and a second parser in the harness is a second place to be wrong.
fn serialize(sql: &str) -> String {
    let quoted = sql.replace('\'', "''");
    format!(
        "SELECT j ->> '$.error' AS error, j ->> '$.error_type' AS error_type, \
         j ->> '$.error_message' AS error_message, j ->> '$.position' AS position \
         FROM (SELECT json_serialize_sql('{quoted}') AS j)"
    )
}

/// Resolve a bare command name against `PATH`.
///
/// This exists so the report names a file rather than a word. Every number the harness prints has
/// to be read next to which DuckDB produced it, and on a machine with a system DuckDB, a Homebrew
/// one and one built from source, the word `duckdb` does not say which of the three ran.
///
/// A name that resolves to nothing comes back unchanged, so the failure is the one from trying to
/// run it, which already says what to do about it.
fn on_path(name: &str) -> PathBuf {
    let Some(path) = std::env::var_os("PATH") else { return PathBuf::from(name) };
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The COPY that writes one result set out.
fn copy_of(statement: &str, to: &Path) -> String {
    let quoted = to.display().to_string().replace('\'', "''");
    format!("COPY ({statement}) TO '{quoted}' (FORMAT csv, HEADER, FORCE_QUOTE *)")
}

/// Put the two CSV files together into one table.
///
/// The column names come from the DESCRIBE rather than from the row header, because they are the
/// same names and DESCRIBE is the one that also carries the types. They are checked against each
/// other anyway, since a disagreement means the two COPYs did not see the same query and every
/// value below is then lined up against the wrong column.
fn assemble(types: &[Vec<Cell>], rows: &[Vec<Cell>]) -> Result<Table, HarnessError> {
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
    for (at, column) in columns.iter().enumerate() {
        if header[at] != Cell::Text(column.name.clone()) {
            return Err(HarnessError::new(format!(
                "column {at} is {} in the result and {} in the DESCRIBE",
                header[at], column.name
            )));
        }
    }

    Ok(Table { columns, rows: rows.iter().skip(1).cloned().collect() })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Duckdb, copy_of, on_path};
    use crate::engine::{Cell, Engine, Outcome};
    use std::path::Path;

    /// Every test here needs a DuckDB on the machine, and CI installs one. A developer without one
    /// gets a skipped test and a line saying so rather than a red build, because a harness that
    /// cannot be built without the thing it compares against is a harness nobody works on.
    fn duckdb() -> Option<Duckdb> {
        match Duckdb::discover() {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("skipping, no DuckDB: {e}");
                None
            }
        }
    }

    #[test]
    fn a_command_on_the_path_resolves_to_the_file_it_is() {
        // `sh` is on PATH on every machine this builds on, and it is a file rather than a shell
        // builtin, which is the pair of properties the lookup needs to be tested against.
        let got = on_path("sh");
        assert!(got.is_absolute(), "{} should have been resolved", got.display());
        assert!(got.is_file());
    }

    #[test]
    fn a_command_that_is_nowhere_comes_back_as_it_was_given() {
        let name = "this-is-not-a-command-on-anybodys-path";
        assert_eq!(on_path(name), PathBuf::from(name));
    }

    #[test]
    fn the_copy_quotes_a_path_with_a_quote_in_it() {
        let got = copy_of("SELECT 1", Path::new("/tmp/it's/rows.csv"));
        assert!(got.ends_with("TO '/tmp/it''s/rows.csv' (FORMAT csv, HEADER, FORCE_QUOTE *)"));
    }

    #[test]
    fn a_query_comes_back_with_its_values_its_names_and_its_types() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Rows(table) = db.run("SELECT 1 AS a, 'x' AS b").unwrap() else {
            panic!("a SELECT that works should not be an error");
        };
        assert_eq!(table.columns[0].name, "a");
        assert_eq!(table.columns[0].ty, "INTEGER");
        assert_eq!(table.columns[1].ty, "VARCHAR");
        assert_eq!(table.rows, vec![vec![Cell::Text("1".into()), Cell::Text("x".into())]]);
    }

    #[test]
    fn a_null_and_an_empty_string_survive_the_round_trip_apart() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Rows(table) = db.run("SELECT NULL AS n, '' AS e").unwrap() else {
            panic!("a SELECT that works should not be an error");
        };
        assert_eq!(table.rows[0][0], Cell::Null);
        assert_eq!(table.rows[0][1], Cell::Text(String::new()));
    }

    #[test]
    fn the_largest_bigint_arrives_with_every_digit() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Rows(table) = db.run("SELECT 9223372036854775807::BIGINT AS b").unwrap()
        else {
            panic!("a SELECT that works should not be an error");
        };
        assert_eq!(table.rows[0][0], Cell::Text("9223372036854775807".into()));
    }

    #[test]
    fn a_syntax_error_is_a_result_and_carries_duckdbs_own_kind() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Error(e) = db.run("SELECT FROM WHERE").unwrap() else {
            panic!("that is not valid SQL");
        };
        assert_eq!(e.kind, "Parser Error");
    }

    #[test]
    fn a_query_with_no_rows_is_a_table_and_not_an_error() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Rows(table) = db.run("SELECT 1 AS a WHERE false").unwrap() else {
            panic!("no rows is still a result");
        };
        assert_eq!(table.width(), 1);
        assert_eq!(table.height(), 0);
    }

    #[test]
    fn setup_statements_are_visible_to_the_query() {
        let Some(db) = duckdb() else { return };
        let mut db = db.with_setup(vec!["CREATE TABLE t AS SELECT 5 AS x".to_owned()]);
        let Outcome::Rows(table) = db.run("SELECT x FROM t").unwrap() else {
            panic!("the setup should have made t");
        };
        assert_eq!(table.rows, vec![vec![Cell::Text("5".into())]]);
    }
}
