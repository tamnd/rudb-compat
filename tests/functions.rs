//! The boundary table and the function catalog, against a live DuckDB.
//!
//! The unit tests in `src/functions.rs` check the shape of what the generator produces. They cannot
//! check the one thing that matters most about the boundary table, which is that every value in it
//! is a value DuckDB accepts. A literal that does not parse is not a boundary. It is a call both
//! engines reject for a reason that has nothing to do with the function under test, and it would
//! count as agreement for as long as nobody looked.
//!
//! It skips rather than fails when there is no DuckDB on the machine, the same way
//! `tests/differential.rs` does and for the same reason.

use rudb_compat::compare::MessageMatch;
use rudb_compat::coverage::{Untested, Verdict, coverage, score};
use rudb_compat::duckdb::Duckdb;
use rudb_compat::engine::{Engine, Outcome};
use rudb_compat::functions::{Kind, TYPES, boundaries, calls, catalog, inventory};
use rudb_compat::rudb::Rudb;

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
fn every_value_in_the_boundary_table_is_one_duckdb_accepts() {
    let Some(mut duckdb) = duckdb() else { return };
    let mut refused = Vec::new();
    for ty in TYPES {
        let set = boundaries(ty).expect("a type on the list has a set");
        for value in std::iter::once(set.ordinary).chain(set.values.iter().copied()) {
            let sql = format!("SELECT {value}");
            match duckdb.run(&sql).expect("duckdb answers") {
                Outcome::Rows(_) => {}
                Outcome::Error(e) => refused.push(format!("{ty}  {value}  {e}")),
            }
        }
    }
    assert!(refused.is_empty(), "DuckDB refused {} of them\n{}", refused.len(), refused.join("\n"));
}

#[test]
fn the_catalog_on_the_pinned_binary_is_the_one_the_project_publishes_a_number_over() {
    let Some(mut duckdb) = duckdb() else { return };
    if !duckdb.is_pinned() {
        eprintln!("skipping, this is not the pinned DuckDB and the counts are per pin");
        return;
    }
    let catalog = catalog(&mut duckdb).expect("the pinned binary has duckdb_functions()");
    let inventory = inventory(&catalog);
    assert_eq!(inventory.overloads, 3245, "the pinned catalog gained or lost an overload");
    assert_eq!(inventory.names, 1159, "the pinned catalog gained or lost a name");
    assert!(
        inventory.callable > 0 && inventory.callable < inventory.overloads,
        "callable is {} of {}, which is either nothing or everything and both are wrong",
        inventory.callable,
        inventory.overloads
    );
}

