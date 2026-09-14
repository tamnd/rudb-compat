//! What a statement cost, beside what it answered.
//!
//! The project goal is not 100 percent DuckDB compatibility. It is 100 percent compatibility at
//! ten times the speed and a tenth of the memory, and `spec/sql/duckdb/01-what-compatible-means.md`
//! section 1.7 puts the second and third of those here rather than only in the benchmark suite.
//! The argument is short. A benchmark suite measures the shapes somebody chose, and the corpus
//! measures the shapes nobody chose, and a feature that is fast on the benchmark and quadratic on
//! the long tail only shows up in the second. This is the early warning. `spec/15-rudb-bench.md`
//! is still where any number anybody quotes in public comes from.
//!
//! ## Why a wrapper process and not a system call
//!
//! The numbers wanted are the ones `getrusage` returns for a child, which is `ru_utime` plus
//! `ru_stime` for the processor time and `ru_maxrss` for the high water mark of the resident set.
//! Nothing in the standard library exposes them, and both ways of reaching them directly cost
//! something this crate has decided not to spend. A call through `libc` needs `unsafe`, which is
//! forbidden at the top of `lib.rs`. A call through `nix` or `rustix` needs a second dependency,
//! and `Cargo.toml` says in as many words that one dependency is the rule and that a second one
//! would be a hole in rudb wearing a workaround.
//!
//! GNU time already does it. It wraps a process, waits on it, and writes what `getrusage` said
//! into a file. It is the same measurement from the same call, it works on any binary rather than
//! only on ours, and it costs a fork per statement on top of the fork the harness was already
//! doing. The one thing to be careful about is that its report must not land on the child's
//! standard error, because that is where the error text being compared comes from, so it is always
//! given `-o` and `--quiet`.
//!
//! When GNU time is not on the machine, which is every macOS laptop, nothing is measured and
//! [`Usage`] comes back as `None`. That is the honest answer and it is better than a number from a
//! different measurement wearing the same name. It is also not a loss, because
//! `spec/sql/duckdb/09-the-harness.md` section 9.8 says the run happens on server1, server2 or the
//! gaming machine anyway, and section 11.2 says a ratio from a shared machine is not a ratio,
//! which is why server3 does not produce these at all.
//!
//! ## What is deliberately not measured
//!
//! The exclusions are written down here rather than applied by feel, because an exclusion nobody
//! wrote down is how a timing number stops meaning anything.
//!
//! A record that failed on either side is not timed, because timing an error path measures the
//! error path. A record under [`FLOOR`] on both engines is not timed, because a shell process that
//! starts, reads a statement and exits is mostly startup and the ratio is noise. A ratio is a
//! median of several runs with the interquartile range beside it and never a minimum, which is the
//! reporting rule `spec/15-rudb-bench.md` section 15.1 already set.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long a record has to take before its ratio is worth anything.
///
/// Below this the two numbers being divided are both mostly the cost of starting a process, so the
/// ratio says more about the two shells than about the two engines. Ten milliseconds is roughly
/// three times what an empty run of either shell costs on the machines the corpus runs on, which
/// is enough that the statement is the larger part of what was measured.
pub const FLOOR: Duration = Duration::from_millis(10);

/// How many times a record that is worth timing is run.
///
/// One run of anything is a sample of size one. Five is the smallest number where a median and an
/// interquartile range are not a joke, and the cost is bounded because only candidate records are
/// repeated, which is the ones that passed on both sides and are above [`FLOOR`].
pub const RUNS: usize = 5;

/// What one run of one engine cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// Wall clock from fork to exit.
    pub wall: Duration,
    /// Processor time, user plus system, which is the number that does not move when the machine
    /// is busy with something else.
    pub cpu: Duration,
    /// The high water mark of the resident set, in bytes.
    pub peak: u64,
}

