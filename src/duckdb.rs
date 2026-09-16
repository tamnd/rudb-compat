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
//!
//! ## Why a statement that is not a query takes a different path
//!
//! Both of those wrappers take a query and nothing else. `COPY (CREATE TABLE t(i INT)) TO` is a
//! parser error and so is `DESCRIBE CREATE TABLE t(i INT)`, and the same goes for every INSERT,
//! UPDATE, DELETE, SET, PRAGMA, CALL, EXPLAIN and transaction control statement. Wrapping one of
//! those anyway does not produce a wrong answer, it produces a parser error attributed to DuckDB,
//! which is worse: it says the pinned binary cannot parse its own CREATE TABLE. So `describable`
//! decides first, and a statement that is not a query is run bare and reported as having succeeded
//! with no rows.
//!
//! That last part is a real limitation and not a detail. `PRAGMA`, `CALL`, `EXPLAIN` and an
//! `INSERT ... RETURNING` all return rows and all of them come back here as none, because the CLI
//! has no way to wrap them that keeps the CSV shape the rest of this file depends on. Every other
//! non-query statement returns nothing anyway, which is the overwhelming majority of them, so this
//! is the smaller of the two wrongs by a long way. tamnd/rudb-compat#57 has the rest.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::csv;
use crate::engine::{Acceptance, Cell, Engine, EngineError, HarnessError, Outcome, assemble};

/// The DuckDB this project tracks, which is the same ref the grammar is vendored from.
///
/// `crates/rudb-parse/grammar/VENDOR` in the rudb repository pins the commit. If the binary on the
/// machine is not this version then the harness still runs, and every report says loudly which
/// version it actually ran against, because a coverage number attached to the wrong version is
/// worse than no number.
pub const PINNED: &str = "v2.0";

/// The commit the grammar is vendored from, which is what the binary has to be built at.
///
/// The version string is not enough on its own. `v2.0-cyanoptera` is a development branch that
/// moves every day, so two binaries can both say `v2.0.0-dev` and disagree about the language, and
/// the whole point of this harness is that the two sides speak the same one. The CLI prints the
/// short hash as the last word of `--version`, so the check is a prefix match against this.
///
/// This is the `commit:` line of `crates/rudb-parse/grammar/VENDOR` in the rudb repository. The two
/// repositories are separate, so it is written here by hand and `rudb-compat duckdb` is what
/// notices when it has gone stale.
pub const PINNED_COMMIT: &str = "cc7e7bac7fcb6e0994359965a87ac4f6a96f2e17";

/// What the binary on this machine is, next to the one the grammar came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// The commit the grammar is vendored from. A number out of this run is about rudb.
    Pinned,
    /// A v2.0 alpha, but built at some other commit. This is the state that used to pass the check
    /// silently, and it is the one worth naming: the version is right, the language may not be.
    OtherCommit,
    /// Some other DuckDB, which in practice is a published release. The run still happens and the
    /// report says which version produced it, because a released binary is what most machines have.
    Fallback,
}

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
    limit: Duration,
}