#[test]
fn every_call_the_generator_makes_is_a_statement_duckdb_parses() {
    let Some(mut duckdb) = duckdb() else { return };
    let catalog = catalog(&mut duckdb).expect("this binary has duckdb_functions()");
    // A parser error only, and a sample rather than all of them, because the point here is the shape
    // of the generated text and not the answers. Most of these calls are meant to fail, on a wrong
    // type or on a value the function cannot take, and that is a binder or a conversion error and is
    // the thing the differential run compares. A parser error means this file wrote something that
    // is not SQL. Comparing the answers is a job with its own command rather than a unit test.
    let mut bad = Vec::new();
    for overload in catalog.iter().filter(|o| !calls(o).is_empty()).take(200) {
        for call in calls(overload).iter().take(3) {
            match duckdb.run(&call.sql).expect("duckdb answers") {
                Outcome::Error(e) if e.kind == "Parser Error" => {
                    bad.push(format!("{}  {}", call.sql, e));
                }
                _ => {}
            }
        }
    }
    assert!(bad.is_empty(), "{} calls were not SQL\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn a_function_both_engines_have_scores_as_passed_and_one_only_duckdb_has_does_not() {
    let Some(mut duckdb) = duckdb() else { return };
    let catalog = catalog(&mut duckdb).expect("this binary has duckdb_functions()");
    let wanted: Vec<_> = catalog
        .into_iter()
        .filter(|o| ["upper", "lower", "generate_series"].contains(&o.name.as_str()))
        .collect();
    assert!(!wanted.is_empty(), "the catalog has none of the names this test is about");
    let mut rudb = Rudb::new();
    let scored = score(&mut duckdb, &mut rudb, &wanted, MessageMatch::Kind).expect("both engines");
    let verdict = |name: &str| {
        scored
            .iter()
            .filter(|s| s.overload.name == name)
            .map(|s| s.verdict.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        verdict("upper").iter().all(|v| *v == Verdict::Passed),
        "upper disagreed somewhere\n{:?}",
        scored.iter().flat_map(|s| &s.failures).collect::<Vec<_>>()
    );
    assert!(verdict("lower").iter().all(|v| *v == Verdict::Passed), "lower disagreed somewhere");
    // The table overloads of `generate_series` are a kind the generator does not build calls for,
    // so they are never tested rather than counted as a pass nobody earned. Its scalar overloads are
    // tested like anything else, which is why this asks by kind and not by name.
    let tabular: Vec<_> =
        scored.iter().filter(|s| s.overload.kind == Kind::Table).map(|s| &s.verdict).collect();
    assert!(!tabular.is_empty(), "the catalog has no table overload among these names");
    assert!(
        tabular.iter().all(|v| **v == Verdict::Untested(Untested::KindNotCalled)),
        "{tabular:?}"
    );
}

#[test]
fn a_function_whose_answer_changes_between_calls_is_never_put_to_either_engine() {
    let Some(mut duckdb) = duckdb() else { return };
    let catalog = catalog(&mut duckdb).expect("this binary has duckdb_functions()");
    let wanted: Vec<_> = catalog
        .into_iter()
        .filter(|o| ["random", "now", "version", "age"].contains(&o.name.as_str()))
        .collect();
    let mut rudb = Rudb::new();
    let scored = score(&mut duckdb, &mut rudb, &wanted, MessageMatch::Kind).expect("both engines");
    for one in &scored {
        let volatile = one.overload.name != "age" || one.overload.parameters.len() == 1;
        if volatile {
            assert_eq!(
                one.verdict,
                Verdict::Untested(Untested::Volatile),
                "{} was put to both engines and it answers differently every time",
                one.signature()
            );
            assert_eq!(one.attempted, 0);
        }
    }
    // The two argument `age` is the difference between two things the caller named, so it is as
    // pure as subtraction and the run has to have actually tried it.
    let two = scored
        .iter()
        .find(|s| s.overload.name == "age" && s.overload.parameters.len() == 2)
        .expect("the catalog has a two argument age");
    assert!(two.attempted > 0, "the two argument age was never tried");
}

#[test]
fn the_number_is_over_the_whole_catalog_and_not_over_what_happened_to_be_tested() {
    let Some(mut duckdb) = duckdb() else { return };
    if !duckdb.is_pinned() {
        eprintln!("skipping, this is not the pinned DuckDB and the counts are per pin");
        return;
    }
    let catalog = catalog(&mut duckdb).expect("the pinned binary has duckdb_functions()");
    // Only the untested rows, which need no engine, because the point here is the denominator and
    // not the answers. Running all 3245 is what `rudb-compat coverage` is for.
    let untested: Vec<_> = catalog.iter().filter(|o| calls(o).is_empty()).cloned().collect();
    let mut rudb = Rudb::new();
    let scored =
        score(&mut duckdb, &mut rudb, &untested, MessageMatch::Kind).expect("both engines");
    let coverage = coverage(&scored);
    assert_eq!(coverage.overloads.total, untested.len());
    assert_eq!(coverage.overloads.passed, 0, "nothing here was tested, so nothing passed");
    assert_eq!(coverage.overloads.untested, untested.len());
    assert!(
        coverage.names.share() < f64::EPSILON,
        "a run that tested nothing came out above zero, which means it divided by what it tested"
    );
}
