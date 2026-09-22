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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::duckdb::on_path;
use crate::engine::{
    Acceptance, Cell, Column, Engine, EngineError, HarnessError, Outcome, Table, assemble,
};
use crate::quote;
use crate::resource::{Meter, Metered, Usage};

/// One shell, and the session state to put in front of every statement.
///
/// This is also where the harness measures what a statement cost, and it is the only place it can.
/// Both engines are processes here and they are reached through one driver, so wrapping the
/// process in a meter measures the two sides the same way, which is the whole requirement.
/// `crate::rudb` links the engine as a library and cannot answer the same question honestly, so it
/// does not answer it at all.
#[derive(Debug, Clone)]
pub struct Shell {
    name: String,
    binary: PathBuf,
    version: String,
    database: String,
    setup: Vec<String>,
    meter: Meter,
    last: Option<Usage>,
    limit: Option<Duration>,
    killer: Option<PathBuf>,
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
            meter: Meter::find(),
            last: None,
            limit: None,
            killer: killer(),
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

    /// Stop a statement that has not answered within this long and call it a refusal.
    ///
    /// A corpus run needs this and a differential run does not. The corpus has statements in it
    /// that one engine answers in a second and the other does not answer at all, and without a
    /// limit the first of those ends the run: every benchmark after it goes unmeasured while one
    /// process sits on a join it is never going to finish. A limit turns that from a dead run into
    /// one refused benchmark with the reason written next to it, which is a result.
    ///
    /// It needs a `timeout` on the machine and quietly does nothing without one, because a caller
    /// that cannot have the limit still wants the numbers.
    #[must_use]
    pub fn within(mut self, limit: Duration) -> Self {
        self.limit = Some(limit);
        self
    }

    /// True when this shell can stop a statement that will not finish.
    #[must_use]
    pub const fn can_stop(&self) -> bool {
        self.killer.is_some()
    }

    /// The binary being driven, for the report.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Run one statement in a fresh process and hand back what it wrote.
    ///
    /// `-init /dev/null` is not optional. Both shells read a startup file otherwise, so without it
    /// a developer with a `.duckdbrc` setting an output mode would get results this cannot read and
    /// a report that says the engines disagree.
    fn invoke(&self, statement: &str) -> Result<Written, HarnessError> {
        let mut metered = self.started();
        let command = metered.command();
        command.arg("-batch").arg("-init").arg(devnull()).arg("-cmd").arg(".mode quote");
        for setup in &self.setup {
            command.arg("-c").arg(setup);
        }
        command.arg(&self.database).arg(statement);
        let (out, cost) = metered
            .output()
            .map_err(|e| HarnessError::new(format!("cannot run {}: {e}", self.binary.display())))?;
        Ok(Written {
            failed: !out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            cost,
        })
    }

    /// The command to start, which is the binary on its own or the binary under the killer.
    ///
    /// The killer goes inside the meter rather than outside it, so the order is time, then timeout,
    /// then the shell. That way the numbers still come back for a statement that was stopped, since
    /// `getrusage` on a child counts everything underneath it, and the process the limit applies to
    /// is the shell rather than the meter. The other order loses both: killing the meter leaves the
    /// shell running with nothing waiting on it.
    fn started(&self) -> Metered {
        let (Some(killer), Some(limit)) = (self.killer.as_ref(), self.limit) else {
            return self.meter.command(&self.binary);
        };
        let mut metered = self.meter.command(killer);
        metered
            .command()
            .arg("-k")
            .arg("1")
            .arg(format!("{}", limit.as_secs().max(1)))
            .arg(&self.binary);
        metered
    }

