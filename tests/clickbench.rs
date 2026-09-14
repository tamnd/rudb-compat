//! The forty three ClickBench answers, against a real DuckDB reading the same file.
//!
//! This is the answer half of the board. tamnd/rudb-bench times these queries and rudb has a row
//! there now, and a row on a board is worth nothing until somebody has checked that the engine
//! returned the right thing. Milestone E0e, issue #112 in tamnd/rudb, asks for that check to live
//! here rather than in the benchmark harness, because the benchmark harness compares answers as a
//! side effect of measuring them and a side effect is not a gate.
//!
//! There are two files and they ask two different questions. `corpus/clickbench.sql` holds the
//! queries as ClickBench writes them, and the question is whether the two engines ever disagree
//! about anything other than which of a set of tied rows came back. `corpus/clickbench-settled.sql`
//! holds the same queries with the tie at the cut broken, and the question is whether the answers
//! match exactly once the query says which rows it wants.
//!
//! It needs the ClickBench corpus, which is fourteen gigabytes and is not in this repository. The
//! test skips with the reason printed when the file is not on the machine, the same way the rest of
//! this suite skips when there is no DuckDB. Two of the machines this project develops on have it.
//!
//! Point `RUDB_COMPAT_HITS` at the file, or `RUDB_BENCH_DATA` at the directory holding it, which is
//! the variable tamnd/rudb-bench already reads. Both are asked for by hand on purpose. A machine
//! that happens to have the corpus in the usual place should not have its gate turn into a run of
//! a hundred million rows through forty three queries twice, which takes hours and is a decision
//! somebody makes rather than one they discover.
//!
//! Set `RUDB_COMPAT_PROGRESS` as well when the file is the full one. The run takes hours and says
//! nothing until it is over, and if it ends by being killed rather than by finishing then the
//! query it died on is the thing worth knowing.
//!
//! Set `RUDB_COMPAT_SHELL` to run both engines as binaries instead of running rudb as the library
//! this crate links. Everything else stays where it is, including the view, so the two runs ask the
//! same question through two different doors and a number that only holds through one of them is a
//! number about the door rather than about the engine. `RUDB_COMPAT_RUDB` names the binary.
//!
//! The hundred thousand row partition works too and answers a weaker question: every query still
//! has to return the same thing on both engines, and ties at a `LIMIT` are rarer on a smaller file,
//! so a green run on the partition is a floor rather than the claim the milestone wants.
//!
//! One query is allowed to disagree, and the list of them is `UPSTREAM_WRONG` below. A difference
//! between two engines is usually this one being wrong, and every other test here is written on
//! that assumption, but it is not a law and the full file found the exception.

use std::path::PathBuf;

use rudb_compat::compare::{Difference, MessageMatch};
use rudb_compat::duckdb::Duckdb;
use rudb_compat::engine::Engine;
use rudb_compat::rudb::Rudb;
use rudb_compat::shell::Shell;
use rudb_compat::suite::{Measure, Report, run, statements};

/// The projection every engine on the board gets, as a view over the file where it lies.
///
/// The published Parquet stores the date as a count of days in a `USMALLINT` and the three
/// timestamps as epoch seconds in a `BIGINT`, so every entry on the board converts them and this
/// one converts them the same way. `binary_as_string` is the other half of it: the string columns
/// are written as byte arrays with no annotation, and read as they are they come back as blobs,
/// which makes `URL LIKE '%google%'` a type error rather than a filter.
///
/// This is the recipe in `src/suite.rs` in tamnd/rudb-bench, under `loading("clickbench", "rudb",
/// "hits")`, and it is written out here rather than shared because the two repositories do not
/// depend on each other. A copy that drifts shows up as a difference in this test.
fn view(at: &std::path::Path) -> String {
    format!(
        "CREATE VIEW hits AS SELECT * REPLACE (make_date(EventDate) AS EventDate, \
         epoch_ms(EventTime * 1000) AS EventTime, epoch_ms(ClientEventTime * 1000) AS \
         ClientEventTime, epoch_ms(LocalEventTime * 1000) AS LocalEventTime) FROM \
         read_parquet('{}', binary_as_string=True)",
        at.display()
    )
}

/// Where somebody said the corpus is, or nothing when nobody said.
fn hits() -> Option<PathBuf> {
    let named = std::env::var_os("RUDB_COMPAT_HITS").map(PathBuf::from);
    let data = std::env::var_os("RUDB_BENCH_DATA").map(|at| PathBuf::from(at).join("hits.parquet"));
    [named, data].into_iter().flatten().find(|at| at.is_file())
}

/// The two engines to put the corpus to, and the view they both read it through.
///
/// `RUDB_COMPAT_SHELL` swaps the rudb library for the rudb binary and leaves everything else alone,
/// which is the comparison the drop in claim is actually about. The rest of this file does not care
/// which it got, and that is the point: a number that only holds through one of the two doors is a
/// number about the door.
fn engines(view: &str) -> Option<(Box<dyn Engine>, Box<dyn Engine>)> {
    if std::env::var_os("RUDB_COMPAT_SHELL").is_some() {
        let left = Shell::duckdb().ok()?.with_setup(vec![view.to_owned()]);
        let right = Shell::rudb().ok()?.with_setup(vec![view.to_owned()]);
        return Some((Box::new(left), Box::new(right)));
    }
    let Ok(duckdb) = Duckdb::discover() else {
        eprintln!("skipping, no DuckDB on this machine");
        return None;
    };
    // DuckDB gets the view again in front of every statement because every statement there is its
    // own process. rudb keeps one database for the run, so it gets it once, and a failure on this
    // line is a broken harness rather than a failing case.
    let left = duckdb.with_setup(vec![view.to_owned()]);
    let mut right = Rudb::new();
    let made = right.run(view).expect("rudb should answer");
    assert!(made.is_rows(), "rudb could not make the view\n{view}\n{made:?}");
    Some((Box::new(left), Box::new(right)))
}

