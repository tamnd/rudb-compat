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

use crate::engine::{Cell, Engine, HarnessError, Outcome, Table};
use crate::hash::hash_values;
use crate::slt::{Directive, ParseError, QueryResult, Record, Sort, StatementResult, TestFile};

/// The names this runner answers to in a `skipif` or an `onlyif`.
///
/// `duckdb` is in the list on purpose. The entire claim of this project is that rudb is DuckDB, so
/// a record DuckDB is expected to pass is one rudb is expected to pass, and a record DuckDB is
/// excused from is one rudb is excused from for the same underlying reason. Leaving `duckdb` out
/// would make the corpus run tests that were disabled because they do not work on DuckDB either,
/// and every one of those would be a failure that means nothing.
pub const NAMES: &[&str] = &["rudb", "duckdb"];

/// Why one record did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Which file.
    pub file: String,
    /// Which line the directive was on.
    pub line: usize,
    /// The SQL, as it ran, which for a loop body is the iteration that failed and not the template.
    pub sql: String,
    /// What went wrong, in one or more lines.
    pub reason: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}:{}", self.file, self.line)?;
        for line in self.sql.lines() {
            writeln!(f, "    {line}")?;
        }
        for line in self.reason.lines() {
            writeln!(f, "  {line}")?;
        }
        Ok(())
    }
}

/// Why a whole file was not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// A `require` for something that is not here.
    Requires(String),
    /// The file does not parse, which is a problem with the harness or with the vendoring.
    Unreadable(ParseError),
    /// The file is not text.
    ///
    /// The corpus has a handful of these on purpose, to check what an engine does with a statement
    /// that is not valid UTF-8. Reading one lossily would run a different statement than the file
    /// says and then report on it, so it is skipped and named instead.
    NotText,
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Requires(what) => write!(f, "requires {what}"),
            Self::Unreadable(e) => write!(f, "does not parse, {e}"),
            Self::NotText => f.write_str("is not valid UTF-8"),
        }
    }
}

/// Records that were not attempted, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skips {
    /// A `skipif` or an `onlyif` that does not name this engine.
    pub conditional: usize,
    /// Inside a `mode skip` block, which is how a file turns off a section it knows is broken.
    pub mode: usize,
    /// Behind a directive this runner does not implement, such as `load` or `restart`.
    ///
    /// Every one of these is a record nobody has run, so it is work for the harness rather than
    /// for the engine, and it is the number to watch when the pass rate looks better than it is.
    pub unsupported: usize,
}

impl Skips {
    /// All three together, for a caller that only wants the total.
    #[must_use]
    pub fn total(self) -> usize {
        self.conditional + self.mode + self.unsupported
    }

    /// Add another set of skips to this one.
    fn absorb(&mut self, other: Self) {
        self.conditional += other.conditional;
        self.mode += other.mode;
        self.unsupported += other.unsupported;
    }
}

impl fmt::Display for Skips {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} skipped, of which {} the file turned off, {} in a skipped section and {} behind a directive the runner does not implement",
            self.total(),
            self.conditional,
            self.mode,
            self.unsupported
        )
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

    /// Fold another summary into this one.
    pub fn absorb(&mut self, other: Self) {
        self.files += other.files;
        self.skipped_files.extend(other.skipped_files);
        self.passed += other.passed;
        self.failed += other.failed;
        self.skipped.absorb(other.skipped);
        self.failures.extend(other.failures);
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
    let mut summary = Summary::default();
    for file in &files {
        let name = file.strip_prefix(root).unwrap_or(file).display().to_string();
        let bytes = std::fs::read(file)
            .map_err(|e| HarnessError::new(format!("cannot read {}: {e}", file.display())))?;
        match String::from_utf8(bytes) {
            Ok(text) => summary.absorb(run_text(engine, &name, &text)?),
            Err(_) => {
                summary.files += 1;
                summary.skipped_files.push((name, Skipped::NotText));
            }
        }
    }
    Ok(summary)
}