impl Usage {
    /// Read one out of what GNU time wrote with `-v`.
    ///
    /// Three lines of the twenty three are wanted and the rest are ignored rather than parsed, so
    /// a version of GNU time that prints a field this one does not know about still works. A
    /// report missing any of the three is no report at all, because a `Usage` with a made up field
    /// in it is worse than nothing.
    #[must_use]
    pub fn parse(report: &str) -> Option<Self> {
        let mut user = None;
        let mut system = None;
        let mut wall = None;
        let mut peak = None;
        for line in report.lines() {
            let Some((label, value)) = line.split_once(':') else { continue };
            let label = label.trim();
            let value = value.trim();
            match label {
                "User time (seconds)" => user = value.parse::<f64>().ok().map(seconds),
                "System time (seconds)" => system = value.parse::<f64>().ok().map(seconds),
                "Maximum resident set size (kbytes)" => {
                    peak = value.parse::<u64>().ok().map(|kb| kb * 1024);
                }
                // The label has colons in it and so does the value, so this one is found by what it
                // starts with and the whole of the rest of the line is the number.
                _ if label.starts_with("Elapsed (wall clock) time") => {
                    wall = elapsed(line.rsplit(": ").next()?);
                }
                _ => {}
            }
        }
        Some(Self { wall: wall?, cpu: user? + system?, peak: peak? })
    }
}

/// Seconds as a float, which is how GNU time prints processor time.
fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value.max(0.0)).unwrap_or_default()
}

/// `h:mm:ss.ss` or `m:ss.ss`, which is how GNU time prints wall clock and nothing else prints
/// anything.
fn elapsed(text: &str) -> Option<Duration> {
    let mut total = 0.0;
    for part in text.trim().split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(seconds(total))
}

/// Whether this machine can measure, and how.
///
/// Found once and carried, because the answer cannot change during a run and asking costs a
/// process. A machine without GNU time gets a meter that measures nothing, which every caller
/// already has to handle, since a record can also be unmeasurable for being an error or for being
/// too fast.
#[derive(Debug, Clone, Default)]
pub struct Meter {
    time: Option<PathBuf>,
}

impl Meter {
    /// Look for a GNU time on the machine.
    ///
    /// BSD time is at the same path on macOS and answers a different set of flags, so the check is
    /// on what `--version` says rather than on the file being there. A binary that is not GNU time
    /// is not asked to behave like one.
    #[must_use]
    pub fn find() -> Self {
        for path in ["/usr/bin/time", "/bin/time"] {
            let path = Path::new(path);
            if !path.exists() {
                continue;
            }
            let Ok(out) = Command::new(path).arg("--version").output() else { continue };
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if said.contains("GNU") {
                return Self { time: Some(path.to_path_buf()) };
            }
        }
        Self { time: None }
    }

    /// A meter that measures nothing, for a caller that does not want the cost.
    #[must_use]
    pub const fn off() -> Self {
        Self { time: None }
    }

    /// True when a run through this meter comes back with numbers.
    #[must_use]
    pub const fn measures(&self) -> bool {
        self.time.is_some()
    }

    /// Start building a command that will be measured.
    ///
    /// The returned value hands out the same `Command` the caller would have built, so arguments,
    /// environment and redirections are set the way they always were and the wrapper is invisible.
    #[must_use]
    pub fn command(&self, program: &Path) -> Metered {
        let Some(time) = &self.time else {
            return Metered { command: Command::new(program), report: None };
        };
        let report = scratch();
        let mut command = Command::new(time);
        command.arg("--quiet").arg("-v").arg("-o").arg(&report).arg(program);
        Metered { command, report: Some(report) }
    }
}

/// A command with a meter around it.
#[derive(Debug)]
pub struct Metered {
    command: Command,
    report: Option<PathBuf>,
}

impl Metered {
    /// The command, to add arguments and redirections to as usual.
    pub fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Run it and hand back what it wrote and what it cost.
    ///
    /// The cost is `None` when this machine has no GNU time, and also when it has one and the
    /// report could not be read, which happens if the child was killed hard enough that the
    /// wrapper never got to write it. Both are the same answer to the caller: this record has no
    /// numbers, so it is not in the denominator.
    ///
    /// # Errors
    ///
    /// When the process could not be run at all, which is the same error the unmeasured path
    /// returns and means the same thing.
    pub fn output(mut self) -> io::Result<(Output, Option<Usage>)> {
        let out = self.command.output()?;
        let Some(report) = self.report.take() else { return Ok((out, None)) };
        let text = fs::read_to_string(&report).ok();
        let _ = fs::remove_file(&report);
        Ok((out, text.as_deref().and_then(Usage::parse)))
    }
}

/// A file for one report, named so two runs in two threads cannot collide.
fn scratch() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("rudb-compat-usage-{}-{at}", std::process::id()))
}

