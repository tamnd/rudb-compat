//! The harness pointed at the corpora in `corpus/`, which is the only test here that involves both
//! engines at once.
//!
//! It skips rather than fails when there is no DuckDB on the machine. CI installs one and gates on
//! this, so the skip is for a laptop that has not got round to it, and a harness nobody can build
//! without the thing it compares against is a harness nobody works on.

use rudb_compat::compare::{Difference, MessageMatch, Side};
use rudb_compat::duckdb::Duckdb;
use rudb_compat::rudb::Rudb;
use rudb_compat::suite::{run_parse, statements};

/// Get a DuckDB, or say why the test is not running.
fn duckdb() -> Option<Duckdb> {
    match Duckdb::discover() {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping, no DuckDB: {e}");
            None
        }
    }
}

#[test]
fn every_statement_the_engine_parses_is_a_statement_duckdb_parses() {
    let Some(mut duckdb) = duckdb() else { return };
    let mut rudb = Rudb::new();
    let text = std::fs::read_to_string("corpus/m0.sql").unwrap();
    let statements = statements(&text);
    assert_eq!(statements.len(), 62, "the corpus lost or gained a statement");

    let report = run_parse(&mut duckdb, &mut rudb, &statements, MessageMatch::Kind).unwrap();
    let disagreed: Vec<_> = report.cases.iter().filter(|c| !c.agreed()).collect();
    assert!(
        disagreed.is_empty(),
        "{} of {} disagreed, first is {}\n{}",
        disagreed.len(),
        report.cases.len(),
        disagreed[0].sql,
        disagreed[0].differences.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn the_dialect_file_agrees_in_full_against_the_pinned_binary() {
    let Some(mut duckdb) = duckdb() else { return };
    let pinned = duckdb.is_pinned();
    let mut rudb = Rudb::new();
    let text = std::fs::read_to_string("corpus/dialect.sql").unwrap();
    let statements = statements(&text);
    let report = run_parse(&mut duckdb, &mut rudb, &statements, MessageMatch::Kind).unwrap();

    let disagreed: Vec<_> = report.cases.iter().filter(|c| !c.agreed()).collect();
    let mut only_duckdb_rejected = 0;
    let mut only_rudb_rejected = 0;
    for case in &disagreed {
        for difference in &case.differences {
            match difference {
                Difference::OneRejected { side: Side::Left, .. } => only_duckdb_rejected += 1,
                Difference::OneRejected { side: Side::Right, .. } => only_rudb_rejected += 1,
                other => panic!("a parse comparison should not produce {other}"),
            }
        }
    }

    if pinned {
        // This is the file that used to show the gap, and against the commit the grammar is
        // vendored from there is no gap to show. Every statement in it lands the same way on both
        // engines, which is what the whole pin is for.
        assert_eq!(
            disagreed.len(),
            0,
            "{} of {} disagreed against the pinned binary",
            disagreed.len(),
            report.cases.len()
        );
        return;
    }
    // Without the pinned binary the comparison is against a release, and the two differences it
    // produces are upstream moving between that release and the v2.0 ref rather than rudb being
    // wrong. `ORDER BY x ASCENDING` is in the vendored grammar and a syntax error on 1.5.5, and
    // `[1, 2] <-> [3, 4]` is array distance on 1.5.5 and cannot be one token under the v2.0
    // tokenizer. So the assertion here is on which way round each one falls.
    assert_eq!(only_duckdb_rejected, 1, "ASCENDING is the only one of these");
    assert_eq!(only_rudb_rejected, 1, "the array distance operator is the only one of these");
}

#[test]
fn a_query_and_a_statement_that_is_not_sql_both_reach_the_full_comparison() {
    let Some(mut duckdb) = duckdb() else { return };
    let mut rudb = Rudb::new();
    let cases = vec!["SELECT 1 AS a".to_owned(), "SELECT FROM WHERE".to_owned()];
    let report = rudb_compat::suite::run(&mut duckdb, &mut rudb, &cases, MessageMatch::Kind)
        .expect("both engines should answer");

    // Both agree now. The first case used to be a difference because rudb had no executor, and it
    // is the one line in this file that had to change the day one arrived. Two engines returning
    // the same one row with the same name and the same type is the smallest complete pass through
    // the loop, the comparison and the report, which is what this test is for.
    assert!(report.cases[0].agreed(), "{:?}", report.cases[0].differences);
    assert!(report.cases[1].agreed(), "both engines reject the same non SQL");
}