/// Read one file's text and run it.
///
/// # Errors
///
/// When the engine itself could not be run, which is a broken harness and not a failing test.
pub fn run_text(engine: &mut dyn Engine, name: &str, text: &str) -> Result<Summary, HarnessError> {
    let mut summary = Summary { files: 1, ..Summary::default() };
    let file = match crate::slt::parse(name, text) {
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

/// Run one parsed file against one engine.
///
/// The engine is reset first, so a file starts from an empty database and cannot be made to pass
/// by something an earlier file left behind.
///
/// # Errors
///
/// When the engine itself could not be run.
pub fn run_file(engine: &mut dyn Engine, file: &TestFile) -> Result<Summary, HarnessError> {
    let mut summary = Summary { files: 1, ..Summary::default() };
    engine.reset()?;

    // A `require` anywhere in the file disables the whole file, which is how the format works: the
    // requirement is about the build and not about the record it happens to sit above.
    for record in &file.records {
        if let Directive::Require(what) = &record.directive {
            summary.skipped_files.push((file.name.clone(), Skipped::Requires(what.clone())));
            return Ok(summary);
        }
    }

    // `mode skip` turns everything off until `mode unskip`, which is how a file marks a section
    // that is known not to work without deleting it.
    let mut skipping = false;
    let mut labels: HashMap<String, Vec<String>> = HashMap::new();

    for record in &file.records {
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
        if matches!(record.directive, Directive::HashThreshold(_)) {
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
        if let Directive::Unsupported(_) = &record.directive {
            summary.skipped.unsupported += 1;
            continue;
        }
        match check(engine, file, record, &mut labels)? {
            Ok(()) => summary.passed += 1,
            Err(failure) => {
                summary.failed += 1;
                summary.failures.push(failure);
            }
        }
    }

    Ok(summary)
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
) -> Result<Result<(), Failure>, HarnessError> {
    let fail = |sql: &str, reason: String| {
        Err(Failure { file: file.name.clone(), line: record.line, sql: sql.to_owned(), reason })
    };

    match &record.directive {
        Directive::Statement { expected, sql } => {
            let outcome = engine.run(sql)?;
            Ok(match (expected, &outcome) {
                (StatementResult::Ok, Outcome::Rows(_)) | (StatementResult::Maybe, _) => Ok(()),
                (StatementResult::Ok, Outcome::Error(e)) => {
                    fail(sql, format!("expected it to work, and it said\n{e}"))
                }
                (StatementResult::Error(None), Outcome::Error(_)) => Ok(()),
                (StatementResult::Error(Some(wanted)), Outcome::Error(e)) => {
                    if contains(e, wanted) {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            format!("expected an error containing\n{wanted}\nand it said\n{e}"),
                        )
                    }
                }
                (StatementResult::Error(_), Outcome::Rows(_)) => {
                    fail(sql, "expected it to fail, and it worked".to_owned())
                }
            })
        }
        Directive::Query { types, sort, label, sql, expected } => {
            let outcome = engine.run(sql)?;
            let table = match (&outcome, expected) {
                (Outcome::Error(_), QueryResult::Error(None)) => return Ok(Ok(())),
                (Outcome::Error(e), QueryResult::Error(Some(wanted))) => {
                    return Ok(if contains(e, wanted) {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            format!("expected an error containing\n{wanted}\nand it said\n{e}"),
                        )
                    });
                }
                (Outcome::Error(e), _) => {
                    return Ok(fail(sql, format!("expected rows, and it said\n{e}")));
                }
                (Outcome::Rows(_), QueryResult::Error(_)) => {
                    return Ok(fail(sql, "expected it to fail, and it returned rows".to_owned()));
                }
                (Outcome::Rows(table), _) => table,
            };

            let width = types.chars().count();
            if table.width() != width {
                return Ok(fail(
                    sql,
                    format!("expected {width} columns and got {}", table.width()),
                ));
            }
            let values = flatten(table, types, *sort);

            if !label.is_empty() {
                if let Some(previous) = labels.get(label) {
                    if previous != &values {
                        return Ok(fail(
                            sql,
                            format!(
                                "this is labelled {label} and does not match what the earlier query with that label returned"
                            ),
                        ));
                    }
                } else {
                    labels.insert(label.clone(), values.clone());
                }
            }

            Ok(match expected {
                QueryResult::Values(wanted) => {
                    if &values == wanted {
                        Ok(())
                    } else {
                        fail(sql, difference(wanted, &values, width))
                    }
                }
                QueryResult::Hash { count, digest } => {
                    if values.len() != *count {
                        fail(sql, format!("expected {count} values and got {}", values.len()))
                    } else if &hash_values(&values) == digest {
                        Ok(())
                    } else {
                        fail(
                            sql,
                            format!(
                                "expected {count} values hashing to {digest} and got {}",
                                hash_values(&values)
                            ),
                        )
                    }
                }
                QueryResult::Error(_) => unreachable!("handled above"),
            })
        }
        Directive::Halt
        | Directive::HashThreshold(_)
        | Directive::Require(_)
        | Directive::Mode(_)
        | Directive::Unsupported(_) => Ok(Ok(())),
    }
}

/// Whether an engine's error is the one the file asked for.
///
/// A substring match on the message and not an equality, because that is what the format means and
/// because the corpus writes short fragments like `Conversion Error` where the real message is a
/// paragraph. The kind is included in the text being searched, so a file can pin either.
fn contains(error: &crate::engine::EngineError, wanted: &str) -> bool {
    let full = format!("{}: {}", error.kind, error.message);
    let wanted = wanted.trim();
    // The corpus writes an expected error over several lines when the real one has several lines,
    // and the leading whitespace on the continuations is not part of the claim.
    wanted.lines().map(str::trim).filter(|line| !line.is_empty()).all(|line| full.contains(line))
}

/// Turn a result set into the flat list of values the format compares.
///
/// Row by row, left to right, each value rendered by the letter its column was declared with, then
/// sorted if the record asked for it.
#[must_use]
pub fn flatten(table: &Table, types: &str, sort: Sort) -> Vec<String> {
    let letters: Vec<char> = types.chars().collect();
    let mut rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(at, cell)| render(cell, letters.get(at).copied().unwrap_or('T')))
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

/// Render one value the way the format's column letter says to.
///
/// The letters are `T` for text, `I` for an integer and `R` for a real. They are a rendering
/// instruction and not a type assertion: a column declared `I` that comes back as a string of
/// digits is fine, and the same column coming back as `2.5` is rendered `2`, which is what
/// sqllogictest has always done and what the expected values in the corpus were written against.
///
/// A value the letter does not fit comes through as itself rather than as a zero. The point of a
/// failure report is to say what the engine actually returned, and a lie in the rendering is a
/// failure report that sends somebody looking in the wrong place.
#[must_use]
pub fn render(cell: &Cell, letter: char) -> String {
    let text = match cell {
        Cell::Null => return "NULL".to_owned(),
        Cell::Text(text) => text.as_str(),
    };
    match letter {
        'I' => {
            if let Ok(n) = text.parse::<i128>() {
                return n.to_string();
            }
            if let Ok(n) = text.parse::<f64>() {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the truncation is the rendering rule, not an accident"
                )]
                return (n.trunc() as i64).to_string();
            }
            match text {
                "true" => "1".to_owned(),
                "false" => "0".to_owned(),
                other => other.to_owned(),
            }
        }
        'R' => match text.parse::<f64>() {
            Ok(n) => format!("{n:.3}"),
            Err(_) => text.to_owned(),
        },
        // An empty string and a null are different values and the format has to be able to tell
        // them apart on a line of their own, so the empty one is written out.
        _ if text.is_empty() => "(empty)".to_owned(),
        _ => text.to_owned(),
    }
}

