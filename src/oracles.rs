//! Two oracles per record, so that a pass rate cannot quietly inflate itself.
//!
//! Every upstream file carries what each record is supposed to produce, and the pinned DuckDB
//! binary is sitting on the same machine. That is two independent answers to the same question and
//! the harness has been using one of them. `spec/sql/duckdb/09-the-harness.md` asks for both,
//! because the two disagreements they produce between them are not the same disagreement and only
//! one of them is a gap in the engine.
//!
//! There are four things a record can be, and the ordinary run can only tell two of them apart.
//!
//! Both engines pass it. The file is right, rudb is right, there is nothing here.
//!
//! rudb fails and the binary passes. This is the one the ordinary run already reports, and it is
//! the honest one: the file says what should happen, the binary agrees, and rudb does not do it.
//!
//! Both engines fail it. The file is stale or the pin has moved past it, because the binary the
//! corpus was written against does not do what the corpus says either. That is worth a note and it
//! is not a job for anybody working on rudb. Counting it as a rudb failure overstates the gap.
//!
//! rudb passes and the binary fails. This is the valuable one and the ordinary run cannot see it,
//! because from one oracle it looks like a pass. The file and rudb agreeing while the binary
//! disagrees means this harness is running the record differently from how the file means it, which
//! is a harness bug, and every one of them is a pass this project has not earned.
//!
//! The last case is the reason this exists. A harness with one oracle counts its own bugs as
//! successes, and the number that comes out the far end goes on a page with the word compatible
//! next to it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::conform::{Reason, Said, Told, corpus_top, run_file_telling};
use crate::engine::{Engine, HarnessError};
use crate::slt::{Directive, Record, TestFile};

/// What the two oracles between them say about one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Both engines did what the file says. Nothing to do.
    Agreed,
    /// rudb did not and the pinned binary did. A gap in the engine, which is the ordinary failure.
    Gap,
    /// Neither engine did. The file is stale or the pin has moved past it.
    Stale,
    /// rudb did and the pinned binary did not. This harness is running the record differently from
    /// how the file means it, which is a bug here.
    Harness,
}

impl Verdict {
    /// Every verdict, in the order the report prints them.
    pub const ALL: [Self; 4] = [Self::Agreed, Self::Gap, Self::Stale, Self::Harness];

    /// The short name, as it appears in the breakdown.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Agreed => "agreed",
            Self::Gap => "engine gap",
            Self::Stale => "stale file",
            Self::Harness => "harness bug",
        }
    }

    /// What the line means, printed beside the count so the report explains itself.
    #[must_use]
    pub const fn blurb(self) -> &'static str {
        match self {
            Self::Agreed => "both engines did what the file says",
            Self::Gap => "rudb did not and the pinned binary did, which is the real gap",
            Self::Stale => "neither did, so the file is stale or the pin moved past it",
            Self::Harness => "rudb did and the binary did not, so this runner is wrong",
        }
    }

    /// Which verdict a pair of answers is.
    #[must_use]
    pub const fn of(rudb: Said, duckdb: Said) -> Self {
        match (rudb, duckdb) {
            (Said::Passed, Said::Passed) => Self::Agreed,
            (Said::Failed(_), Said::Passed) => Self::Gap,
            (Said::Failed(_), Said::Failed(_)) => Self::Stale,
            (Said::Passed, Said::Failed(_)) => Self::Harness,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One record both oracles were asked about and did not answer the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// Which file.
    pub file: String,
    /// Which line the directive is on.
    pub line: usize,
    /// The SQL, as the file wrote it.
    pub sql: String,
    /// Which of the four this is.
    pub verdict: Verdict,
    /// Why rudb failed, when it did.
    pub rudb: Option<Reason>,
    /// Why the pinned binary failed, when it did.
    pub duckdb: Option<Reason>,
}

impl fmt::Display for Split {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}:{}  {}", self.file, self.line, self.verdict)?;
        match (self.rudb, self.duckdb) {
            (Some(rudb), Some(duckdb)) => writeln!(f, "    rudb {rudb}, duckdb {duckdb}")?,
            (Some(rudb), None) => writeln!(f, "    rudb {rudb}, duckdb passed")?,
            (None, Some(duckdb)) => writeln!(f, "    rudb passed, duckdb {duckdb}")?,
            (None, None) => {}
        }
        write!(f, "    {}", self.sql)
    }
}

