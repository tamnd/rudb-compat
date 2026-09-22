//! What the sqllogictest corpus costs on both engines, record by record.
//!
//! `crate::cost` measures the benchmark corpus and says in its own module doc that the
//! sqllogictest corpus cannot supply a measurement, because a record there only means anything
//! under a session that replays every statement before it and timing a record would time the
//! replay. That was true when it was written and it is not true any more. A session keeps one
//! database file now and the engine remembers between statements, so what a record costs is what
//! the record costs, which is `crate::shell::Session::on_a_file` and tamnd/rudb-compat#207.
//!
//! The two corpora answer different questions and this is the reason for having both. A benchmark
//! suite measures the shapes somebody chose to measure. The corpus measures the shapes nobody
//! chose, because a sqllogictest file is written to pin an answer rather than to be fast, so a
//! feature that is quick on the benchmark and quadratic on the long tail shows up here and nowhere
//! else. That is what `spec/sql/duckdb/01-what-compatible-means.md` section 1.7 asks for and what
//! exit criterion 6 of the D3 milestone, tamnd/rudb#466, is about.
//!
//! ## How a record gets a pair of numbers
//!
//! The whole corpus is run through one engine and then through the other, several times each, and
//! the runs are lined up afterwards on the file and the line the record started at. Running the
//! corpus five times per engine and pairing at the end is the same set of measurements as running
//! each record five times in place and costs the same, and it keeps the running of a file in one
//! piece, which matters because a record only makes sense after the records above it in its file.
//!
//! A record is in the denominator when both engines have a number for it, and the exclusions that
//! decides are all `crate::resource`'s rather than new ones here. A record that failed on either
//! side is never timed, because `crate::conform` only times a record that passed, so a failure on
//! one engine leaves the pair incomplete and the record drops out. A record the session replayed
//! its history in front of has no number either, because the session says so, and that is right:
//! that number is the cost of the history. A record under the floor on both sides is dropped by
//! [`Ratios::of`]. What is left is a record both engines answered, on its own, above the noise.
//!
//! ## What the number is mostly
//!
//! Most of what is left is a process rather than a query. A corpus record is a handful of rows, so
//! the engine answers it in well under a millisecond and what the clock sees is the shell starting,
//! opening the file, reading the catalog and printing. The first run of this said 0.14 on wall
//! clock with the quartiles at 0.11 and 0.22, and a spread that tight over eight hundred records of
//! very different shapes is a fixed cost and not a query cost. Read it as what it is: rudb answers
//! a small question from cold about seven times cheaper than the pin does, which is a real number
//! that matters to anything running a shell per statement, and it is not the number for a query
//! that does work. That one is `crate::cost` and the benchmark corpus, and the two belong on the
//! page side by side for exactly this reason.
//!
//! The processor time is worth less than the other two at this size and it is left in anyway. GNU
//! time reports user and system time in hundredths of a second and rudb answers a corpus record in
//! less than one of those, so the numerator is zero wherever the pin spent long enough to give the
//! division a denominator, and the column reads 0.00 down nearly its whole length. That is the
//! meter running out of digits rather than a result. Dropping the column would be worse, because it
//! is the one that catches an engine buying its wall clock with cores, and a number that is missing
//! for a reason somebody can read beats a number that quietly is not there.
//!
//! ## The three granularities
//!
//! The whole corpus, then per file, then the twenty records rudb is worst on, which is what
//! `spec/sql/duckdb/11-the-number.md` section 11.2 asks of any resource number this project
//! publishes. The last of the three is the one that gets read. An engine that is even on the median
//! and two hundred times slower on one record has a bug rather than a distribution, and a median
//! hides that by construction.

use std::collections::BTreeMap;
use std::path::Path;

use crate::conform::{Summary, Timing, run_path};
use crate::engine::{Engine, HarnessError};
use crate::resource::{Ratios, Usage, middle};
use crate::shell::{Session, Shell};

/// How many files a per file row needs behind it before it is worth printing.
///
/// One record is an anecdote and putting it in a table beside a file of two hundred invites
/// somebody to read it as a trend, which is the same rule `crate::cost::Costs::per_group` applies
/// to a suite.
const ENOUGH: usize = 2;

/// What one record cost on both engines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spend {
    /// The file the record is in.
    pub file: String,
    /// The line its directive was on, which names the record inside the file.
    pub line: usize,
    /// The middle run on rudb.
    pub ours: Usage,
    /// The middle run on the pinned binary.
    pub theirs: Usage,
}

impl Spend {
    /// The pair [`Ratios`] wants.
    #[must_use]
    pub const fn pair(&self) -> (Usage, Usage) {
        (self.ours, self.theirs)
    }

