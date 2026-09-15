//! What the generators reach inside rudb, read out of an `llvm-cov` report.
//!
//! This is feedback for the generators and it is not a published number. Line coverage of an engine
//! says very little about compatibility with another engine, and section 10.5 of
//! `spec/sql/duckdb/10-generation-and-fuzzing.md` says so in as many words. What it is good for is
//! the one question a generator cannot answer about itself: which parts of the binder and the
//! executor is it never reaching, so that a million generated statements do not turn out to be a
//! million variations of the same three code paths.
//!
//! The input is lcov, which is four record types and a blank line, rather than the JSON export.
//! Both come out of the same `llvm-cov export` and lcov needs no parser worth the name, which for a
//! crate with one dependency on purpose is the difference between reading it here and not reading it
//! at all.
//!
//! The rows are grouped by crate because that is the unit a person acts on. A generator that never
//! reaches `rudb-opt` is a generator that writes no query an optimizer would touch, which is a
//! sentence somebody can do something about. A file at 43 percent is not.

use std::collections::BTreeMap;
use std::fmt;

/// How many of the least reached files the page names.
///
/// Enough to be a work list and not so many that it is the whole engine printed in a worse order.
const WORST: usize = 15;

/// One source file of the engine, and how much of it the run reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The path as a reader wants it, which is `rudb-bind/src/binder.rs` and not the absolute path
    /// of a git checkout under a cargo home.
    pub name: String,
    /// Which crate it belongs to.
    pub crate_name: String,
    /// Lines the compiler recorded.
    pub lines: usize,
    /// Lines the run executed at least once.
    pub hit: usize,
}

impl File {
    /// How much of the file the run reached, between zero and one, and zero for an empty file.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.lines == 0 { 0.0 } else { self.hit as f64 / self.lines as f64 }
    }
}

/// One crate of the engine, which is the unit a generator is written against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    /// The package name, `rudb-bind` and so on.
    pub name: String,
    /// Lines over every file of it.
    pub lines: usize,
    /// Lines the run reached.
    pub hit: usize,
    /// Files of it the run never entered at all.
    pub untouched: usize,
}

impl Crate {
    /// How much of the crate the run reached.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.lines == 0 { 0.0 } else { self.hit as f64 / self.lines as f64 }
    }
}

/// What one instrumented run reached.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    files: Vec<File>,
}

impl Reach {
    /// Read an lcov report.
    ///
    /// Four record types matter. `SF` starts a file, `DA` is a line and how many times it ran, `LF`
    /// and `LH` are the file's own totals, and `end_of_record` ends it. The totals are trusted when
    /// they are there and counted from the `DA` lines when they are not, because a writer that emits
    /// one and not the other is common enough that guessing wrong would mean a table of zeroes.
    #[must_use]
    pub fn read(text: &str) -> Self {
        let mut files = Vec::new();
        let mut name = String::new();
        let mut lines = 0;
        let mut hit = 0;
        let mut said = (false, false);
        for line in text.lines() {
            let line = line.trim_end();
            if let Some(path) = line.strip_prefix("SF:") {
                name = path.to_owned();
                lines = 0;
                hit = 0;
                said = (false, false);
            } else if let Some(rest) = line.strip_prefix("DA:") {
                if said.0 {
                    continue;
                }
                let mut parts = rest.split(',');
                let _at = parts.next();
                let ran: u64 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                lines += 1;
                if ran > 0 {
                    hit += 1;
                }
            } else if let Some(rest) = line.strip_prefix("LF:") {
                lines = rest.parse().unwrap_or(lines);
                said.0 = true;
            } else if let Some(rest) = line.strip_prefix("LH:") {
                hit = rest.parse().unwrap_or(hit);
                said.1 = true;
            } else if line == "end_of_record" && !name.is_empty() {
                files.push(File { crate_name: crate_of(&name), name: shorten(&name), lines, hit });
                name = String::new();
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Self { files }
    }

    /// Only the files of the engine, dropping the harness's own.
    ///
    /// The harness is instrumented too because it is the binary being run, and its coverage is not
    /// the question. A generator that never reaches half of `crate::reduce` is a generator, not a
    /// gap in rudb.
    #[must_use]
    pub fn engine(self, ours: &str) -> Self {
        Self { files: self.files.into_iter().filter(|f| !f.name.starts_with(ours)).collect() }
    }

    /// Every file, least reached first, which is the order somebody reads this in.
    #[must_use]
    pub fn files(&self) -> Vec<&File> {
        let mut out: Vec<&File> = self.files.iter().collect();
        out.sort_by(|a, b| a.rate().total_cmp(&b.rate()).then_with(|| b.lines.cmp(&a.lines)));
        out
    }

    /// The files the run never entered at all, which is the list with something in it to do.
    #[must_use]
    pub fn untouched(&self) -> Vec<&File> {
        let mut out: Vec<&File> = self.files.iter().filter(|f| f.hit == 0 && f.lines > 0).collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.lines));
        out
    }

