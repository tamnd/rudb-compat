//! Running each corpus file in a process of its own, with a clock and a memory cap on it.
//!
//! The corpus is full of queries that are meant to be enormous. `create_table_as_abort.test` writes
//! `range(10000000000000000)` and expects the engine to give up, and `max_execution_time.test`
//! writes `SELECT COUNT(*) FROM range(100000000) t1, range(1000) t2` and expects a timeout to cut
//! it off. DuckDB has a memory manager and a cancellation path and answers both of those in
//! milliseconds, and rudb has both of those now, so the limits are handed to the engine and the
//! engine is what stops the statement. That is what makes the difference between a file that has
//! an outcome and a file that has nothing: a statement the engine stopped is an error the record
//! can be counted against, and a process that was killed takes every record in the file with it,
//! including the ones that had already passed.
//!
//! A conformance runner still cannot be built on the assumption that the engine under test
//! terminates. Not terminating is one of the behaviours it is there to measure, and an engine that
//! is asked to enforce its own limit is an engine that can have a bug in the enforcing. So the run
//! is still four thousand processes with one file each, and this process still holds a clock and a
//! cap over each of them. They are a backstop now rather than the first thing to fire, which is
//! why the clock here is twelve times the one the engine was given.
//!
//! The memory cap is checked by asking `ps` rather than by setting a resource limit, because
//! `ulimit -v` is not honoured on macOS and the alternative is a `setrlimit` call, which means
//! `unsafe`, which this crate does not have. Asking `ps` costs a process, so it is only asked
//! about a child that has already been alive for a while, and the overwhelming majority of files
//! finish in under a millisecond and are never asked about at all.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::conform::{Failure, Reason, Reasons, Skips, Summary};
use crate::engine::HarnessError;

/// How long a statement may run and how large a file may get.
///
/// Neither number is used as it stands. What the engine is told and what this process enforces are
/// both worked out from them, by [`Limits::statement`], [`Limits::deadline`] and [`Limits::budget`],
/// and the two are deliberately not the same so that the engine reaches its limit first.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Wall clock per statement.
    pub time: Duration,
    /// Resident set per file, in bytes.
    pub memory: u64,
}

impl Default for Limits {
    /// Ten seconds and two gigabytes.
    ///
    /// Both are far above what any file that works needs. The whole corpus runs in a second and a
    /// half in one process, so a statement that has been going for ten seconds on its own is not
    /// slow, it is not going to stop. The point of the limits is to name it, not to time anything.
    fn default() -> Self {
        Self { time: Duration::from_secs(10), memory: 2 * 1024 * 1024 * 1024 }
    }
}

/// How much longer than one statement's clock a whole file is given before it is killed.
///
/// The engine stops a statement at [`Limits::time`] and this runner stops the file at twelve times
/// that, so the normal way a runaway file ends is the engine ending it.
///
/// Twelve because a file with ten slow statements in it is a file that exists. `max_execution_time`
/// is four of them on purpose and `hash_join_dict_surviving` is several more by accident, and both
/// of those are the limits working rather than the limits being missed, so a backstop that kills
/// them is a backstop that throws away the outcome it was meant to preserve. A file that takes
/// longer than ten runaway statements is the engine not stopping, which is what this is here for.
const BACKSTOP: u32 = 12;

impl Limits {
    /// What the engine is told, which is a limit per statement and not per file.
    ///
    /// A statement is what the engine can put a clock on, so that is what the number becomes. A
    /// statement that on its own takes as long as the whole file was given is the thing being
    /// caught, and one that takes half of it twice is not, which is the right way round.
    #[must_use]
    pub const fn statement(self) -> Duration {
        self.time
    }

    /// How long the whole file gets before this runner kills it.
    #[must_use]
    pub fn deadline(self) -> Duration {
        self.time * BACKSTOP
    }

    /// The budget the engine charges its operators against.
    ///
    /// Half the resident cap, because the two count different things. The engine counts what its
    /// operators say they are holding and the cap is the size of the process, which is that plus
    /// the binary, the allocator's free lists and whatever the reader mapped. Setting them equal
    /// would mean the process was over the cap while the engine still thought it had room, and the
    /// kill from outside would win every time, which is the thing this is here to stop.
    #[must_use]
    pub const fn budget(self) -> u64 {
        self.memory / 2
    }
}

