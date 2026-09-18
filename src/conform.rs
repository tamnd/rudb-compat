//! Running a `.test` file and counting what happened.
//!
//! This is the half of the harness that does not need DuckDB on the machine. A sqllogictest file
//! already carries what every statement is supposed to produce, so the comparison is against the
//! file rather than against a second engine, which is what makes it something CI can run on every
//! commit and publish a number from.
//!
//! The differential loop in `crate::suite` and this are not competing. They answer different
//! questions. The differential loop finds behaviour DuckDB has that nobody wrote a test for, which
//! is most of it. This one finds behaviour DuckDB's own authors thought was worth pinning down,
//! and it finds it at a volume and a speed the differential loop cannot reach because it never
//! starts a second process.
//!
//! One rule runs through all of it: a record that could not be attempted is skipped and counted as
//! skipped, and it never becomes a pass. `spec/14-rudb-compat.md` section 14.1 says a percentage
//! with no test behind it is not a claim, and the fastest way to a fake percentage is a runner
//! that treats what it cannot do as fine.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::engine::{Cell, Engine, EngineError, HarnessError, Outcome, Table};
use crate::hash::hash_values;
use crate::kinds::Kinds;
use crate::slt::{
    Directive, ParseError, QueryResult, Record, Setting, Sort, StatementResult, TestFile,
};

/// The names this runner answers to in a `skipif` or an `onlyif`.
///
/// `duckdb` is in the list on purpose. The entire claim of this project is that rudb is DuckDB, so
/// a record DuckDB is expected to pass is one rudb is expected to pass, and a record DuckDB is
/// excused from is one rudb is excused from for the same underlying reason. Leaving `duckdb` out
/// would make the corpus run tests that were disabled because they do not work on DuckDB either,
/// and every one of those would be a failure that means nothing.
pub const NAMES: &[&str] = &["rudb", "duckdb"];

/// rudb's vector size, which is what a `require vector_size` line is asking about.
///
/// It mirrors `rudb_vector::VECTOR_SIZE`, which the `rudb` facade does not re-export, so this
/// repeats the number rather than reading it. The direction of a drift is the thing to know. If
/// rudb grows its vector and this stays where it is, the runner skips files it could have run,
/// which shows up as a skip count that will not go down. If rudb shrinks its vector and this stays,
/// the runner attempts files written for a larger one, which shows up as failures. Both are
/// visible, and the first is the one that happens.
const VECTOR_SIZE: usize = 1024;

/// The things the corpus requires that rudb has without loading anything.
///
/// DuckDB ships these as extensions and the corpus asks for them by name. rudb has no extension
/// mechanism at all, so the question is not whether the extension is loaded but whether the
/// capability is there, and for parquet it is: `read_parquet` and the parquet reader are in the
/// engine. A name that lands here wrongly costs a wall of failures with a reason on each, and a
/// name missing from here costs a silently smaller corpus, so the short list is the safe one.
const BUILT_IN: &[&str] = &["parquet"];

/// What kind of thing went wrong, as opposed to what went wrong.
///
/// `spec/14-rudb-compat.md` section 14.1 asks for the report to break failures down rather than
/// publish one percentage, and this is the axis it breaks down on. The point is that the nine
/// reasons below are nine different jobs for nine different people. A `Syntax` is a grammar rule
/// nobody has written, an `Unbound` is usually one function, a `WrongAnswer` is a bug at
/// `priority/p0` and the three error reasons are a message somebody has to copy exactly. A number
/// that adds them together tells whoever reads it nothing about what to do next, which is the only
/// thing a conformance report is for.
///
/// The classification is made where the failure is, from what the engine actually said, and not
/// afterwards by matching on the text of the report. Doing it afterwards would mean the categories
/// drift every time somebody rewords a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The engine stopped it, on the query timeout or on the memory budget.
    ///
    /// Furthest from working, because the engine did not get to an answer at all. It is a reason
    /// of its own and not a `Runtime` because it is the only one that is not about the statement:
    /// the same statement with a longer clock or a larger budget might pass, and grouping it with
    /// the errors that are about the SQL would put a query that is merely slow on a list of bugs.
    Stopped,
    /// The file expected it to work and the engine could not parse it.
    Syntax,
    /// The file expected it to work and the engine parsed it and has not built it yet.
    NotImplemented,
    /// The file expected it to work and the engine could not resolve a name in it.
    ///
    /// A function, a table, a column or a type. Almost always one missing function, which is why
    /// this is worth its own line: it is the reason with the best ratio of records recovered to
    /// work done, and reading it off the report beats reading four thousand files.
    Unbound,
    /// The file expected it to work and the engine raised something else.
    Runtime,
    /// The engine returned rows and they are not the rows the file says.
    ///
    /// The one reason on this list that is a bug rather than a schedule item. Everything else is
    /// the engine being honest about something it cannot do, and this is the engine being wrong
    /// while looking right, which is the failure mode the whole project is trying not to have.
    WrongAnswer,
    /// The file expected an error and the engine was happy.
    MissedError,
    /// The file expected an error and the engine raised one of a different kind.
    ErrorClass,
    /// The kind was right and the message did not contain what the file asked for.
    ErrorText,
}

impl Reason {
    /// Every reason, in the order the report prints them.
    ///
    /// Roughly from furthest from working to closest, so a report read top to bottom is read in
    /// the order the work happens in.
    pub const ALL: [Self; 9] = [
        Self::Stopped,
        Self::Syntax,
        Self::NotImplemented,
        Self::Unbound,
        Self::Runtime,
        Self::WrongAnswer,
        Self::MissedError,
        Self::ErrorClass,
        Self::ErrorText,
    ];

    /// The short name used in the breakdown and in the wire format.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Syntax => "syntax",
            Self::NotImplemented => "not-implemented",
            Self::Unbound => "unbound",
            Self::Runtime => "runtime",
            Self::WrongAnswer => "wrong-answer",
            Self::MissedError => "missed-error",
            Self::ErrorClass => "error-class",
            Self::ErrorText => "error-text",
        }
    }

    /// What the name means, for the line the report prints next to the count.
    #[must_use]
    pub const fn blurb(self) -> &'static str {
        match self {
            Self::Stopped => "the engine stopped it, on the clock or on the memory budget",
            Self::Syntax => "the engine could not parse it",
            Self::NotImplemented => "parsed, and the engine has not built it yet",
            Self::Unbound => "a function, table, column or type the engine does not have",
            Self::Runtime => "it should have worked and the engine raised something else",
            Self::WrongAnswer => "rows came back and they are the wrong rows",
            Self::MissedError => "the file expected an error and the engine was happy",
            Self::ErrorClass => "an error of the wrong kind",
            Self::ErrorText => "the right kind of error with the wrong words in it",
        }
    }

    /// Read a reason back from its name, for the wire format.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.name() == name)
    }

    /// Which reason an error is, when the file expected the statement to work.
    ///
    /// The kinds are DuckDB's own prefixes, which `rudb-common` reproduces, so this is reading a
    /// closed set rather than guessing from prose. `Binder Error` is grouped with `Catalog Error`
    /// because from the outside they are the same complaint, which is that a name in the statement
    /// does not resolve, and splitting them would put the same missing function in two rows
    /// depending on which layer noticed it first.
    #[must_use]
    pub fn of(error: &EngineError) -> Self {
        match error.kind.as_str() {
            "Interrupt Error" | "Out of Memory Error" => Self::Stopped,
            "Parser Error" => Self::Syntax,
            "Not implemented Error" => Self::NotImplemented,
            "Catalog Error" | "Binder Error" => Self::Unbound,
            _ => Self::Runtime,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How many failures there were of each reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reasons([usize; Reason::ALL.len()]);

impl Reasons {
    /// Count a list of failures by reason.
    #[must_use]
    pub fn of(failures: &[Failure]) -> Self {
        let mut out = Self::default();
        for failure in failures {
            out.0[Reason::ALL.iter().position(|r| *r == failure.reason).unwrap_or(0)] += 1;
        }
        out
    }

    /// How many failures had this reason.
    #[must_use]
    pub fn count(&self, reason: Reason) -> usize {
        Reason::ALL.iter().position(|r| *r == reason).map_or(0, |at| self.0[at])
    }

    /// Every reason that happened at least once, most frequent first.
    ///
    /// A reason with no failures is left out rather than printed as a zero, because a report where
    /// most lines are zero is a report whose non zero lines are hard to find.
    #[must_use]
    pub fn rows(&self) -> Vec<(Reason, usize)> {
        let mut rows: Vec<(Reason, usize)> =
            Reason::ALL.into_iter().zip(self.0).filter(|(_, count)| *count > 0).collect();
        rows.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        rows
    }

    /// Every failure, however it happened.
    #[must_use]
    pub fn total(&self) -> usize {
        self.0.iter().sum()
    }
}

impl fmt::Display for Reasons {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rows = self.rows();
        if rows.is_empty() {
            return f.write_str("no failures to break down");
        }
        writeln!(f, "failures by reason")?;
        for (reason, count) in rows {
            writeln!(f, "    {count:>7}  {:<16}{}", reason.name(), reason.blurb())?;
        }
        Ok(())
    }
}

/// Why one record did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Which file.
    pub file: String,
    /// Which line the directive was on.
    pub line: usize,
    /// The SQL, as it ran, which for a loop body is the iteration that failed and not the template.
    pub sql: String,
    /// What kind of failure it is, which is what the breakdown counts.
    pub reason: Reason,
    /// What went wrong in this particular case, in one or more lines.
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}:{}  {}", self.file, self.line, self.reason)?;
        for line in self.sql.lines() {
            writeln!(f, "    {line}")?;
        }
        for line in self.detail.lines() {
            writeln!(f, "  {line}")?;
        }
        Ok(())
    }
}

/// Whose gap a skipped record is.
///
/// This is the split `spec/sql/duckdb/11-the-number.md` asks for, and without it the skip count is
/// one number covering four different situations that mean opposite things. A record the file
/// itself turned off is nobody's problem. A record behind a feature rudb does not have is on the
/// rudb schedule. A record behind a directive this runner does not implement is work here, and it
/// is the one that quietly makes the pass rate look better than it is, because it is the only kind
/// that can be removed without the engine improving at all. A record the machine could not host is
/// none of those and would be misleading in any of their rows.
///
/// The four are never added together into one headline. Adding them is exactly what made the old
/// skip count unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gap {
    /// The file turned the record off itself, with a `skipif`, an `onlyif` or a `mode skip`.
    Excused,
    /// The engine does not have what the record needs. The real gap, and the one that goes down
    /// when rudb gets better.
    Engine,
    /// The harness does not do what the record needs. Work here, not there.
    Harness,
    /// The machine the run is on does not have it: not Windows, not enough memory, an environment
    /// variable pointing at a service somebody else runs.
    Machine,
}

impl Gap {
    /// Every gap, in the order the report prints them.
    pub const ALL: [Self; 4] = [Self::Excused, Self::Engine, Self::Harness, Self::Machine];

    /// The short name used in the report and in the wire format.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Excused => "excused",
            Self::Engine => "engine",
            Self::Harness => "harness",
            Self::Machine => "machine",
        }
    }

    /// What the name means, for the line the report prints next to the count.
    #[must_use]
    pub const fn blurb(self) -> &'static str {
        match self {
            Self::Excused => "the file turned the record off itself",
            Self::Engine => "something rudb does not have, which is the real gap",
            Self::Harness => "something this runner does not do, which is work here",
            Self::Machine => "something the machine this ran on does not have",
        }
    }
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a whole file was not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// A `require` for something that is not here, with whose gap that is and how many records
    /// went with it.
    ///
    /// The count is the point. A file skipped whole used to contribute nothing to the skip count
    /// and nothing to the denominator, so several hundred files worth of records were in neither
    /// column and no line of the report said so.
    Requires {
        /// What the file asked for, worded as the file wrote it.
        what: String,
        /// Whose gap it is.
        gap: Gap,
        /// How many records were behind it.
        records: usize,
    },
    /// A directive this run cannot follow and cannot safely ignore, so everything after it was not
    /// attempted.
    ///
    /// This is the one that used to be silent. A `restart` means the file is about to check that
    /// what it wrote is still there after the database was reopened from its file. Carrying the
    /// directive and then running past it leaves the database exactly as it was, so every one of
    /// those checks passes, and it passes for the reason the file was written to rule out.
    ///
    /// The rule generalises past the database. Anything that changes which records come next, and
    /// which this runner cannot work out, stops the file here rather than letting the records after
    /// it be scored against a state nobody set up.
    Changes {
        /// The line, worded as the file wrote it.
        what: String,
        /// Whose gap it is.
        gap: Gap,
        /// How many records came after it, including the directive itself.
        records: usize,
    },
    /// The file does not parse, which is a problem with the harness or with the vendoring.
    Unreadable(ParseError),
    /// The file is not text.
    ///
    /// The corpus has a handful of these on purpose, to check what an engine does with a statement
    /// that is not valid UTF-8. Reading one lossily would run a different statement than the file
    /// says and then report on it, so it is skipped and named instead.
    NotText,
}