/// What both oracles said about a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Both {
    counts: BTreeMap<Verdict, usize>,
    /// Every record the two oracles did not answer the same way.
    pub splits: Vec<Split>,
    /// How many records only one of the two runs reached, so neither oracle could be checked
    /// against the other. A file that ends early on one engine and not on the other puts records
    /// here, and they are not counted as anything else.
    pub unpaired: usize,
    /// How many files went in.
    pub files: usize,
    /// What ended a file early because an engine could not be asked, and how many files each one
    /// ended.
    ///
    /// Keyed by the message rather than by the file, because these are driver bugs and one driver
    /// bug ends a hundred files with the same sentence. A list by file would be a hundred lines
    /// saying one thing.
    pub broke: BTreeMap<String, usize>,
}

impl Both {
    /// Nothing asked yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many records came out this way.
    #[must_use]
    pub fn count(&self, verdict: Verdict) -> usize {
        self.counts.get(&verdict).copied().unwrap_or_default()
    }

    /// How many records both oracles were asked about.
    #[must_use]
    pub fn paired(&self) -> usize {
        Verdict::ALL.iter().map(|verdict| self.count(*verdict)).sum()
    }

    /// The pass rate the ordinary one oracle run would report over these records.
    ///
    /// That is everything rudb passed, which is the agreed records plus the harness bugs, because
    /// from one oracle a harness bug looks exactly like a pass.
    #[must_use]
    pub fn claimed(&self) -> usize {
        self.count(Verdict::Agreed) + self.count(Verdict::Harness)
    }

    /// Take in what the two runs of one file said.
    pub fn take(&mut self, file: &TestFile, rudb: &Told, duckdb: &Told) {
        self.files += 1;
        for (at, said) in rudb {
            let Some(other) = duckdb.get(at) else {
                self.unpaired += 1;
                continue;
            };
            let verdict = Verdict::of(*said, *other);
            *self.counts.entry(verdict).or_default() += 1;
            if verdict == Verdict::Agreed {
                continue;
            }
            let Some(record) = file.records.get(*at) else { continue };
            self.splits.push(Split {
                file: file.name.clone(),
                line: record.line,
                sql: sql_of(record).unwrap_or_default(),
                verdict,
                rudb: failed(*said),
                duckdb: failed(*other),
            });
        }
        // Records the binary reached and rudb did not are unpaired the same way round, and they are
        // counted here rather than skipped, because a file that rudb ends early is a file whose
        // remaining records nothing checked.
        self.unpaired += duckdb.keys().filter(|at| !rudb.contains_key(at)).count();
    }

    /// Fold another run's answers in, so a run over many files is one report.
    pub fn absorb(&mut self, other: Self) {
        for (verdict, count) in other.counts {
            *self.counts.entry(verdict).or_default() += count;
        }
        self.splits.extend(other.splits);
        self.unpaired += other.unpaired;
        self.files += other.files;
        for (message, count) in other.broke {
            *self.broke.entry(message).or_default() += count;
        }
    }

    /// Note that an engine could not be asked and the file ended there.
    pub fn broke_off(&mut self, why: &HarnessError) {
        *self.broke.entry(why.to_string()).or_default() += 1;
    }
}

impl fmt::Display for Both {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} files, {} records both oracles answered", self.files, self.paired())?;
        writeln!(f)?;
        for verdict in Verdict::ALL {
            writeln!(f, "{:>8}  {:<12}{}", self.count(verdict), verdict.name(), verdict.blurb())?;
        }
        if self.unpaired > 0 {
            writeln!(f)?;
            writeln!(
                f,
                "{} records only one of the two runs reached, so neither oracle checked the other on them. A file ends early on a halt or on an error it said to stop on, and the two engines do not have to end it in the same place.",
                self.unpaired
            )?;
        }
        if !self.broke.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "an engine could not be asked and the file ended there, which is a driver bug here rather than anything about the records"
            )?;
            for (message, count) in &self.broke {
                writeln!(f, "{count:>8}  {message}")?;
            }
        }
        let harness = self.count(Verdict::Harness);
        if harness > 0 {
            writeln!(f)?;
            writeln!(
                f,
                "{harness} of the {} records the one oracle run calls a pass are not passes. The file and rudb agree and the pinned binary does not, which means this runner is running the record differently from how the file means it. Every one of them is a bug here rather than a bug in the engine.",
                self.claimed()
            )?;
        }
        Ok(())
    }
}

