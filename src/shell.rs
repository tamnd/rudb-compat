//! Either engine driven through its command line shell.
//!
//! `crate::duckdb` drives the DuckDB binary and `crate::rudb` links rudb as a library, so the
//! comparison the harness makes is between a process and a library. That is the right comparison
//! for most things and it is the wrong one for the claim the project actually makes. A drop in
//! replacement is a claim about a binary: somebody has a script that runs `duckdb -c '...'` and
//! they change the word `duckdb`. Nothing in the harness tested that until this file.
//!
//! So this is one driver with the binary as a field, and both engines go through it. The rudb side
//! and the DuckDB side then differ in exactly one way, which is the path of the executable, and any
//! difference the comparison reports is a difference in the shells rather than a difference in how
//! the harness reached them.
//!
//! ## Why the rows come back in `.mode quote`
//!
//! The harness has to tell a NULL from the empty string from the four letter string `NULL`, and
//! almost none of the shell's output modes keep those apart. `crate::duckdb` solves it by writing
//! a file with `COPY ... (FORCE_QUOTE *)`, which is not available here: rudb does not implement
//! `COPY` yet, and a driver that needs a feature of the engine under test is a driver that cannot
//! test an engine missing it.
//!
//! `.mode quote` needs nothing from the engine. It quotes by type rather than by need, so a string
//! is always in single quotes, a number is always bare and a NULL is always the bare word `NULL`.
//! Both shells were checked byte for byte before this was written and they already agree, down to
//! the doubled quote inside a value and the raw newline inside one.
//!
//! ## Why it is two processes
//!
//! One for the rows and one for the types. The shell writes every result to the same stream, so a
//! `DESCRIBE` and the statement in one process arrive as one run of text with nothing between them
//! saying where the first ended. Two processes with the same setup cost a process and remove the
//! guessing. It also means the rows are fetched first, which is what lets a statement with no
//! result set, a `CREATE TABLE` or a `SET`, come back as an empty table rather than as the error
//! that `DESCRIBE CREATE TABLE` would have produced.

use std::path::PathBuf;
use std::process::Command;

use crate::duckdb::on_path;
use crate::engine::{
    Acceptance, Cell, Column, Engine, EngineError, HarnessError, Outcome, Table, assemble,
};
use crate::quote;

/// One shell, and the session state to put in front of every statement.
#[derive(Debug, Clone)]
pub struct Shell {
    name: String,
    binary: PathBuf,
    version: String,
    database: String,
    setup: Vec<String>,
}

impl Shell {
    /// The DuckDB shell, found the same way `crate::duckdb::Duckdb` finds it.
    ///
    /// `RUDB_COMPAT_DUCKDB` names the binary when it is set. Sharing the variable is deliberate: a
    /// machine with three DuckDBs on it should not be able to compare one of them against rudb
    /// through the library and a different one through the shell.
    ///
    /// # Errors
    ///
    /// When the binary is missing, is not executable, or does not answer `--version`.
    pub fn duckdb() -> Result<Self, HarnessError> {
        Self::at("duckdb-shell", "RUDB_COMPAT_DUCKDB", "duckdb")
    }

    /// The rudb shell, named by `RUDB_COMPAT_RUDB` or found on the path as `rudb`.
    ///
    /// This is the one place in the harness that wants a built rudb binary rather than the library
    /// it links, and there is no way around that. The claim being tested is about an executable.
    ///
    /// # Errors
    ///
    /// When the binary is missing, is not executable, or does not answer `--version`.
    pub fn rudb() -> Result<Self, HarnessError> {
        Self::at("rudb-shell", "RUDB_COMPAT_RUDB", "rudb")
    }