impl Skipped {
    /// Whose gap it is that the file did not run.
    ///
    /// A file that does not parse and a file that is not text are both this runner, whatever is in
    /// them. The corpus is vendored from a release DuckDB's own runner reads end to end.
    #[must_use]
    pub const fn gap(&self) -> Gap {
        match self {
            Self::Requires { gap, .. } | Self::Changes { gap, .. } => *gap,
            Self::Unreadable(_) | Self::NotText => Gap::Harness,
        }
    }

    /// How many records were behind it, where that is known.
    ///
    /// Zero for a file that could not be read, because a file that did not parse has no records to
    /// count and guessing from its line count would put a made up number in a report whose whole
    /// point is that every number in it was computed.
    #[must_use]
    pub const fn records(&self) -> usize {
        match self {
            Self::Requires { records, .. } | Self::Changes { records, .. } => *records,
            Self::Unreadable(_) | Self::NotText => 0,
        }
    }
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Requires { what, records, .. } => {
                let each = if *records == 1 { "record" } else { "records" };
                write!(f, "requires {what}, and {records} {each} went with it")
            }
            Self::Changes { what, records, .. } => {
                let each = if *records == 1 { "record" } else { "records" };
                write!(f, "stops at {what}, and {records} {each} came after it")
            }
            Self::Unreadable(e) => write!(f, "does not parse, {e}"),
            Self::NotText => f.write_str("is not valid UTF-8"),
        }
    }
}

/// Records that were not attempted, by reason.
///
/// The reasons are kept apart rather than totalled because they belong to different people, and
/// [`Skips::by_gap`] is where that is said in one number per person.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skips {
    /// A `skipif` or an `onlyif` that does not name this engine.
    pub conditional: usize,
    /// Inside a `mode skip` block, which is how a file turns off a section it knows is broken.
    pub mode: usize,
    /// A `statement maybe`, which is the file saying it does not know what should happen.
    ///
    /// Counted here rather than as a pass, which is a deliberate difference from upstream and the
    /// reason is what the two numbers are for. DuckDB's runner asks whether the suite failed, and a
    /// `maybe` cannot fail, so passing it costs nothing there. This runner publishes a percentage,
    /// and a record that cannot fail is not evidence about the engine in either direction. One file
    /// in the corpus, `catalog/dependencies/test_concurrent_alter.test`, is a hundred by ten loop
    /// around two `maybe` statements, and counting them put 1783 free passes into the number, which
    /// was nearly a tenth of everything that passed.
    pub maybe: usize,
    /// Behind a directive this runner does not implement, such as `load` or `restart`.
    ///
    /// Every one of these is a record nobody has run, so it is work for the harness rather than
    /// for the engine, and it is the number to watch when the pass rate looks better than it is.
    pub unsupported: usize,
    /// In a file that asked for something rudb does not have.
    pub engine: usize,
    /// In a file that asked for something the machine does not have.
    pub machine: usize,
    /// In a file this runner could not read at all.
    ///
    /// Always zero today, because a file that did not parse has no records to count. It is a field
    /// rather than a missing one so that the gap totals and the file list cannot drift apart if
    /// that ever changes.
    pub unreadable: usize,
}

impl Skips {
    /// Every one of them together, for a caller that only wants the total.
    #[must_use]
    pub fn total(self) -> usize {
        self.conditional
            + self.mode
            + self.maybe
            + self.unsupported
            + self.engine
            + self.machine
            + self.unreadable
    }

    /// How many records each gap accounts for.
    ///
    /// Returned in [`Gap::ALL`] order rather than sorted by size, because these four are read as a
    /// fixed set of columns over a series of runs and a row moving because it grew is a row that is
    /// hard to follow.
    #[must_use]
    pub fn by_gap(self) -> [(Gap, usize); Gap::ALL.len()] {
        [
            (Gap::Excused, self.conditional + self.mode + self.maybe),
            (Gap::Engine, self.engine),
            (Gap::Harness, self.unsupported + self.unreadable),
            (Gap::Machine, self.machine),
        ]
    }

    /// Count records against the row for whoever owns the gap.
    fn owe(&mut self, gap: Gap, records: usize) {
        match gap {
            Gap::Excused => self.conditional += records,
            Gap::Engine => self.engine += records,
            Gap::Harness => self.unsupported += records,
            Gap::Machine => self.machine += records,
        }
    }

    /// Count one file's records against the gap that stopped it.
    ///
    /// A file that could not be read goes on its own row rather than through [`Skips::owe`],
    /// because the two harness reasons are different work. A directive this runner does not carry
    /// out is a feature to write, and a file it cannot parse at all is a bug to fix.
    fn charge(&mut self, why: &Skipped) {
        match why {
            Skipped::Unreadable(_) | Skipped::NotText => self.unreadable += why.records(),
            Skipped::Requires { .. } | Skipped::Changes { .. } => {
                self.owe(why.gap(), why.records());
            }
        }
    }

    /// Add another set of skips to this one.
    ///
    /// The one place that does it, so that a row added to this struct is added to every total by
    /// the compiler rather than by somebody remembering.
    pub fn absorb(&mut self, other: Self) {
        self.conditional += other.conditional;
        self.mode += other.mode;
        self.maybe += other.maybe;
        self.unsupported += other.unsupported;
        self.engine += other.engine;
        self.machine += other.machine;
        self.unreadable += other.unreadable;
    }
}

impl fmt::Display for Skips {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} records not attempted, by whose gap it is", self.total())?;
        for (gap, count) in self.by_gap() {
            writeln!(f, "    {count:>7}  {:<10}{}", gap.name(), gap.blurb())?;
        }
        Ok(())
    }
}

/// What one file's run produced.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// How many files were read.
    pub files: usize,
    /// How many files were not run at all, and why.
    pub skipped_files: Vec<(String, Skipped)>,
    /// Records that ran and did what the file said.
    pub passed: usize,
    /// Records that ran and did something else.
    pub failed: usize,
    /// Records that were not attempted, split by why.
    ///
    /// Split rather than totalled because the three reasons mean different things to whoever reads
    /// the report. A record the file itself turned off is not our problem, a record behind a
    /// directive this runner does not implement is a job for the harness, and the two should never
    /// be added together into one number that sounds like either.
    pub skipped: Skips,
    /// Every failure, in the order they happened.
    pub failures: Vec<Failure>,
    /// Which statement kind each record that ran was, and whether it did what the file said.
    ///
    /// The statement coverage number in section 1.1 comes out of this and out of nothing else. It
    /// has to be counted here rather than worked out afterwards from the failures, because the
    /// failures are the only records whose SQL survives the run and a kind every record of which
    /// passed would be invisible in them.
    pub kinds: Kinds,
}

impl Summary {
    /// How many records were attempted, which is the denominator of the pass rate.
    #[must_use]
    pub fn attempted(&self) -> usize {
        self.passed + self.failed
    }

    /// The share of attempted records that passed, between zero and one.
    ///
    /// Skipped records are not in the denominator, and that has to be read together with the skip
    /// count rather than on its own. A runner that skips everything it would fail reports one
    /// hundred percent, so the two numbers are always published side by side and the report prints
    /// them on the same line for exactly that reason.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.attempted() == 0 {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a corpus with more records than a double can count does not exist"
        )]
        {
            self.passed as f64 / self.attempted() as f64
        }
    }

    /// The failures broken down by reason.
    ///
    /// Derived from the failures rather than counted alongside them, so the breakdown and the list
    /// can never disagree about how many there were.
    #[must_use]
    pub fn reasons(&self) -> Reasons {
        Reasons::of(&self.failures)
    }

    /// Fold another summary into this one.
    pub fn absorb(&mut self, other: Self) {
        self.files += other.files;
        self.skipped_files.extend(other.skipped_files);
        self.passed += other.passed;
        self.failed += other.failed;
        self.skipped.absorb(other.skipped);
        self.failures.extend(other.failures);
        self.kinds.absorb(&other.kinds);
    }
}

/// Run every `.test` file under a path, which may be one file or a directory.
///
/// `slow` decides whether the `.test_slow` files come too. They are a separate suite in DuckDB's
/// own CI for the obvious reason, and most of them are the concurrency tests, which a single
/// threaded engine can run but cannot learn anything from. Off by default, on for a nightly.
///
/// # Errors
///
/// When the path cannot be read. A file that does not parse is a skipped file and not an error,
/// because one unreadable file in a vendored corpus should not stop the other nine hundred.
pub fn run_path(engine: &mut dyn Engine, path: &Path, slow: bool) -> Result<Summary, HarnessError> {
    let mut files = Vec::new();
    collect(path, slow, &mut files)?;
    files.sort();

    let root = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    let top = corpus_top(root);
    let mut summary = Summary::default();
    for file in &files {
        let name = file.strip_prefix(root).unwrap_or(file).display().to_string();
        let bytes = std::fs::read(file)
            .map_err(|e| HarnessError::new(format!("cannot read {}: {e}", file.display())))?;
        match String::from_utf8(bytes) {
            Ok(text) => summary.absorb(run_under(engine, top.as_deref(), &name, &text)?),
            Err(_) => {
                summary.files += 1;
                summary.skipped_files.push((name, Skipped::NotText));
            }
        }
    }
    Ok(summary)
}

/// The top of the corpus, which is where an `include` path starts from.
///
/// A run is normally pointed at `test/sql` inside the corpus, and an `include` writes the whole path
/// from above `test`, which is what DuckDB's own parser does. Rather than count directories up, this
/// walks up until it finds one with `test/sql` under it, so pointing the run at a subdirectory two
/// levels down still finds the same top. `None` when there is no corpus shape around the path,
/// which is what a run over a directory of loose files looks like, and there an `include` is an
/// error with a line number rather than a silently missing setup.
#[must_use]
pub fn corpus_top(root: &Path) -> Option<PathBuf> {
    let mut at = Some(root);
    while let Some(dir) = at {
        if dir.join("test").join("sql").is_dir() {
            return Some(dir.to_owned());
        }
        at = dir.parent();
    }
    None
}

/// Read one file's text and run it, with no corpus around it.
///
/// # Errors
///
/// When the engine itself could not be run, which is a broken harness and not a failing test.
pub fn run_text(engine: &mut dyn Engine, name: &str, text: &str) -> Result<Summary, HarnessError> {
    run_under(engine, None, name, text)
}

/// Read one file's text and run it, resolving an `include` under the given corpus top.
///
/// # Errors
///
/// When the engine itself could not be run, which is a broken harness and not a failing test.
pub fn run_under(
    engine: &mut dyn Engine,
    top: Option<&Path>,
    name: &str,
    text: &str,
) -> Result<Summary, HarnessError> {
    let mut summary = Summary { files: 1, ..Summary::default() };
    let file = match crate::slt::parse_under(top, name, text) {
        Ok(file) => file,
        Err(e) => {
            summary.skipped_files.push((name.to_owned(), Skipped::Unreadable(e)));
            return Ok(summary);
        }
    };
    summary.absorb(run_file(engine, &file)?);
    summary.files = 1;
    Ok(summary)
}