    /// Per crate, least reached first.
    #[must_use]
    pub fn crates(&self) -> Vec<Crate> {
        let mut by_name: BTreeMap<&str, Crate> = BTreeMap::new();
        for file in &self.files {
            let row = by_name.entry(&file.crate_name).or_insert_with(|| Crate {
                name: file.crate_name.clone(),
                lines: 0,
                hit: 0,
                untouched: 0,
            });
            row.lines += file.lines;
            row.hit += file.hit;
            if file.hit == 0 && file.lines > 0 {
                row.untouched += 1;
            }
        }
        let mut out: Vec<Crate> = by_name.into_values().collect();
        out.sort_by(|a, b| a.rate().total_cmp(&b.rate()).then_with(|| b.lines.cmp(&a.lines)));
        out
    }

    /// Lines over everything read.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.files.iter().map(|f| f.lines).sum()
    }

    /// Lines the run reached.
    #[must_use]
    pub fn hit(&self) -> usize {
        self.files.iter().map(|f| f.hit).sum()
    }

    /// How much of everything read the run reached.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.lines() == 0 { 0.0 } else { self.hit() as f64 / self.lines() as f64 }
    }

    /// Whether anything was read at all, so a caller can say the report was empty rather than
    /// printing a table of nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl fmt::Display for Reach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            writeln!(
                f,
                "The report has no engine files in it at all, which is the build rather than the generators. The engine is a dependency here, so it is only instrumented when it is named, and a run that did not name it measures the harness reaching itself."
            )?;
            return Ok(());
        }
        let crates = self.crates();
        writeln!(
            f,
            "{} crates, {} lines, {} of them reached, which is {:.1} percent",
            crates.len(),
            self.lines(),
            self.hit(),
            self.rate() * 100.0
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "    {:<26}{:>9}{:>9}{:>9}{:>8}",
            "crate", "lines", "reached", "percent", "cold"
        )?;
        for one in &crates {
            writeln!(
                f,
                "    {:<26}{:>9}{:>9}{:>8.1}{:>8}",
                fitted(&one.name, 25),
                one.lines,
                one.hit,
                one.rate() * 100.0,
                one.untouched
            )?;
        }
        writeln!(f)?;
        writeln!(
            f,
            "The cold column is files nothing entered at all, which is the column with work in it. A crate at forty percent is a crate the generators are walking into and not finishing, and a file at zero is one they have never once reached."
        )?;
        writeln!(f)?;
        let worst = self.untouched();
        if worst.is_empty() {
            writeln!(
                f,
                "No file of the engine is at zero, which is further than any of this expected to get."
            )?;
        } else {
            writeln!(
                f,
                "the largest {} files nothing entered, of {}",
                WORST.min(worst.len()),
                worst.len()
            )?;
            for file in worst.iter().take(WORST) {
                writeln!(f, "    {:<52}{:>8}", ending(&file.name, 51), file.lines)?;
            }
        }
        writeln!(f)?;
        writeln!(
            f,
            "None of this is a number the project publishes. Line coverage of an engine says very little about whether it matches another engine, and section 10.5 of the generation spec says so. What it is for is the one question a generator cannot answer about itself, which is which parts of the engine it never reaches."
        )
    }
}

/// A name cut to something a column can hold.
fn fitted(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_owned();
    }
    name.chars().take(width.saturating_sub(3)).chain("...".chars()).collect()
}

/// A path cut from the front, because the end of a path is the part that names the file.
fn ending(name: &str, width: usize) -> String {
    let count = name.chars().count();
    if count <= width {
        return name.to_owned();
    }
    let keep = width.saturating_sub(3);
    "...".chars().chain(name.chars().skip(count - keep)).collect()
}

/// Which crate a source path belongs to.
///
/// Both layouts the engine is ever read from have `crates/<name>/src/` in them, the git checkout
/// under a cargo home and a path dependency on a working copy. Anything else keeps its first path
/// segment, which is what the harness's own `src/report.rs` comes out as.
fn crate_of(path: &str) -> String {
    let mut parts = path.split('/');
    while let Some(part) = parts.next() {
        if part == "crates" {
            if let Some(name) = parts.next() {
                return name.to_owned();
            }
        }
    }
    shorten(path).split('/').next().unwrap_or("other").to_owned()
}

