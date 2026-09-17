//! Running the whole corpus once per optimizer pass, with that pass on and every other one off.
//!
//! `spec/09-optimizer.md` section 9.1 wants two things of the optimizer and this is the second of
//! them. The first is that the corpus answers the same with every pass on and with every pass off,
//! which is one gate per commit and is in `tests/corpus.rs`. It is the stronger property and it is
//! also the less useful failure: it says some pass changed an answer and leaves whoever reads it to
//! find out which.
//!
//! This is the other half. Pass k on and the rest off, once per pass, and the run that fails is the
//! pass that did it. The attribution comes out of the arrangement rather than out of a search
//! afterwards, which is what makes it worth a run of the corpus per pass instead of one run.
//!
//! The baseline with every pass off is run too, and it is not redundant. Without it a record that
//! the binder or the executor gets wrong fails in every run and gets reported once against each
//! pass, every one of them innocent. With it those records are named once and set aside, which is
//! the same distinction [`crate::bisect::Blame::Elsewhere`] draws for a single statement.
//!
//! What a sweep cannot see is two passes that are only wrong together, because no run here has two
//! passes in it. That case is the on against off gate's, and between them the two cover it: the
//! gate says there is something and a clean sweep beside a red gate says it takes a pair.
//!
//! #102 asks for this nightly, on the expectation that it is one fourteen second corpus run per
//! pass. It is not: ten runs of the committed corpus take three and a half seconds in a debug
//! build, because fourteen seconds was the upstream corpus and this is the committed one. So it is
//! a test as well as a nightly. A property that can be gated per commit and is only checked at
//! night is a property that is found to be broken a day late for no reason.
//!
//! The upstream corpus is where the nightly framing was right, and pointing this at it needs
//! something this does not have yet. It runs the corpus in this process, which is fine for a corpus
//! whose every record is supposed to pass, and upstream has records that do not stop. Those need
//! the isolating runner in [`crate::isolate`], which re-runs this binary per file and has no way to
//! tell the child which pass to leave on. That is the follow up, and it is a change to the child's
//! command line rather than to anything here.
//!
//! [`crate::report::Sweep`] is a different thing that shares the word. That one is a sweep of the
//! function catalog and what it leaves behind is a coverage number for the published page. The two
//! never meet, and the word is right for both, so both keep it and this says so.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use crate::conform::{Failure, Summary, run_path};
use crate::engine::HarnessError;
use crate::rudb::Rudb;

/// What the run with every pass off is called where the runs are listed.
const BASELINE: &str = "every pass off";

/// One run of the corpus with one pass on and the rest off.
#[derive(Debug)]
pub struct PassRun {
    /// Which pass was the one left on.
    pub pass: &'static str,
    /// What that run produced.
    pub summary: Summary,
}

/// The whole sweep, which is one run per pass and one with none of them.
#[derive(Debug)]
pub struct Sweep {
    /// The corpus with every pass off, which is the plan the binder produced and so is the right
    /// answer by construction.
    ///
    /// A record that fails here fails for a reason no pass is responsible for, and reporting it
    /// against a pass would send somebody to read a rewrite that is doing its job.
    pub baseline: Summary,
    /// One run per pass, in the order [`rudb::optimizers`] publishes them.
    pub runs: Vec<PassRun>,
}

/// A record that passes with every pass off and fails with one pass on.
///
/// The pass named is the one that changed the answer. Not a suspect, since it is the only rewrite
/// that ran.
#[derive(Debug, Clone, Copy)]
pub struct Changed<'a> {
    /// The pass whose run this failure came out of.
    pub pass: &'static str,
    /// The record, as that run reported it.
    pub failure: &'a Failure,
}

impl fmt::Display for Changed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} changed the answer to:", self.pass)?;
        write!(f, "{}", self.failure)
    }
}

