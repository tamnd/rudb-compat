//! What a generated run has to leave behind for anybody to get it back.
//!
//! Four modes here write their own input: `sqlsmith`, `grammar`, `tlp` and `norec`. Each of them
//! already prints its seed, which is most of the way there and is not all of it. A seed on its own
//! reproduces a run against the rudb, the DuckDB and the generator that were on the machine at the
//! time, and every one of those moves. Somebody reading a finding three weeks later needs to know
//! which three things it was a finding about, and somebody reading two runs needs to know whether
//! the difference between them is the engine or the binary or the corpus.
//!
//! So a generated run prints the six fields from section 11.2 of
//! `spec/sql/duckdb/11-the-number.md` and the command that reproduces it, and appends one row to a
//! series beside the corpus pages. The six are the rudb commit, this harness's commit, the DuckDB
//! commit with the hash of the actual binary, the corpus commit, the machine, and the seed. Most of
//! that is already gathered for the report page by [`crate::report::Provenance`], which is why this
//! module is short: what it adds is the seed row said properly and the shape of a generated run.
//!
//! # Why the command rather than the seed
//!
//! Because the seed is not enough to name the run and a reader should not have to work out what the
//! rest of it was. A grammar run is a seed and a count and a rule, a TLP run is a seed and a count
//! and a form, and a sqlsmith run is a seed and a count and which binary generated it. Printing the
//! command line puts all of that in one line somebody can paste, and it means the replay
//! instructions cannot drift away from the flags the mode actually takes, because they are built
//! out of the same values the run used.
//!
//! # Why one series for all four
//!
//! Because the column that matters is the same in all four: how many cases were generated, how many
//! of them said anything, and how many of those came out wrong. The modes disagree about what a
//! case is and about what wrong means, and that is what the mode column is for. Four files would
//! make a reader open four of them to answer whether generated testing found more this month than
//! last, and that question is the reason the series exists.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::engine::HarnessError;
use crate::report::{Provenance, append};

/// The file every generated run appends a row to, beside the corpus series.
///
/// A file of its own rather than more columns on [`crate::report::SERIES`], for the same reason the
/// coverage sweep has one: a corpus run and a generated run happen at different times and measure
/// different denominators, and a row with most of its columns empty is a row that makes both harder
/// to read.
pub const GENERATED: &str = "generated.tsv";

/// The columns of [`GENERATED`], in order.
pub const COLUMNS: &[&str] = &[
    "when",
    "host",
    "mode",
    "shape",
    "engines",
    "rudb",
    "rudb commit",
    "compat commit",
    "duckdb",
    "seed",
    "cases",
    "usable",
    "findings",
    "groups",
    "command",
];

/// One generated run, in the shape all four modes have in common.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Which mode ran, spelled the way it is typed.
    pub mode: &'static str,
    /// The rule, the form, or whatever else picks what the mode generates.
    pub shape: Shape,
    /// The first seed, which with the count names the run.
    pub seed: u64,
    /// How many cases were generated.
    pub cases: usize,
    /// How many of them said anything about rudb at all.
    ///
    /// Every mode here throws some cases away and they are not the same cases. A query the pinned
    /// binary cannot run says nothing about rudb, a statement neither parser accepts says nothing
    /// about either, and a predicate the engine refused says nothing either way. The rate worth
    /// reading is over this rather than over [`Run::cases`].
    pub usable: usize,
    /// How many of the usable ones came out wrong.
    pub findings: usize,
    /// How many distinct shapes those came apart into, which is the number of things to fix.
    ///
    /// The two differential modes group their findings by what the engine that refused said, so this
    /// is smaller than the count beside it and is the more useful of the two. The two metamorphic
    /// oracles do not group at all, because what they report is a predicate whose parts did not add
    /// up and there is nothing to group it by until the reducer has been over it, so they report the
    /// same number twice and the series says so by having both columns.
    pub groups: usize,
    /// What the run was put to, since two of these modes need no DuckDB on the machine.
    pub engines: &'static str,
}