    /// What to call this record in a table, which is the file and the line.
    #[must_use]
    pub fn name(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }

    /// Wall clock, rudb over the pin. One is even and below one is faster.
    #[must_use]
    pub fn time(&self) -> f64 {
        let theirs = self.theirs.wall.as_secs_f64();
        if theirs == 0.0 { f64::INFINITY } else { self.ours.wall.as_secs_f64() / theirs }
    }

    /// Processor time, rudb over the pin.
    #[must_use]
    pub fn cpu(&self) -> f64 {
        let theirs = self.theirs.cpu.as_secs_f64();
        if theirs == 0.0 { f64::INFINITY } else { self.ours.cpu.as_secs_f64() / theirs }
    }

    /// Peak resident set, rudb over the pin.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "a resident set a double cannot count does not exist"
    )]
    pub fn memory(&self) -> f64 {
        if self.theirs.peak == 0 { 0.0 } else { self.ours.peak as f64 / self.theirs.peak as f64 }
    }
}

/// Everything a corpus cost run found.
#[derive(Debug, Clone, Default)]
pub struct Spends {
    /// The records both engines put a number on, which are the only ones with a ratio.
    pub records: Vec<Spend>,
    /// How many records rudb alone put a number on.
    ///
    /// Counted rather than listed, and counted because it is the number that says whether a run is
    /// worth reading. A corpus where one side measured half of what the other side measured is a
    /// corpus whose ratios are over a set of records nobody chose, and the two counts beside the
    /// measured count are what makes that visible instead of invisible.
    pub ours_only: usize,
    /// How many records the pinned binary alone put a number on.
    pub theirs_only: usize,
    /// How many records ran and neither engine could measure.
    pub unmeasured: usize,
    /// What the last run of each engine said about the corpus, for the counts a page prints.
    pub outcome: Option<(Summary, Summary)>,
}

impl Spends {
    /// The three ratios over every record measured on both sides.
    #[must_use]
    pub fn overall(&self) -> Option<Ratios> {
        let pairs: Vec<(Usage, Usage)> = self.records.iter().map(Spend::pair).collect();
        Ratios::of(&pairs)
    }

    /// The three ratios per file, worst median wall clock first.
    #[must_use]
    pub fn per_file(&self) -> Vec<(String, Ratios)> {
        let mut by: BTreeMap<&str, Vec<(Usage, Usage)>> = BTreeMap::new();
        for one in &self.records {
            by.entry(one.file.as_str()).or_default().push(one.pair());
        }
        let mut rows: Vec<(String, Ratios)> = by
            .into_iter()
            .filter(|(_, pairs)| pairs.len() >= ENOUGH)
            .filter_map(|(file, pairs)| Ratios::of(&pairs).map(|r| (file.to_owned(), r)))
            .collect();
        rows.sort_by(|a, b| {
            b.1.time.median.total_cmp(&a.1.time.median).then_with(|| a.0.cmp(&b.0))
        });
        rows
    }

    /// The records rudb is worst on, slowest first.
    #[must_use]
    pub fn worst(&self, how_many: usize) -> Vec<&Spend> {
        let mut all: Vec<&Spend> = self.records.iter().collect();
        all.sort_by(|a, b| b.time().total_cmp(&a.time()));
        all.truncate(how_many);
        all
    }
}

/// Run a corpus on both engines and report what every record cost.
///
/// `runs` passes over the corpus per engine, and the middle of a record's runs is its answer,
/// following `spec/15-rudb-bench.md` section 15.1. Never the minimum, which is a number about how
/// quiet the machine got rather than about the engine.
///
/// Both sides are driven as sessions on a database file, which is the only arrangement where what a
/// record costs is what the record costs. It is also the only arrangement where the two sides are
/// doing the same work, since a session that replays and a session that does not are answering the
/// same records with a different amount of the file in front of each one.
///
/// # Errors
///
/// When a corpus path cannot be read or an engine cannot be run at all. A record either engine
/// fails is not an error, it is a record with no ratio.
pub fn measure(
    ours: &Shell,
    theirs: &Shell,
    path: &Path,
    runs: usize,
) -> Result<Spends, HarnessError> {
    let (our_runs, our_last) = sweep(ours, path, runs)?;
    let (their_runs, their_last) = sweep(theirs, path, runs)?;

    let mut spends = Spends { outcome: Some((our_last, their_last)), ..Spends::default() };
    for (record, ours) in &our_runs {
        match their_runs.get(record) {
            Some(theirs) => spends.records.push(Spend {
                file: record.0.clone(),
                line: record.1,
                ours: *ours,
                theirs: *theirs,
            }),
            None => spends.ours_only += 1,
        }
    }
    spends.theirs_only = their_runs.keys().filter(|record| !our_runs.contains_key(*record)).count();
    spends.records.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    Ok(spends)
}

