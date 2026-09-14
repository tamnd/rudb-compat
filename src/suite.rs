//! Running a list of statements through two engines and writing down what happened.
//!
//! This is the smallest thing that is still the real shape. There is no reducer, no bisector and
//! no generator, and all three are named in `spec/14-rudb-compat.md` as the parts that make the
//! output usable at volume. What is here is the loop they all hang off, and the report they all
//! write into, because getting those two wrong is expensive later and cheap now.

use crate::compare::{Difference, MessageMatch, Rules, compare, compare_acceptance};
use crate::engine::{Engine, HarnessError, Outcome};
use crate::resource::{FLOOR, RUNS, Ratios, Usage, middle};
use crate::rudb::ordering_of;

/// One statement's worth of comparison.
#[derive(Debug, Clone)]
pub struct Case {
    /// The statement, as written.
    pub sql: String,
    /// Everything the two engines disagreed about. Empty means they agreed.
    pub differences: Vec<Difference>,
    /// What the statement cost on the left engine and then on the right one, when it was a record
    /// worth timing at all.
    ///
    /// Nothing for most records and that is expected rather than a gap. A record is timed only
    /// when the run asked for timing, both engines returned rows, they agreed, both sides reported
    /// a cost, and at least one of them was above [`FLOOR`]. Every one of those conditions is in
    /// `spec/sql/duckdb/09-the-harness.md` section 9.7 and each of them exists because dropping it
    /// puts a number on the page that measures something other than the engine.
    pub cost: Option<(Usage, Usage)>,
}

impl Case {
    /// True when the two engines agreed on everything the rules asked about.
    #[must_use]
    pub fn agreed(&self) -> bool {
        self.differences.is_empty()
    }
}

/// Whether a run also measures what each record cost.
///
/// Off by default, because measuring costs a fork per statement per repeat and most runs are
/// looking for a wrong answer rather than for a slow one. On when the run is the one that feeds
/// the report page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Measure {
    /// Answers only.
    #[default]
    Off,
    /// Answers, and the cost of every record that is worth timing.
    On,
}

/// Which engine in a run is ours.
///
/// The published ratio is rudb over DuckDB and a run does not otherwise care which side is which,
/// so this is worked out once from the engine's own name and carried, rather than guessed at again
/// wherever a number is printed. A run of two engines that are neither of them rudb, which is a
/// DuckDB against a DuckDB and is a real thing to do when checking the harness itself, has no side
/// and publishes no ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ours {
    /// The left engine.
    Left,
    /// The right engine.
    Right,
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct Report {
    /// The left engine's name and version, for the header.
    pub left: String,
    /// The right engine's name and version.
    pub right: String,
    /// Which side is rudb, when either of them is.
    pub ours: Option<Ours>,
    /// Every case, in the order they ran.
    pub cases: Vec<Case>,
}

impl Report {
    /// How many cases agreed.
    #[must_use]
    pub fn agreed(&self) -> usize {
        self.cases.iter().filter(|c| c.agreed()).count()
    }

    /// The share of cases that agreed, between zero and one.
    ///
    /// This is not the compatibility percentage in `spec/10-sql-and-types.md` section 10.7 and it
    /// must not be presented as one. That number is weighted by how much each construct is
    /// actually used, and this one weights `SELECT 1` and a seven way join the same. It is the
    /// number a developer watches while working, not a number that gets published.
    #[must_use]
    pub fn share(&self) -> f64 {
        if self.cases.is_empty() {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a corpus large enough for this to matter would not fit on the machine"
        )]
        {
            self.agreed() as f64 / self.cases.len() as f64
        }
    }

    /// The three resource ratios over this run, rudb over DuckDB, or nothing when there are none.
    ///
    /// Nothing is a normal answer and it means one of three things, all of them worth telling
    /// apart from a ratio of zero. The run did not ask to be measured. The machine has no GNU
    /// time, so `crate::resource` measured nothing. Or every record was an error, a disagreement,
    /// or too fast to be worth dividing, which is most of a corpus written to test semantics.
    ///
    /// The goal is [`Ratios::GOAL`] on all three, which is ten times the speed and a tenth of the
    /// memory. The per milestone gate is weaker than the goal and gets stricter as the milestones
    /// go on, and it lives in `spec/sql/duckdb/12-the-order-of-work.md` rather than here, because
    /// what this function owes the reader is the number rather than an opinion about it.
    #[must_use]
    pub fn ratios(&self) -> Option<Ratios> {
        let ours = self.ours?;
        let pairs: Vec<(Usage, Usage)> = self
            .cases
            .iter()
            .filter_map(|case| case.cost)
            .map(|(left, right)| match ours {
                Ours::Left => (left, right),
                Ours::Right => (right, left),
            })
            .collect();
        Ratios::of(&pairs)
    }

    /// The records this run measured, worst first by wall clock ratio.
    ///
    /// The column that gets read on the report page, per
    /// `spec/sql/duckdb/11-the-number.md` section 11.2. An engine that is fast on the median and
    /// two hundred times slower on one shape has a bug rather than a distribution, and the median
    /// is exactly the statistic that hides it.
    #[must_use]
    pub fn worst(&self, how_many: usize) -> Vec<(&str, f64)> {
        let Some(ours) = self.ours else { return Vec::new() };
        let mut rows: Vec<(&str, f64)> = self
            .cases
            .iter()
            .filter_map(|case| {
                let (left, right) = case.cost?;
                let (ours, theirs) = if ours == Ours::Left { (left, right) } else { (right, left) };
                if theirs.wall.is_zero() {
                    return None;
                }
                Some((case.sql.as_str(), ours.wall.as_secs_f64() / theirs.wall.as_secs_f64()))
            })
            .collect();
        rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        rows.truncate(how_many);
        rows
    }
}

