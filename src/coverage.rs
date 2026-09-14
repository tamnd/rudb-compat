//! Putting the generated calls to both engines, and the function coverage number that comes out.
//!
//! `crate::functions` reads the catalog and writes the calls. This runs them and scores them, which
//! is the second half of `spec/14-rudb-compat.md` section 14.4 and the thing behind one of the three
//! level two denominators in `spec/sql/duckdb/01-what-compatible-means.md` section 1.1.
//!
//! The scoring rule is section 1.2 and it is stricter than it first looks. A function that is
//! implemented but differs from DuckDB on any tested input counts as not implemented. So an overload
//! passes only when every call generated for it agreed, and a name passes only when every overload
//! of it passed. `date_part` is thirty overloads and an engine with twenty nine of them has not got
//! `date_part`.
//!
//! The number is allowed to go down and it is meant to. An overload that passes today and disagrees
//! tomorrow because somebody added a boundary value goes back to failing, which is the whole point
//! of publishing it over a fixed denominator rather than over what happened to be tested.
//!
//! Three outcomes and not two. Passed, failed, and never put to either engine, which is a parameter
//! type nothing has a boundary set for, a kind the generator does not build calls for, or a function
//! whose answer changes between two calls. All three of those count against the denominator, because
//! a name nothing tested is a name nobody can claim, and all three are printed apart from each
//! other, because "we tested it and it was wrong" and "we never tested it" are different facts and a
//! report that adds them together is a report that hides the second one.

use std::collections::BTreeMap;
use std::fmt;

use crate::ask::differences;
use crate::compare::{Difference, MessageMatch, Ordering, Rules};
use crate::engine::{Engine, HarnessError};
use crate::functions::{Overload, calls, volatile};

/// What a run says about one overload row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every generated call agreed.
    Passed,
    /// At least one generated call disagreed.
    Failed,
    /// Nothing was put to it, and why.
    Untested(Untested),
}

/// Why an overload was never put to either engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Untested {
    /// A parameter type nothing in `crate::functions` has a boundary set for.
    NoBoundarySet,
    /// A kind the generator does not build calls for yet, which today is everything that is not
    /// scalar.
    KindNotCalled,
    /// The answer depends on something that is not in the call, so two working engines disagree.
    Volatile,
}

impl Untested {
    /// Every reason, in the order the summary prints them.
    pub const ALL: [Self; 3] = [Self::NoBoundarySet, Self::KindNotCalled, Self::Volatile];

    /// The short phrase the summary prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoBoundarySet => "no boundary set for a parameter type",
            Self::KindNotCalled => "a kind the generator does not call yet",
            Self::Volatile => "answers differently every time it is called",
        }
    }
}

/// One call the two engines did not agree about.
#[derive(Debug, Clone)]
pub struct Failure {
    /// The call, ready to paste into either engine.
    pub sql: String,
    /// What the call was for, which is the argument and the boundary it was at.
    pub note: String,
    /// Every way they disagreed.
    pub differences: Vec<Difference>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.sql)?;
        writeln!(f, "    {}", self.note)?;
        for difference in &self.differences {
            writeln!(f, "    {difference}")?;
        }
        Ok(())
    }
}

/// What a run says about one overload, with the cases behind it.
#[derive(Debug, Clone)]
pub struct Scored {
    /// The row from the catalog.
    pub overload: Overload,
    /// Passed, failed, or never tested.
    pub verdict: Verdict,
    /// How many calls were put to both engines.
    pub attempted: usize,
    /// Every call they disagreed about, in full. Never summarized away, per section 14.1.
    pub failures: Vec<Failure>,
}

impl Scored {
    /// How this overload would be written in a call, for a report line.
    #[must_use]
    pub fn signature(&self) -> String {
        format!(
            "{}({}) -> {}",
            self.overload.name,
            self.overload.parameters.join(", "),
            self.overload.returns
        )
    }
}