    /// A shell at whatever the variable names, falling back to a bare command on the path.
    ///
    /// # Errors
    ///
    /// When the binary is missing, is not executable, or does not answer `--version`.
    pub fn at(name: &str, variable: &str, command: &str) -> Result<Self, HarnessError> {
        let binary = std::env::var_os(variable).map_or_else(|| on_path(command), PathBuf::from);
        let out = Command::new(&binary).arg("--version").output().map_err(|e| {
            HarnessError::new(format!(
                "cannot run {}: {e}. Set {variable} to a {command} binary, or put one on PATH",
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
        Ok(Self {
            name: name.to_owned(),
            binary,
            version: String::from_utf8_lossy(&out.stdout).trim().to_owned(),
            database: ":memory:".to_owned(),
            setup: Vec::new(),
        })
    }

    /// Point it at a database file instead of an in-memory one.
    #[must_use]
    pub fn on(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    /// Statements to run before every query, in order.
    #[must_use]
    pub fn with_setup(mut self, statements: Vec<String>) -> Self {
        self.setup = statements;
        self
    }

    /// The binary being driven, for the report.
    #[must_use]
    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }

    /// Run one statement in a fresh process and hand back what it wrote.
    ///
    /// `-init /dev/null` is not optional. Both shells read a startup file otherwise, so without it
    /// a developer with a `.duckdbrc` setting an output mode would get results this cannot read and
    /// a report that says the engines disagree.
    fn invoke(&self, statement: &str) -> Result<Written, HarnessError> {
        let mut command = Command::new(&self.binary);
        command.arg("-batch").arg("-init").arg(devnull()).arg("-cmd").arg(".mode quote");
        for setup in &self.setup {
            command.arg("-c").arg(setup);
        }
        command.arg(&self.database).arg(statement);
        let out = command
            .output()
            .map_err(|e| HarnessError::new(format!("cannot run {}: {e}", self.binary.display())))?;
        Ok(Written {
            failed: !out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// What one run of a shell wrote.
struct Written {
    failed: bool,
    stdout: String,
    stderr: String,
}

impl Engine for Shell {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
        let statement = sql.trim().trim_end_matches(';');
        let rows = self.invoke(statement)?;
        if rows.failed {
            return Ok(Outcome::Error(EngineError::parse(&rows.stderr)));
        }
        let rows = quote::read(&rows.stdout)?;
        // Nothing written at all is a statement with no result set, which is what a CREATE TABLE
        // and a SET produce on both shells. That is a result and not an absence of one, and asking
        // DESCRIBE about it would turn it into a parser error that says nothing about the engine.
        if rows.is_empty() {
            return Ok(Outcome::Rows(Table::default()));
        }
        let types = self.invoke(&describe(statement))?;
        if types.failed {
            return Ok(Outcome::Rows(undescribable(&rows, &EngineError::parse(&types.stderr))));
        }
        assemble(&quote::read(&types.stdout)?, &rows)
            .map(Outcome::Rows)
            .map_err(|e| HarnessError::new(format!("{}: {e}", self.name)))
    }

    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError> {
        // Weaker than the library path and deliberately so. There is no entry point in either shell
        // that parses without running, so this runs the statement and reads the error kind: a
        // parser error is a rejection and everything else, including a catalog error, means the
        // text parsed and something later declined it. That is the right answer for the question
        // being asked and it is the reason `rudb-compat parse` still drives the library by default.
        let out = self.invoke(sql.trim().trim_end_matches(';'))?;
        if !out.failed {
            return Ok(Acceptance::Accepted);
        }
        let error = EngineError::parse(&out.stderr);
        if error.kind == "Parser Error" {
            Ok(Acceptance::Rejected(error))
        } else {
            Ok(Acceptance::Accepted)
        }
    }
}

/// A shell that remembers what it has been told, so that a file of statements runs as one session.
///
/// [`Shell`] runs every statement in a fresh process, which is what the differential corpora want,
/// because each of their statements stands alone and a process per statement is the cheapest way to
/// be sure of it. A sqllogictest file is the other shape: it creates a table, fills it, asks it
/// questions and drops it, and none of that works if the process running the second statement never
/// heard the first one.
///
/// The way to keep state across processes is usually a database file, and that is not available
/// here, because rudb has no persistence until E2. So the session is rebuilt in front of every
/// statement instead, out of the statements that printed nothing. That set is exactly the one that
/// leaves something behind and nothing on the screen: `CREATE`, `INSERT`, `DROP`, `SET`. A statement
/// that printed rows is not replayed, both because replaying it would put its rows in front of the
/// next answer where the reader expects one table, and because a query has nothing to leave behind.
///
/// The cost is a process per statement in the file plus a replay of the file so far, which is
/// quadratic and is fine at the size of a corpus written by hand. It would not be fine on the four
/// thousand upstream files, and that is not what this is for: those run against the library, where
/// a connection stays open and none of this is needed.
#[derive(Debug, Clone)]
pub struct Session {
    /// The shell underneath, with no setup of its own.
    shell: Shell,
    /// What has been run and printed nothing, in the order it was run.
    history: Vec<String>,
}

impl Session {
    /// Wrap a shell so that what it is told sticks until the next reset.
    #[must_use]
    pub fn new(shell: Shell) -> Self {
        Self { shell, history: Vec::new() }
    }

    /// The statements this session replays in front of the next one.
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// The shell with the session in front of it, ready for one statement.
    fn primed(&self) -> Shell {
        self.shell.clone().with_setup(self.history.clone())
    }
}

impl Engine for Session {
    fn name(&self) -> &str {
        self.shell.name()
    }

    fn version(&self) -> &str {
        self.shell.version()
    }

    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
        let statement = sql.trim().trim_end_matches(';');
        let outcome = self.primed().run(statement)?;
        if let Outcome::Rows(table) = &outcome {
            if table.width() == 0 {
                self.history.push(statement.to_owned());
            }
        }
        Ok(outcome)
    }

    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError> {
        // Nothing is remembered here on purpose. `accepts` answers whether the text is SQL, it
        // calls a catalog error accepted, and a session it built itself would not change one of
        // its answers. Running the history in front of it still matters, because a statement that
        // needs a table is a different statement to parse when the table is there.
        self.primed().accepts(sql)
    }

    fn reset(&mut self) -> Result<(), HarnessError> {
        self.history.clear();
        Ok(())
    }
}

/// The query that asks a shell what a statement's columns are.
fn describe(statement: &str) -> String {
    format!("SELECT column_name, column_type FROM (DESCRIBE {statement})")
}

/// The table for a statement that returned rows and then would not be described.
///
/// `EXPLAIN ANALYZE SELECT 1` is the case that found this. It returns two columns of text on both
/// shells, and wrapping it in a `DESCRIBE` is a parser error on both, because `DESCRIBE` takes a
/// query or a name and `EXPLAIN` is neither. `PRAGMA` and `SHOW` are the same shape. There is
/// nothing wrong with the engine when that happens, so the old behaviour of failing the whole run
/// was the harness refusing to report on a statement it had already run successfully.
///
/// The names come off the row header, which is the only other place the shell prints them. The type
/// column then has to say something, and it says what happened, kind included. That keeps the
/// comparison honest in both directions: two engines that both refuse in the same way agree here,
/// and an engine that can describe a statement the other cannot shows up as a type difference
/// rather than as silence. What it does not do is claim to know a type it was never told.
fn undescribable(rows: &[Vec<Cell>], why: &EngineError) -> Table {
    let header = rows.first().map_or(&[][..], Vec::as_slice);
    let columns = header
        .iter()
        .map(|cell| Column {
            name: match cell {
                Cell::Null => String::new(),
                Cell::Text(text) => text.clone(),
            },
            ty: format!("(not describable: {})", why.kind),
        })
        .collect();
    Table { columns, rows: rows.iter().skip(1).cloned().collect() }
}

/// The path that discards what is written to it, on whichever platform this is.
fn devnull() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

#[cfg(test)]
mod tests {
    use super::{Session, Shell};
    use crate::engine::{Cell, Engine, Outcome};

    /// Every test here needs both binaries on the machine. A developer without one gets a skipped
    /// test and a line saying so rather than a red build, for the same reason `crate::duckdb` does
    /// it: a harness that cannot be built without the thing it compares against is a harness
    /// nobody works on.
    fn shells() -> Vec<Shell> {
        let mut found = Vec::new();
        for (what, made) in [("duckdb", Shell::duckdb()), ("rudb", Shell::rudb())] {
            match made {
                Ok(shell) => found.push(shell),
                Err(e) => eprintln!("skipping, no {what} shell: {e}"),
            }
        }
        found
    }

    #[test]
    fn both_shells_return_a_query_with_its_values_its_names_and_its_types() {
        for mut shell in shells() {
            let Outcome::Rows(table) = shell.run("SELECT 1 AS a, 'x' AS b").unwrap() else {
                panic!("{}: a SELECT that works should not be an error", shell.name());
            };
            assert_eq!(table.columns[0].name, "a", "{}", shell.name());
            assert_eq!(table.columns[0].ty, "INTEGER", "{}", shell.name());
            assert_eq!(table.columns[1].ty, "VARCHAR", "{}", shell.name());
            assert_eq!(table.rows, vec![vec![Cell::Text("1".into()), Cell::Text("x".into())]]);
        }
    }

    #[test]
    fn both_shells_keep_a_null_and_an_empty_string_apart() {
        for mut shell in shells() {
            let Outcome::Rows(table) = shell.run("SELECT NULL AS n, '' AS e, 'NULL' AS w").unwrap()
            else {
                panic!("{}: a SELECT that works should not be an error", shell.name());
            };
            assert_eq!(table.rows[0][0], Cell::Null, "{}", shell.name());
            assert_eq!(table.rows[0][1], Cell::Text(String::new()), "{}", shell.name());
            assert_eq!(table.rows[0][2], Cell::Text("NULL".into()), "{}", shell.name());
        }
    }

    #[test]
    fn both_shells_hand_back_every_digit_of_the_largest_bigint() {
        for mut shell in shells() {
            let Outcome::Rows(table) =
                shell.run("SELECT 9223372036854775807::BIGINT AS b").unwrap()
            else {
                panic!("{}: a SELECT that works should not be an error", shell.name());
            };
            assert_eq!(table.rows[0][0], Cell::Text("9223372036854775807".into()));
        }
    }

    #[test]
    fn a_syntax_error_is_a_result_and_carries_the_kind_the_shell_printed() {
        for mut shell in shells() {
            let Outcome::Error(e) = shell.run("SELECT FROM WHERE").unwrap() else {
                panic!("{}: that is not valid SQL", shell.name());
            };
            assert_eq!(e.kind, "Parser Error", "{}", shell.name());
        }
    }

    #[test]
    fn a_query_with_no_rows_is_a_table_and_not_an_error() {
        for mut shell in shells() {
            let Outcome::Rows(table) = shell.run("SELECT 1 AS a WHERE false").unwrap() else {
                panic!("{}: no rows is still a result", shell.name());
            };
            assert_eq!(table.width(), 1, "{}", shell.name());
            assert_eq!(table.height(), 0, "{}", shell.name());
        }
    }

    #[test]
    fn a_statement_with_no_result_set_comes_back_empty_rather_than_as_an_error() {
        // This is the case the library driver cannot do, because it wraps everything in a DESCRIBE
        // and DESCRIBE CREATE TABLE is a parser error. Running the rows first is what fixes it.
        for mut shell in shells() {
            let Outcome::Rows(table) = shell.run("CREATE TABLE t(a INTEGER)").unwrap() else {
                panic!("{}: a CREATE TABLE that works should not be an error", shell.name());
            };
            assert_eq!(table.width(), 0, "{}", shell.name());
            assert_eq!(table.height(), 0, "{}", shell.name());
        }
    }

    #[test]
    fn a_statement_describe_cannot_wrap_still_comes_back_with_its_rows() {
        // PRAGMA returns a proper result set and DESCRIBE PRAGMA is a parser error, so this is the
        // shape that used to kill the run. rudb does not implement PRAGMA at all today, so only
        // one of the two shells reaches the rows, and both of them have to reach an outcome: the
        // thing being asserted is that the harness reports what happened instead of giving up.
        for shell in shells() {
            let mut shell = shell.with_setup(vec!["CREATE TABLE t(a INTEGER)".to_owned()]);
            let outcome = shell.run("PRAGMA table_info('t')").unwrap();
            let Outcome::Rows(table) = outcome else {
                continue;
            };
            assert_eq!(table.columns[0].name, "cid", "{}", shell.name());
            assert_eq!(table.columns[0].ty, "(not describable: Parser Error)", "{}", shell.name());
            assert_eq!(table.rows[0][1], Cell::Text("a".into()), "{}", shell.name());
        }
    }

    #[test]
    fn a_session_holds_on_to_what_a_statement_left_behind() {
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            let mut session = Session::new(shell);
            session.run("CREATE TABLE t (a INTEGER)").unwrap();
            session.run("INSERT INTO t VALUES (1), (2)").unwrap();
            let Outcome::Rows(table) = session.run("SELECT sum(a) FROM t").unwrap() else {
                panic!("{name}: the two statements before this one should have made t");
            };
            assert_eq!(table.rows, vec![vec![Cell::Text("3".into())]], "{name}");
            assert_eq!(session.history().len(), 2, "{name}");
        }
    }

    #[test]
    fn a_session_remembers_nothing_that_printed_rows_or_failed() {
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            let mut session = Session::new(shell);
            session.run("CREATE TABLE t (a INTEGER)").unwrap();
            session.run("SELECT 1").unwrap();
            let Outcome::Error(_) = session.run("DROP TABLE nosuchtable").unwrap() else {
                panic!("{name}: dropping a table that is not there is an error");
            };
            assert_eq!(session.history(), ["CREATE TABLE t (a INTEGER)"], "{name}");
        }
    }

    #[test]
    fn a_reset_session_starts_from_an_empty_database() {
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            let mut session = Session::new(shell);
            session.run("CREATE TABLE t (a INTEGER)").unwrap();
            session.reset().unwrap();
            assert!(session.history().is_empty(), "{name}");
            let Outcome::Error(e) = session.run("SELECT a FROM t").unwrap() else {
                panic!("{name}: t was made before the reset and should be gone");
            };
            assert_eq!(e.kind, "Catalog Error", "{name}");
        }
    }

    #[test]
    fn setup_statements_are_visible_to_the_query() {
        for shell in shells() {
            let mut shell = shell.with_setup(vec!["CREATE TABLE t AS SELECT 5 AS x".to_owned()]);
            let Outcome::Rows(table) = shell.run("SELECT x FROM t").unwrap() else {
                panic!("{}: the setup should have made t", shell.name());
            };
            assert_eq!(table.rows, vec![vec![Cell::Text("5".into())]], "{}", shell.name());
        }
    }
}