impl Sweep {
    /// Every record a single pass changed the answer to.
    ///
    /// A record the baseline fails as well is left out, because the baseline is the right answer by
    /// construction and a record it already gets wrong is one the optimizer did not break. Those
    /// are in [`Sweep::unoptimized_failures`] instead, which is where somebody reading a red nightly
    /// should look second.
    #[must_use]
    pub fn changed(&self) -> Vec<Changed<'_>> {
        let already: BTreeSet<(&str, usize)> = records(&self.baseline);
        self.runs
            .iter()
            .flat_map(|run| {
                run.summary
                    .failures
                    .iter()
                    .filter(|failure| !already.contains(&(failure.file.as_str(), failure.line)))
                    .map(|failure| Changed { pass: run.pass, failure })
            })
            .collect()
    }

    /// The records that fail with every pass off, which are nobody here's fault.
    #[must_use]
    pub fn unoptimized_failures(&self) -> &[Failure] {
        &self.baseline.failures
    }

    /// Whether no pass changed an answer, which is what a green nightly means.
    ///
    /// The baseline failing does not make a sweep dirty. That is the per commit gate's failure and
    /// it is already red there, and a nightly that goes red for it as well reports one bug twice
    /// against the wrong milestone.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.changed().is_empty()
    }

    /// Which passes were swept, which is what pins the sweep to the engine it ran against.
    #[must_use]
    pub fn passes(&self) -> Vec<&'static str> {
        self.runs.iter().map(|run| run.pass).collect()
    }
}

impl fmt::Display for Sweep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} passes swept over {} files", self.runs.len(), self.baseline.files)?;
        // Measured rather than written down, because a pass name is as long as somebody felt like
        // making it and a column that a new pass overflows is a column that reads as a typo.
        let width = self
            .runs
            .iter()
            .map(|run| run.pass.len())
            .chain(std::iter::once(BASELINE.len()))
            .max()
            .unwrap_or(BASELINE.len())
            + 2;
        writeln!(
            f,
            "    {:>7}  {:<width$}{} of {} records passed",
            self.baseline.failed,
            BASELINE,
            self.baseline.passed,
            self.baseline.attempted()
        )?;
        for run in &self.runs {
            writeln!(
                f,
                "    {:>7}  {:<width$}{} of {} records passed",
                run.summary.failed,
                run.pass,
                run.summary.passed,
                run.summary.attempted()
            )?;
        }

        let changed = self.changed();
        writeln!(f)?;
        if changed.is_empty() {
            writeln!(f, "no pass changed an answer")?;
        } else {
            writeln!(f, "{} records a single pass changed the answer to", changed.len())?;
            for one in &changed {
                writeln!(f)?;
                write!(f, "{one}")?;
            }
        }

        if !self.baseline.failures.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "{} records fail with every pass off as well, so they are the binder or the executor and not the optimizer",
                self.baseline.failures.len()
            )?;
            for failure in &self.baseline.failures {
                writeln!(f)?;
                write!(f, "{failure}")?;
            }
        }
        Ok(())
    }
}

/// Which records a run failed, by file and line, which is what identifies one across two runs.
///
/// Not the whole [`Failure`], because the detail is what the two runs disagreed about and comparing
/// it would make every record look like a new one. Not the SQL either, because a loop body is a
/// different statement on every iteration and the record is the loop.
fn records(summary: &Summary) -> BTreeSet<(&str, usize)> {
    summary.failures.iter().map(|failure| (failure.file.as_str(), failure.line)).collect()
}

/// Run the corpus once per pass, and once with every pass off.
///
/// # Errors
///
/// When the corpus cannot be read. A record that fails is the result rather than an error, which is
/// the same rule the rest of the conformance runner follows.
pub fn run(path: &Path, slow: bool) -> Result<Sweep, HarnessError> {
    let mut off = Rudb::unoptimized();
    let baseline = run_path(&mut off, path, slow)?;

    let mut runs = Vec::new();
    for pass in rudb::optimizers() {
        let mut engine = Rudb::only(pass);
        runs.push(PassRun { pass, summary: run_path(&mut engine, path, slow)? });
    }
    Ok(Sweep { baseline, runs })
}