/// Put every generated call to both engines and score each overload.
///
/// The engines run in the order they were given and the comparison is the ordinary one, with the
/// row order taken as written because every call here returns exactly one row.
///
/// Progress goes to standard error when `RUDB_COMPAT_WATCH` is set, the same way `crate::suite`
/// does it, because a full sweep is fifteen thousand calls and a run that prints nothing for twenty
/// minutes looks like a run that has hung.
///
/// An engine that comes apart on one call does not end the sweep. That is [`crate::ask`] rather
/// than anything here: a panic is caught, recorded against the overload that caused it, and the
/// engine that panicked is reset before the next call. The first sweep that found one lost forty
/// minutes of work to a single bad call, which is why it is a rule of the harness and not a flag.
///
/// # Errors
///
/// When either engine could not be run at all, which is a broken harness rather than a failing
/// overload.
pub fn score(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    catalog: &[Overload],
    messages: MessageMatch,
) -> Result<Vec<Scored>, HarnessError> {
    let watching = std::env::var_os("RUDB_COMPAT_WATCH").is_some();
    let rules = Rules { ordering: Ordering::AsWritten, messages };
    let mut out = Vec::with_capacity(catalog.len());
    for (at, overload) in catalog.iter().enumerate() {
        if let Some(why) = untested(overload) {
            out.push(Scored {
                overload: overload.clone(),
                verdict: Verdict::Untested(why),
                attempted: 0,
                failures: Vec::new(),
            });
            continue;
        }
        let calls = calls(overload);
        let mut failures = Vec::new();
        for call in &calls {
            let differences = differences(left, right, &call.sql, rules)?;
            if !differences.is_empty() {
                failures.push(Failure {
                    sql: call.sql.clone(),
                    note: call.note.clone(),
                    differences,
                });
            }
        }
        if watching {
            eprintln!(
                "{} of {}, {}, {} calls, {}",
                at + 1,
                catalog.len(),
                overload.name,
                calls.len(),
                if failures.is_empty() { "passed" } else { "failed" }
            );
        }
        out.push(Scored {
            overload: overload.clone(),
            verdict: if failures.is_empty() { Verdict::Passed } else { Verdict::Failed },
            attempted: calls.len(),
            failures,
        });
    }
    Ok(out)
}

/// Why this overload is not going to be run, or nothing when it is.
///
/// Volatility is asked first, because a volatile function with a perfectly good boundary set is
/// still not something two engines can be compared on, and reporting it as a failure would be the
/// harness blaming the engine for the harness's own gap.
fn untested(overload: &Overload) -> Option<Untested> {
    if volatile(overload) {
        return Some(Untested::Volatile);
    }
    if calls(overload).is_empty() {
        return Some(if overload.kind == crate::functions::Kind::Scalar {
            Untested::NoBoundarySet
        } else {
            Untested::KindNotCalled
        });
    }
    None
}

/// How many passed, failed and were never tested, out of how many there are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Everything in the denominator.
    pub total: usize,
    /// Tested and agreed on every call.
    pub passed: usize,
    /// Tested and disagreed on at least one.
    pub failed: usize,
    /// Never put to either engine.
    pub untested: usize,
}

impl Tally {
    /// The share that passed, between zero and one.
    ///
    /// Over the total and not over what was tested. Dividing by what was tested is how a coverage
    /// number goes up by testing less, and it is the single easiest way to publish something
    /// meaningless, so the denominator here is always the whole catalog.
    #[must_use]
    pub fn share(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a catalog large enough for this to matter would not fit in memory"
        )]
        {
            self.passed as f64 / self.total as f64
        }
    }
}

/// The function coverage number and everything it is made of.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// Over the distinct names, which is the denominator section 1.1 names.
    pub names: Tally,
    /// Over the overload rows, which is the finer number and the one that moves first.
    pub overloads: Tally,
    /// How many overloads each untested reason accounts for.
    pub reasons: Vec<(Untested, usize)>,
    /// How many calls made an engine come apart instead of answering.
    ///
    /// Counted apart from the failures it is also part of. A crash and a wrong answer are both one
    /// failing call in the tally, and they are not the same news.
    pub crashes: usize,
    /// The names that failed, with how many of their overloads did, worst first.
    pub failing: Vec<(String, usize, usize)>,
}

/// Roll a run up into the published numbers.
///
/// A name fails when any overload of it fails, is untested when any overload of it is untested and
/// none failed, and passes only when every overload passed. That is section 1.2 read strictly, and
/// it is strict on purpose: a name is what a user writes, and a user who writes `date_part` and gets
/// the one overload nobody tested does not care that the other twenty nine were fine.
#[must_use]
pub fn coverage(scored: &[Scored]) -> Coverage {
    let mut overloads = Tally { total: scored.len(), ..Tally::default() };
    let mut reasons: BTreeMap<Untested, usize> = BTreeMap::new();
    let mut names: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    let mut failing: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for one in scored {
        let entry = names.entry(&one.overload.name).or_default();
        let counts = failing.entry(&one.overload.name).or_default();
        counts.1 += 1;
        match &one.verdict {
            Verdict::Passed => overloads.passed += 1,
            Verdict::Failed => {
                overloads.failed += 1;
                entry.0 = true;
                counts.0 += 1;
            }
            Verdict::Untested(why) => {
                overloads.untested += 1;
                *reasons.entry(*why).or_default() += 1;
                entry.1 = true;
            }
        }
    }
    let mut tally = Tally { total: names.len(), ..Tally::default() };
    for (failed, untested) in names.values() {
        match (failed, untested) {
            (true, _) => tally.failed += 1,
            (false, true) => tally.untested += 1,
            (false, false) => tally.passed += 1,
        }
    }
    let crashes = scored
        .iter()
        .flat_map(|one| &one.failures)
        .filter(|failure| {
            failure.differences.iter().any(|d| matches!(d, Difference::Panicked { .. }))
        })
        .count();
    let mut failing: Vec<(String, usize, usize)> = failing
        .into_iter()
        .filter(|(_, (failed, _))| *failed > 0)
        .map(|(name, (failed, total))| (name.to_owned(), failed, total))
        .collect();
    failing.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Coverage {
        names: tally,
        overloads,
        reasons: Untested::ALL
            .into_iter()
            .map(|why| (why, reasons.get(&why).copied().unwrap_or_default()))
            .collect(),
        crashes,
        failing,
    }
}