/// Whether somebody asked to watch a run happen, which is `RUDB_COMPAT_PROGRESS` set to anything
/// other than `0`.
///
/// A run prints nothing until it is over, which is right for a gate and wrong for the one run this
/// project most needs to watch. The full ClickBench file is a hundred million rows through forty
/// three queries twice and it takes hours, and a query that gets the process killed by the kernel
/// takes the report with it, so afterwards there is no way to say which query it died on. One line
/// per statement as it finishes answers that. Off by default, because a gate should say one thing.
#[must_use]
pub fn watching() -> bool {
    std::env::var_os("RUDB_COMPAT_PROGRESS").is_some_and(|on| on != "0")
}

/// Run every statement through both engines.
///
/// The ordering rule is decided per statement rather than for the run, because it depends on
/// whether that statement said what its order is. The message rule is the run's, because how
/// closely error text has to match is a policy and not a property of the query.
///
/// # Errors
///
/// When either engine could not be run at all, which is a broken harness and not a failing case.
pub fn run(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    statements: &[String],
    messages: MessageMatch,
    measure: Measure,
) -> Result<Report, HarnessError> {
    let watching = watching();
    let ours = side(left.name(), right.name());
    let mut cases = Vec::with_capacity(statements.len());
    for (at, sql) in statements.iter().enumerate() {
        let rules = Rules { ordering: ordering_of(sql), messages };
        let started = std::time::Instant::now();
        let a = left.run(sql)?;
        let ours = left.usage();
        let between = std::time::Instant::now();
        let b = right.run(sql)?;
        let theirs = right.usage();
        let differences = compare(&a, &b, rules);
        let cost = if measure == Measure::On && worth_timing(&a, &b, &differences, ours, theirs) {
            repeat(left, right, sql, ours, theirs)?
        } else {
            None
        };
        if watching {
            eprintln!(
                "{} of {}, {} {:.1?}, {} {:.1?}, {}",
                at + 1,
                statements.len(),
                left.name(),
                between - started,
                right.name(),
                between.elapsed(),
                if differences.is_empty() { "agreed" } else { "differed" }
            );
        }
        cases.push(Case { sql: sql.clone(), differences, cost });
    }
    Ok(Report {
        left: format!("{} {}", left.name(), left.version()),
        right: format!("{} {}", right.name(), right.version()),
        ours,
        cases,
    })
}

/// Which side is rudb, by what the engine calls itself.
///
/// Both rudb drivers name themselves starting with `rudb` and both DuckDB drivers with `duckdb`,
/// and those names are already in the report header, so reading them here does not add a way for
/// the report to be wrong that was not already there. Two engines with the same answer to this,
/// which is two DuckDBs or two rudbs, have no side.
fn side(left: &str, right: &str) -> Option<Ours> {
    let ours = |name: &str| name.starts_with("rudb");
    match (ours(left), ours(right)) {
        (true, false) => Some(Ours::Left),
        (false, true) => Some(Ours::Right),
        _ => None,
    }
}

/// Whether this record's cost is worth writing down.
///
/// Five conditions and every one of them is an exclusion from
/// `spec/sql/duckdb/09-the-harness.md` section 9.7, written here once rather than applied by feel
/// wherever a number is read. Both engines returned rows, because timing an error path measures
/// the error path. They agreed, because a ratio between a right answer and a wrong one compares
/// two different amounts of work. Both sides reported a cost, because this machine may not be able
/// to measure at all. And at least one side was above the floor, because below it the two numbers
/// are mostly the cost of starting a shell.
fn worth_timing(
    a: &Outcome,
    b: &Outcome,
    differences: &[Difference],
    ours: Option<Usage>,
    theirs: Option<Usage>,
) -> bool {
    if !differences.is_empty() {
        return false;
    }
    if !matches!(a, Outcome::Rows(_)) || !matches!(b, Outcome::Rows(_)) {
        return false;
    }
    let (Some(ours), Some(theirs)) = (ours, theirs) else { return false };
    ours.wall >= FLOOR || theirs.wall >= FLOOR
}

