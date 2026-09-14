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

use rudb_compat::duckdb::Duckdb;
use rudb_compat::engine::{Engine, Outcome};
use rudb_compat::functions::{TYPES, boundaries, calls, catalog, inventory};

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
