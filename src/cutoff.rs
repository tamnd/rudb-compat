//! The upstream files that do not stop, written down so that the list changing is visible.
//!
//! The upstream corpus is a measurement and not a gate, for the reason `.github/workflows/ci.yml`
//! gives: the pass rate is low by construction until the milestones are done, and a check that is
//! red every day until the day it goes green is a check nobody reads. So the run publishes a number
//! and exits zero whatever the number is.
//!
//! A handful of files are different in kind. They are not slow, they do not stop: the runner in
//! [`crate::isolate`] kills them at the backstop and they appear in a list at the bottom of a run
//! nobody scrolls to, which means the day one of them starts finishing is a day nothing happens.
//! That is the case tamnd/rudb#102 names. This is the list, with what each file actually is written
//! beside it, and a whole upstream run says how today differs from it instead of leaving the
//! difference to be noticed.
//!
//! It reports and does not fail the build, and that is a measurement rather than a preference. The
//! set is not reproducible. Running the files that have been seen here one at a time, alone, twice
//! each on an idle machine: `overflow/expression_tree_depth.test` finished once and ran past two
//! minutes once, and `catalog/table/create_table_as_abort.test` finished once and reached 2982 MB
//! against a 2048 MB cap once. Same file, same binary, same machine, nothing else running. An exact
//! comparison over a set that moves on its own would be red about half the time for no change at
//! all, which is the check-nobody-reads failure again and worse, because this one would be red
//! about something real.
//!
//! `optimizer/table_filters.test` was on this list and is the one name that has come off it. It was
//! cut off at the two minute backstop every time it was run, alone or in the corpus, because its
//! joins fell to a nested loop over a million driving rows. tamnd/rudb#866 taught the lookup to
//! recognise an equality with a cast around an operand and tamnd/rudb#883 gave it a residual
//! predicate, and the file now finishes in five seconds alone. That is the direction the printing
//! below was written for, and it is what the name coming off the list looks like.
//!
//! The nondeterminism is the more interesting half of what this found and it is tamnd/rudb#736
//! rather than something worked around here. A plausible shape: the engine is handed the statement
//! limit and stops itself
//! when it reaches an interrupt check, and on these paths it does not reliably reach one, so whether
//! a file ends as a counted failure at ten seconds or as a process killed at two minutes depends on
//! where the work happened to be. That is an engine question and this module is not where it gets
//! answered.
//!
//! So the difference is printed, in both directions, and both are worth reading. A file that
//! appears is a query that used to get through and now does not. A file that stops appearing is the
//! direction the box was written for, and the name coming off this list is the thing somebody is
//! being asked to do.
//!
//! What is compared is the names and not the reasons. A file that runs out of time on one run and
//! out of memory on the next is the same file with the same problem, and `create_table_as_abort`
//! above is literally that file.

use std::collections::BTreeSet;
use std::fmt;

use crate::isolate::Isolated;

/// Where the list lives, relative to the crate root.
pub const LIST: &str = "corpus/cutoff.txt";

/// What a run of the upstream corpus did to the written down list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cutoff {
    /// Cut off and not on the list, so something that used to finish no longer does.
    pub appeared: Vec<String>,
    /// On the list and not cut off, so it finished this time and the list may be one name too long.
    pub finished: Vec<String>,
    /// On the list and not in the corpus, which is the vendored ref moving and not the engine.
    pub missing: Vec<String>,
    /// On the list and cut off, which is every name on a run that matched.
    pub still: Vec<String>,
}

impl Cutoff {
    /// Whether the run matched the list exactly.
    ///
    /// Not a verdict on the build. See the module docs: the set moves on its own, so this is false
    /// often enough that failing on it would be failing on the weather.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.appeared.is_empty() && self.finished.is_empty() && self.missing.is_empty()
    }
}

impl fmt::Display for Cutoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.settled() {
            return writeln!(
                f,
                "the {} files that did not stop are the ones {LIST} names, and no others",
                self.still.len()
            );
        }
        for name in &self.appeared {
            writeln!(f, "{name} did not stop and is not in {LIST}, so it used to get through")?;
        }
        for name in &self.finished {
            writeln!(f, "{name} is in {LIST} and ran to the end this time")?;
        }
        for name in &self.missing {
            writeln!(f, "{name} is in {LIST} and is not in the corpus, so the vendored ref moved")?;
        }
        writeln!(
            f,
            "a name that has settled down either way wants {LIST} edited. Several of these files \
             go both ways on their own, so one run is not that."
        )
    }
}