/// What one record did, kept per record so that a second engine's run can be lined up against the
/// first one record by record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    /// It did what the file said it would.
    Passed,
    /// It did not, for this reason.
    Failed(Reason),
}

/// What every record of a file did, by its index in the file.
///
/// A map rather than a list because a file can end early, on a `halt` or on an error the file said
/// to stop on, and two engines do not have to end it at the same record. Only the records both runs
/// reached can be compared, and a map says which those are without any counting.
pub type Told = std::collections::BTreeMap<usize, Said>;

/// Run one parsed file against one engine.
///
/// The engine is reset first, so a file starts from an empty database and cannot be made to pass
/// by something an earlier file left behind.
///
/// # Errors
///
/// When the engine itself could not be run.
pub fn run_file(engine: &mut dyn Engine, file: &TestFile) -> Result<Summary, HarnessError> {
    run_file_telling(engine, file, &mut Told::new())
}

/// Run one parsed file against one engine and say what each record did.
///
/// This is [`run_file`] with the per record answers kept, which is what a second oracle needs and
/// what nothing else does. `crate::oracles` runs this twice over the same file with two engines and
/// pairs the answers up.
///
/// # Errors
///
/// When the engine itself could not be run.
pub fn run_file_telling(
    engine: &mut dyn Engine,
    file: &TestFile,
    told: &mut Told,
) -> Result<Summary, HarnessError> {
    let mut summary = Summary { files: 1, ..Summary::default() };
    engine.reset()?;

    // A `require` the harness cannot satisfy disables the whole file, which is how the format
    // works: the requirement is about the build and not about the record it happens to sit above.
    // A `require` it can satisfy is not a skip at all, and most of them can be satisfied.
    for record in &file.records {
        if let Directive::Require { env, params } = &record.directive {
            if let Some(gap) = have(*env, params) {
                // The records go on the skip count rather than nowhere. A file skipped whole used
                // to contribute to neither the skip count nor the denominator, so the corpus
                // silently got smaller and no line of the report said by how much.
                let why = Skipped::Requires {
                    what: requirement(*env, params),
                    gap,
                    records: runnable(&file.records),
                };
                summary.skipped.charge(&why);
                summary.skipped_files.push((file.name.clone(), why));
                return Ok(summary);
            }
        }
    }

    // `mode skip` turns everything off until `mode unskip`, which is how a file marks a section
    // that is known not to work without deleting it.
    let mut skipping = false;
    let mut labels: HashMap<String, Vec<String>> = HashMap::new();
    // Every path the file has opened so far, which is what decides whether the next `load` or
    // `restart` is asking for an empty database or for one with history in it.
    let mut loaded: Vec<String> = Vec::new();
    // What the file has said about the run, which lasts to the end of it.
    let mut settings = Settings::default();

    for (at, record) in file.records.iter().enumerate() {
        if let Directive::Mode(mode) = &record.directive {
            match mode.as_str() {
                "skip" => skipping = true,
                "unskip" => skipping = false,
                _ => {}
            }
            continue;
        }
        if matches!(record.directive, Directive::Halt) {
            break;
        }
        if matches!(record.directive, Directive::HashThreshold(_) | Directive::Require { .. }) {
            continue;
        }
        // Forgetting a shared result, so the next turn of a loop compares its own pair of queries
        // rather than the first turn's. Unconditional, because the corpus never puts a condition on
        // one and because a label that is half forgotten is worse than either answer.
        if let Directive::ResetLabel(name) = &record.directive {
            labels.remove(name);
            continue;
        }
        if skipping {
            summary.skipped.mode += 1;
            continue;
        }
        if !record.condition.applies_to(NAMES) {
            summary.skipped.conditional += 1;
            continue;
        }
        // A `continue` ends the turn of the loop it is in. Every one in the corpus is conditional on
        // the loop variable and the parser settles those when it expands the loop, so one that
        // reaches here is conditional on the engine instead, and by now the loop is flat and the
        // end of the turn is not something this runner can find. Stopping is the only answer that
        // does not score the rest of the file against a state the file said to skip.
        if matches!(record.directive, Directive::Continue) {
            let why = Skipped::Changes {
                what: "continue".to_owned(),
                gap: Gap::Harness,
                records: runnable(&file.records[at..]),
            };
            summary.skipped.charge(&why);
            summary.skipped_files.push((file.name.clone(), why));
            return Ok(summary);
        }
        // The file said it does not know what should happen, so there is nothing here to be right
        // or wrong about. See [`Skips::maybe`] for why this is not a pass.
        if let Directive::Statement { expected: StatementResult::Maybe(_), .. } = &record.directive
        {
            summary.skipped.maybe += 1;
            continue;
        }
        // The settings a file makes about itself. A `seed` is the one that reaches the engine,
        // because upstream turns it into a statement and a file that sets one is a file whose
        // later answers depend on it.
        if let Directive::Set(setting) = &record.directive {
            settings.take(setting);
            if let Setting::Seed(seed) = setting {
                if let Outcome::Error(e) = engine.run(&format!("SELECT setseed({seed})"))? {
                    let why = Skipped::Changes {
                        what: format!("set seed {seed}, and the engine said {e}"),
                        gap: Gap::Engine,
                        records: runnable(&file.records[at..]),
                    };
                    summary.skipped.charge(&why);
                    summary.skipped_files.push((file.name.clone(), why));
                    return Ok(summary);
                }
            }
            continue;
        }
        if let Directive::TestEnv { name, default } = &record.directive {
            let value = std::env::var(name).unwrap_or_else(|_| default.clone());
            settings.take(&Setting::Variable { name: name.clone(), value });
            continue;
        }
        if let Directive::Sleep(how_long) = &record.directive {
            std::thread::sleep(*how_long);
            continue;
        }
        if let Directive::Unsupported(line) = &record.directive {
            match effect(line, &mut loaded) {
                Effect::Fresh => engine.reset()?,
                Effect::Skip(gap) => summary.skipped.owe(gap, 1),
                Effect::Ends(gap) => {
                    let why = Skipped::Changes {
                        what: line.clone(),
                        gap,
                        records: runnable(&file.records[at..]),
                    };
                    summary.skipped.charge(&why);
                    summary.skipped_files.push((file.name.clone(), why));
                    return Ok(summary);
                }
            }
            continue;
        }
        match check(engine, file, record, &mut labels, &settings)? {
            Verdict::Ran(Ok(())) => {
                summary.passed += 1;
                charge(&mut summary.kinds, record, true);
                told.insert(at, Said::Passed);
            }
            Verdict::Ran(Err(failure)) => {
                summary.failed += 1;
                charge(&mut summary.kinds, record, false);
                told.insert(at, Said::Failed(failure.reason));
                summary.failures.push(failure);
            }
            // The file named this error and said that if it happens there is nothing here worth
            // running, so the rest of it is excused rather than failed. Upstream stops reading the
            // file at the same point.
            Verdict::Ignored(which) => {
                let why = Skipped::Changes {
                    what: format!("an error the file said to stop on, {which}"),
                    gap: Gap::Excused,
                    records: runnable(&file.records[at..]),
                };
                summary.skipped.charge(&why);
                summary.skipped_files.push((file.name.clone(), why));
                return Ok(summary);
            }
        }
    }

    Ok(summary)
}

/// Whether this harness has what a `require` line is asking for, and when it does not, whose gap
/// that is.
///
/// This follows `CheckRequire` in DuckDB's own `test/sqlite/sqllogic_test_runner.cpp`, and it is
/// worth following closely rather than approximating, because most of what the corpus requires is
/// not a feature at all. `require skip_reload` tells DuckDB's runner not to reopen the database in
/// the middle of the file. `require noforcestorage` tells it not to run the file in the mode that
/// writes everything to disk first. `require no_alternative_verify` and `require
/// no_vector_verification` turn off debug modes, and `require no_extension_autoloading` turns off
/// a convenience. Upstream's own runner answers yes to every one of those on an ordinary build and
/// runs the file. Reading them as a missing feature and skipping the file, which is what this
/// runner did before, put several hundred files of the corpus behind directives that never meant
/// anything here, and every record in them was invisible to the pass rate.
///
/// Answering no is the expensive direction and answering yes is the honest one. A file that runs
/// and fails produces a failure with a reason on it, which is a job for somebody. A file that is
/// skipped produces nothing and makes the pass rate look better, which is the failure mode the
/// whole report is built to avoid.
fn have(env: bool, params: &[String]) -> Option<Gap> {
    // `require-env` asks whether an environment variable is set, and when it has a second argument
    // whether it holds that value. Every one of these in the corpus points at a machine somebody
    // else has, an extension repository or a secrets store, so in practice they are all missing,
    // but the question is answerable so it gets answered rather than assumed.
    if env {
        let met = params
            .first()
            .and_then(|name| std::env::var(name).ok())
            .is_some_and(|value| params.get(1).is_none_or(|wanted| *wanted == value));
        return (!met).then_some(Gap::Machine);
    }

    let Some(first) = params.first() else { return Some(Gap::Harness) };
    let what = first.to_ascii_lowercase();
    let size = || params.get(1).and_then(|p| p.parse::<usize>().ok());
    let unless = |met: bool, gap: Gap| (!met).then_some(gap);
    match what.as_str() {
        // Guards on how DuckDB was built or on the mode its runner is in. None of them describe
        // anything this harness does, so all of them are satisfied, which is the same answer
        // upstream gives on an ordinary build.
        "notmusl"
        | "nothreadsan"
        | "strinline"
        | "noforcestorage"
        | "no_force_storage"
        | "skip_reload"
        | "no_alternative_verify"
        | "no_latest_storage"
        | "no_vector_verification"
        | "no_extension_autoloading" => None,

        // Guards on the platform, read off the target rather than off a build flag. A file that
        // needs an operating system this is not running on is the machine and not the engine, and
        // it would come back if the run moved.
        "notmingw" | "notwindows" => unless(!cfg!(windows), Gap::Machine),
        "mingw" | "windows" => unless(cfg!(windows), Gap::Machine),
        "64bit" => unless(cfg!(target_pointer_width = "64"), Gap::Machine),

        // The size of the vector the engine works a chunk at a time in. `vector_size` is a floor
        // and `exact_vector_size` is an equality, which is upstream's reading and not ours.
        "vector_size" => unless(size().is_some_and(|wanted| VECTOR_SIZE >= wanted), Gap::Engine),
        "exact_vector_size" => {
            unless(size().is_some_and(|wanted| VECTOR_SIZE == wanted), Gap::Engine)
        }

        // rudb keeps its tables in memory and has no block size for a file to match, so a file
        // that pins one is asking about something that does not exist here.
        "block_size" => Some(Gap::Engine),

        // How much memory or disk the machine has. Answerable on Linux by reading `/proc`, and not
        // answerable portably without a second dependency this crate will not take. The files
        // behind these ask for eight to forty gigabytes, so they would be stopped on the memory
        // budget anyway, and a skip that names the requirement beats a stop that names a number.
        "ram" | "disk_space" => Some(Gap::Machine),

        // Settings DuckDB's own runner is only sometimes started with, and that are off by default
        // there too.
        "allow_unsigned_extensions" | "vacuum_rebuild_indexes" => Some(Gap::Engine),

        // An eighty bit float, which rudb does not have and does not intend to.
        "longdouble" => Some(Gap::Engine),

        other => unless(BUILT_IN.contains(&other), Gap::Engine),
    }
}

/// What a directive this runner does not carry out does to the rest of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// The database is an empty one from here on, which is a reset and nothing else.
    Fresh,
    /// The record does not run and the file carries on, because the directive speaks only to
    /// itself.
    Skip(Gap),
    /// Nothing after this point means anything, because the database the rest of the file is
    /// written against is one this run cannot produce.
    Ends(Gap),
}

/// The directories the corpus makes for itself, fresh, once per run.
///
/// A file that opens a database under one of these and writes to it is starting from nothing, and
/// starting from nothing is what an in memory engine does anyway. A file that opens anything else
/// is opening something that already has content in it, which is a different question and not one
/// rudb can answer today.
const SCRATCH: &[&str] = &["{TEST_DIR}", "{TEMP_DIR}", "__TEST_DIR__"];