    /// What a failed run means, which is usually what it said and sometimes that it said nothing.
    ///
    /// A stopped statement writes no error text, so reading the empty stderr would file it under
    /// the same kind as every other unrecognizable failure and lose the one thing worth knowing
    /// about it.
    fn declined(&self, out: &Written) -> EngineError {
        match self.limit {
            Some(limit) if out.code == Some(STOPPED) => EngineError {
                kind: "Timeout".to_owned(),
                message: format!("no answer within {} seconds", limit.as_secs()),
            },
            _ => EngineError::parse(&out.stderr),
        }
    }

    /// Run one statement for what it costs rather than for what it answers.
    ///
    /// Nothing written is read back beyond whether the process failed, and that is the point. The
    /// shell writes every result to one stream, so a setup statement that returns a row, and
    /// `SELECT setseed(0.1)` is one, lands in front of the statement's own rows and the two arrive
    /// as one run of text that no reader can split. `Engine::run` fails on exactly that, and a
    /// caller timing a benchmark does not want the rows in the first place. What the rows say is
    /// the differential loop's question and it is asked somewhere else.
    ///
    /// It is also one process instead of two, because there is no `DESCRIBE` to send when nobody
    /// wants the types, which halves what a measurement costs the machine running it.
    ///
    /// The `Ok(None)` case is a machine that cannot measure, per `crate::resource`.
    ///
    /// # Errors
    ///
    /// When the binary cannot be started. A statement the engine refuses comes back as the inner
    /// `Err` and is a fact about the engine rather than about the harness.
    pub fn timed(&self, sql: &str) -> Result<Result<Option<Usage>, EngineError>, HarnessError> {
        let out = self.invoke(sql.trim().trim_end_matches(';'))?;
        if out.failed {
            return Ok(Err(self.declined(&out)));
        }
        Ok(Ok(out.cost))
    }

    /// Turn the meter off, for a caller that wants the answers and not the cost of measuring.
    ///
    /// Measuring costs a fork per statement on top of the fork that was already happening, which
    /// is nothing next to starting a shell and is not nothing when it happens four thousand times
    /// for a run that is going to throw the numbers away.
    #[must_use]
    pub fn unmetered(mut self) -> Self {
        self.meter = Meter::off();
        self
    }
}

/// What `timeout` exits with when it stopped the thing it was watching.
const STOPPED: i32 = 124;