/// Why a file was cut off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// It was still running when the clock ran out.
    Time(Duration),
    /// It was over the memory cap when it was last looked at.
    Memory(u64),
    /// It exited on its own and not with success, which is a panic or a signal.
    Died(String),
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Time(how_long) => write!(f, "still running after {} seconds", how_long.as_secs()),
            Self::Memory(bytes) => write!(f, "over the memory cap at {} MB", bytes / (1024 * 1024)),
            Self::Died(what) => write!(f, "exited without finishing: {what}"),
        }
    }
}

/// How many files were cut off, by which limit caught them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cut {
    /// Still running when the clock ran out.
    pub timeout: usize,
    /// Over the resident set cap.
    pub memory: usize,
    /// Exited on its own without finishing, which is a panic or a signal.
    pub crashed: usize,
}

impl Cut {
    /// Every file that was cut off, however it was caught.
    #[must_use]
    pub const fn total(self) -> usize {
        self.timeout + self.memory + self.crashed
    }
}

impl fmt::Display for Cut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} files cut off, of which {} timed out, {} went over the memory cap and {} came apart",
            self.total(),
            self.timeout,
            self.memory,
            self.crashed
        )
    }
}

/// What an isolated run produced.
///
/// The same counts a [`Summary`] carries, plus the files that were cut off. The reasons a file was
/// not run are strings here rather than the `Skipped` enum, because they came back through a pipe
/// and the report only ever prints them.
#[derive(Debug, Clone, Default)]
pub struct Isolated {
    /// How many files were read.
    pub files: usize,
    /// Records that ran and did what the file said.
    pub passed: usize,
    /// Records that ran and did something else.
    pub failed: usize,
    /// Records that were not attempted, split by why.
    pub skips: Skips,
    /// Files that were not run at all, with the reason as the runner worded it.
    pub skipped_files: Vec<(String, String)>,
    /// Every failure, in file order.
    pub failures: Vec<Failure>,
    /// Files that were cut off, which are counted as neither passed nor failed.
    ///
    /// Kept out of both counts on purpose. A file that was killed part way through has records
    /// nobody has an answer for, and putting them in either column would be a number with nothing
    /// behind it. They are a list of names, which is the only honest thing to publish about them.
    pub stopped: Vec<(String, Stopped)>,
}

impl Isolated {
    /// How many records were attempted, which is the denominator of the pass rate.
    #[must_use]
    pub const fn attempted(&self) -> usize {
        self.passed + self.failed
    }

    /// The share of attempted records that passed, between zero and one.
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
    #[must_use]
    pub fn reasons(&self) -> Reasons {
        Reasons::of(&self.failures)
    }

    /// How many files were cut off for each of the three reasons a file gets cut off.
    ///
    /// The record level breakdown cannot see these, because a file that was killed has no records
    /// with outcomes at all. They are the crash and timeout rows of the report and they are
    /// counted separately for the same reason they are excluded from the pass rate, which is that
    /// nobody knows what the records inside them would have done.
    #[must_use]
    pub fn cut_off(&self) -> Cut {
        let mut out = Cut::default();
        for (_, why) in &self.stopped {
            match why {
                Stopped::Time(_) => out.timeout += 1,
                Stopped::Memory(_) => out.memory += 1,
                Stopped::Died(_) => out.crashed += 1,
            }
        }
        out
    }

    fn absorb(&mut self, other: Self) {
        self.files += other.files;
        self.passed += other.passed;
        self.failed += other.failed;
        self.skips.conditional += other.skips.conditional;
        self.skips.mode += other.skips.mode;
        self.skips.unsupported += other.skips.unsupported;
        self.skipped_files.extend(other.skipped_files);
        self.failures.extend(other.failures);
        self.stopped.extend(other.stopped);
    }
}