/// Run a record enough times that its number is a median rather than a sample of one.
///
/// The first run already happened and is counted, so this is the rest of them. Only candidate
/// records get here, which is why running everything five times is not what this costs.
fn repeat(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    sql: &str,
    ours: Option<Usage>,
    theirs: Option<Usage>,
) -> Result<Option<(Usage, Usage)>, HarnessError> {
    let mut lefts: Vec<Usage> = ours.into_iter().collect();
    let mut rights: Vec<Usage> = theirs.into_iter().collect();
    for _ in 1..RUNS {
        left.run(sql)?;
        right.run(sql)?;
        if let Some(one) = left.usage() {
            lefts.push(one);
        }
        if let Some(one) = right.usage() {
            rights.push(one);
        }
    }
    Ok(middle(&lefts).zip(middle(&rights)))
}

/// Ask both engines about every statement without running any of them.
///
/// This is the mode that works today and it is not a placeholder for the one that does not. The
/// dialect is the compatibility claim, the grammar is vendored from DuckDB precisely so that the
/// dialect cannot drift, and this is the check that the vendoring worked. It stays useful after
/// there is an executor, because a query that fails to parse and a query that returns the wrong
/// answer are different bugs and mixing them in one number hides both.
///
/// # Errors
///
/// When either engine could not be run at all.
pub fn run_parse(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    statements: &[String],
    messages: MessageMatch,
) -> Result<Report, HarnessError> {
    let mut cases = Vec::with_capacity(statements.len());
    for sql in statements {
        let a = left.accepts(sql)?;
        let b = right.accepts(sql)?;
        cases.push(Case {
            sql: sql.clone(),
            differences: compare_acceptance(&a, &b, messages),
            // Parsing is not work, so there is nothing here worth timing. A parser that is slower
            // than DuckDB's matters and it is measured on statements that run, not on this mode,
            // where what would be timed is two process startups with a parse between them.
            cost: None,
        });
    }
    Ok(Report {
        left: format!("{} {}", left.name(), left.version()),
        right: format!("{} {}", right.name(), right.version()),
        ours: side(left.name(), right.name()),
        cases,
    })
}