/// The `timeout` binary, when the machine has one.
///
/// GNU coreutils calls it `timeout` and Homebrew's coreutils calls it `gtimeout`, and the BSD
/// userland on macOS ships neither, so a mac without Homebrew gets no limit. That is the same
/// answer the meter gives on the same machine and for the same reason: the corpus runs on the
/// Linux boxes, and a mac is where the tests run rather than where the numbers come from.
fn killer() -> Option<PathBuf> {
    ["/usr/bin/timeout", "/bin/timeout", "/opt/homebrew/bin/gtimeout", "/usr/local/bin/gtimeout"]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// What one run of a shell wrote, and what it cost.
struct Written {
    failed: bool,
    /// The exit status, which is `None` when a signal ended it.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// Nothing when this machine cannot measure, per `crate::resource`.
    cost: Option<Usage>,
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
        // The statement's cost is the run that answered it. The DESCRIBE below is the harness
        // asking a second question of its own, and charging that to the engine would be measuring
        // this file rather than the engine it drives.
        self.last = rows.cost;
        if rows.failed {
            return Ok(Outcome::Error(EngineError::parse(&rows.stderr)));
        }
        let stdout = if self.name.starts_with("duckdb") {
            without_duckdb_warnings(&rows.stdout)
        } else {
            &rows.stdout
        };
        let rows = quote::read(stdout)?;
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
        let stdout = if self.name.starts_with("duckdb") {
            without_duckdb_warnings(&types.stdout)
        } else {
            &types.stdout
        };
        assemble(&quote::read(stdout)?, &rows)
            .map(Outcome::Rows)
            .map_err(|e| HarnessError::new(format!("{}: {e}", self.name)))
    }

    fn usage(&self) -> Option<Usage> {
        self.last
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

/// Removes DuckDB warning blocks from the result stream consumed by the quote reader.
///
/// The shell writes warnings to standard output with ANSI markers even in batch mode.
/// They are not result rows, and treating one as a row makes a successful `SET` look like a query and prevents [`Session`] from replaying it.
fn without_duckdb_warnings(mut output: &str) -> &str {
    const HEADING: &str = "\u{1b}[90mWARNING:\n\u{1b}[00m";
    const RESET: &str = "\u{1b}[00m";
    while let Some(after_heading) = output.strip_prefix(HEADING) {
        let Some(end) = after_heading.find(RESET) else { break };
        output = &after_heading[end + RESET.len()..];
    }
    output
}

/// A shell that remembers what it has been told, so that a file of statements runs as one session.
///
/// [`Shell`] runs every statement in a fresh process, which is what the differential corpora want,
/// because each of their statements stands alone and a process per statement is the cheapest way to
/// be sure of it. A sqllogictest file is the other shape: it creates a table, fills it, asks it
/// questions and drops it, and none of that works if the process running the second statement never
/// heard the first one.
///
/// There are two ways to do that here. The old one, which is still the default, rebuilds the
/// session in front of every statement out of the statements that printed nothing. That set is
/// exactly the one that leaves something behind and nothing on the screen: `CREATE`, `INSERT`,
/// `DROP`, `SET`. A statement that printed rows is not replayed, both because replaying it would put
/// its rows in front of the next answer where the reader expects one table, and because a query has
/// nothing to leave behind. It costs a process per statement plus a replay of the file so far, which
/// is quadratic, and it is also why a measurement of one record means nothing under it.
///
/// The new one is [`Session::on_a_file`], and it is what the replay is meant to turn into. rudb
/// writes its database file when the last handle on it goes away, tamnd/rudb#1226, and lets a table
/// that is already in the file take an append, tamnd/rudb#1228, so a session can now be a file the
/// way it is for anybody who uses either engine for real. The session gets a directory of its own
/// with a database called `memory` in it, so `current_database()` answers the same word it answers
/// with no file, and the file keeps the state, so nothing is replayed and what a record costs is
/// what the record costs.
///
/// Two things a file does not keep, and they are handled differently because they cost differently.
/// The first is the settings, which are per process, so `SET`, `RESET` and a `PRAGMA` that printed
/// nothing are remembered and put in front of every later statement. There are a handful of those
/// in a file and they are cheap. The second is everything else a process owns rather than a database
/// does, which is a temporary table or view, an `ATTACH`, a transaction, a prepared statement and a
/// loaded extension. Those cannot be put in front of one statement without putting the whole file in
/// front of it, so the first one turns the file off, deletes it and goes back to the replay.
///
/// The file is not the default yet because rudb cannot write half the column types down.
/// `CREATE TABLE t (x DOUBLE)` fails on a database with a file, and so does `FLOAT`, `HUGEINT`,
/// `TIME`, `TIMESTAMPTZ`, `INTERVAL`, `UUID`, `BLOB`, `BIT`, and every nested type, which is
/// tamnd/rudb#1244, tamnd/rudb#1245 and tamnd/rudb#1246. Four files in the committed corpus create a
/// table with one of those in it. When #1244 and #1245 land the default flips, this paragraph goes,
/// and `on_a_file` goes with it.
#[derive(Debug)]
pub struct Session {
    /// The shell underneath, with no setup of its own.
    shell: Shell,
    /// What has been run and printed nothing, in the order it was run.
    history: Vec<String>,
    /// The ones of those the file does not keep, which is the settings.
    settings: Vec<String>,
    /// The directory holding this session's database, made when the first statement needs it.
    home: Option<PathBuf>,
    /// Whether a file was asked for, which is what a reset goes back to.
    wanted: bool,
    /// Whether the file is still carrying the session, which a fallback turns off until the reset.
    on_file: bool,
    /// What the last statement cost, when nothing was replayed in front of it.
    last: Option<Usage>,
}

impl Session {
    /// Wrap a shell so that what it is told sticks until the next reset.
    #[must_use]
    pub fn new(shell: Shell) -> Self {
        Self {
            shell,
            history: Vec::new(),
            settings: Vec::new(),
            home: None,
            wanted: false,
            on_file: false,
            last: None,
        }
    }

    /// Keep the session in a database file rather than replaying it in front of every statement.
    ///
    /// Opt in for now, for the reason on the type. Call it before the first statement: a session
    /// that has already run something has its state in the replay and moving it is not a thing this
    /// does.
    #[must_use]
    pub fn on_a_file(mut self) -> Self {
        self.wanted = true;
        self.on_file = true;
        self
    }

    /// Everything this session has been told that printed nothing, in the order it was told.
    ///
    /// Only the settings are replayed while the file is carrying the session. The rest is here
    /// because the fallback needs it: a statement the file cannot keep turns the file off, and
    /// what has to be rebuilt in memory at that point is the whole file so far.
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// True while the file is carrying the session rather than the replay.
    #[must_use]
    pub const fn on_file(&self) -> bool {
        self.on_file
    }

    /// The directory this session's database lives in, made the first time it is asked for.
    ///
    /// # Errors
    ///
    /// When the directory cannot be made.
    fn home(&mut self) -> Result<&Path, HarnessError> {
        if self.home.is_none() {
            let n = SESSIONS.fetch_add(1, Ordering::Relaxed);
            let at = std::env::temp_dir()
                .join(format!("rudb-compat-session-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&at);
            std::fs::create_dir_all(&at)
                .map_err(|e| HarnessError::new(format!("cannot make {}: {e}", at.display())))?;
            self.home = Some(at);
        }
        Ok(self.home.as_deref().expect("the directory was just made"))
    }

    /// The shell with the session in front of it, ready for one statement.
    ///
    /// # Errors
    ///
    /// When the session's directory cannot be made.
    fn primed(&mut self) -> Result<Shell, HarnessError> {
        if !self.on_file {
            return Ok(self.shell.clone().with_setup(self.history.clone()));
        }
        let database = self.home()?.join("memory").to_string_lossy().into_owned();
        Ok(self.shell.clone().on(database).with_setup(self.settings.clone()))
    }

    /// Give the file up and go back to replaying the whole session in memory.
    ///
    /// Everything that has run so far is in the history, so the replay rebuilds the same state the
    /// file was holding. The file itself is deleted rather than left behind, because the statement
    /// that caused this is about to run against a database with no file at all and a directory
    /// nobody is going to open again is a directory nobody is going to delete either.
    fn leave_the_file(&mut self) {
        self.on_file = false;
        if let Some(home) = self.home.take() {
            let _ = std::fs::remove_dir_all(home);
        }
    }

    /// Remember a statement that printed nothing, under whichever of the two rules it falls.
    fn remember(&mut self, statement: &str) {
        if is_a_setting(statement) {
            self.settings.push(statement.to_owned());
        }
        self.history.push(statement.to_owned());
    }
}

/// How many sessions this process has built, so that two of them never share a directory.
static SESSIONS: AtomicUsize = AtomicUsize::new(0);

impl Engine for Session {
    fn name(&self) -> &str {
        self.shell.name()
    }

    fn version(&self) -> &str {
        self.shell.version()
    }

    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
        let statement = sql.trim().trim_end_matches(';');
        if self.on_file && !the_file_keeps_it(statement) {
            self.leave_the_file();
        }
        let mut shell = self.primed()?;
        let outcome = shell.run(statement)?;
        // The number is only worth keeping when nothing ran in front of the statement, because the
        // meter is around the process and the process ran the setup too. That is every statement in
        // a file with no settings in it, which is most of the corpus, and it is none of a file that
        // gave the file up. A wrong number with a confident name is worse than no number.
        self.last = if shell.setup.is_empty() { shell.usage() } else { None };
        if let Outcome::Rows(table) = &outcome {
            if table.width() == 0 {
                self.remember(statement);
            }
        }
        Ok(outcome)
    }

    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError> {
        // Nothing is remembered here on purpose. `accepts` answers whether the text is SQL, it
        // calls a catalog error accepted, and a session it built itself would not change one of
        // its answers. Running it against the session's own database still matters, because a
        // statement that needs a table is a different statement to parse when the table is there.
        self.primed()?.accepts(sql)
    }

    fn reset(&mut self) -> Result<(), HarnessError> {
        self.history.clear();
        self.settings.clear();
        self.last = None;
        self.on_file = self.wanted;
        if let Some(home) = self.home.take() {
            let _ = std::fs::remove_dir_all(home);
        }
        Ok(())
    }

    fn usage(&self) -> Option<Usage> {
        self.last
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(home) = self.home.take() {
            let _ = std::fs::remove_dir_all(home);
        }
    }
}

/// Whether a statement that printed nothing is a setting, which a new process does not inherit.
///
/// A `PRAGMA` is on the list because the spelling is interchangeable with `SET` for a setting, and
/// a `PRAGMA` that reads something prints rows and never reaches here.
fn is_a_setting(statement: &str) -> bool {
    matches!(opens_with(statement).as_str(), "SET" | "RESET" | "PRAGMA")
}

/// Whether the database file keeps what this statement leaves behind.
///
/// False for the things a process owns rather than a database. A temporary table lives in the
/// `temp` catalog, which is per connection and is never written down. An `ATTACH` is a name this
/// process has for another database. A transaction cannot span two processes at all. A prepared
/// statement and a loaded extension are the same shape again. None of those can be put in front of
/// one statement without putting the whole file in front of it, so meeting one is what sends the
/// session back to the replay.
fn the_file_keeps_it(statement: &str) -> bool {
    let mut words = statement.split_whitespace().map(str::to_uppercase);
    let Some(first) = words.next() else {
        return true;
    };
    match first.as_str() {
        "ATTACH" | "DETACH" | "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "ABORT" | "PREPARE"
        | "DEALLOCATE" | "EXECUTE" | "LOAD" | "INSTALL" => false,
        "CREATE" => {
            // `CREATE TEMPORARY TABLE` and `CREATE OR REPLACE TEMPORARY TABLE` are the two ways to
            // say it, so the word being looked for is the next one or the one three along and
            // nowhere else. A table called `temp` is a table and lands between them.
            let rest: Vec<String> = words.take(3).collect();
            let temporary =
                |at: usize| rest.get(at).is_some_and(|word| word == "TEMP" || word == "TEMPORARY");
            !(temporary(0) || temporary(2))
        }
        _ => true,
    }
}

/// The first word of a statement, upper cased, or the empty string when there is not one.
fn opens_with(statement: &str) -> String {
    statement.split_whitespace().next().unwrap_or_default().to_uppercase()
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
    use super::{Session, Shell, the_file_keeps_it, without_duckdb_warnings};
    use crate::engine::{Cell, Engine, Outcome};

    #[test]
    fn duckdb_warnings_are_not_read_as_result_rows() {
        let warning =
            "\u{1b}[90mWARNING:\n\u{1b}[00m\u{1b}[90mThe setting is deprecated.\n\n\u{1b}[00m";
        assert_eq!(without_duckdb_warnings(warning), "");
        let followed = format!("{warning}'value'\n");
        assert_eq!(without_duckdb_warnings(&followed), "'value'\n");
    }

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

    /// Whether this shell can be written to twice on one file, which is tamnd/rudb#1228.
    ///
    /// The binary these tests drive is whatever is on the machine, and the machine running the
    /// gate does not build it from the commit under test. A rudb older than v0.3.83 refuses the
    /// second insert into a table that is already in the file, so the test that writes twice says
    /// which fix it is waiting for rather than failing as though the harness were wrong.
    fn twice_writable(shell: &Shell) -> bool {
        let mut session = Session::new(shell.clone()).on_a_file();
        let _ = session.run("CREATE TABLE probe (a INTEGER)");
        let _ = session.run("INSERT INTO probe VALUES (1)");
        matches!(session.run("INSERT INTO probe VALUES (2)"), Ok(Outcome::Rows(_)))
    }

    #[test]
    fn a_session_runs_on_a_file_and_replays_nothing_in_front_of_a_statement() {
        // The point of the file. Three statements that all left something behind, and the fourth
        // one still starts with an empty setup, so what it costs is what it costs.
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            if !twice_writable(&shell) {
                eprintln!("skipping {name}, this shell predates tamnd/rudb#1228");
                continue;
            }
            let mut session = Session::new(shell).on_a_file();
            session.run("CREATE TABLE t (a INTEGER)").unwrap();
            session.run("INSERT INTO t VALUES (1)").unwrap();
            session.run("INSERT INTO t VALUES (2)").unwrap();
            assert!(session.on_file(), "{name}");
            assert_eq!(session.history().len(), 3, "{name}");
            assert!(session.settings.is_empty(), "{name}");
            let Outcome::Rows(table) = session.run("SELECT sum(a) FROM t").unwrap() else {
                panic!("{name}: the file should still have both rows in it");
            };
            assert_eq!(table.rows, vec![vec![Cell::Text("3".into())]], "{name}");
        }
    }

    #[test]
    fn a_session_on_a_file_is_still_called_memory() {
        // Four files in the corpus read the database's name out of the catalog, and every one of
        // them was written against a session with no file. The file is called `memory` so that
        // they go on saying what they said.
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            let mut session = Session::new(shell).on_a_file();
            let Outcome::Rows(table) = session.run("SELECT current_database()").unwrap() else {
                panic!("{name}: that query works on both engines");
            };
            assert_eq!(table.rows, vec![vec![Cell::Text("memory".into())]], "{name}");
        }
    }

    #[test]
    fn a_temporary_table_turns_the_file_off_and_the_session_goes_on_working() {
        // A temporary table lives in the connection rather than in the database, so a file cannot
        // carry one. The session gives the file up at that point and rebuilds itself out of the
        // history instead, which is what it used to do for every statement of every file.
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            if !twice_writable(&shell) {
                eprintln!("skipping {name}, this shell predates tamnd/rudb#1228");
                continue;
            }
            let mut session = Session::new(shell).on_a_file();
            session.run("CREATE TABLE kept (a INTEGER)").unwrap();
            session.run("INSERT INTO kept VALUES (4)").unwrap();
            assert!(session.on_file(), "{name}");
            let made = session.run("CREATE TEMPORARY TABLE gone (b INTEGER)").unwrap();
            let Outcome::Rows(_) = made else {
                panic!("{name}: both engines take a temporary table: {made:?}");
            };
            assert!(!session.on_file(), "{name}: a temporary table should have given the file up");
            session.run("INSERT INTO gone VALUES (5)").unwrap();
            let Outcome::Rows(table) = session.run("SELECT a, b FROM kept, gone").unwrap() else {
                panic!("{name}: the table from before the fallback should still be there");
            };
            assert_eq!(
                table.rows,
                vec![vec![Cell::Text("4".into()), Cell::Text("5".into())]],
                "{name}"
            );
        }
    }

    #[test]
    fn a_setting_is_put_back_in_front_of_every_later_statement() {
        // The other half of what a file does not keep. A setting belongs to the process, so it has
        // to be replayed, and there are few enough of them per file that replaying them is cheap.
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            let mut session = Session::new(shell).on_a_file();
            session.run("SET disabled_optimizers = 'filter_pushdown'").unwrap();
            assert_eq!(session.settings.len(), 1, "{name}");
            assert!(session.on_file(), "{name}: a setting does not give the file up");
            let Outcome::Rows(table) =
                session.run("SELECT current_setting('disabled_optimizers')").unwrap()
            else {
                panic!("{name}: both engines can read a setting back");
            };
            assert_eq!(table.rows, vec![vec![Cell::Text("filter_pushdown".into())]], "{name}");
        }
    }

    #[test]
    fn a_session_reports_what_a_statement_cost_when_nothing_ran_in_front_of_it() {
        // The whole reason for the file. This used to answer nothing, because the number would
        // have been the statement plus the file so far.
        for shell in shells() {
            let name = Engine::name(&shell).to_owned();
            if !shell.meter.measures() {
                eprintln!("skipping {name}, nothing on this machine measures a child");
                continue;
            }
            let mut session = Session::new(shell).on_a_file();
            session.run("CREATE TABLE t AS SELECT range AS a FROM range(1000)").unwrap();
            session.run("SELECT sum(a) FROM t").unwrap();
            assert!(session.usage().is_some(), "{name}: a plain statement should be measured");
            session.run("SET disabled_optimizers = 'filter_pushdown'").unwrap();
            session.run("SELECT sum(a) FROM t").unwrap();
            assert!(
                session.usage().is_none(),
                "{name}: a statement with a setting in front of it is not measured on its own"
            );
        }
    }

    #[test]
    fn what_the_file_keeps_is_read_off_the_first_words() {
        for kept in [
            "CREATE TABLE t (a INTEGER)",
            "CREATE OR REPLACE TABLE t (a INTEGER)",
            "CREATE VIEW v AS SELECT 1",
            "CREATE TABLE temp (a INTEGER)",
            "INSERT INTO t VALUES (1)",
            "DROP TABLE t",
            "SET memory_limit = '1GiB'",
            "",
        ] {
            assert!(the_file_keeps_it(kept), "{kept}");
        }
        for lost in [
            "CREATE TEMPORARY TABLE t (a INTEGER)",
            "CREATE TEMP TABLE t (a INTEGER)",
            "CREATE OR REPLACE TEMPORARY VIEW v AS SELECT 1",
            "create temporary table t (a integer)",
            "ATTACH ':memory:' AS other",
            "BEGIN TRANSACTION",
            "PREPARE p AS SELECT 1",
            "LOAD json",
        ] {
            assert!(!the_file_keeps_it(lost), "{lost}");
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

    #[test]
    fn timing_a_statement_reads_nothing_back_so_a_setup_that_prints_rows_cannot_break_it() {
        // The shell writes the setup's rows and the statement's rows to one stream with nothing in
        // between saying where the first ended, so a reader sees one table with two shapes in it
        // and gives up. A run for the cost does not want either set of rows, and this is the whole
        // reason it does not ask for them. `SELECT 42` stands in for `SELECT setseed(0.1)`, which
        // is what the benchmark corpus actually opens with.
        for shell in shells() {
            let shell = shell.with_setup(vec!["SELECT 42".to_owned()]);
            let timed = shell.timed("SELECT 1").unwrap();
            assert!(timed.is_ok(), "{}: {timed:?}", Engine::name(&shell));
        }
    }

    #[test]
    fn a_statement_that_is_not_going_to_finish_is_stopped_and_reads_as_a_timeout() {
        for shell in shells() {
            let shell = shell.within(std::time::Duration::from_secs(1));
            if !shell.can_stop() {
                eprintln!("skipping, no timeout on this machine");
                continue;
            }
            let forever = "SELECT sum(a.range * b.range) \
                           FROM range(100000000) a, range(100000000) b";
            let Err(e) = shell.timed(forever).unwrap() else {
                panic!("{}: nothing answers that in a second", Engine::name(&shell));
            };
            assert_eq!(e.kind, "Timeout", "{}", Engine::name(&shell));
        }
    }
}