/// Run every test file under `path`, each in its own process.
///
/// `exe` is this binary, which is re-run once per file with the hidden `slt-one` subcommand. Taking
/// it as an argument rather than calling `current_exe` here is what lets the tests point it at
/// something that is not a whole harness.
///
/// # Errors
///
/// When the corpus cannot be walked, or a child cannot be spawned at all, which is a broken
/// harness rather than a failing file.
pub fn run_corpus(
    exe: &Path,
    path: &Path,
    slow: bool,
    limits: Limits,
) -> Result<Isolated, HarnessError> {
    let files = crate::conform::files(path, slow)?;
    let base = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    let names: Vec<String> =
        files.iter().map(|f| f.strip_prefix(base).unwrap_or(f).display().to_string()).collect();

    let width = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let mut done: Vec<Option<Isolated>> = (0..files.len()).map(|_| None).collect();
    let mut running: Vec<Running> = Vec::new();
    let mut next = 0;

    while next < files.len() || !running.is_empty() {
        while running.len() < width && next < files.len() {
            running.push(Running::spawn(exe, &files[next], &names[next], next, limits)?);
            next += 1;
        }
        let mut at = 0;
        let mut settled = false;
        while at < running.len() {
            if let Some(result) = running[at].poll(limits) {
                let job = running.swap_remove(at);
                done[job.at] = Some(result);
                settled = true;
            } else {
                at += 1;
            }
        }
        // Only sleep when nothing came back, so a corpus of files that each take a microsecond is
        // not paced by the sleep instead of by the work.
        if !settled {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    let mut total = Isolated::default();
    for one in done.into_iter().flatten() {
        total.absorb(one);
    }
    Ok(total)
}

/// One child and what is known about it.
#[derive(Debug)]
struct Running {
    at: usize,
    name: String,
    child: Child,
    out: PathBuf,
    started: Instant,
    asked: Instant,
}

impl Running {
    fn spawn(
        exe: &Path,
        file: &Path,
        name: &str,
        at: usize,
        limits: Limits,
    ) -> Result<Self, HarnessError> {
        let out = std::env::temp_dir().join(format!("rudb-compat-{}-{at}.out", std::process::id()));
        let sink = File::create(&out)
            .map_err(|e| HarnessError::new(format!("cannot make {}: {e}", out.display())))?;
        let errors = sink
            .try_clone()
            .map_err(|e| HarnessError::new(format!("cannot share {}: {e}", out.display())))?;
        let child = Command::new(exe)
            .arg("slt-one")
            .arg(file)
            .arg(name)
            // The limits go with it, so the engine in the child stops a statement itself and the
            // file comes back with an outcome. What this process does with its own clock below is
            // the backstop for when that does not work.
            .arg(limits.statement().as_secs().to_string())
            .arg(limits.budget().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(sink))
            .stderr(Stdio::from(errors))
            .spawn()
            .map_err(|e| HarnessError::new(format!("cannot run {}: {e}", exe.display())))?;
        let now = Instant::now();
        Ok(Self { at, name: name.to_owned(), child, out, started: now, asked: now })
    }

    /// Look at the child once, and take it apart if it is finished or over a limit.
    fn poll(&mut self, limits: Limits) -> Option<Isolated> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let text = self.take();
                if status.success() {
                    return Some(decode(&self.name, &text));
                }
                let why = last_line(&text).unwrap_or_else(|| format!("{status}"));
                return Some(self.cut(Stopped::Died(why)));
            }
            Ok(None) => {}
            Err(e) => return Some(self.cut(Stopped::Died(e.to_string()))),
        }

        let alive = self.started.elapsed();
        if alive >= limits.deadline() {
            self.kill();
            let _ = self.take();
            return Some(self.cut(Stopped::Time(alive)));
        }
        // A child is only asked about its memory after it has been alive long enough to have used
        // any, and then twice a second, because asking costs a process of its own.
        if alive >= Duration::from_millis(500) && self.asked.elapsed() >= Duration::from_millis(500)
        {
            self.asked = Instant::now();
            if let Some(bytes) = resident(self.child.id()) {
                if bytes > limits.memory {
                    self.kill();
                    let _ = self.take();
                    return Some(self.cut(Stopped::Memory(bytes)));
                }
            }
        }
        None
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Read the child's output and delete the file it went to.
    fn take(&self) -> String {
        let text = std::fs::read_to_string(&self.out).unwrap_or_default();
        let _ = std::fs::remove_file(&self.out);
        text
    }

    fn cut(&self, why: Stopped) -> Isolated {
        Isolated { files: 1, stopped: vec![(self.name.clone(), why)], ..Isolated::default() }
    }
}

/// The last line of a child's output that has anything on it.
fn last_line(text: &str) -> Option<String> {
    text.lines().rev().find(|line| !line.trim().is_empty()).map(str::to_owned)
}