/// Split a file of SQL into statements.
///
/// The split is on the tokenizer's statement terminator rather than on a `;` in the text, so a
/// semicolon inside a string literal, a dollar quoted block or a comment does not end a statement.
/// That distinction is not hypothetical for a corpus that contains any string at all, and the
/// tokenizer that answers it is the same one the parser uses, so the corpus is split the way the
/// engine would split it.
///
/// [`rudb::split`] is that same tokenizer, reached through the embedding API rather than through
/// `rudb-parse`, and it keeps the two things this harness needs that a plain `;` split does not:
/// text that does not tokenize comes back as one statement, because the harness's job is to hand
/// it to both engines and see what they say rather than to decide in advance that it is not SQL,
/// and a trailing block of comments is not a statement, because handing that to an engine gets an
/// error that is about the harness rather than about the corpus.
#[must_use]
pub fn statements(text: &str) -> Vec<String> {
    rudb::split(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Measure, Ours, Report, side, statements, worth_timing};
    use crate::compare::Difference;
    use crate::engine::{Outcome, Table};
    use crate::resource::{FLOOR, Usage};
    use crate::suite::Case;
    use std::time::Duration;

    fn case(sql: &str, agreed: bool) -> Case {
        Case {
            sql: sql.to_owned(),
            differences: if agreed {
                Vec::new()
            } else {
                vec![Difference::Height { left: 1, right: 0 }]
            },
            cost: None,
        }
    }

    fn timed(sql: &str, ours_ms: u64, theirs_ms: u64) -> Case {
        let usage = |ms| Usage {
            wall: Duration::from_millis(ms),
            cpu: Duration::from_millis(ms),
            peak: ms * 1024 * 1024,
        };
        Case {
            sql: sql.to_owned(),
            differences: Vec::new(),
            cost: Some((usage(ours_ms), usage(theirs_ms))),
        }
    }

    fn slow() -> Usage {
        Usage { wall: FLOOR * 2, cpu: FLOOR * 2, peak: 1024 }
    }

    fn rows() -> Outcome {
        Outcome::Rows(Table::default())
    }

    #[test]
    fn a_semicolon_ends_a_statement_and_the_last_one_needs_none() {
        let got = statements("SELECT 1; SELECT 2");
        assert_eq!(got, vec!["SELECT 1".to_owned(), "SELECT 2".to_owned()]);
    }

    #[test]
    fn a_semicolon_inside_a_string_does_not_end_a_statement() {
        let got = statements("SELECT ';'; SELECT 2");
        assert_eq!(got, vec!["SELECT ';'".to_owned(), "SELECT 2".to_owned()]);
    }

    #[test]
    fn a_semicolon_inside_a_comment_does_not_end_a_statement() {
        let got = statements("SELECT 1 -- one; two\n; SELECT 2");
        assert_eq!(got.len(), 2);
        assert!(got[0].starts_with("SELECT 1"));
    }

    #[test]
    fn a_trailing_semicolon_does_not_produce_an_empty_statement() {
        assert_eq!(statements("SELECT 1;\n\n"), vec!["SELECT 1".to_owned()]);
    }

    #[test]
    fn a_file_of_only_comments_has_no_statements_in_it() {
        assert!(statements("-- nothing here\n").is_empty());
    }

    #[test]
    fn the_share_is_the_cases_that_agreed_and_nothing_cleverer() {
        let report = Report {
            left: "a".to_owned(),
            right: "b".to_owned(),
            ours: None,
            cases: vec![case("SELECT 1", true), case("SELECT 2", false)],
        };
        assert_eq!(report.agreed(), 1);
        assert!((report.share() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_report_with_no_cases_is_zero_and_not_a_division_by_zero() {
        let report =
            Report { left: "a".to_owned(), right: "b".to_owned(), ours: None, cases: Vec::new() };
        assert!(report.share().abs() < f64::EPSILON);
    }

    #[test]
    fn the_side_that_is_ours_comes_off_the_engine_name() {
        assert_eq!(side("rudb-shell", "duckdb-shell"), Some(Ours::Left));
        assert_eq!(side("duckdb-shell", "rudb-shell"), Some(Ours::Right));
        assert_eq!(side("duckdb-shell", "duckdb"), None);
    }

    #[test]
    fn the_ratio_is_ours_over_theirs_whichever_side_we_are_on() {
        let cases = vec![timed("SELECT 1", 100, 1000), timed("SELECT 2", 100, 1000)];
        let left = Report {
            left: "rudb-shell".to_owned(),
            right: "duckdb-shell".to_owned(),
            ours: Some(Ours::Left),
            cases: cases.clone(),
        };
        let right = Report {
            left: "duckdb-shell".to_owned(),
            right: "rudb-shell".to_owned(),
            ours: Some(Ours::Right),
            cases,
        };
        let fast = left.ratios().expect("two measured records");
        assert!((fast.time.median - 0.1).abs() < 1e-9);
        let slow = right.ratios().expect("two measured records");
        assert!((slow.time.median - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_run_with_no_side_publishes_no_ratio_rather_than_a_guess() {
        let report = Report {
            left: "duckdb-shell".to_owned(),
            right: "duckdb".to_owned(),
            ours: None,
            cases: vec![timed("SELECT 1", 100, 1000)],
        };
        assert!(report.ratios().is_none());
        assert!(report.worst(5).is_empty());
    }

    #[test]
    fn the_worst_records_come_back_worst_first() {
        let report = Report {
            left: "rudb-shell".to_owned(),
            right: "duckdb-shell".to_owned(),
            ours: Some(Ours::Left),
            cases: vec![timed("fast", 10, 100), timed("slow", 500, 100)],
        };
        let worst = report.worst(2);
        assert_eq!(worst[0].0, "slow");
        assert!((worst[0].1 - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_record_the_engines_disagreed_on_is_never_timed() {
        let differences = vec![Difference::Height { left: 1, right: 0 }];
        assert!(!worth_timing(&rows(), &rows(), &differences, Some(slow()), Some(slow())));
    }

    #[test]
    fn an_error_on_either_side_is_never_timed_because_that_times_the_error_path() {
        let error = Outcome::Error(crate::engine::EngineError::parse("Parser Error: no"));
        assert!(!worth_timing(&error, &rows(), &[], Some(slow()), Some(slow())));
        assert!(!worth_timing(&rows(), &error, &[], Some(slow()), Some(slow())));
    }

    #[test]
    fn a_record_below_the_floor_on_both_sides_is_never_timed() {
        let quick = Usage { wall: FLOOR / 2, cpu: FLOOR / 2, peak: 1024 };
        assert!(!worth_timing(&rows(), &rows(), &[], Some(quick), Some(quick)));
        assert!(worth_timing(&rows(), &rows(), &[], Some(quick), Some(slow())));
    }

    #[test]
    fn a_machine_that_cannot_measure_times_nothing() {
        assert!(!worth_timing(&rows(), &rows(), &[], None, Some(slow())));
        assert!(!worth_timing(&rows(), &rows(), &[], Some(slow()), None));
    }

    #[test]
    fn measuring_is_off_unless_it_is_asked_for() {
        assert_eq!(Measure::default(), Measure::Off);
    }
}