/// What a mode was pointed at, when it takes a choice about that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// The mode generates one thing and takes no choice about it.
    Only,
    /// The flag and the value, as they would be typed.
    Flag(&'static str, String),
}

/// Both engines, which is the default for the two differential modes.
pub const BOTH: &str = "the pinned duckdb and rudb";

/// rudb on its own, which the two metamorphic oracles need.
pub const ALONE: &str = "rudb alone, no duckdb needed";

/// The pinned binary on its own, which is what `--pinned` does to the two oracles.
///
/// That run checks the oracle rather than the engine. A property that fails on DuckDB is a property
/// this harness has written down wrong, or a DuckDB bug, and either way it is not a rudb finding, so
/// the two have to be told apart in the series.
pub const PINNED: &str = "the pinned duckdb alone, which checks the oracle";

impl Run {
    /// The command line that runs this again.
    ///
    /// Built out of the values the run used rather than written down beside them, so it cannot come
    /// to disagree with them. `--count` and `--seed` are always given even when they were the
    /// defaults, because a default is a thing that changes and a recorded command should keep
    /// working after it does.
    #[must_use]
    pub fn command(&self) -> String {
        let mut line = format!("rudb-compat {}", self.mode);
        if let Shape::Flag(flag, value) = &self.shape {
            line.push_str(&format!(" {flag} {value}"));
        }
        line.push_str(&format!(" --count {} --seed {}", self.cases, self.seed));
        line
    }

    /// What the shape is called on the page, when it has one.
    #[must_use]
    pub fn shape(&self) -> String {
        match &self.shape {
            Shape::Only => "none, this mode generates one thing".to_owned(),
            Shape::Flag(_, value) => value.clone(),
        }
    }

    /// The share of usable cases that came out wrong, or none when nothing was usable.
    ///
    /// A run where nothing was usable is not a run with a zero percent failure rate, and the two
    /// have to print differently or a broken run reads like a clean one.
    #[must_use]
    pub fn rate(&self) -> Option<f64> {
        if self.usable == 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "counts of generated cases, nowhere near the mantissa"
        )]
        Some(self.findings as f64 / self.usable as f64)
    }
}

/// Everything about the machine and the three moving parts, with the seed row said properly.
///
/// The corpus rows are set to say they do not apply rather than left saying unknown, because a
/// generated run reading no corpus and a corpus run that could not find one are different things
/// and a page has to be able to tell them apart.
#[must_use]
pub fn provenance(root: &Path, rudb: &str, run: &Run) -> Provenance {
    let mut p = Provenance::of_machine(root, rudb);
    p.corpus_path = "none, this run wrote its own cases".to_owned();
    p.seed = format!("{}, and the run replays with: {}", run.seed, run.command());
    p
}

/// The block a generated run prints under what it found.
///
/// Under rather than over, which looks backwards and is not. Three of the six fields are counts the
/// run does not know until it is over, and a reader who has just scrolled through two hundred
/// groups wants the command that reproduces them to be the last thing on the screen rather than the
/// thing they have to scroll back up past the groups to find.
#[derive(Debug)]
pub struct Note<'a> {
    run: &'a Run,
    provenance: &'a Provenance,
}

impl<'a> Note<'a> {
    /// Point a note at a run and what was gathered about the machine it ran on.
    #[must_use]
    pub const fn of(run: &'a Run, provenance: &'a Provenance) -> Self {
        Self { run, provenance }
    }
}

impl fmt::Display for Note<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.provenance;
        writeln!(f, "{} on {}, {}", self.run.mode, p.host, p.stamp)?;
        writeln!(f, "put to {}", self.run.engines)?;
        writeln!(f)?;
        writeln!(f, "  rudb           {} at {}", p.rudb, p.rudb_commit)?;
        writeln!(f, "  harness        {}", p.compat_commit)?;
        writeln!(f, "  duckdb         {} md5 {}", p.duckdb_version, p.duckdb_hash)?;
        writeln!(f, "  pinned         {}", p.duckdb_pinned)?;
        writeln!(f, "  corpus         {}", p.corpus_path)?;
        writeln!(f, "  machine        {}", p.platform)?;
        writeln!(f, "  seed           {}", self.run.seed)?;
        writeln!(f, "  replay with    {}", self.run.command())?;
        Ok(())
    }
}