/// A set of numbers described the way a set of numbers has to be described.
///
/// The median rather than the mean, because one record that swapped should not move the figure,
/// and the interquartile range beside it rather than a standard deviation, because the
/// distribution of query times is not normal and never has been. Never a minimum, which is the
/// number that makes every engine look good and is the reason `spec/15-rudb-bench.md` section 15.1
/// bans it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    /// The middle value.
    pub median: f64,
    /// The lower quartile.
    pub low: f64,
    /// The upper quartile.
    pub high: f64,
    /// How many values went into it, because a spread over four records is not a spread.
    pub count: usize,
}

impl Spread {
    /// Describe a set of numbers, or nothing when the set is empty.
    ///
    /// # Panics
    ///
    /// Never. The sort cannot see a NaN, because every value reaching here is a ratio of two
    /// durations with a positive divisor.
    #[must_use]
    pub fn of(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("a ratio is never NaN"));
        Some(Self {
            median: quantile(&sorted, 0.5),
            low: quantile(&sorted, 0.25),
            high: quantile(&sorted, 0.75),
            count: sorted.len(),
        })
    }
}

/// The value at a fraction of the way through a sorted list, interpolating between neighbours.
fn quantile(sorted: &[f64], at: f64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a corpus with more records than a double can count does not exist"
    )]
    let position = at * (sorted.len() - 1) as f64;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the position is inside a list that fits in memory and is never negative"
    )]
    let below = position.floor() as usize;
    let above = (below + 1).min(sorted.len() - 1);
    #[expect(clippy::cast_precision_loss, reason = "the same bound as above")]
    let fraction = position - below as f64;
    sorted[below].mul_add(1.0 - fraction, sorted[above] * fraction)
}

/// The three ratios, rudb over DuckDB, over one set of records.
///
/// Three and not one, because they fail differently. An engine can be fast and enormous, which is
/// a chunk size that is too large, and it can be small and slow, which is a spill that should not
/// have happened. It can also be even on wall clock and far behind on processor time, which is an
/// engine getting its speed from cores rather than from work, and that one is invisible in every
/// report that publishes elapsed time alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ratios {
    /// Wall clock, rudb over DuckDB. Below one is faster.
    pub time: Spread,
    /// Processor time, rudb over DuckDB.
    pub cpu: Spread,
    /// Peak resident set, rudb over DuckDB.
    pub memory: Spread,
}

impl Ratios {
    /// The goal, which is a tenth on all three.
    pub const GOAL: f64 = 0.1;

    /// Work the three out over a list of records that were measured on both sides.
    ///
    /// Records below [`FLOOR`] on both engines are dropped here rather than by the caller, so that
    /// the exclusion is applied the same way everywhere it is applied. A record that is above the
    /// floor on one engine and below it on the other is kept, because that is not noise, that is
    /// one engine being ten times faster and is the whole point.
    #[must_use]
    pub fn of(pairs: &[(Usage, Usage)]) -> Option<Self> {
        let mut time = Vec::new();
        let mut cpu = Vec::new();
        let mut memory = Vec::new();
        for (ours, theirs) in pairs {
            if ours.wall < FLOOR && theirs.wall < FLOOR {
                continue;
            }
            if let Some(r) = share(ours.wall, theirs.wall) {
                time.push(r);
            }
            if let Some(r) = share(ours.cpu, theirs.cpu) {
                cpu.push(r);
            }
            if theirs.peak > 0 {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a resident set larger than a double can count does not exist"
                )]
                memory.push(ours.peak as f64 / theirs.peak as f64);
            }
        }
        Some(Self {
            time: Spread::of(&time)?,
            cpu: Spread::of(&cpu)?,
            memory: Spread::of(&memory)?,
        })
    }

    /// True when all three medians are at or below the goal.
    #[must_use]
    pub fn at_goal(&self) -> bool {
        self.time.median <= Self::GOAL
            && self.cpu.median <= Self::GOAL
            && self.memory.median <= Self::GOAL
    }
}

/// One duration over another, or nothing when the divisor is zero and the answer would be a lie.
fn share(ours: Duration, theirs: Duration) -> Option<f64> {
    if theirs.is_zero() {
        return None;
    }
    Some(ours.as_secs_f64() / theirs.as_secs_f64())
}