/// Put one of the corpus files to both engines over the same view, or say why it did not run.
fn against(corpus: &str) -> Option<(Report, PathBuf)> {
    let at = hits()?;
    let view = view(&at);
    let (mut left, mut right) = engines(&view)?;

    let text = std::fs::read_to_string(corpus).unwrap();
    let queries = statements(&text);
    assert_eq!(queries.len(), 43, "ClickBench is forty three queries and {corpus} lost one");
    let report =
        run(left.as_mut(), right.as_mut(), &queries, MessageMatch::Kind, Measure::Off).unwrap();
    Some((report, at))
}

/// The statements where the two engines disagree and DuckDB is the one that is wrong.
///
/// The one entry is DuckDB summing a BIGINT column short by a multiple of two to the sixty fourth
/// on the full file. `SELECT SUM(UserID::HUGEINT) FROM hits` and `SELECT SUM(UserID::DECIMAL(38,
/// 0)) FROM hits` in the same binary return what rudb returns, and so does adding the column up
/// outside a database, so rudb has the right answer here and DuckDB does not. The reduction and the
/// numbers are in issue #12. It does not show on the hundred thousand row partition, where this
/// query agrees.
///
/// A list rather than a skip, because the difference still has to be a difference in a value. If
/// one of these ever comes back with a different column count, a different type, a different row
/// count or an error on one side, that is a new bug wearing an old bug's name and this still fails
/// on it.
const UPSTREAM_WRONG: &[&str] = &["SELECT AVG(UserID) FROM hits"];

/// Whether a statement is one of the ones DuckDB gets wrong.
fn upstream_wrong(sql: &str) -> bool {
    UPSTREAM_WRONG.contains(&sql.trim().trim_end_matches(';').trim_end())
}

/// Every case that disagreed, with the statement above the reasons, for a message worth reading.
///
/// A value difference on a statement in [`UPSTREAM_WRONG`] is not counted and nothing else there is
/// forgiven.
fn disagreements(report: &Report) -> Vec<String> {
    report
        .cases
        .iter()
        .filter_map(|case| {
            let allowed = upstream_wrong(&case.sql);
            let why: Vec<String> = case
                .differences
                .iter()
                .filter(|difference| !(allowed && matches!(difference, Difference::Value { .. })))
                .map(ToString::to_string)
                .collect();
            (!why.is_empty()).then(|| format!("{}\n    {}", case.sql, why.join("\n    ")))
        })
        .collect()
}

#[test]
fn the_queries_as_written_never_disagree_about_anything_but_which_tied_row_came_back() {
    let Some((report, at)) = against("corpus/clickbench.sql") else {
        eprintln!("skipping, no ClickBench corpus here, set RUDB_COMPAT_HITS to hits.parquet");
        return;
    };

    // A tie at the cut can only move which rows come back. It cannot change how many columns there
    // are, what they are called, what type they have, how many rows came back, or whether the query
    // ran at all, and every one of those is a bug this file has to fail on. So the assertion is on
    // the kind of difference rather than on a list of query names: a list would be a property of
    // the partition the corpus was cut at, and this holds on any of them.
    let mut wrong = Vec::new();
    for case in &report.cases {
        for difference in &case.differences {
            if !matches!(difference, Difference::Value { .. }) {
                wrong.push(format!("{}\n    {difference}", case.sql));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} differ by more than a tied row, on {} against {}\n\n{}",
        wrong.len(),
        report.cases.len(),
        at.display(),
        report.left,
        wrong.join("\n\n")
    );
    eprintln!(
        "{} of {} agreed to the last bit as written, the rest tie at the cut",
        report.agreed(),
        report.cases.len()
    );
}

#[test]
fn breaking_the_tie_at_the_cut_settles_every_one_of_the_forty_three() {
    let Some((report, at)) = against("corpus/clickbench-settled.sql") else {
        eprintln!("skipping, no ClickBench corpus here, set RUDB_COMPAT_HITS to hits.parquet");
        return;
    };

    // This is the one the milestone is about. Every query fixes its own order here, so there is
    // nothing left for an engine to choose and a difference is a wrong answer, on one side or the
    // other.
    for case in &report.cases {
        if upstream_wrong(&case.sql) && !case.agreed() {
            eprintln!("{} differed, which is DuckDB's bug and is allowed, see #12", case.sql);
        }
    }
    let disagreed = disagreements(&report);
    assert!(
        disagreed.is_empty(),
        "{} of {} disagreed with the tie broken, on {} against {}\n\n{}",
        disagreed.len(),
        report.cases.len(),
        at.display(),
        report.left,
        disagreed.join("\n\n")
    );
}

/// Every statement named as one DuckDB gets wrong is still a statement in both corpus files.
///
/// This is the only thing keeping that list honest, and it is the only test in this file that runs
/// without the fourteen gigabytes, so it runs on every machine in the gate. A query that gets
/// rewritten or dropped has to take its entry with it.
#[test]
fn the_statements_duckdb_gets_wrong_are_still_in_both_corpus_files() {
    for corpus in ["corpus/clickbench.sql", "corpus/clickbench-settled.sql"] {
        let text = std::fs::read_to_string(corpus).unwrap();
        let queries = statements(&text);
        for sql in UPSTREAM_WRONG {
            assert!(
                queries.iter().any(|query| upstream_wrong(query) && query.trim() == *sql),
                "{sql} is named as one DuckDB gets wrong and is not in {corpus} any more"
            );
        }
    }
}