/// Read the list, which is one file name per line with `#` comments and blank lines allowed.
#[must_use]
pub fn read(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Compare a whole run of the upstream corpus against the list.
///
/// `present` says whether a name is a file the corpus still has, which is what separates a file
/// that ran to the end from a file upstream deleted. The two want different edits to the list and
/// the same sentence would send somebody looking for a commit that is not there.
#[must_use]
pub fn check(
    expected: &BTreeSet<String>,
    run: &Isolated,
    present: impl Fn(&str) -> bool,
) -> Cutoff {
    let stopped: BTreeSet<&str> = run.stopped.iter().map(|(name, _)| name.as_str()).collect();
    let mut out = Cutoff::default();
    for name in &stopped {
        if !expected.contains(*name) {
            out.appeared.push((*name).to_owned());
        }
    }
    for name in expected {
        if stopped.contains(name.as_str()) {
            out.still.push(name.clone());
        } else if present(name) {
            out.finished.push(name.clone());
        } else {
            out.missing.push(name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Cutoff, check, read};
    use crate::isolate::{Isolated, Stopped};

    /// A run that was cut off on those files.
    fn ran(stopped: &[&str]) -> Isolated {
        Isolated {
            files: 10,
            stopped: stopped
                .iter()
                .map(|name| ((*name).to_owned(), Stopped::Time(Duration::from_secs(120))))
                .collect(),
            ..Isolated::default()
        }
    }

    /// Every name is a file the corpus still has.
    fn everything(_: &str) -> bool {
        true
    }

    #[test]
    fn a_run_that_matches_the_list_is_settled() {
        let expected = read("a.test\nb.test\n");
        let out = check(&expected, &ran(&["a.test", "b.test"]), everything);
        assert!(out.settled(), "{out}");
        assert_eq!(out.still, vec!["a.test", "b.test"]);
        assert!(out.to_string().contains("and no others"), "{out}");
    }

    #[test]
    fn a_file_that_stopped_finishing_is_named() {
        let expected = read("a.test\n");
        let out = check(&expected, &ran(&["a.test", "new.test"]), everything);
        assert!(!out.settled());
        assert_eq!(out.appeared, vec!["new.test"]);
        assert!(out.to_string().contains("so it used to get through"), "{out}");
    }

    /// The direction the box was written for. Somebody made it finish and the run says so.
    #[test]
    fn a_file_that_now_runs_to_the_end_is_named_too() {
        let expected = read("a.test\nb.test\n");
        let out = check(&expected, &ran(&["a.test"]), everything);
        assert!(!out.settled());
        assert_eq!(out.finished, vec!["b.test"]);
        assert!(out.missing.is_empty(), "the corpus still has it, so it was not deleted");
        assert!(out.to_string().contains("ran to the end this time"), "{out}");
    }

    /// A file upstream deleted wants a different edit than a file that got faster, so it gets a
    /// different sentence. Reporting it as finished would send somebody looking for the commit.
    #[test]
    fn a_file_the_corpus_no_longer_has_is_the_vendored_ref_and_not_the_engine() {
        let expected = read("a.test\ngone.test\n");
        let out = check(&expected, &ran(&["a.test"]), |name| name != "gone.test");
        assert!(!out.settled());
        assert_eq!(out.missing, vec!["gone.test"]);
        assert!(out.finished.is_empty());
        assert!(out.to_string().contains("the vendored ref moved"), "{out}");
    }

    /// One run is not evidence, because these files go both ways on their own, and a report that
    /// did not say so would have somebody editing the list off a coin flip.
    #[test]
    fn a_difference_says_that_one_run_is_not_enough_to_edit_the_list_on() {
        let out = check(&read("a.test\nb.test\n"), &ran(&["a.test"]), everything);
        assert!(out.to_string().contains("one run is not that"), "{out}");
    }

    #[test]
    fn the_list_reads_past_comments_and_blank_lines() {
        let expected = read("# why these are here\n\na.test  # a reason\n\n  b.test\n");
        assert_eq!(expected.len(), 2);
        assert!(expected.contains("a.test"), "{expected:?}");
        assert!(expected.contains("b.test"), "{expected:?}");
    }

    #[test]
    fn an_empty_list_and_a_run_that_was_not_cut_off_is_settled() {
        let out = check(&read(""), &ran(&[]), everything);
        assert!(out.settled(), "{out}");
        assert_eq!(out, Cutoff::default());
    }
}