/// Why a record failed, when it did.
const fn failed(said: Said) -> Option<Reason> {
    match said {
        Said::Passed => None,
        Said::Failed(reason) => Some(reason),
    }
}

/// The SQL a record runs, when it runs any.
fn sql_of(record: &Record) -> Option<String> {
    match &record.directive {
        Directive::Statement { sql, .. } | Directive::Query { sql, .. } => Some(sql.clone()),
        _ => None,
    }
}

/// Run one parsed file against both engines and pair the answers up.
///
/// An engine that cannot be asked ends that file and nothing else. Everywhere else here a harness
/// error stops the run, which is right when there is one engine and one answer, and wrong here. A
/// driver that cannot read one result out of one binary would otherwise take a four thousand file
/// corpus run down on its first file, and the records that did get two answers are still two
/// answers. What the file did get is kept and the reason is counted, and the reason is printed at
/// the end so a driver bug is visible rather than silently shortening the run.
pub fn over_file(rudb: &mut dyn Engine, duckdb: &mut dyn Engine, file: &TestFile) -> Both {
    let mut both = Both::new();
    let mut left = Told::new();
    let mut right = Told::new();
    if let Err(e) = run_file_telling(rudb, file, &mut left) {
        both.broke_off(&e);
    }
    if let Err(e) = run_file_telling(duckdb, file, &mut right) {
        both.broke_off(&e);
    }
    both.take(file, &left, &right);
    both
}