/// How much memory a process is holding, in bytes, or nothing if it cannot be asked.
///
/// `ps -o rss=` gives kilobytes on both macOS and Linux. A process that has already exited gives no
/// output rather than an error, and that reads as nothing here, which is right: a child that is
/// gone is not over the cap.
fn resident(pid: u32) -> Option<u64> {
    let out =
        Command::new("ps").arg("-o").arg("rss=").arg("-p").arg(pid.to_string()).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    text.trim().parse::<u64>().ok().map(|kb| kb * 1024)
}

/// Write a summary in the form the parent reads back.
///
/// Line based and tab separated, with newlines and tabs escaped, because the two things that have
/// to survive the trip are SQL and error text and both of them have newlines in them. Not JSON,
/// because this crate has no dependencies and a hand written JSON writer is a larger thing to get
/// wrong than this is.
#[must_use]
pub fn encode(summary: &Summary) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "counts\t{}\t{}\t{}\t{}\t{}\t{}\n",
        summary.files,
        summary.passed,
        summary.failed,
        summary.skipped.conditional,
        summary.skipped.mode,
        summary.skipped.unsupported
    ));
    for (name, why) in &summary.skipped_files {
        out.push_str(&format!("skipfile\t{}\t{}\n", escape(name), escape(&why.to_string())));
    }
    for failure in &summary.failures {
        out.push_str(&format!(
            "failure\t{}\t{}\t{}\t{}\t{}\n",
            escape(&failure.file),
            failure.line,
            escape(&failure.sql),
            failure.reason.name(),
            escape(&failure.detail)
        ));
    }
    out
}