/// How long one statement gets before the process running it is killed.
///
/// The same ten seconds `--limit` defaults to, for the same reason. DuckDB has functions that are
/// supposed to take forever, `sleep_ms` being the plain one, and a generated call is going to find
/// them: the first full sweep spent an hour asleep inside
/// `sleep_ms(9223372036854775807::BIGINT)`, which is about nine billion seconds.
pub const LIMIT: Duration = Duration::from_secs(10);

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
            limit: LIMIT,
        })
    }

    /// Give one statement a different amount of time before the process is killed.
    #[must_use]
    pub fn within(mut self, limit: Duration) -> Self {
        self.limit = limit;
        self
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

    /// The commit the binary was built at, as it printed it.
    ///
    /// `duckdb --version` is one line and the hash is the last word of it, on a release and on a
    /// development build alike: `v1.5.5 (Variegata) d8cdaa33fd` and `v2.0.0-dev84237 (Development
    /// Version) cc7e7bac7f`. Anything that is not a hash comes back as nothing rather than as a
    /// wrong answer, so a binary that prints something else is a mismatch and not a crash.
    #[must_use]
    pub fn commit(&self) -> Option<&str> {
        hash_in(&self.version)
    }

    /// Whether this binary is the one the grammar came from, and if not, how far off it is.
    ///
    /// The hash is matched as a prefix in both directions, so a binary that prints ten characters
    /// and one that prints all forty both compare equal to the commit written down here.
    #[must_use]
    pub fn pin(&self) -> Pin {
        classify(&self.version)
    }

    /// True when the binary is the commit this project pins.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.pin() == Pin::Pinned
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
        let said_at = self.scratch.join(format!("said-{n}.txt"));

        let statement = sql.trim().trim_end_matches(';');
        let query = describable(statement);
        let mut command = Command::new(&self.binary);
        command.arg("-batch").arg(&self.database);
        for setup in &self.setup {
            command.arg("-c").arg(setup);
        }
        if query {
            command
                .arg("-c")
                .arg(copy_of(
                    &format!("SELECT column_name, column_type FROM (DESCRIBE {statement})"),
                    &types_at,
                ))
                .arg("-c")
                .arg(copy_of(statement, &rows_at));
        } else {
            command.arg("-c").arg(statement);
        }

        // What DuckDB says about a failure goes to a file rather than to a pipe, because the wait
        // below is a poll and a poll that is not reading a pipe is a poll that deadlocks the moment
        // the pipe fills. Nothing reads the standard output: every result comes back through the
        // two CSV files.
        let said = std::fs::File::create(&said_at)
            .map_err(|e| HarnessError::new(format!("cannot make {}: {e}", said_at.display())))?;
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(said));

        let mut child = command
            .spawn()
            .map_err(|e| HarnessError::new(format!("cannot run {}: {e}", self.binary.display())))?;
        let Some(status) = wait_for(&mut child, self.limit)? else {
            let _ = child.kill();
            let _ = child.wait();
            for at in [&types_at, &rows_at, &said_at] {
                let _ = std::fs::remove_file(at);
            }
            return Ok(Outcome::Error(EngineError {
                kind: "Timeout Error".to_owned(),
                message: format!("killed after {} seconds", self.limit.as_secs()),
            }));
        };
        let text = std::fs::read_to_string(&said_at).unwrap_or_default();
        let _ = std::fs::remove_file(&said_at);
        if !status.success() {
            let _ = std::fs::remove_file(&types_at);
            let _ = std::fs::remove_file(&rows_at);
            return Ok(Outcome::Error(EngineError::parse(&text)));
        }

        // It worked and there was never a result set to read, so the table is the empty one. A
        // caller asking whether the statement ran gets yes, which is what it did.
        if !query {
            return Ok(Outcome::Rows(crate::engine::Table::default()));
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

/// Where a version line sits next to the vendored commit, for a caller holding only the line.
///
/// [`Duckdb::pin`] is the same question asked of a driver that has one. The shell driver in
/// `crate::shell` has the version string and nothing else, and the pin belongs next to every number
/// however the binary was reached.
#[must_use]
pub fn pin_of(version: &str) -> Pin {
    classify(version)
}

/// The commit hash in a `duckdb --version` line, which is the last word of it.
///
/// A word that is not hexadecimal, or is too short to be a short hash, is not one. That way a
/// binary whose version line has some other shape reads as a version with no commit in it, and the
/// pin then says the honest thing rather than comparing a hash against a word.
fn hash_in(version: &str) -> Option<&str> {
    let last = version.split_whitespace().next_back()?;
    let looks_like_a_hash = last.len() >= 8 && last.chars().all(|c| c.is_ascii_hexdigit());
    looks_like_a_hash.then_some(last)
}

/// Where a version line sits next to the commit the grammar is vendored from.
fn classify(version: &str) -> Pin {
    if let Some(commit) = hash_in(version) {
        if PINNED_COMMIT.starts_with(commit) || commit.starts_with(PINNED_COMMIT) {
            return Pin::Pinned;
        }
    }
    if version.starts_with(PINNED) { Pin::OtherCommit } else { Pin::Fallback }
}

/// Resolve a bare command name against `PATH`.
///
/// This exists so the report names a file rather than a word. Every number the harness prints has
/// to be read next to which DuckDB produced it, and on a machine with a system DuckDB, a Homebrew
/// one and one built from source, the word `duckdb` does not say which of the three ran.
///
/// A name that resolves to nothing comes back unchanged, so the failure is the one from trying to
/// run it, which already says what to do about it.
pub(crate) fn on_path(name: &str) -> PathBuf {
    let Some(path) = std::env::var_os("PATH") else { return PathBuf::from(name) };
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Wait for a child, or give up on it and say so by coming back with nothing.
///
/// A poll rather than a blocking wait, because the standard library has no wait with a deadline and
/// the alternatives are a thread per call or a signal handler. The sleep is short enough that it
/// costs a couple of milliseconds on a call that finishes quickly and long enough that a ten second
/// wait is a few thousand cheap syscalls rather than a spin.
fn wait_for(child: &mut Child, limit: Duration) -> Result<Option<ExitStatus>, HarnessError> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(e) => return Err(HarnessError::new(format!("cannot wait for DuckDB: {e}"))),
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The COPY that writes one result set out.
/// Whether this statement is one `COPY (...) TO` and `DESCRIBE` will take.
///
/// The two wrappers accept exactly the same set, which was checked against the pinned binary rather
/// than assumed: SELECT, WITH, VALUES, TABLE, FROM, SHOW, DESCRIBE, SUMMARIZE, PIVOT, UNPIVOT and a
/// parenthesised query. Everything else is a parser error inside the wrapper, and a parser error
/// inside the wrapper is reported as DuckDB failing to parse a statement it parses perfectly well.
///
/// This reads the first word and nothing else, deliberately. Asking the binary would be a second
/// subprocess for every statement, and the answer is decided by the first word in DuckDB's grammar
/// too, so there is nothing a heavier check would learn. A leading comment or leading whitespace is
/// stepped over first, because the corpus has both.
fn describable(statement: &str) -> bool {
    const QUERIES: [&str; 10] = [
        "select",
        "with",
        "values",
        "table",
        "from",
        "show",
        "describe",
        "summarize",
        "pivot",
        "unpivot",
    ];
    let mut rest = statement.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
            continue;
        }
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
            continue;
        }
        break;
    }
    // A parenthesised query, which is how the corpus writes a bare set operation.
    if rest.starts_with('(') {
        return true;
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .flat_map(char::to_lowercase)
        .collect();
    QUERIES.contains(&word.as_str())
}