/// What names one record of one corpus, which is its file and the line it starts on.
type Which = (String, usize);

/// Run a corpus through one engine several times and take the middle of each record.
///
/// The summary of the last pass comes back with it. Every pass runs the same corpus and reaches the
/// same records, so any of them would do, and the last one is the one whose numbers the caller is
/// otherwise holding.
fn sweep(
    shell: &Shell,
    path: &Path,
    runs: usize,
) -> Result<(BTreeMap<Which, Usage>, Summary), HarnessError> {
    let mut seen: BTreeMap<Which, Vec<Usage>> = BTreeMap::new();
    let mut last = Summary::default();
    let watching = crate::suite::watching();
    let passes = runs.max(1);
    for pass in 0..passes {
        if watching {
            eprintln!("{} pass {} of {passes}", shell.name(), pass + 1);
        }
        let mut session = Session::new(shell.clone()).on_a_file();
        last = run_path(&mut session, path, false)?;
        for Timing { file, line, usage } in &last.timings {
            seen.entry((file.clone(), *line)).or_default().push(*usage);
        }
    }
    let middles = seen
        .into_iter()
        .filter_map(|(record, runs)| middle(&runs).map(|usage| (record, usage)))
        .collect();
    Ok((middles, last))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Spend, Spends};
    use crate::resource::Usage;

    fn usage(ms: u64, peak: u64) -> Usage {
        Usage { wall: Duration::from_millis(ms), cpu: Duration::from_millis(ms), peak }
    }

    fn spend(file: &str, line: usize, ours: u64, theirs: u64) -> Spend {
        Spend {
            file: file.to_owned(),
            line,
            ours: usage(ours, ours * 1024),
            theirs: usage(theirs, theirs * 1024),
        }
    }

    #[test]
    fn a_record_is_named_by_its_file_and_the_line_it_starts_on() {
        assert_eq!(spend("join.test", 42, 100, 100).name(), "join.test:42");
    }

    #[test]
    fn the_three_ratios_are_rudb_over_the_pin_so_below_one_is_better() {
        let one = spend("join.test", 1, 50, 100);
        assert!((one.time() - 0.5).abs() < 1e-9, "{}", one.time());
        assert!((one.cpu() - 0.5).abs() < 1e-9, "{}", one.cpu());
        assert!((one.memory() - 0.5).abs() < 1e-9, "{}", one.memory());
    }

    #[test]
    fn a_file_with_one_measured_record_is_left_out_of_the_per_file_table() {
        // One record is an anecdote. A table that prints it beside a file of two hundred is a
        // table that invites somebody to read an anecdote as a trend.
        let spends = Spends {
            records: vec![
                spend("alone.test", 1, 50, 100),
                spend("many.test", 1, 50, 100),
                spend("many.test", 9, 60, 100),
            ],
            ..Spends::default()
        };
        let files: Vec<String> = spends.per_file().into_iter().map(|(file, _)| file).collect();
        assert_eq!(files, vec!["many.test".to_owned()]);
    }

    #[test]
    fn the_per_file_table_is_worst_first() {
        let spends = Spends {
            records: vec![
                spend("quick.test", 1, 10, 100),
                spend("quick.test", 9, 20, 100),
                spend("slow.test", 1, 300, 100),
                spend("slow.test", 9, 400, 100),
            ],
            ..Spends::default()
        };
        let files: Vec<String> = spends.per_file().into_iter().map(|(file, _)| file).collect();
        assert_eq!(files, vec!["slow.test".to_owned(), "quick.test".to_owned()]);
    }

    #[test]
    fn the_worst_records_are_the_slowest_ones_and_there_are_no_more_than_asked_for() {
        let spends = Spends {
            records: vec![
                spend("a.test", 1, 10, 100),
                spend("b.test", 1, 900, 100),
                spend("c.test", 1, 500, 100),
            ],
            ..Spends::default()
        };
        let worst: Vec<String> = spends.worst(2).into_iter().map(Spend::name).collect();
        assert_eq!(worst, vec!["b.test:1".to_owned(), "c.test:1".to_owned()]);
    }

    #[test]
    fn a_record_under_the_floor_on_both_engines_is_not_in_the_overall_ratio() {
        // The exclusion lives in the resource module and this is the check that a corpus run gets
        // it. Both of these are a millisecond, which is mostly the cost of starting a shell.
        let spends = Spends { records: vec![spend("fast.test", 1, 1, 1)], ..Spends::default() };
        assert_eq!(spends.overall(), None);
    }
}