/// Read back what [`encode`] wrote.
///
/// A line that is not one of the three known kinds is ignored, which is what lets the child write
/// to the same place its panics go without the parser having to know what a panic looks like.
#[must_use]
pub fn decode(name: &str, text: &str) -> Isolated {
    let mut out = Isolated::default();
    let mut counted = false;
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["counts", files, passed, failed, conditional, mode, unsupported] => {
                out.files = files.parse().unwrap_or(0);
                out.passed = passed.parse().unwrap_or(0);
                out.failed = failed.parse().unwrap_or(0);
                out.skips.conditional = conditional.parse().unwrap_or(0);
                out.skips.mode = mode.parse().unwrap_or(0);
                out.skips.unsupported = unsupported.parse().unwrap_or(0);
                counted = true;
            }
            ["skipfile", file, why] => {
                out.skipped_files.push((unescape(file), unescape(why)));
            }
            // A reason that does not parse becomes `Runtime` rather than dropping the failure.
            // Losing a failure would make the pass rate look better than it is, and `Runtime` is
            // the row that means the harness has nothing more specific to say, so it is where an
            // unreadable one belongs.
            ["failure", file, line, sql, reason, detail] => {
                out.failures.push(Failure {
                    file: unescape(file),
                    line: line.parse().unwrap_or(0),
                    sql: unescape(sql),
                    reason: Reason::from_name(reason).unwrap_or(Reason::Runtime),
                    detail: unescape(detail),
                });
            }
            _ => {}
        }
    }
    // A child that exited zero without saying anything is a child that was cut off between the
    // work and the printing, and counting it as an empty file would quietly lose it.
    if !counted {
        out.files = 1;
        out.stopped.push((name.to_owned(), Stopped::Died("said nothing".to_owned())));
    }
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conform::Skipped;

    #[test]
    fn a_summary_survives_the_trip_out_and_back() {
        let summary = Summary {
            files: 1,
            passed: 3,
            failed: 1,
            skipped: Skips { conditional: 2, mode: 1, unsupported: 4 },
            skipped_files: vec![("a.test".to_owned(), Skipped::Requires("parquet".to_owned()))],
            failures: vec![Failure {
                file: "b.test".to_owned(),
                line: 12,
                sql: "SELECT 1,\n  2".to_owned(),
                reason: Reason::WrongAnswer,
                detail: "wanted 1\tgot 2".to_owned(),
            }],
        };
        let back = decode("b.test", &encode(&summary));
        assert_eq!(back.files, 1);
        assert_eq!(back.passed, 3);
        assert_eq!(back.failed, 1);
        assert_eq!(back.skips.total(), 7);
        assert_eq!(back.skipped_files, vec![("a.test".to_owned(), "requires parquet".to_owned())]);
        assert_eq!(back.failures[0].sql, "SELECT 1,\n  2");
        assert_eq!(back.failures[0].detail, "wanted 1\tgot 2");
        assert_eq!(back.failures[0].reason, Reason::WrongAnswer);
        assert_eq!(back.reasons().count(Reason::WrongAnswer), 1);
        assert!(back.stopped.is_empty());
    }

    #[test]
    fn the_engine_is_given_a_tighter_limit_than_the_one_this_process_enforces() {
        // The whole point of handing the limits down is that the engine reaches them first. If
        // these two ever come out the other way round the kill from outside wins every time and
        // the files go back to having no outcome at all.
        let limits = Limits::default();
        assert_eq!(limits.statement(), Duration::from_secs(10));
        assert_eq!(limits.deadline(), Duration::from_secs(120));
        assert_eq!(limits.budget(), 1024 * 1024 * 1024);
        assert!(limits.statement() < limits.deadline());
        assert!(limits.budget() < limits.memory);
    }

    #[test]
    fn a_reason_the_parent_does_not_know_keeps_the_failure_rather_than_losing_it() {
        // A child built from a newer tree than the parent is not a thing that happens on purpose,
        // and it is a thing that would make the pass rate go up if an unreadable line were
        // dropped. A pass rate that improves because the two halves of the harness disagree is the
        // worst failure this file could have.
        let text = "counts\t1\t0\t1\t0\t0\t0\nfailure\tz.test\t3\tSELECT 1\tfrobnicated\tno idea\n";
        let back = decode("z.test", text);
        assert_eq!(back.failed, 1);
        assert_eq!(back.failures[0].reason, Reason::Runtime);
        assert_eq!(back.failures[0].detail, "no idea");
    }

    #[test]
    fn the_files_that_were_cut_off_are_counted_by_which_limit_caught_them() {
        let mut total = Isolated::default();
        total.absorb(Isolated {
            files: 3,
            stopped: vec![
                ("a.test".to_owned(), Stopped::Time(Duration::from_secs(10))),
                ("b.test".to_owned(), Stopped::Memory(3 << 30)),
                ("c.test".to_owned(), Stopped::Died("signal 11".to_owned())),
            ],
            ..Isolated::default()
        });
        let cut = total.cut_off();
        assert_eq!((cut.timeout, cut.memory, cut.crashed), (1, 1, 1));
        assert_eq!(cut.total(), 3);
    }

    #[test]
    fn a_child_that_printed_nothing_is_a_file_that_was_cut_off_and_not_an_empty_one() {
        let back = decode("gone.test", "");
        assert_eq!(back.files, 1);
        assert_eq!(back.attempted(), 0);
        assert_eq!(back.stopped.len(), 1);
    }

    #[test]
    fn a_panic_on_the_way_out_does_not_stop_the_counts_being_read() {
        let mut text = encode(&Summary { files: 1, passed: 2, ..Summary::default() });
        text.push_str("thread 'main' panicked at src/rudb.rs:1:1:\nnot yet\n");
        let back = decode("c.test", &text);
        assert_eq!(back.passed, 2);
        assert!(back.stopped.is_empty());
    }

    #[test]
    fn a_backslash_in_the_sql_comes_back_as_one_backslash() {
        let summary = Summary {
            files: 1,
            failures: vec![Failure {
                file: "d.test".to_owned(),
                line: 1,
                sql: "SELECT '\\n'".to_owned(),
                reason: Reason::Syntax,
                detail: String::new(),
            }],
            ..Summary::default()
        };
        let back = decode("d.test", &encode(&summary));
        assert_eq!(back.failures[0].sql, "SELECT '\\n'");
    }

    #[test]
    fn the_rate_is_over_what_was_attempted_and_a_stopped_file_is_in_neither_column() {
        let mut total = Isolated { files: 1, passed: 3, failed: 1, ..Isolated::default() };
        total.absorb(Isolated {
            files: 1,
            stopped: vec![("e.test".to_owned(), Stopped::Time(Duration::from_secs(10)))],
            ..Isolated::default()
        });
        assert_eq!(total.files, 2);
        assert_eq!(total.attempted(), 4);
        assert!((total.rate() - 0.75).abs() < 1e-9);
        assert_eq!(total.stopped.len(), 1);
    }
}