fn copy_of(statement: &str, to: &Path) -> String {
    let quoted = to.display().to_string().replace('\'', "''");
    format!("COPY ({statement}) TO '{quoted}' (FORMAT csv, HEADER, FORCE_QUOTE *)")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Duckdb, PINNED_COMMIT, Pin, classify, copy_of, describable, hash_in, on_path};
    use crate::engine::{Cell, Engine, Outcome};
    use std::path::Path;

    /// The version line of the binary built at the vendored commit, read off one of the test
    /// machines rather than made up here.
    const VENDORED: &str = "v2.0.0-dev84237 (Development Version) cc7e7bac7f";

    #[test]
    fn the_commit_is_the_last_word_of_the_version_line() {
        assert_eq!(hash_in(VENDORED), Some("cc7e7bac7f"));
        assert_eq!(hash_in("v1.5.5 (Variegata) d8cdaa33fd"), Some("d8cdaa33fd"));
        assert_eq!(hash_in("v1.4.4 (Andium) 6ddac802ff"), Some("6ddac802ff"));
        // Nothing that is not a hash is read as one, including the version itself.
        assert_eq!(hash_in("v1.5.5"), None);
        assert_eq!(hash_in(""), None);
    }

    #[test]
    fn the_binary_at_the_vendored_commit_is_the_pinned_one() {
        assert_eq!(classify(VENDORED), Pin::Pinned);
        // The whole hash and the short one are the same commit.
        assert_eq!(
            classify(&format!("v2.0.0-dev84237 (Development Version) {PINNED_COMMIT}")),
            Pin::Pinned
        );
    }

    #[test]
    fn a_v2_alpha_at_another_commit_is_not_the_pinned_one() {
        // This is the case the old version string check passed. The branch moves every day, so a
        // v2.0 alpha from another day is a different language from the vendored grammar.
        assert_eq!(classify("v2.0.0-dev84100 (Development Version) 0123456789"), Pin::OtherCommit);
    }

    #[test]
    fn a_released_binary_is_the_fallback() {
        assert_eq!(classify("v1.5.5 (Variegata) d8cdaa33fd"), Pin::Fallback);
        assert_eq!(classify("v1.4.4 (Andium) 6ddac802ff"), Pin::Fallback);
    }

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

    #[test]
    fn the_statements_the_two_wrappers_take_are_the_ones_that_go_through_them() {
        for query in [
            "SELECT 1",
            "select 1",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "VALUES (1)",
            "TABLE t",
            "FROM range(3)",
            "SHOW TABLES",
            "DESCRIBE t",
            "SUMMARIZE SELECT 1",
            "PIVOT t ON i",
            "UNPIVOT t ON i",
            "(SELECT 1) UNION (SELECT 2)",
        ] {
            assert!(describable(query), "{query} is a query");
        }
    }

    #[test]
    fn a_statement_that_is_not_a_query_does_not_go_through_them() {
        for other in [
            "CREATE TABLE t(i INTEGER)",
            "INSERT INTO t VALUES (1)",
            "INSERT INTO t VALUES (1) RETURNING i",
            "UPDATE t SET i = 2",
            "DELETE FROM t",
            "SET memory_limit = '1GB'",
            "PRAGMA version",
            "CALL range(3)",
            "EXPLAIN SELECT 1",
            "BEGIN TRANSACTION",
            "COPY t TO 'x.csv'",
            "ATTACH 'x.db'",
            "",
        ] {
            assert!(!describable(other), "{other} is not a query");
        }
    }

    #[test]
    fn a_leading_comment_is_stepped_over_rather_than_read_as_the_first_word() {
        assert!(describable("-- what this is about\nSELECT 1"));
        assert!(describable("/* what this is about */ SELECT 1"));
        assert!(describable("\n  \t SELECT 1"));
        assert!(!describable("-- this selects nothing\nCREATE TABLE t(i INTEGER)"));
    }

    #[test]
    fn a_word_that_only_starts_with_a_query_word_is_not_a_query() {
        assert!(!describable("selectivity(1)"));
        assert!(!describable("from_base64('x')"));
        assert!(!describable("table_name"));
    }

    #[test]
    fn a_statement_that_is_not_a_query_runs_and_comes_back_as_having_worked() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Rows(table) = db.run("CREATE TABLE made_here(i INTEGER)").unwrap() else {
            panic!("DuckDB parses its own CREATE TABLE");
        };
        assert_eq!(table.width(), 0);
        assert_eq!(table.height(), 0);
    }

    #[test]
    fn a_statement_that_is_not_a_query_and_is_wrong_still_comes_back_as_the_error() {
        let Some(mut db) = duckdb() else { return };
        let Outcome::Error(e) = db.run("INSERT INTO nothing_made_this VALUES (1)").unwrap() else {
            panic!("there is no such table");
        };
        assert!(e.kind.contains("Catalog"), "{e}");
    }
}