#[cfg(test)]
mod tests {
    use super::{PassRun, Sweep};
    use crate::conform::{Failure, Reason, Summary};

    /// A summary that failed those records and passed everything else.
    fn ran(passed: usize, failures: &[(&str, usize)]) -> Summary {
        Summary {
            files: 2,
            passed,
            failed: failures.len(),
            failures: failures
                .iter()
                .map(|(file, line)| Failure {
                    file: (*file).to_owned(),
                    line: *line,
                    sql: "SELECT 1".to_owned(),
                    reason: Reason::WrongAnswer,
                    detail: "got 2, wanted 1".to_owned(),
                })
                .collect(),
            ..Summary::default()
        }
    }

    /// A sweep over two passes, with that baseline and those two runs.
    fn swept(
        baseline: &[(&str, usize)],
        first: &[(&str, usize)],
        second: &[(&str, usize)],
    ) -> Sweep {
        Sweep {
            baseline: ran(10, baseline),
            runs: vec![
                PassRun { pass: "filter_pushdown", summary: ran(10, first) },
                PassRun { pass: "top_n", summary: ran(10, second) },
            ],
        }
    }

    #[test]
    fn a_record_one_pass_fails_and_the_unoptimized_run_passes_is_named_against_that_pass() {
        let sweep = swept(&[], &[], &[("order.test", 7)]);
        let changed = sweep.changed();
        assert_eq!(changed.len(), 1, "{sweep}");
        assert_eq!(changed[0].pass, "top_n");
        assert_eq!(changed[0].failure.line, 7);
        assert!(!sweep.clean());
    }

    #[test]
    fn a_record_that_fails_with_every_pass_off_is_not_reported_against_any_pass() {
        // The case the baseline earns its run for. Without it this record would be reported twice,
        // once against each pass, and neither of them touched it.
        let broken = [("bind.test", 3)];
        let sweep = swept(&broken, &broken, &broken);
        assert!(sweep.changed().is_empty(), "{sweep}");
        assert!(sweep.clean(), "a bug in the binder does not make the sweep dirty");
        assert_eq!(sweep.unoptimized_failures().len(), 1);
    }

    #[test]
    fn a_record_two_passes_each_break_on_their_own_is_named_against_both() {
        // Both runs had one pass in them, so both names are answers and not suspects.
        let sweep = swept(&[], &[("join.test", 12)], &[("join.test", 12)]);
        let changed = sweep.changed();
        assert_eq!(changed.len(), 2, "{sweep}");
        assert_eq!(sweep.passes(), vec!["filter_pushdown", "top_n"]);
    }

    #[test]
    fn a_sweep_that_found_nothing_says_so_rather_than_printing_an_empty_list() {
        let sweep = swept(&[], &[], &[]);
        assert!(sweep.clean());
        let printed = sweep.to_string();
        assert!(printed.contains("no pass changed an answer"), "{printed}");
        assert!(printed.contains("filter_pushdown"), "{printed}");
        assert!(printed.contains("top_n"), "{printed}");
    }

    #[test]
    fn the_printed_sweep_names_the_pass_beside_the_record_it_changed() {
        let sweep = swept(&[("bind.test", 3)], &[], &[("order.test", 7)]);
        let printed = sweep.to_string();
        assert!(printed.contains("top_n changed the answer to:"), "{printed}");
        assert!(printed.contains("order.test:7"), "{printed}");
        assert!(printed.contains("binder or the executor"), "{printed}");
        assert!(printed.contains("bind.test:3"), "{printed}");
    }

    #[test]
    fn a_record_at_another_line_of_a_file_the_baseline_fails_is_still_the_passs_doing() {
        // The baseline is matched per record and not per file, or one broken record would excuse a
        // whole file's worth of rewrites.
        let sweep = swept(&[("join.test", 3)], &[], &[("join.test", 40)]);
        let changed = sweep.changed();
        assert_eq!(changed.len(), 1, "{sweep}");
        assert_eq!(changed[0].failure.line, 40);
    }
}