/// The header line of the generated series.
#[must_use]
pub fn header() -> String {
    COLUMNS.join("\t")
}

/// One generated run as a row of that file.
///
/// The same fields in the same order as [`COLUMNS`], which a test checks, because a row that has
/// drifted from its header is worse than no row at all.
#[must_use]
pub fn row(run: &Run, p: &Provenance) -> String {
    let fields = [
        p.stamp.clone(),
        p.host.clone(),
        run.mode.to_owned(),
        run.shape(),
        run.engines.to_owned(),
        p.rudb.clone(),
        p.rudb_commit.clone(),
        p.compat_commit.clone(),
        p.duckdb_version.clone(),
        run.seed.to_string(),
        run.cases.to_string(),
        run.usable.to_string(),
        run.findings.to_string(),
        run.groups.to_string(),
        run.command(),
    ];
    fields.join("\t")
}

/// Append one generated run to the series, and say which file it went in.
///
/// No page of its own. What a generated run has to say beyond these counts is the findings
/// themselves, and those go to whoever ran it, in full, because a group with an example in it is
/// something to act on and a group in a file nobody opens is not.
///
/// # Errors
///
/// When the directory cannot be made or the file cannot be appended to.
pub fn record(dir: &Path, run: &Run, p: &Provenance) -> Result<PathBuf, HarnessError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| HarnessError::new(format!("cannot make {}: {e}", dir.display())))?;
    let file = dir.join(GENERATED);
    append(&file, &header(), &row(run, p))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::{ALONE, BOTH, COLUMNS, Run, Shape, header, row};
    use crate::report::Provenance;
    use std::path::Path;

    fn run() -> Run {
        Run {
            mode: "grammar",
            shape: Shape::Flag("--rule", "SelectStatement".to_owned()),
            seed: 1,
            cases: 2000,
            usable: 1738,
            findings: 1264,
            groups: 159,
            engines: BOTH,
        }
    }

    #[test]
    fn the_command_it_prints_is_the_run_it_recorded() {
        assert_eq!(
            run().command(),
            "rudb-compat grammar --rule SelectStatement --count 2000 --seed 1"
        );
    }

    #[test]
    fn a_mode_with_nothing_to_choose_prints_no_flag() {
        let one = Run { mode: "norec", shape: Shape::Only, engines: ALONE, ..run() };
        assert_eq!(one.command(), "rudb-compat norec --count 2000 --seed 1");
    }

    #[test]
    fn a_run_where_nothing_was_usable_has_no_rate_rather_than_a_rate_of_zero() {
        let empty = Run { usable: 0, findings: 0, ..run() };
        assert!(empty.rate().is_none());
        assert!((run().rate().expect("usable cases") - 0.727_273).abs() < 0.000_01);
    }

    #[test]
    fn the_row_has_one_field_for_every_column() {
        // The reason this is a test rather than a comment. The header is written once and the row is
        // built by hand beside it, so the only thing stopping the two drifting apart is a check that
        // counts them.
        let p = Provenance::of_machine(Path::new("."), "rudb 0.0.0");
        assert_eq!(row(&run(), &p).split('\t').count(), COLUMNS.len());
        assert_eq!(header().split('\t').count(), COLUMNS.len());
    }

    #[test]
    fn the_seed_row_says_how_to_get_the_run_back() {
        let p = super::provenance(Path::new("."), "rudb 0.0.0", &run());
        assert!(p.seed.contains("--seed 1"), "{}", p.seed);
        assert!(p.seed.contains("--rule SelectStatement"), "{}", p.seed);
        // And the corpus rows say they do not apply rather than saying unknown, which is what a
        // corpus run that could not find its corpus says.
        assert!(p.corpus_path.contains("wrote its own"), "{}", p.corpus_path);
    }
}