/// What one of the directives this runner does not carry out does here, and where it cannot be
/// done, whose gap that is.
///
/// The reason this exists is a false pass, and it was a large one. Six hundred files in the corpus
/// write to a database, say `restart`, and then check that what they wrote is still there. The
/// restart is the whole test. Skipping the `restart` record on its own and running the rest of the
/// file against a database that was never closed makes every one of those checks pass, and it
/// makes them pass for precisely the reason the file was written to rule out. A harness that does
/// that reports a persistence guarantee rudb does not have.
///
/// So the rule is that a directive which replaces the database ends the file, and the records after
/// it go on the skip count against whoever owns the gap. rudb keeps its tables in memory, so
/// reopening a database from a file is the engine's gap and not this runner's, and it is the one
/// that would close if rudb grew storage.
///
/// The exception is worth having rather than folding in. `load {TEST_DIR}/whatever.db` at the top of
/// a file, on a path nothing has written to yet and not opened read only, is asking for an empty
/// database, and an empty database is what a reset gives. Those files then go on to create their
/// own tables and query them, which is an ordinary test that rudb really does pass, and ending them
/// at the `load` would throw away thousands of honest records to no purpose.
fn effect(line: &str, loaded: &mut Vec<String>) -> Effect {
    let mut words = line.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    match first {
        "load" => {
            // A `load` with no path is DuckDB's spelling for an in memory database, and it leaves
            // the path empty, so a later `restart` is a wipe rather than a reopen.
            let Some(path) = rest.first().copied() else { return Effect::Fresh };
            let empty = SCRATCH.iter().any(|dir| path.starts_with(dir))
                && !rest.contains(&"readonly")
                && !loaded.iter().any(|seen| seen == path);
            loaded.push(path.to_owned());
            if empty { Effect::Fresh } else { Effect::Ends(Gap::Engine) }
        }

        // Upstream reopens the database from the path it was loaded from. With no path that is an
        // empty database again, which is a reset. With one it is the persistence question.
        "restart" => {
            if loaded.is_empty() {
                Effect::Fresh
            } else {
                Effect::Ends(Gap::Engine)
            }
        }

        // A second connection to a database that is already open. This runner talks to both engines
        // as processes through one shell each, so there is no second connection to be had, and
        // everything after it is about what one connection sees of another.
        "reconnect" => Effect::Ends(Gap::Harness),

        // Unpacking a gzip to make a database file to load. The decompressor is the problem, not the
        // directive: this crate has one dependency on purpose and a second one for this is not a
        // trade worth making while `load` cannot use the result anyway.
        "unzip" => Effect::Ends(Gap::Harness),

        // `sleep` and `set`, which speak to themselves and leave the database alone.
        _ => Effect::Skip(Gap::Harness),
    }
}

/// Charge a record that ran to whichever of the thirty six statement kinds it is.
///
/// Only the two directives that carry SQL. Everything else in a file is the file talking to the
/// runner, and the two that reach here are the two that reach the engine.
///
/// A record with several statements in it is charged to the first one. That is a real
/// simplification and it is the right one: the corpus writes a `statement ok` block of two
/// statements to set something up, and the second one being a different kind would put a record in
/// two buckets and make the counts add up to more than the records that ran.
fn charge(kinds: &mut Kinds, record: &Record, passed: bool) {
    match &record.directive {
        Directive::Statement { sql, .. } | Directive::Query { sql, .. } => {
            kinds.charge(sql, passed);
        }
        _ => {}
    }
}

/// How many records a file would have put in front of the engine.
///
/// Everything down to a `halt` that is not a directive. A `skipif` and a `load` are counted, because
/// they are records the run would have reached and had an answer about, and leaving them out would
/// make a file skipped whole look smaller than the same file skipped record by record.
fn runnable(records: &[Record]) -> usize {
    records
        .iter()
        .take_while(|record| !matches!(record.directive, Directive::Halt))
        .filter(|record| {
            matches!(
                record.directive,
                Directive::Statement { .. } | Directive::Query { .. } | Directive::Unsupported(_)
            )
        })
        .count()
}

/// How a requirement is worded in the report, which is how the file wrote it.
fn requirement(env: bool, params: &[String]) -> String {
    let what = params.join(" ");
    if env { format!("the environment to have {what}") } else { what }
}

/// What a file has said about the run rather than about any one record.
///
/// Four `set` lines and a `test-env` write to this, and it lasts to the end of the file. It is not
/// the SQL `SET`, which the engine reads and this runner never sees.
#[derive(Debug, Clone)]
struct Settings {
    /// Errors that mean the rest of the file is not worth running.
    ignore: Vec<String>,
    /// Errors that no expected error may be satisfied by.
    always_fail: Vec<String>,
    /// Names the SQL below may write as `{name}` or `${name}`.
    variables: Vec<(String, String)>,
}

impl Default for Settings {
    /// What every file starts with, which is not nothing.
    ///
    /// Upstream begins with `INTERNAL` in the always fail list and no line in any file puts it
    /// there. An internal error is the engine saying it has broken an invariant of its own, and a
    /// file that asked for an error and got one of those did not get what it asked for.
    fn default() -> Self {
        Self { ignore: Vec::new(), always_fail: vec!["INTERNAL".to_owned()], variables: Vec::new() }
    }
}

impl Settings {
    /// Take a `set` line at its word.
    fn take(&mut self, setting: &Setting) {
        match setting {
            // Both of these replace rather than add, including replacing the `INTERNAL` a file
            // never asked for, which is what upstream does with the same line.
            Setting::Ignore(messages) => self.ignore.clone_from(messages),
            Setting::AlwaysFail(messages) => self.always_fail.clone_from(messages),
            Setting::Variable { name, value } => {
                self.variables.retain(|(seen, _)| seen != name);
                self.variables.push((name.clone(), value.clone()));
            }
            // Handled where the engine is, since it is a statement and not a note.
            Setting::Seed(_) => {}
        }
    }

    /// Put the variables the file has set into a piece of text.
    ///
    /// Both spellings, because the corpus writes both and upstream replaces both. A name nothing
    /// has set is left as it stands, which is how `{TEST_DIR}` and the rest survive to be read by
    /// whoever does know what they mean.
    fn fill(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (name, value) in &self.variables {
            out = out.replace(&format!("${{{name}}}"), value);
            out = out.replace(&format!("{{{name}}}"), value);
        }
        out
    }

    /// The `ignore_error_messages` entry this error matches, if any.
    fn ignored(&self, error: &EngineError) -> Option<String> {
        names(&self.ignore, error)
    }

    /// The `always_fail_error_messages` entry this error matches, if any.
    fn internal(&self, error: &EngineError) -> Option<String> {
        names(&self.always_fail, error)
    }
}

/// The first entry in a list that this error's text contains.
fn names(list: &[String], error: &EngineError) -> Option<String> {
    let full = format!("{}: {}", error.kind, error.message);
    list.iter().find(|entry| full.contains(entry.as_str())).cloned()
}

/// What a record did, which is one of three things and not two.
enum Verdict {
    /// It ran, and the file was either right or wrong about what it would do.
    Ran(Result<(), Failure>),
    /// It hit an error the file named in `set ignore_error_messages`, which is the file saying
    /// that if this happens there is nothing here worth running.
    Ignored(String),
}

/// Run one record and decide whether it did what the file said.
///
/// The outer result is the harness failing and the inner one is the record failing, which is the
/// same split the [`Engine`] trait makes and for the same reason.
fn check(
    engine: &mut dyn Engine,
    file: &TestFile,
    record: &Record,
    labels: &mut HashMap<String, Vec<String>>,
    settings: &Settings,
) -> Result<Verdict, HarnessError> {
    let fail = |sql: &str, reason: Reason, detail: String| {
        Err(Failure {
            file: file.name.clone(),
            line: record.line,
            sql: sql.to_owned(),
            reason,
            detail,
        })
    };

    match &record.directive {
        Directive::Statement { expected, sql } => {
            let sql = &settings.fill(sql);
            let outcome = engine.run(sql)?;
            if let Outcome::Error(e) = &outcome {
                if let Some(which) = settings.ignored(e) {
                    return Ok(Verdict::Ignored(which));
                }
                // An error the file said can never be the one it was asking for. Without this, a
                // `statement error` above a statement that makes the engine break an invariant of
                // its own is a pass, and that is the one place a passing record is worse than a
                // failing one.
                if let Some(which) = settings.internal(e) {
                    if !matches!(expected, StatementResult::Ok) {
                        return Ok(Verdict::Ran(fail(
                            sql,
                            Reason::ErrorClass,
                            format!("expected an error, and it said\n{e}\nwhich contains {which}"),
                        )));
                    }
                }
            }
            Ok(Verdict::Ran(match (expected, &outcome) {
                // A `maybe` is excused before it reaches here, by the loop in `run_file`. The arm is
                // upstream's reading of it, for any other caller and for the day one of the four
                // report levels wants to count them separately.
                (StatementResult::Ok, Outcome::Rows(_)) | (StatementResult::Maybe(None), _) => {
                    Ok(())
                }
                (StatementResult::Maybe(Some(_)), Outcome::Rows(_)) => Ok(()),
                (StatementResult::Maybe(Some(wanted)), Outcome::Error(e)) => {
                    if contains(e, wanted) {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            wrong_error(e, wanted),
                            format!("expected it to work or say\n{wanted}\nand it said\n{e}"),
                        )
                    }
                }
                (StatementResult::Ok, Outcome::Error(e)) => {
                    fail(sql, Reason::of(e), format!("expected it to work, and it said\n{e}"))
                }
                (StatementResult::Error(None), Outcome::Error(_)) => Ok(()),
                (StatementResult::Error(Some(wanted)), Outcome::Error(e)) => {
                    if contains(e, wanted) {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            wrong_error(e, wanted),
                            format!("expected an error containing\n{wanted}\nand it said\n{e}"),
                        )
                    }
                }
                (StatementResult::Error(_), Outcome::Rows(_)) => {
                    fail(sql, Reason::MissedError, "expected it to fail, and it worked".to_owned())
                }
            }))
        }
        Directive::Query { types, sort, label, sql, expected } => {
            let sql = &settings.fill(sql);
            let outcome = engine.run(sql)?;
            if let Outcome::Error(e) = &outcome {
                if let Some(which) = settings.ignored(e) {
                    return Ok(Verdict::Ignored(which));
                }
                if let Some(which) = settings.internal(e) {
                    if matches!(expected, QueryResult::Error(_)) {
                        return Ok(Verdict::Ran(fail(
                            sql,
                            Reason::ErrorClass,
                            format!("expected an error, and it said\n{e}\nwhich contains {which}"),
                        )));
                    }
                }
            }
            let table = match (&outcome, expected) {
                (Outcome::Error(_), QueryResult::Error(None)) => {
                    return Ok(Verdict::Ran(Ok(())));
                }
                (Outcome::Error(e), QueryResult::Error(Some(wanted))) => {
                    return Ok(Verdict::Ran(if contains(e, wanted) {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            wrong_error(e, wanted),
                            format!("expected an error containing\n{wanted}\nand it said\n{e}"),
                        )
                    }));
                }
                (Outcome::Error(e), _) => {
                    return Ok(Verdict::Ran(fail(
                        sql,
                        Reason::of(e),
                        format!("expected rows, and it said\n{e}"),
                    )));
                }
                (Outcome::Rows(_), QueryResult::Error(_)) => {
                    return Ok(Verdict::Ran(fail(
                        sql,
                        Reason::MissedError,
                        "expected it to fail, and it returned rows".to_owned(),
                    )));
                }
                (Outcome::Rows(table), _) => table,
            };

            let width = types.chars().count();
            if table.width() != width {
                return Ok(Verdict::Ran(fail(
                    sql,
                    Reason::WrongAnswer,
                    format!("expected {width} columns and got {}", table.width()),
                )));
            }
            let kinds: Vec<String> = table.columns.iter().map(|column| column.ty.clone()).collect();
            let values = flatten(table, *sort);

            if !label.is_empty() {
                if let Some(previous) = labels.get(label) {
                    if previous != &values {
                        return Ok(Verdict::Ran(fail(
                            sql,
                            Reason::WrongAnswer,
                            format!(
                                "this is labelled {label} and does not match what the earlier query with that label returned"
                            ),
                        )));
                    }
                } else {
                    labels.insert(label.clone(), values.clone());
                }
            }

            Ok(Verdict::Ran(match expected {
                QueryResult::Lines(raw) => match wanted(raw, width, table.height()) {
                    Ok(wanted) => {
                        if agrees(&wanted, &values, &kinds, width) {
                            Ok(())
                        } else {
                            fail(
                                sql,
                                Reason::WrongAnswer,
                                difference(&wanted, &values, &kinds, width),
                            )
                        }
                    }
                    Err(why) => fail(sql, Reason::WrongAnswer, why),
                },
                QueryResult::Hash { count, digest } => {
                    // The count first and the digest second, because the count is the only part of
                    // this that says anything a person can act on. Section 9.3.1 of the harness spec
                    // is why the digest stands as the outcome here rather than the record being put
                    // to a live binary: there are 19 of these in a default run and requiring a
                    // DuckDB for them would cost the property that this runs on every commit.
                    if values.len() != *count {
                        fail(
                            sql,
                            Reason::WrongAnswer,
                            format!("expected {count} values and got {}", values.len()),
                        )
                    } else if &hash_values(&values) == digest {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            Reason::WrongAnswer,
                            format!(
                                "expected {count} values hashing to {digest} and got {}",
                                hash_values(&values)
                            ),
                        )
                    }
                }
                QueryResult::Error(_) => unreachable!("handled above"),
            }))
        }
        Directive::Halt
        | Directive::HashThreshold(_)
        | Directive::Require { .. }
        | Directive::Mode(_)
        | Directive::ResetLabel(_)
        | Directive::Continue
        | Directive::Set(_)
        | Directive::Sleep(_)
        | Directive::TestEnv { .. }
        | Directive::Unsupported(_) => Ok(Verdict::Ran(Ok(()))),
    }
}