/// The middle run of several, which is what one record's number is.
///
/// Taken on wall clock, and the processor time and the peak that go with it come from the same
/// run rather than from three separate medians, so the three numbers a record reports are three
/// numbers from one run and describe something that actually happened.
#[must_use]
pub fn middle(runs: &[Usage]) -> Option<Usage> {
    let mut sorted = runs.to_vec();
    sorted.sort_by_key(|u| u.wall);
    sorted.get(sorted.len() / 2).copied()
}

#[cfg(test)]
mod tests {
    use super::{FLOOR, Meter, Ratios, Spread, Usage, middle};
    use std::time::Duration;

    const REPORT: &str = "\tCommand being timed: \"sh -c sleep 0.2\"
\tUser time (seconds): 0.04
\tSystem time (seconds): 0.02
\tPercent of CPU this job got: 1%
\tElapsed (wall clock) time (h:mm:ss or m:ss): 0:00.20
\tMaximum resident set size (kbytes): 1792
\tExit status: 0";

    fn usage(wall_ms: u64, cpu_ms: u64, peak_kb: u64) -> Usage {
        Usage {
            wall: Duration::from_millis(wall_ms),
            cpu: Duration::from_millis(cpu_ms),
            peak: peak_kb * 1024,
        }
    }

    #[test]
    fn the_three_numbers_come_out_of_what_gnu_time_writes() {
        let read = Usage::parse(REPORT).expect("the report has all three fields");
        assert_eq!(read.wall, Duration::from_millis(200));
        assert_eq!(read.cpu, Duration::from_millis(60));
        assert_eq!(read.peak, 1792 * 1024);
    }

    #[test]
    fn an_hour_long_run_is_read_as_an_hour() {
        let report = REPORT.replace("0:00.20", "1:02:03.50");
        let read = Usage::parse(&report).expect("the report still has all three fields");
        assert_eq!(read.wall, Duration::from_secs_f64(3723.5));
    }

    #[test]
    fn a_report_missing_a_field_is_no_report_rather_than_a_guess() {
        let short = REPORT
            .lines()
            .filter(|l| !l.contains("Maximum resident"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(Usage::parse(&short).is_none());
    }

    #[test]
    fn a_spread_is_the_middle_and_the_quartiles_and_never_the_smallest() {
        let spread = Spread::of(&[1.0, 2.0, 3.0, 4.0, 5.0]).expect("five values");
        assert!((spread.median - 3.0).abs() < 1e-9);
        assert!((spread.low - 2.0).abs() < 1e-9);
        assert!((spread.high - 4.0).abs() < 1e-9);
        assert_eq!(spread.count, 5);
    }

    #[test]
    fn a_spread_of_nothing_is_nothing() {
        assert!(Spread::of(&[]).is_none());
    }

    #[test]
    fn the_ratios_are_ours_over_theirs_so_below_one_is_better() {
        let pairs: Vec<_> = (0..5).map(|_| (usage(100, 90, 100), usage(1000, 900, 1000))).collect();
        let ratios = Ratios::of(&pairs).expect("five measured records");
        assert!((ratios.time.median - 0.1).abs() < 1e-9);
        assert!((ratios.cpu.median - 0.1).abs() < 1e-9);
        assert!((ratios.memory.median - 0.1).abs() < 1e-9);
        assert!(ratios.at_goal());
    }

    #[test]
    fn a_record_too_fast_to_measure_is_not_in_the_denominator() {
        let quick = FLOOR / 2;
        let fast = Usage { wall: quick, cpu: quick, peak: 1024 };
        let slow = usage(100, 100, 1024);
        let ratios = Ratios::of(&[(fast, fast), (slow, slow)]).expect("one record survived");
        assert_eq!(ratios.time.count, 1);
    }

    #[test]
    fn nothing_measurable_at_all_produces_nothing_rather_than_a_zero() {
        let quick = FLOOR / 2;
        let fast = Usage { wall: quick, cpu: quick, peak: 1024 };
        assert!(Ratios::of(&[(fast, fast)]).is_none());
    }

    #[test]
    fn a_records_number_is_the_middle_run_and_the_three_come_from_that_one_run() {
        let runs = vec![usage(10, 1, 100), usage(50, 9, 900), usage(30, 5, 500)];
        let one = middle(&runs).expect("three runs");
        assert_eq!(one, usage(30, 5, 500));
    }

    #[test]
    fn a_meter_that_is_off_says_so_rather_than_measuring_badly() {
        let off = Meter::off();
        assert!(!off.measures());
    }
}