/// The path with the machine taken off the front of it.
///
/// A report full of `/home/somebody/.cargo/git/checkouts/rudb-662d7682d9a7f76e/eac9b7c/crates/...`
/// is a report nobody reads twice, and the part that varies between two machines is exactly the
/// part nobody needs.
fn shorten(path: &str) -> String {
    if let Some(at) = path.rfind("/crates/") {
        return path[at + 1..].to_owned();
    }
    // A crate that is not in a `crates` directory, which is what this harness itself is. Keep the
    // directory above `src`, because that is the package name and without it every crate in the
    // report is called `src`.
    if let Some(at) = path.rfind("/src/") {
        let start = path[..at].rfind('/').map_or(0, |cut| cut + 1);
        return path[start..].to_owned();
    }
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

#[cfg(test)]
mod tests {
    use super::Reach;

    const REPORT: &str = "\
SF:/home/x/.cargo/git/checkouts/rudb-66/eac9b7c/crates/rudb-bind/src/binder.rs
DA:1,4
DA:2,0
LF:100
LH:60
end_of_record
SF:/home/x/.cargo/git/checkouts/rudb-66/eac9b7c/crates/rudb-bind/src/scope.rs
LF:40
LH:0
end_of_record
SF:/home/x/.cargo/git/checkouts/rudb-66/eac9b7c/crates/rudb-opt/src/pushdown.rs
LF:60
LH:30
end_of_record
SF:/home/x/gate/rudb-compat/src/report.rs
LF:1000
LH:10
end_of_record
";

    #[test]
    fn a_report_is_read_into_files_with_a_crate_on_each_one() {
        let reach = Reach::read(REPORT);
        assert_eq!(reach.files().len(), 4);
        let binder = reach.files().into_iter().find(|f| f.name.ends_with("binder.rs")).unwrap();
        assert_eq!(binder.crate_name, "rudb-bind");
        assert_eq!(binder.name, "crates/rudb-bind/src/binder.rs");
        assert_eq!((binder.lines, binder.hit), (100, 60));
    }

    #[test]
    fn the_harness_can_be_left_out_of_its_own_measurement() {
        // Instrumenting the binary instruments the harness, and how much of the harness a generator
        // reaches is not a question anybody asked.
        let reach = Reach::read(REPORT).engine("rudb-compat/");
        assert_eq!(reach.files().len(), 3);
        assert_eq!(reach.lines(), 200);
        assert_eq!(reach.hit(), 90);
        assert!((reach.rate() - 0.45).abs() < 1e-9);
    }

    #[test]
    fn the_least_reached_crate_comes_first_because_that_is_the_one_to_act_on() {
        let reach = Reach::read(REPORT).engine("rudb-compat/");
        let crates = reach.crates();
        assert_eq!(crates[0].name, "rudb-bind");
        assert_eq!((crates[0].lines, crates[0].hit, crates[0].untouched), (140, 60, 1));
        assert_eq!(crates[1].name, "rudb-opt");
        assert!(crates[0].rate() < crates[1].rate());
    }

    #[test]
    fn a_file_nothing_entered_is_named_and_an_empty_file_is_not() {
        // A file with no executable lines in it has nothing to reach, so calling it untouched would
        // put a `mod.rs` full of `pub use` at the top of a list of work.
        let text = format!("{REPORT}SF:/x/crates/rudb-ir/src/lib.rs\nLF:0\nLH:0\nend_of_record\n");
        let reach = Reach::read(&text).engine("rudb-compat/");
        let untouched = reach.untouched();
        assert_eq!(untouched.len(), 1);
        assert_eq!(untouched[0].name, "crates/rudb-bind/src/scope.rs");
    }

    #[test]
    fn a_report_with_line_records_and_no_totals_is_counted_rather_than_read_as_zero() {
        // `llvm-cov export -format=lcov` writes both, and a report that came through any other tool
        // may write only the per line records. Counting them is four lines and the alternative is a
        // table of zeroes that looks like a generator that reaches nothing.
        let text = "SF:/x/crates/rudb-exec/src/join.rs\nDA:1,3\nDA:2,0\nDA:3,1\nend_of_record\n";
        let reach = Reach::read(text);
        assert_eq!(reach.lines(), 3);
        assert_eq!(reach.hit(), 2);
    }

    #[test]
    fn the_page_names_the_crate_to_act_on_first_and_the_files_at_zero() {
        let page = Reach::read(REPORT).engine("rudb-compat/").to_string();
        let lines: Vec<&str> = page.lines().collect();
        let first = lines.iter().position(|l| l.contains("rudb-bind")).expect("a row");
        let second = lines.iter().position(|l| l.contains("rudb-opt")).expect("a row");
        assert!(first < second);
        assert!(page.contains("scope.rs"), "{page}");
        assert!(page.contains("45.0 percent"), "{page}");
        assert!(!page.contains("report.rs"), "the harness is not the measurement");
    }

    #[test]
    fn a_page_with_no_engine_in_it_blames_the_build_rather_than_the_generators() {
        // The failure this is about is real and quiet. The engine is a dependency, so it is only
        // instrumented when the build names it, and a run that forgot produces a full report of the
        // harness reaching itself, which looks like a measurement.
        let page = Reach::read("").to_string();
        assert!(page.contains("only instrumented when it is named"), "{page}");
    }

    #[test]
    fn a_report_of_nothing_says_so_rather_than_dividing_by_zero() {
        let reach = Reach::read("");
        assert!(reach.is_empty());
        assert!((reach.rate() - 0.0).abs() < f64::EPSILON);
        assert!(reach.crates().is_empty());
    }
}