/// Whether an engine's error is the one the file asked for.
///
/// A substring match on the message and not an equality, because that is what the format means and
/// because the corpus writes short fragments like `Conversion Error` where the real message is a
/// paragraph. The kind is included in the text being searched, so a file can pin either.
/// Whether an error that was not the one the file asked for is the wrong kind or the wrong words.
///
/// The distinction is worth drawing because the two are different sizes of problem. A kind that
/// does not match means the engine reached a different conclusion about the statement, which is a
/// behaviour difference. The right kind with different words is a message somebody has to copy
/// from DuckDB, which is an afternoon and no design.
///
/// A file that writes only a fragment of the message and never names a kind cannot be a kind
/// mismatch, so it falls to the text, which is what the fragment was about.
fn wrong_error(got: &EngineError, wanted: &str) -> Reason {
    match wanted_kind(wanted) {
        Some(kind) if kind != got.kind => Reason::ErrorClass,
        _ => Reason::ErrorText,
    }
}

/// The error kind a file's expected text names, when it names one.
///
/// Both spellings the corpus uses. `Conversion Error: cannot cast` names one with a colon after
/// it, and a bare `Conversion Error` on its own line names one without, and both are common enough
/// that reading only the first would put most of these in the wrong row.
fn wanted_kind(wanted: &str) -> Option<&str> {
    let first = wanted.lines().map(str::trim).find(|line| !line.is_empty())?;
    let head = first.split_once(": ").map_or(first, |(kind, _)| kind);
    head.ends_with("Error").then_some(head)
}

fn contains(error: &EngineError, wanted: &str) -> bool {
    let full = format!("{}: {}", error.kind, error.message);
    let wanted = wanted.trim();
    // The corpus writes an expected error over several lines when the real one has several lines,
    // and the leading whitespace on the continuations is not part of the claim.
    wanted.lines().map(str::trim).filter(|line| !line.is_empty()).all(|line| full.contains(line))
}

/// Turn a result set into the flat list of values the format compares.
///
/// Row by row, left to right, each value written the way its own type says to, then sorted if the
/// record asked for it.
#[must_use]
pub fn flatten(table: &Table, sort: Sort) -> Vec<String> {
    let mut rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(at, cell)| {
                    render(cell, table.columns.get(at).map_or("", |column| column.ty.as_str()))
                })
                .collect()
        })
        .collect();

    match sort {
        Sort::NoSort => {}
        Sort::RowSort => rows.sort(),
        Sort::ValueSort => {
            let mut values: Vec<String> = rows.into_iter().flatten().collect();
            values.sort();
            return values;
        }
    }
    rows.into_iter().flatten().collect()
}

/// Render one value the way the reference runner renders it.
///
/// `SQLLogicTestConvertValue` in `test/sqlite/result_helper.cpp` is four lines long and this is
/// those four lines. A null is the word NULL, a boolean is 1 or 0, and everything else is the value
/// cast to `VARCHAR`, with the empty string written out so a line of the file can hold it.
///
/// The column letter is deliberately not consulted, and that is the part worth saying out loud,
/// because reading it as a rendering instruction is what this used to do. Upstream uses the letters
/// for the column count and for nothing else. A `query I` whose column is a `DOUBLE` is not
/// truncated to an integer there, and a `query R` is not rounded to three decimals, so doing either
/// here produced values upstream never produces and then compared them against a file upstream
/// wrote. What makes that safe to drop is the comparison below, which is where upstream puts the
/// tolerance: `0.500` and `0.5` agree because both are cast to the column's type, not because one
/// of them was rounded on the way in.
#[must_use]
pub fn render(cell: &Cell, ty: &str) -> String {
    let text = match cell {
        Cell::Null => return "NULL".to_owned(),
        Cell::Text(text) => text.as_str(),
    };
    if boolean(ty) {
        return match text {
            "true" | "TRUE" | "True" | "t" | "1" => "1".to_owned(),
            "false" | "FALSE" | "False" | "f" | "0" => "0".to_owned(),
            other => other.to_owned(),
        };
    }
    // An empty string and a null are different values and the format has to be able to tell them
    // apart on a line of their own, so the empty one is written out.
    if text.is_empty() { "(empty)".to_owned() } else { text.to_owned() }
}

/// Whether every value agrees with what the file said, column type by column type.
///
/// Positional, because both sides are already in whatever order the record asked for. The column a
/// value belongs to is its position modulo the width, which is how `CompareValues` finds it too,
/// and after a value sort that is not the column the value came from. Upstream has the same hole
/// and closing it here would mean two runners disagreeing about a record neither can read.
fn agrees(wanted: &[String], got: &[String], types: &[String], width: usize) -> bool {
    if wanted.len() != got.len() {
        return false;
    }
    wanted
        .iter()
        .zip(got)
        .enumerate()
        .all(|(at, (wanted, got))| matched(wanted, got, kind_at(types, width, at)))
}

/// The type name of the column a flat value belongs to.
///
/// Empty when the width is zero, which cannot happen for a record that got this far but is cheaper
/// to answer than to argue about.
fn kind_at(types: &[String], width: usize, at: usize) -> &str {
    if width == 0 {
        return "";
    }
    types.get(at % width).map_or("", String::as_str)
}

/// Whether one value agrees with the one the file wrote, given the type of its column.
///
/// Three rules, and they are `CompareValues` in `result_helper.cpp` rather than a policy of ours.
/// The text matching is the whole of it for most types. A boolean column compares true against 1
/// and false against 0 in either spelling and either case, because the corpus writes all four and
/// the engine prints one. A numeric column compares the two as numbers, which is what lets a file
/// that wrote `2.000000` agree with an engine that printed `2.0` without anybody rounding anything.
///
/// The numeric rule splits in two where upstream's does not have to. Upstream casts both sides to
/// the column's own type and compares the results, so a `HUGEINT` is compared as a 128 bit integer
/// and never goes near a float. Doing the whole thing in `f64` here would make two hugeints that
/// differ in their last digit read as equal, which is a wrong answer the harness would then call a
/// pass, so the exact types compare digit by digit and only the float types compare as floats.
///
/// A value that does not parse where the type says it should is a disagreement rather than a fall
/// back to the text, which is upstream's rule and the stricter of the two readings.
fn matched(wanted: &str, got: &str, ty: &str) -> bool {
    if wanted == got {
        return true;
    }
    if boolean(ty) {
        return truth(wanted).is_some() && truth(wanted) == truth(got);
    }
    if wanted == "NULL" || got == "NULL" {
        return false;
    }
    match kind(ty) {
        Some(Numeric::Float) => match (wanted.parse::<f64>(), got.parse::<f64>()) {
            (Ok(wanted), Ok(got)) => wanted == got || (wanted.is_nan() && got.is_nan()),
            _ => false,
        },
        Some(Numeric::Exact) => match (exact(wanted), exact(got)) {
            (Some(wanted), Some(got)) => wanted == got,
            _ => false,
        },
        None => false,
    }
}

/// A number written with no trailing zeros and no leading ones, so two spellings of it are one
/// string.
///
/// Plain digits only, with an optional sign and an optional fraction. Exponents and the infinities
/// are refused rather than guessed at, because the types that come through here are the ones that
/// never print either.
fn exact(value: &str) -> Option<String> {
    let (sign, digits) = match value.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", value.strip_prefix('+').unwrap_or(value)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let fraction = fraction.trim_end_matches('0');
    // A zero has one spelling and it is not the empty string, and it does not carry a sign either,
    // so that -0.0 and 0 read as the same number the way casting them both would.
    if whole.is_empty() && fraction.is_empty() {
        return Some("0".to_owned());
    }
    Some(format!("{sign}{whole}.{fraction}"))
}

/// The 1 or the 0 a boolean value is written as, in any of the spellings the corpus uses.
fn truth(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Whether a type name is the boolean one.
fn boolean(ty: &str) -> bool {
    matches!(ty.to_ascii_uppercase().as_str(), "BOOLEAN" | "BOOL" | "LOGICAL")
}

/// The two ways a number can be compared, and nothing for a type that is not a number at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Numeric {
    /// Integers and decimals, compared digit by digit with no width limit.
    Exact,
    /// Floats, compared as `f64` because that is what they are.
    Float,
}

/// How values of a type compare, read off the type name.
///
/// The name rather than a parsed type, because that is all an engine hands back here, and the
/// widths are spelled out rather than matched by prefix so that a `VARCHAR` never falls in.
fn kind(ty: &str) -> Option<Numeric> {
    let ty = ty.to_ascii_uppercase();
    let head = ty.split_once('(').map_or(ty.as_str(), |(head, _)| head).trim();
    match head {
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT"
        | "UINTEGER" | "UBIGINT" | "UHUGEINT" | "DECIMAL" | "NUMERIC" => Some(Numeric::Exact),
        "FLOAT" | "REAL" | "DOUBLE" => Some(Numeric::Float),
        _ => None,
    }
}

/// Split a result block into the values it means, which takes the result that came back to decide.
///
/// A line under a `----` is a whole row in some files and one value in others, and nothing in the
/// file says which. The old readers guessed from the text, which works until a file writes a row of
/// three values where one of them happens to contain a tab, and then the guess is wrong and there
/// is nothing the reader can do about it. DuckDB does not guess. `result_helper.cpp` counts the
/// rows the engine actually returned and calls the block row-wise when there is a line per row and
/// more than one column, falls back to the every line has a tab guess only when that does not fit,
/// and fails the record when what is left does not divide by the column count.
///
/// Deciding it here rather than in the parser is also what stops a badly written block taking a
/// whole file with it. Sixteen files in the corpus have a result block that does not divide, and as
/// a parse error that was sixteen files with no outcome at all rather than sixteen records that
/// fail.
///
/// The error is the text for the failure report, because there is no other place for it to go.
pub fn wanted(raw: &[String], columns: usize, rows: usize) -> Result<Vec<String>, String> {
    if columns == 0 {
        return Ok(raw.to_vec());
    }

    let mut row_wise = columns > 1 && raw.len() == rows;
    if !row_wise {
        row_wise = !raw.is_empty() && raw.iter().all(|line| line.contains('\t'));
    }

    if row_wise {
        let mut out = Vec::with_capacity(raw.len() * columns);
        for (at, line) in raw.iter().enumerate() {
            let values: Vec<&str> = line.split('\t').collect();
            if values.len() != columns {
                return Err(format!(
                    "row {} of the expected result has {} values under a query of {columns} columns",
                    at + 1,
                    values.len()
                ));
            }
            out.extend(values.into_iter().map(str::to_owned));
        }
        return Ok(out);
    }

    if raw.len() % columns != 0 {
        return Err(format!(
            "{} values under a query of {columns} columns, which is not a whole number of rows",
            raw.len()
        ));
    }
    Ok(raw.to_vec())
}