/// Run a file or a directory of them against both engines.
///
/// A file that cannot be read or cannot be parsed is left out rather than counted, which is the
/// same thing the one oracle run does with it. The question here is about the records that ran.
///
/// # Errors
///
/// When the files could not be walked at all. An engine that cannot be asked about one file ends
/// that file and is counted, per [`over_file`].
pub fn over(
    rudb: &mut dyn Engine,
    duckdb: &mut dyn Engine,
    path: &Path,
    slow: bool,
) -> Result<Both, HarnessError> {
    let mut files = crate::conform::files(path, slow)?;
    files.sort();
    let root = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    let top = corpus_top(root);
    let watching = std::env::var_os("RUDB_COMPAT_WATCH").is_some();
    let mut out = Both::new();
    for (at, one) in files.iter().enumerate() {
        let name = one.strip_prefix(root).unwrap_or(one).display().to_string();
        let Ok(bytes) = std::fs::read(one) else { continue };
        let Ok(text) = String::from_utf8(bytes) else { continue };
        let Ok(parsed) = crate::slt::parse_under(top.as_deref(), &name, &text) else { continue };
        out.absorb(over_file(rudb, duckdb, &parsed));
        if watching {
            eprintln!(
                "{} of {}, {name}, {} harness bugs so far",
                at + 1,
                files.len(),
                out.count(Verdict::Harness)
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Both, Said, Told, Verdict};
    use crate::conform::Reason;
    use crate::engine::HarnessError;
    use crate::slt::parse;

    fn told(answers: &[(usize, Said)]) -> Told {
        answers.iter().copied().collect()
    }

    #[test]
    fn the_four_things_a_pair_of_answers_can_be() {
        let passed = Said::Passed;
        let failed = Said::Failed(Reason::Unbound);
        assert_eq!(Verdict::of(passed, passed), Verdict::Agreed);
        assert_eq!(Verdict::of(failed, passed), Verdict::Gap);
        assert_eq!(Verdict::of(failed, failed), Verdict::Stale);
        assert_eq!(Verdict::of(passed, failed), Verdict::Harness);
    }

    #[test]
    fn a_record_rudb_passes_and_the_binary_fails_is_a_harness_bug_and_not_a_pass() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut both = Both::new();
        both.take(&file, &told(&[(0, Said::Passed)]), &told(&[(0, Said::Failed(Reason::Runtime))]));
        assert_eq!(both.count(Verdict::Harness), 1, "{both}");
        assert_eq!(both.count(Verdict::Agreed), 0, "{both}");
        assert_eq!(both.splits.len(), 1, "{both}");
        assert_eq!(both.splits[0].sql, "SELECT 1");
        assert_eq!(both.splits[0].duckdb, Some(Reason::Runtime));
        assert!(both.splits[0].rudb.is_none());
    }

    #[test]
    fn a_record_neither_engine_passes_is_a_stale_file_rather_than_a_gap_in_the_engine() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut both = Both::new();
        both.take(
            &file,
            &told(&[(0, Said::Failed(Reason::WrongAnswer))]),
            &told(&[(0, Said::Failed(Reason::WrongAnswer))]),
        );
        assert_eq!(both.count(Verdict::Stale), 1, "{both}");
        assert_eq!(both.count(Verdict::Gap), 0, "{both}");
    }

    #[test]
    fn the_rate_one_oracle_would_claim_counts_the_harness_bugs_as_passes() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut both = Both::new();
        both.take(&file, &told(&[(0, Said::Passed)]), &told(&[(0, Said::Passed)]));
        both.take(&file, &told(&[(0, Said::Passed)]), &told(&[(0, Said::Failed(Reason::Syntax))]));
        assert_eq!(both.claimed(), 2, "{both}");
        assert_eq!(both.count(Verdict::Agreed), 1, "{both}");
        assert!(both.to_string().contains("are not passes"), "{both}");
    }

    #[test]
    fn a_record_only_one_run_reached_is_unpaired_and_is_not_any_of_the_four() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut both = Both::new();
        both.take(
            &file,
            &told(&[(0, Said::Passed), (1, Said::Passed)]),
            &told(&[(0, Said::Passed)]),
        );
        assert_eq!(both.unpaired, 1, "{both}");
        assert_eq!(both.paired(), 1, "{both}");
    }

    #[test]
    fn a_record_the_binary_reached_and_rudb_did_not_is_unpaired_the_same_way_round() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut both = Both::new();
        both.take(
            &file,
            &told(&[(0, Said::Passed)]),
            &told(&[(0, Said::Passed), (1, Said::Passed)]),
        );
        assert_eq!(both.unpaired, 1, "{both}");
    }

    #[test]
    fn every_verdict_is_on_the_page_including_the_ones_at_zero() {
        let both = Both::new();
        let printed = both.to_string();
        for verdict in Verdict::ALL {
            assert!(printed.contains(verdict.name()), "{verdict} is missing from {printed}");
        }
    }

    #[test]
    fn a_driver_that_could_not_answer_ends_one_file_and_is_counted_by_what_it_said() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut one = Both::new();
        one.broke_off(&HarnessError::new("the column name is not in the DESCRIBE".to_owned()));
        one.take(&file, &told(&[(0, Said::Passed)]), &told(&[(0, Said::Passed)]));
        let mut two = Both::new();
        two.broke_off(&HarnessError::new("the column name is not in the DESCRIBE".to_owned()));
        two.take(&file, &told(&[]), &told(&[]));
        one.absorb(two);
        assert_eq!(one.broke.len(), 1, "one driver bug is one line, not two");
        assert_eq!(one.broke.values().sum::<usize>(), 2, "{one}");
        assert_eq!(one.paired(), 1, "the records that did get two answers are kept");
        assert!(one.to_string().contains("not in the DESCRIBE"), "{one}");
    }

    #[test]
    fn two_runs_fold_together_into_one_report() {
        let file = parse("t.test", "query I\nSELECT 1\n----\n1\n").expect("parses");
        let mut one = Both::new();
        one.take(&file, &told(&[(0, Said::Passed)]), &told(&[(0, Said::Passed)]));
        let mut two = Both::new();
        two.take(&file, &told(&[(0, Said::Failed(Reason::Unbound))]), &told(&[(0, Said::Passed)]));
        one.absorb(two);
        assert_eq!(one.files, 2, "{one}");
        assert_eq!(one.paired(), 2, "{one}");
        assert_eq!(one.count(Verdict::Gap), 1, "{one}");
    }
}