/// A readable account of how two lists of values differ.
///
/// Printed as rows rather than as a flat list, because a result that is off by one column reads as
/// every value being wrong when it is printed flat, and reads as one missing column when it is
/// printed in rows.
fn difference(wanted: &[String], got: &[String], width: usize) -> String {
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
        let mark = if left == right { ' ' } else { '*' };
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
    use super::{NAMES, Skips, Summary, flatten, render, run_text};
    use crate::engine::{Cell, Column, Engine, HarnessError, Outcome, Table};
    use crate::slt::{Condition, Sort};

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
    fn a_query_that_returns_something_else_fails_and_the_report_shows_both() {
        let answers = vec![Outcome::Rows(table(1, &["1", "3"]))];
        let summary = run(answers, "query I\nSELECT a FROM t\n----\n1\n2\n");
        assert_eq!(summary.failed, 1);
        let reason = &summary.failures[0].reason;
        assert!(reason.contains('2'), "{reason}");
        assert!(reason.contains('3'), "{reason}");
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
        assert!(summary.failures[0].reason.contains("columns"));
    }

    #[test]
    fn a_statement_that_has_to_fail_and_does_not_is_a_failure() {
        let answers = vec![Outcome::Rows(Table::default())];
        let summary = run(answers, "statement error\nDROP TABLE nope\n");
        assert_eq!(summary.failed, 1);
    }

    #[test]
    fn an_expected_error_is_matched_as_a_fragment_of_the_real_one() {
        let answers = vec![Outcome::Error(crate::engine::EngineError {
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
    fn a_file_that_requires_something_is_skipped_whole() {
        let summary = run(Vec::new(), "require parquet\n\nstatement ok\nSELECT 1\n");
        assert_eq!(summary.skipped_files.len(), 1);
        assert_eq!(summary.attempted(), 0);
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
    fn the_column_letter_decides_how_a_value_is_written_before_it_is_compared() {
        assert_eq!(render(&Cell::Text("2.5".to_owned()), 'I'), "2");
        assert_eq!(render(&Cell::Text("2.5".to_owned()), 'R'), "2.500");
        assert_eq!(render(&Cell::Text("2.5".to_owned()), 'T'), "2.5");
        assert_eq!(render(&Cell::Null, 'T'), "NULL");
        assert_eq!(render(&Cell::Text(String::new()), 'T'), "(empty)");
    }

    #[test]
    fn a_value_the_letter_does_not_fit_comes_through_as_itself() {
        assert_eq!(render(&Cell::Text("banana".to_owned()), 'I'), "banana");
        assert_eq!(render(&Cell::Text("banana".to_owned()), 'R'), "banana");
    }

    #[test]
    fn valuesort_loses_which_row_a_value_came_from_and_rowsort_does_not() {
        let table = table(2, &["2", "9", "1", "8"]);
        assert_eq!(flatten(&table, "II", Sort::RowSort), ["1", "8", "2", "9"]);
        assert_eq!(flatten(&table, "II", Sort::ValueSort), ["1", "2", "8", "9"]);
    }
}