/// A readable account of how two lists of values differ.
///
/// Printed as rows rather than as a flat list, because a result that is off by one column reads as
/// every value being wrong when it is printed flat, and reads as one missing column when it is
/// printed in rows.
fn difference(wanted: &[String], got: &[String], types: &[String], width: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "expected {} rows and got {}\n",
        rows_of(wanted.len(), width),
        rows_of(got.len(), width)
    ));
    out.push_str("expected                        got\n");
    let wanted_rows = chunk(wanted, width);
    let got_rows = chunk(got, width);
    for at in 0..wanted_rows.len().max(got_rows.len()).min(20) {
        let left = wanted_rows.get(at).map_or_else(String::new, |r| r.join("  "));
        let right = got_rows.get(at).map_or_else(String::new, |r| r.join("  "));
        // Marked by the same rule that decided the record, so a row the comparison accepted is
        // never starred here. Two spellings of the same number in a report that calls them a
        // difference is how somebody ends up debugging a value that was fine.
        let same = wanted_rows.get(at).zip(got_rows.get(at)).is_some_and(|(left, right)| {
            left.len() == right.len()
                && left.iter().zip(right).enumerate().all(|(column, (left, right))| {
                    matched(left, right, kind_at(types, width, column))
                })
        });
        let mark = if same { ' ' } else { '*' };
        out.push_str(&format!("{mark} {left:<28}  {right}\n"));
    }
    if wanted_rows.len().max(got_rows.len()) > 20 {
        out.push_str("  and more, cut off at twenty rows\n");
    }
    out
}

/// How many rows a flat list of values is, given the width.
fn rows_of(values: usize, width: usize) -> usize {
    values.checked_div(width).unwrap_or(0)
}

/// Cut a flat list of values into rows.
fn chunk(values: &[String], width: usize) -> Vec<Vec<String>> {
    if width == 0 {
        return Vec::new();
    }
    values.chunks(width).map(<[String]>::to_vec).collect()
}