impl fmt::Display for Coverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = |f: &mut fmt::Formatter<'_>, what: &str, tally: &Tally| {
            writeln!(
                f,
                "{what:10} {} of {}, which is {:.1} percent, with {} failed and {} never tested",
                tally.passed,
                tally.total,
                tally.share() * 100.0,
                tally.failed,
                tally.untested
            )
        };
        line(f, "names", &self.names)?;
        line(f, "overloads", &self.overloads)?;
        writeln!(f)?;
        writeln!(f, "why an overload was never tested")?;
        for (why, count) in &self.reasons {
            writeln!(f, "    {count:>6}  {}", why.name())?;
        }
        if self.crashes > 0 {
            writeln!(f)?;
            writeln!(
                f,
                "{} calls made an engine panic rather than answer, and every one of them is a bug",
                self.crashes
            )?;
            writeln!(f, "in the engine that panicked rather than a difference between the two")?;
        }
        writeln!(f)?;
        writeln!(
            f,
            "the weighted number section 1.1 asks for is not here, because it needs the real query"
        )?;
        writeln!(
            f,
            "corpus to weight by and that corpus does not exist yet. This is the unweighted one."
        )?;
        if self.failing.is_empty() {
            return Ok(());
        }
        writeln!(f)?;
        writeln!(f, "names that failed, by how many of their overloads did")?;
        for (name, failed, total) in &self.failing {
            writeln!(f, "    {failed:>6} of {total:<4} {name}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Scored, Untested, Verdict, coverage, score};
    use crate::compare::{Difference, MessageMatch, Side};
    use crate::engine::{Acceptance, Engine, HarnessError, Outcome, Table};
    use crate::functions::{Kind, Overload};

    /// An engine that comes apart on one statement and answers every other one the same way.
    ///
    /// This is what a real engine with a bug in it looks like from here. rudb is linked into this
    /// process, so its panic is this process's panic, and the thing being tested is that one bad
    /// call costs one call rather than the whole run.
    #[derive(Debug)]
    struct Brittle {
        bad: &'static str,
        ran: usize,
        reset: usize,
    }

    impl Brittle {
        fn new(bad: &'static str) -> Self {
            Self { bad, ran: 0, reset: 0 }
        }
    }

    impl Engine for Brittle {
        fn name(&self) -> &str {
            "brittle"
        }

        fn version(&self) -> &str {
            "0"
        }

        fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
            self.ran += 1;
            assert!(!sql.contains(self.bad), "the engine came apart");
            Ok(Outcome::Rows(Table::default()))
        }

        fn accepts(&mut self, _sql: &str) -> Result<Acceptance, HarnessError> {
            Ok(Acceptance::Accepted)
        }

        fn reset(&mut self) -> Result<(), HarnessError> {
            self.reset += 1;
            Ok(())
        }
    }

    fn overload(name: &str) -> Overload {
        Overload {
            name: name.to_owned(),
            kind: Kind::Scalar,
            returns: "VARCHAR".to_owned(),
            parameters: vec!["VARCHAR".to_owned()],
            varargs: None,
            internal: true,
        }
    }

    #[test]
    fn an_engine_that_panics_on_one_call_costs_that_call_and_not_the_rest_of_the_run() {
        let mut left = Brittle::new("nothing generated looks like this");
        let mut right = Brittle::new("upper(NULL)");
        let catalog = [overload("upper"), overload("lower")];
        let scored =
            score(&mut left, &mut right, &catalog, MessageMatch::Kind).expect("neither gave up");

        assert_eq!(scored[0].verdict, Verdict::Failed, "the call that panicked is a failure");
        let crashed: Vec<_> = scored[0]
            .failures
            .iter()
            .filter(|f| {
                f.differences.contains(&Difference::Panicked {
                    side: Side::Right,
                    message: "the engine came apart".to_owned(),
                })
            })
            .collect();
        assert_eq!(crashed.len(), 1, "one call has a NULL in it, so one call panicked");
        assert!(crashed[0].sql.contains("NULL"), "{}", crashed[0].sql);
        assert_eq!(right.reset, 1, "the engine that came apart was put back before the next call");
        assert_eq!(scored[1].verdict, Verdict::Passed, "the second overload still ran");
        assert!(right.ran > scored[0].attempted, "the run went on past the panic");

        let coverage = coverage(&scored);
        assert_eq!(coverage.crashes, 1);
        assert!(coverage.to_string().contains("panic"), "{coverage}");
    }

    #[test]
    fn a_run_with_nothing_crashing_says_nothing_about_crashes() {
        let coverage = coverage(&[scored("a", &["VARCHAR"], Verdict::Passed)]);
        assert_eq!(coverage.crashes, 0);
        assert!(!coverage.to_string().contains("panic"));
    }

    fn scored(name: &str, parameters: &[&str], verdict: Verdict) -> Scored {
        Scored {
            overload: Overload {
                name: name.to_owned(),
                kind: Kind::Scalar,
                returns: "VARCHAR".to_owned(),
                parameters: parameters.iter().map(|p| (*p).to_owned()).collect(),
                varargs: None,
                internal: true,
            },
            verdict,
            attempted: 0,
            failures: Vec::new(),
        }
    }

    #[test]
    fn a_name_passes_only_when_every_overload_of_it_passed() {
        let run = vec![
            scored("date_part", &["VARCHAR", "DATE"], Verdict::Passed),
            scored("date_part", &["VARCHAR", "TIMESTAMP"], Verdict::Passed),
            scored("upper", &["VARCHAR"], Verdict::Passed),
        ];
        assert_eq!(coverage(&run).names.passed, 2);

        let mut one_bad = run;
        one_bad[1].verdict = Verdict::Failed;
        let coverage = coverage(&one_bad);
        assert_eq!(coverage.names.passed, 1);
        assert_eq!(coverage.names.failed, 1);
        assert_eq!(coverage.overloads.passed, 2);
    }

    #[test]
    fn a_name_with_one_overload_nobody_tested_is_not_a_name_that_passed() {
        let run = vec![
            scored("date_part", &["VARCHAR", "DATE"], Verdict::Passed),
            scored("date_part", &["VARCHAR", "ANY"], Verdict::Untested(Untested::NoBoundarySet)),
        ];
        let coverage = coverage(&run);
        assert_eq!(coverage.names.passed, 0);
        assert_eq!(coverage.names.untested, 1);
        assert_eq!(coverage.names.failed, 0);
    }

    #[test]
    fn a_name_that_both_failed_and_went_untested_counts_as_failed() {
        let run = vec![
            scored("date_part", &["VARCHAR", "DATE"], Verdict::Failed),
            scored("date_part", &["VARCHAR", "ANY"], Verdict::Untested(Untested::NoBoundarySet)),
        ];
        let coverage = coverage(&run);
        assert_eq!(coverage.names.failed, 1);
        assert_eq!(coverage.names.untested, 0);
    }

    #[test]
    fn the_share_is_over_everything_and_not_over_what_was_tested() {
        let run = vec![
            scored("a", &["VARCHAR"], Verdict::Passed),
            scored("b", &["VARCHAR"], Verdict::Untested(Untested::Volatile)),
            scored("c", &["VARCHAR"], Verdict::Untested(Untested::Volatile)),
            scored("d", &["VARCHAR"], Verdict::Untested(Untested::Volatile)),
        ];
        let coverage = coverage(&run);
        assert!(
            (coverage.names.share() - 0.25).abs() < f64::EPSILON,
            "one of four passed, so it is a quarter and not all of it"
        );
    }

    #[test]
    fn every_reason_is_printed_including_the_ones_at_zero() {
        let run = vec![scored("a", &["VARCHAR"], Verdict::Passed)];
        let coverage = coverage(&run);
        assert_eq!(coverage.reasons.len(), Untested::ALL.len());
        assert!(coverage.reasons.iter().all(|(_, count)| *count == 0));
        let page = coverage.to_string();
        for why in Untested::ALL {
            assert!(page.contains(why.name()), "{page}");
        }
    }

    #[test]
    fn the_names_that_failed_are_listed_with_the_worst_one_first() {
        let run = vec![
            scored("a", &["VARCHAR"], Verdict::Failed),
            scored("b", &["VARCHAR"], Verdict::Failed),
            scored("b", &["BIGINT"], Verdict::Failed),
            scored("c", &["VARCHAR"], Verdict::Passed),
        ];
        let coverage = coverage(&run);
        assert_eq!(
            coverage.failing,
            [("b".to_owned(), 2, 2), ("a".to_owned(), 1, 1)],
            "c passed so it is not on the list"
        );
    }

    #[test]
    fn the_page_says_the_weighted_number_is_not_the_one_it_is_printing() {
        let page = coverage(&[scored("a", &["VARCHAR"], Verdict::Passed)]).to_string();
        assert!(page.contains("unweighted"), "{page}");
        assert!(page.contains("real query"), "{page}");
    }
}