/// Every `.test` file under a path, or the path itself when it is a file.
/// Every test file under a path, in a stable order.
///
/// Public because the isolating runner in [`crate::isolate`] walks the corpus itself and then
/// hands the files out one at a time to child processes, so it needs the same list this module
/// would have built and it needs it before anything runs.
///
/// # Errors
///
/// When a directory cannot be read.
pub fn files(path: &Path, slow: bool) -> Result<Vec<PathBuf>, HarnessError> {
    let mut out = Vec::new();
    collect(path, slow, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect(path: &Path, slow: bool, out: &mut Vec<PathBuf>) -> Result<(), HarnessError> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    let entries = std::fs::read_dir(path)
        .map_err(|e| HarnessError::new(format!("cannot read {}: {e}", path.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| HarnessError::new(format!("cannot read {}: {e}", path.display())))?;
        let at = entry.path();
        if at.is_dir() {
            collect(&at, slow, out)?;
        } else if at.extension().is_some_and(|e| e == "test" || (slow && e == "test_slow")) {
            out.push(at);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Gap, HashMap, NAMES, Reason, Settings, Skips, Summary, VECTOR_SIZE, Verdict, check, exact,
        flatten, matched, render, run_text,
    };
    use crate::engine::{Cell, Column, Engine, EngineError, HarnessError, Outcome, Table};
    use crate::slt::{Condition, Directive, Record, Sort, StatementResult, TestFile};

    /// An engine that answers from a script, so the runner can be tested without a database.
    #[derive(Debug, Default)]
    struct Canned {
        answers: Vec<Outcome>,
        at: usize,
    }

    impl Engine for Canned {
        fn name(&self) -> &str {
            "canned"
        }

        fn version(&self) -> &str {
            "0"
        }

        fn run(&mut self, _sql: &str) -> Result<Outcome, HarnessError> {
            let out = self.answers.get(self.at).cloned().unwrap_or(Outcome::Rows(Table::default()));
            self.at += 1;
            Ok(out)
        }

        fn accepts(&mut self, _sql: &str) -> Result<crate::engine::Acceptance, HarnessError> {
            Ok(crate::engine::Acceptance::Accepted)
        }
    }

    fn table(width: usize, values: &[&str]) -> Table {
        let columns = (0..width)
            .map(|i| Column { name: format!("c{i}"), ty: "VARCHAR".to_owned() })
            .collect();
        let rows = values
            .chunks(width)
            .map(|row| row.iter().map(|v| Cell::Text((*v).to_owned())).collect())
            .collect();
        Table { columns, rows }
    }

    fn run(answers: Vec<Outcome>, text: &str) -> Summary {
        let mut engine = Canned { answers, at: 0 };
        run_text(&mut engine, "x.test", text).expect("the canned engine cannot fail")
    }

    #[test]
    fn a_query_that_returns_what_the_file_says_passes() {
        let answers = vec![Outcome::Rows(table(1, &["1", "2"]))];
        let summary = run(answers, "query I\nSELECT a FROM t\n----\n1\n2\n");
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn a_line_per_row_is_told_from_a_line_per_value_by_the_result_and_not_by_the_text() {
        // Four values over two columns, written both ways, against the same two row result.
        let answers = || vec![Outcome::Rows(table(2, &["1", "2", "3", "4"]))];
        let stacked = run(answers(), "query II\nSELECT a, b FROM t\n----\n1\n2\n3\n4\n");
        assert_eq!(stacked.passed, 1);
        let rowwise = run(answers(), "query II\nSELECT a, b FROM t\n----\n1\t2\n3\t4\n");
        assert_eq!(rowwise.passed, 1);

        // The same two lines against a two row result are two rows, and against a four row result
        // they are two of the four values, which is the whole reason this is not decided by the
        // parser. Both are wrong answers here and neither is a file the reader cannot read.
        let four = vec![Outcome::Rows(table(2, &["1", "2", "3", "4"]))];
        let summary = run(four, "query II\nSELECT a, b FROM t\n----\n1\n2\n");
        assert_eq!(summary.failed, 1);
    }

    #[test]
    fn a_result_block_that_does_not_divide_fails_the_record_and_leaves_the_file_alone() {
        // Three values under two columns. Sixteen corpus files do this, and as a parse error it
        // took the whole file with it.
        let text = "query II\nSELECT a, b FROM t\n----\n1\n2\n3\n\nquery I\nSELECT 9\n----\n9\n";
        let answers = vec![Outcome::Rows(table(2, &["1", "2"])), Outcome::Rows(table(1, &["9"]))];
        let summary = run(answers, text);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.passed, 1);
        assert!(
            summary.failures[0].detail.contains("whole number of rows"),
            "{}",
            summary.failures[0].detail
        );
    }

    #[test]
    fn a_row_with_the_wrong_number_of_values_in_it_says_which_row() {
        let answers = vec![Outcome::Rows(table(2, &["1", "2", "3", "4"]))];
        let summary = run(answers, "query II\nSELECT a, b FROM t\n----\n1\t2\n3\t4\t5\n");
        assert_eq!(summary.failed, 1);
        assert!(summary.failures[0].detail.contains("row 2"), "{}", summary.failures[0].detail);
    }

    #[test]
    fn a_query_that_returns_something_else_fails_and_the_report_shows_both() {
        let answers = vec![Outcome::Rows(table(1, &["1", "3"]))];
        let summary = run(answers, "query I\nSELECT a FROM t\n----\n1\n2\n");
        assert_eq!(summary.failed, 1);
        let detail = &summary.failures[0].detail;
        assert!(detail.contains('2'), "{detail}");
        assert!(detail.contains('3'), "{detail}");
        assert_eq!(summary.failures[0].reason, Reason::WrongAnswer);
    }

    #[test]
    fn rowsort_makes_the_order_not_part_of_the_answer_and_nosort_makes_it_part_of_it() {
        let answers = vec![Outcome::Rows(table(1, &["2", "1"]))];
        let sorted = run(answers.clone(), "query I rowsort\nSELECT a FROM t\n----\n1\n2\n");
        assert_eq!(sorted.passed, 1);
        let unsorted = run(answers, "query I\nSELECT a FROM t\n----\n1\n2\n");
        assert_eq!(unsorted.failed, 1);
    }

    #[test]
    fn a_hashed_result_is_checked_by_its_digest() {
        let answers = vec![Outcome::Rows(table(1, &["1", "2"]))];
        let digest = crate::hash::hash_values(&["1".to_owned(), "2".to_owned()]);
        let text = format!("query I\nSELECT a FROM t\n----\n2 values hashing to {digest}\n");
        assert_eq!(run(answers, &text).passed, 1);
    }

    #[test]
    fn a_result_of_the_wrong_width_is_a_failure_and_not_a_reshaped_comparison() {
        let answers = vec![Outcome::Rows(table(1, &["1", "2"]))];
        let summary = run(answers, "query II\nSELECT a, b FROM t\n----\n1\n2\n");
        assert_eq!(summary.failed, 1);
        assert!(summary.failures[0].detail.contains("columns"));
        assert_eq!(summary.failures[0].reason, Reason::WrongAnswer);
    }

    #[test]
    fn a_statement_that_has_to_fail_and_does_not_is_a_failure() {
        let answers = vec![Outcome::Rows(Table::default())];
        let summary = run(answers, "statement error\nDROP TABLE nope\n");
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failures[0].reason, Reason::MissedError);
    }

    #[test]
    fn an_expected_error_is_matched_as_a_fragment_of_the_real_one() {
        let answers = vec![Outcome::Error(EngineError {
            kind: "Catalog Error".to_owned(),
            message: "Table with name nope does not exist!".to_owned(),
        })];
        let text = "statement error\nDROP TABLE nope\n----\ndoes not exist\n";
        assert_eq!(run(answers, text).passed, 1);
    }

    #[test]
    fn a_record_the_condition_excludes_is_skipped_and_never_counted_as_a_pass() {
        let summary = run(Vec::new(), "skipif duckdb\nstatement ok\nSELECT 1\n");
        assert_eq!(summary.skipped.conditional, 1);
        assert_eq!(summary.passed, 0);
        assert!(!Condition::SkipIf("duckdb".to_owned()).applies_to(NAMES));
    }

    #[test]
    fn a_file_that_requires_something_we_do_not_have_is_skipped_whole() {
        let summary = run(Vec::new(), "require icu\n\nstatement ok\nSELECT 1\n");
        assert_eq!(summary.skipped_files.len(), 1);
        assert_eq!(summary.attempted(), 0);
    }

    #[test]
    fn the_modes_duckdbs_runner_is_not_in_are_not_missing_features_and_do_not_skip_a_file() {
        // These are the largest group of `require` lines in the corpus and none of them is about a
        // feature. Upstream's own runner answers yes to every one of them on an ordinary build.
        for what in [
            "skip_reload",
            "noforcestorage",
            "no_force_storage",
            "no_alternative_verify",
            "no_latest_storage",
            "no_vector_verification",
            "no_extension_autoloading EXPECTED: it explains itself",
            "nothreadsan",
            "notmusl",
            "strinline",
        ] {
            let text = format!("require {what}\n\nstatement ok\nSELECT 1\n");
            let summary = run(vec![Outcome::Rows(Table::default())], &text);
            assert!(summary.skipped_files.is_empty(), "{what}");
            assert_eq!(summary.passed, 1, "{what}");
        }
    }

    #[test]
    fn a_satisfied_require_is_not_counted_as_a_record_that_passed() {
        // The directive is not work, so counting it would add one free pass to every file that
        // carries one, and the corpus carries about fifteen hundred of them.
        let summary = run(Vec::new(), "require skip_reload\n");
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped.total(), 0);
    }

    #[test]
    fn a_vector_size_is_a_floor_and_an_exact_vector_size_is_an_equality() {
        let runs = |what: &str| {
            let text = format!("require {what}\n\nstatement ok\nSELECT 1\n");
            run(vec![Outcome::Rows(Table::default())], &text).skipped_files.is_empty()
        };
        assert!(runs("vector_size 64"));
        assert!(runs(&format!("vector_size {VECTOR_SIZE}")));
        assert!(!runs(&format!("vector_size {}", VECTOR_SIZE * 2)));
        assert!(runs(&format!("exact_vector_size {VECTOR_SIZE}")));
        assert!(!runs("exact_vector_size 2"));
    }

    #[test]
    fn an_extension_rudb_has_the_capability_of_is_not_a_reason_to_skip_a_file() {
        // rudb has no extensions, so the question a `require parquet` asks is whether the reader is
        // in the engine rather than whether an extension is loaded, and it is.
        let summary = run(
            vec![Outcome::Rows(Table::default())],
            "require parquet\n\nstatement ok\nSELECT 1\n",
        );
        assert!(summary.skipped_files.is_empty());
        assert_eq!(summary.passed, 1);
    }

    #[test]
    fn a_require_env_is_answered_from_the_environment_and_not_assumed() {
        // Answered rather than hard coded to missing, because a run with the variable set is a run
        // that should attempt the file.
        let name = "RUDB_COMPAT_A_VARIABLE_NOBODY_SETS";
        assert_eq!(super::have(true, &[name.to_owned()]), Some(Gap::Machine));
        assert_eq!(
            super::have(true, &["PATH".to_owned(), "not what PATH holds".to_owned()]),
            Some(Gap::Machine)
        );
        assert_eq!(super::have(true, &["PATH".to_owned()]), None);
    }

    #[test]
    fn the_report_says_what_the_file_asked_for_and_how_many_records_went_with_it() {
        let text =
            "require vector_size 4096\n\nstatement ok\nSELECT 1\n\nquery I\nSELECT 2\n----\n2\n";
        let summary = run(Vec::new(), text);
        let (_, why) = &summary.skipped_files[0];
        assert_eq!(why.to_string(), "requires vector_size 4096, and 2 records went with it");
        assert_eq!(why.gap(), Gap::Engine);
        assert_eq!(summary.skipped.engine, 2);
        assert_eq!(
            summary.skipped.by_gap(),
            [(Gap::Excused, 0), (Gap::Engine, 2), (Gap::Harness, 0), (Gap::Machine, 0)]
        );
    }

    #[test]
    fn a_file_skipped_whole_puts_its_records_on_the_skip_count_rather_than_nowhere() {
        // Before this they were in neither the skip count nor the denominator, so the corpus got
        // quietly smaller and no line of the report said by how much.
        let summary =
            run(Vec::new(), "require icu\n\nstatement ok\nSELECT 1\n\nstatement ok\nSELECT 2\n");
        assert_eq!(summary.attempted(), 0);
        assert_eq!(summary.skipped.total(), 2);
        assert_eq!(summary.skipped.engine, 2);
    }

    #[test]
    fn the_gap_a_requirement_falls_in_is_the_one_that_would_close_it() {
        // The split is the whole point. A missing extension goes down when rudb gets better, an
        // operating system does not, and a file this runner cannot read is work here. Adding them
        // together is what made the old skip count unreadable.
        let cases = [
            ("icu", Gap::Engine),
            ("block_size 262144", Gap::Engine),
            ("vector_size 65536", Gap::Engine),
            ("ram 16gb", Gap::Machine),
            ("windows", Gap::Machine),
        ];
        for (what, want) in cases {
            let text = format!("require {what}\n\nstatement ok\nSELECT 1\n");
            let summary = run(Vec::new(), &text);
            assert_eq!(summary.skipped_files[0].1.gap(), want, "{what}");
        }
        assert_eq!(super::Skipped::NotText.gap(), Gap::Harness);
    }

    #[test]
    fn the_records_behind_a_file_that_could_not_be_read_are_not_guessed_at() {
        // A file that did not parse has no records to count, and putting its line count in the
        // report would be a made up number in the one place every number is computed.
        let summary = run(Vec::new(), "frobnicate 3\n");
        assert_eq!(summary.skipped_files.len(), 1);
        assert_eq!(summary.skipped_files[0].1.records(), 0);
        assert_eq!(summary.skipped.total(), 0);
    }

    #[test]
    fn a_restart_ends_the_file_rather_than_letting_the_checks_after_it_pass_for_free() {
        // The file writes something, reopens the database and checks it survived. rudb keeps its
        // tables in memory, so it cannot reopen anything, and running past the restart leaves the
        // data exactly where it was. Every check after it then passes for the one reason the file
        // was written to rule out, which is the worst kind of number a report like this can carry.
        let text = "load {TEST_DIR}/x.db\n\nstatement ok\nCREATE TABLE t(i INTEGER)\n\nrestart\n\nquery I\nSELECT count(*) FROM t\n----\n0\n";
        let summary = run(vec![Outcome::Rows(Table::default())], text);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped.engine, 2);
        let (_, why) = &summary.skipped_files[0];
        assert_eq!(why.to_string(), "stops at restart, and 2 records came after it");
        assert_eq!(why.gap(), Gap::Engine);
    }

    #[test]
    fn a_load_of_a_scratch_path_nothing_has_written_to_is_an_empty_database_and_runs() {
        // Most of the corpus writes `load {TEST_DIR}/whatever.db` at the top and then builds its own
        // tables, which is an ordinary test an in memory engine passes. Ending those files at the
        // load would throw away thousands of honest records for nothing.
        let text = "load {TEST_DIR}/x.db\n\nstatement ok\nCREATE TABLE t(i INTEGER)\n";
        let summary = run(vec![Outcome::Rows(Table::default())], text);
        assert_eq!(summary.passed, 1);
        assert!(summary.skipped_files.is_empty());
    }

    #[test]
    fn a_restart_with_nothing_loaded_is_a_wipe_and_not_a_question_about_storage() {
        // With no path there is nothing to reopen, so upstream gets an empty database back and so
        // do we. That is a reset, which this runner can do, so the file carries on.
        let text = "restart\n\nstatement ok\nCREATE TABLE t(i INTEGER)\n";
        let summary = run(vec![Outcome::Rows(Table::default())], text);
        assert_eq!(summary.passed, 1);
        assert!(summary.skipped_files.is_empty());
    }

    #[test]
    fn opening_a_database_that_already_has_something_in_it_ends_the_file() {
        // Three ways of saying the same thing. A path outside the scratch directories is a file
        // shipped with the corpus, `readonly` says the content is already there, and a second load
        // of a path this file wrote to earlier is the persistence question spelled differently.
        for line in [
            "load data/storage/views_092.db readonly",
            "load {TEST_DIR}/x.db readonly",
            "load {TEST_DIR}/x.db\n\nstatement ok\nCREATE TABLE t(i INTEGER)\n\nload {TEST_DIR}/x.db",
        ] {
            let text = format!("{line}\n\nquery I\nSELECT 1\n----\n1\n");
            let summary = run(vec![Outcome::Rows(Table::default())], &text);
            assert_eq!(summary.skipped_files.len(), 1, "{line}");
            assert_eq!(summary.skipped_files[0].1.gap(), Gap::Engine, "{line}");
        }
    }

    #[test]
    fn a_setting_the_file_makes_is_carried_out_and_costs_the_records_after_it_nothing() {
        let text = "set ignore_error_messages HTTP\n\nstatement ok\nSELECT 1\n";
        let summary = run(vec![Outcome::Rows(Table::default())], text);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.skipped.total(), 0);
        assert!(summary.skipped_files.is_empty());
    }

    #[test]
    fn an_error_the_file_said_to_stop_on_stops_it_and_is_excused_rather_than_failed() {
        // The corpus writes this above statements that reach out to a network, so the error named
        // is about the machine the run is on and not about the engine.
        let text = "set ignore_error_messages HTTP Error\n\nstatement ok\nFROM 'https://x/y.csv'\n\nstatement ok\nSELECT 1\n";
        let answers = vec![Outcome::Error(EngineError {
            kind: "IO Error".to_owned(),
            message: "HTTP Error: 404".to_owned(),
        })];
        let summary = run(answers, text);
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped.conditional, 2);
        assert_eq!(summary.skipped_files.len(), 1);
    }

    #[test]
    fn an_internal_error_is_never_the_error_a_file_was_asking_for() {
        // Upstream puts INTERNAL in the always fail list with no line in any file asking for it,
        // and this is the one place where passing a record would be worse than failing it.
        let boom = Outcome::Error(EngineError {
            kind: "INTERNAL Error".to_owned(),
            message: "Attempted to access index 5 in vector of size 3".to_owned(),
        });
        let text = "statement error\nSELECT 1\n----\nindex 5\n";
        let summary = run(vec![boom], text);
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failures[0].reason, Reason::ErrorClass);
    }

    #[test]
    fn a_variable_the_file_set_is_put_into_the_sql_before_it_runs() {
        let text = "set variable sf 0.01\n\nquery I\nSELECT {sf}, '${sf}'\n----\n9\n";
        let summary = run(vec![Outcome::Rows(table(1, &["1"]))], text);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failures[0].sql, "SELECT 0.01, '0.01'");
    }

    #[test]
    fn a_name_nothing_set_is_left_alone_rather_than_emptied() {
        let settings = Settings::default();
        assert_eq!(settings.fill("COPY t TO '{TEST_DIR}/x.csv'"), "COPY t TO '{TEST_DIR}/x.csv'");
    }

    #[test]
    fn a_seed_the_engine_will_not_take_ends_the_file_on_the_engine_row() {
        // Everything after it is written against a sequence of random numbers this run cannot
        // produce, so there is nothing after it to be right or wrong about.
        let text = "set seed 0.42\n\nquery I\nSELECT random()\n----\n1\n";
        let answers = vec![Outcome::Error(EngineError {
            kind: "Catalog Error".to_owned(),
            message: "Scalar Function with name setseed does not exist!".to_owned(),
        })];
        let summary = run(answers, text);
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped_files.len(), 1);
        assert_eq!(summary.skipped_files[0].1.gap(), Gap::Engine);
    }

    #[test]
    fn the_records_a_stopped_file_had_run_already_are_kept_rather_than_thrown_away() {
        // The file stops where the database changes, not from the top, so the work in front of the
        // directive stays in the denominator with its result on it.
        let text = "statement ok\nSELECT 1\n\nquery I\nSELECT 2\n----\nnope\n\nreconnect\n\nstatement ok\nSELECT 3\n";
        let summary =
            run(vec![Outcome::Rows(Table::default()), Outcome::Rows(table(1, &["2"]))], text);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped.unsupported, 2);
        assert_eq!(summary.skipped_files[0].1.gap(), Gap::Harness);
    }

    #[test]
    fn a_maybe_is_the_file_saying_it_does_not_know_so_it_is_excused_and_not_passed() {
        // A `statement maybe` cannot fail, so counting it as a pass puts a number in the report
        // that no engine had to earn. One corpus file is a thousand of them.
        let boom = |message: &str| {
            Outcome::Error(EngineError {
                kind: "Constraint Error".to_owned(),
                message: message.to_owned(),
            })
        };
        let text = "statement maybe\nINSERT INTO t VALUES (1)\n----\nduplicate key\n";
        for answer in [Outcome::Rows(Table::default()), boom("duplicate key value"), boom("other")]
        {
            let summary = run(vec![answer], text);
            assert_eq!(summary.passed, 0);
            assert_eq!(summary.failed, 0);
            assert_eq!(summary.skipped.maybe, 1);
        }
    }

    #[test]
    fn a_maybe_read_upstreams_way_works_or_says_what_it_says_it_will_and_nothing_else() {
        // The reading itself, for the day a report level wants to count these rather than excuse
        // them. A file writes `statement maybe` where the answer depends on a build option or on
        // which of two concurrent things happened first, and writes the error it would give under
        // the `----`. Accepting any error at all would turn a new bug in that statement into a pass.
        let boom = |message: &str| {
            Outcome::Error(EngineError {
                kind: "Constraint Error".to_owned(),
                message: message.to_owned(),
            })
        };
        let read = |answer: Outcome, wanted: Option<&str>| {
            let file = TestFile { name: "x.test".to_owned(), records: Vec::new() };
            let record = Record {
                line: 1,
                condition: Condition::Always,
                directive: Directive::Statement {
                    sql: "INSERT INTO t VALUES (1)".to_owned(),
                    expected: StatementResult::Maybe(wanted.map(str::to_owned)),
                },
            };
            let mut engine = Canned { answers: vec![answer], at: 0 };
            let mut labels = HashMap::new();
            let Verdict::Ran(got) =
                check(&mut engine, &file, &record, &mut labels, &Settings::default())
                    .expect("the canned engine cannot fail")
            else {
                panic!("nothing here is ignored")
            };
            got
        };
        assert!(read(Outcome::Rows(Table::default()), Some("duplicate key")).is_ok());
        assert!(read(boom("duplicate key value"), Some("duplicate key")).is_ok());
        assert!(read(boom("out of memory"), Some("duplicate key")).is_err());

        // With nothing under the `----` anything goes, which is what the fuzzer files in the corpus
        // mean by it.
        assert!(read(boom("whatever"), None).is_ok());
    }

    #[test]
    fn a_reset_label_makes_the_next_pair_of_queries_answer_to_each_other() {
        // Without it the second turn of the loop is compared against the first turn's result and a
        // file that is doing exactly what it means to do is reported as a wrong answer.
        let shared = |a: &str, b: &str| {
            format!(
                "query I nosort lbl\nSELECT 1\n----\n{a}\n\nreset label lbl\n\nquery I nosort lbl\nSELECT 2\n----\n{b}\n"
            )
        };
        let answers = vec![Outcome::Rows(table(1, &["7"])), Outcome::Rows(table(1, &["9"]))];
        let summary = run(answers, &shared("7", "9"));
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn a_continue_the_parser_could_not_settle_stops_the_file_rather_than_being_ignored() {
        // The parser settles a `continue` on a loop variable when it expands the loop, so one that
        // reaches the runner is conditional on the engine and the end of its turn is no longer in
        // the records. Running on would score the rest of the file against a state the file said to
        // skip.
        let text = "loop i 0 1\n\nonlyif duckdb\ncontinue\n\nstatement ok\nSELECT 1\n\nendloop\n";
        let summary = run(vec![Outcome::Rows(Table::default())], text);
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.skipped_files[0].1.gap(), Gap::Harness);
    }

    #[test]
    fn everything_after_a_halt_is_not_counted_in_either_direction() {
        let summary = run(Vec::new(), "statement ok\nSELECT 1\n\nhalt\n\nstatement ok\nSELECT 2\n");
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped.total(), 0);
    }

    #[test]
    fn mode_skip_turns_records_off_until_mode_unskip() {
        let text = "mode skip\n\nstatement ok\nSELECT 1\n\nmode unskip\n\nstatement ok\nSELECT 2\n";
        let summary = run(Vec::new(), text);
        assert_eq!(summary.skipped.mode, 1);
        assert_eq!(summary.passed, 1);
    }

    #[test]
    fn the_pass_rate_leaves_out_what_was_skipped_and_the_skip_count_is_the_thing_that_says_so() {
        let skipped = Skips { conditional: 96, ..Skips::default() };
        let summary = Summary { passed: 3, failed: 1, skipped, ..Summary::default() };
        assert!((summary.rate() - 0.75).abs() < f64::EPSILON);
        assert_eq!(summary.attempted(), 4);
    }

    #[test]
    fn the_column_type_decides_how_a_value_is_written_and_the_letter_never_does() {
        assert_eq!(render(&Cell::Text("2.5".to_owned()), "DOUBLE"), "2.5");
        assert_eq!(render(&Cell::Text("2.5".to_owned()), "INTEGER"), "2.5");
        assert_eq!(render(&Cell::Text("2.5".to_owned()), "VARCHAR"), "2.5");
        assert_eq!(render(&Cell::Null, "VARCHAR"), "NULL");
        assert_eq!(render(&Cell::Text(String::new()), "VARCHAR"), "(empty)");
    }

    #[test]
    fn a_boolean_is_written_as_one_or_zero_whichever_spelling_the_engine_used() {
        assert_eq!(render(&Cell::Text("true".to_owned()), "BOOLEAN"), "1");
        assert_eq!(render(&Cell::Text("FALSE".to_owned()), "BOOLEAN"), "0");
        assert_eq!(render(&Cell::Text("1".to_owned()), "BOOLEAN"), "1");
        assert_eq!(render(&Cell::Null, "BOOLEAN"), "NULL");
    }

    #[test]
    fn a_numeric_column_compares_by_value_and_a_string_column_compares_by_text() {
        assert!(matched("2.000000", "2.0", "DOUBLE"));
        assert!(matched("0.500", "0.5", "DECIMAL(18,3)"));
        assert!(!matched("2.5", "2.6", "DOUBLE"));
        assert!(!matched("2.000000", "2.0", "VARCHAR"));
        assert!(!matched("2.0", "NULL", "DOUBLE"));
        assert!(matched("true", "1", "BOOLEAN"));
        assert!(!matched("true", "0", "BOOLEAN"));
    }

    #[test]
    fn two_hugeints_that_differ_in_their_last_digit_are_not_the_same_number() {
        let left = "170141183460469231731687303715884105726";
        let right = "170141183460469231731687303715884105727";
        assert!(!matched(left, right, "HUGEINT"));
        // The same pair through a double, which is the comparison this is here to rule out.
        assert!(matched(left, right, "DOUBLE"));
    }

    #[test]
    fn a_number_written_two_ways_normalizes_to_one_string() {
        assert_eq!(exact("2.000000").as_deref(), exact("2").as_deref());
        assert_eq!(exact("0100").as_deref(), exact("100").as_deref());
        assert_eq!(exact("-0.0").as_deref(), Some("0"));
        assert_eq!(exact("+1").as_deref(), exact("1").as_deref());
        assert_ne!(exact("-1").as_deref(), exact("1").as_deref());
        assert_eq!(exact("1e3"), None);
        assert_eq!(exact("inf"), None);
        assert_eq!(exact(""), None);
    }

    #[test]
    fn valuesort_loses_which_row_a_value_came_from_and_rowsort_does_not() {
        let table = table(2, &["2", "9", "1", "8"]);
        assert_eq!(flatten(&table, Sort::RowSort), ["1", "8", "2", "9"]);
        assert_eq!(flatten(&table, Sort::ValueSort), ["1", "2", "8", "9"]);
    }

    fn errored(kind: &str, message: &str) -> Outcome {
        Outcome::Error(EngineError { kind: kind.to_owned(), message: message.to_owned() })
    }

    #[test]
    fn a_statement_that_should_have_worked_is_classified_by_what_the_engine_said_about_it() {
        let cases = [
            ("Parser Error", "syntax error at or near \"qualify\"", Reason::Syntax),
            ("Not implemented Error", "a cast from VARCHAR to TIME", Reason::NotImplemented),
            ("Catalog Error", "Scalar Function with name typeof does not exist!", Reason::Unbound),
            ("Binder Error", "Referenced column \"x\" not found", Reason::Unbound),
            ("Out of Range Error", "overflow in addition", Reason::Runtime),
        ];
        for (kind, message, want) in cases {
            let summary = run(vec![errored(kind, message)], "statement ok\nSELECT 1\n");
            assert_eq!(summary.failed, 1, "{kind}");
            assert_eq!(summary.failures[0].reason, want, "{kind}");
        }
    }

    #[test]
    fn an_error_of_the_wrong_kind_and_an_error_with_the_wrong_words_are_different_failures() {
        // Both of these are records where the file wanted an error and got one. The first is a
        // different conclusion about the statement and the second is the same conclusion worded
        // differently, and they are a design question and a copying job in that order.
        let class = run(
            vec![errored("Binder Error", "Referenced column \"x\" not found")],
            "statement error\nSELECT 1\n----\nConversion Error: Could not convert\n",
        );
        assert_eq!(class.failures[0].reason, Reason::ErrorClass);

        let text = run(
            vec![errored("Conversion Error", "something else entirely")],
            "statement error\nSELECT 1\n----\nConversion Error: Could not convert\n",
        );
        assert_eq!(text.failures[0].reason, Reason::ErrorText);
    }

    #[test]
    fn a_file_that_names_no_kind_cannot_be_a_kind_mismatch() {
        // Most of the corpus writes a fragment of the message and nothing else, and calling that a
        // kind mismatch would put nearly every error failure in one row and make the split useless.
        let summary = run(
            vec![errored("Binder Error", "Referenced column \"x\" not found")],
            "statement error\nSELECT 1\n----\ndoes not exist\n",
        );
        assert_eq!(summary.failures[0].reason, Reason::ErrorText);
    }

    #[test]
    fn a_bare_kind_on_its_own_line_is_still_a_kind() {
        let summary = run(
            vec![errored("Binder Error", "Referenced column \"x\" not found")],
            "statement error\nSELECT 1\n----\nConversion Error\n",
        );
        assert_eq!(summary.failures[0].reason, Reason::ErrorClass);
    }

    #[test]
    fn the_breakdown_counts_every_failure_and_leaves_out_the_reasons_that_did_not_happen() {
        let summary = run(
            vec![
                errored("Parser Error", "syntax error"),
                Outcome::Rows(table(1, &["9"])),
                errored("Parser Error", "syntax error"),
            ],
            "statement ok\nSELECT 1\n\nquery I\nSELECT a FROM t\n----\n1\n\nstatement ok\nSELECT 2\n",
        );
        let reasons = summary.reasons();
        assert_eq!(reasons.total(), summary.failed);
        assert_eq!(reasons.count(Reason::Syntax), 2);
        assert_eq!(reasons.count(Reason::WrongAnswer), 1);
        assert_eq!(reasons.count(Reason::Unbound), 0);
        // Most frequent first, because the top line is what somebody picks up next.
        assert_eq!(reasons.rows(), vec![(Reason::Syntax, 2), (Reason::WrongAnswer, 1)]);
        let printed = reasons.to_string();
        assert!(printed.contains("syntax"), "{printed}");
        assert!(!printed.contains("unbound"), "{printed}");
    }

    #[test]
    fn the_two_ways_the_engine_gives_up_are_one_reason_and_it_is_not_runtime() {
        // These two are the engine refusing rather than the engine being wrong, and they used to
        // fall into `Runtime` with the real bugs. They only reach the report at all because the
        // runner hands the limits down, so before that there was nothing to classify.
        let interrupt = EngineError { kind: "Interrupt Error".into(), message: "too slow".into() };
        let memory = EngineError { kind: "Out of Memory Error".into(), message: "too big".into() };
        assert_eq!(Reason::of(&interrupt), Reason::Stopped);
        assert_eq!(Reason::of(&memory), Reason::Stopped);
        assert_eq!(Reason::Stopped.name(), "stopped");
    }

    #[test]
    fn every_reason_has_a_name_of_its_own_that_reads_back_as_itself() {
        // The names travel through a pipe between the child process and the parent, so two reasons
        // sharing one would silently merge two rows of the report.
        for reason in Reason::ALL {
            assert_eq!(Reason::from_name(reason.name()), Some(reason));
        }
        let names: std::collections::BTreeSet<&str> =
            Reason::ALL.iter().map(|r| r.name()).collect();
        assert_eq!(names.len(), Reason::ALL.len());
        assert_eq!(Reason::from_name("frobnicated"), None);
    }
}
