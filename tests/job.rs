//! The hundred and thirteen Join Order Benchmark queries, against a real DuckDB on the same load.
//!
//! JOB is the board tamnd/rudb is chasing in milestones J0 to J9, issues #1844 to #1853 there, and
//! every query on it is an ungrouped `MIN` over an acyclic equi join. rudb answers that class by a
//! full semijoin reduction rather than by joining, which is a different algorithm from the one
//! DuckDB runs, and a different algorithm is exactly where a wrong answer hides. This file is the
//! check that the answers are the same.
//!
//! There are two tests and they ask two different questions. The first needs no data: it loads the
//! JOB schema into an empty rudb and asks every query for its plan, and every plan has to be the
//! reduction. A query that falls back to the ordinary join still answers correctly, so nothing else
//! in this suite would notice, but it would be slow on the board and this says which one and why.
//!
//! The second needs the IMDb load, which is four gigabytes of CSV and is not in this repository. It
//! skips with the reason printed unless `RUDB_COMPAT_IMDB` names a directory holding `imdb.duckdb`
//! and `imdb.rudb`, the two databases tamnd/rudb-bench loads from the May 2013 snapshot, and
//! `RUDB_COMPAT_RUDB` names the rudb binary. Both engines run as binaries on their own file, one
//! process per query, so the comparison costs no load at all and the run takes a few minutes.
//!
//! Every JOB query returns exactly one row, there is no `LIMIT` and there is no floating point
//! aggregate, so there is nothing to forgive: the two engines have to agree to the byte.

use std::path::{Path, PathBuf};

use rudb_compat::compare::MessageMatch;
use rudb_compat::engine::{Cell, Engine, Outcome};
use rudb_compat::rudb::Rudb;
use rudb_compat::shell::Shell;
use rudb_compat::suite::{Measure, run, statements};

/// The directory the queries and the schema live in.
const CORPUS: &str = "corpus/job";

/// Every query file in the corpus, by name, in name order.
///
/// The schema and the index file sit in the same directory, as they do in the JOB repository, and
/// are not queries.
fn queries() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .filter(|path| {
            let stem = path.file_stem().unwrap().to_string_lossy();
            stem != "schema" && stem != "fkindexes"
        })
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let mut one = statements(&text);
            assert_eq!(one.len(), 1, "{} should be one statement", path.display());
            (name, one.remove(0))
        })
        .collect();
    found.sort();
    assert_eq!(found.len(), 113, "JOB is a hundred and thirteen queries and {CORPUS} lost one");
    found
}

/// Every cell of a result on its own line, which is how a plan reads once it is text again.
fn text(outcome: &Outcome) -> String {
    let Outcome::Rows(table) = outcome else {
        return format!("{outcome:?}");
    };
    table
        .rows
        .iter()
        .flatten()
        .map(|cell| match cell {
            Cell::Null => String::new(),
            Cell::Text(text) => text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_job_query_is_planned_as_a_semijoin_reduction() {
    let mut rudb = Rudb::new();
    let schema = std::fs::read_to_string(Path::new(CORPUS).join("schema.sql")).unwrap();
    for statement in statements(&schema) {
        let made = rudb.run(&statement).expect("rudb should answer");
        assert!(made.is_rows(), "rudb could not make the JOB schema\n{statement}\n{made:?}");
    }

    // The rule decides on the shape of the query and never on the rows, so an empty table is as
    // good as a full one for asking what the plan is. That is what lets this run on every machine.
    let mut missed = Vec::new();
    for (name, sql) in queries() {
        let plan = text(&rudb.run(&format!("EXPLAIN {sql}")).expect("rudb should answer"));
        if !plan.contains("Consistent") {
            missed.push(format!("{name}\n{plan}"));
        }
    }
    assert!(
        missed.is_empty(),
        "{} of 113 JOB queries are not planned as a reduction\n\n{}",
        missed.len(),
        missed.join("\n\n")
    );
}

/// Where somebody said the two loaded databases are, or nothing when nobody said.
fn imdb() -> Option<PathBuf> {
    let at = PathBuf::from(std::env::var_os("RUDB_COMPAT_IMDB")?);
    (at.join("imdb.duckdb").is_file() && at.join("imdb.rudb").is_file()).then_some(at)
}

#[test]
fn every_job_query_answers_what_duckdb_answers_on_the_imdb_load() {
    let Some(at) = imdb() else {
        eprintln!("skipping, no IMDb load here, set RUDB_COMPAT_IMDB to the directory holding it");
        return;
    };
    let (Ok(duckdb), Ok(rudb)) = (Shell::duckdb(), Shell::rudb()) else {
        eprintln!("skipping, set RUDB_COMPAT_DUCKDB and RUDB_COMPAT_RUDB to the two binaries");
        return;
    };
    let mut left = duckdb.on(at.join("imdb.duckdb").to_string_lossy());
    let mut right = rudb.on(at.join("imdb.rudb").to_string_lossy());

    // One query at a time rather than the whole list, so a failure is reported by the name the
    // board uses and not by a statement somebody has to go and find.
    let mut wrong = Vec::new();
    for (name, sql) in queries() {
        let report = run(&mut left, &mut right, &[sql], MessageMatch::Kind, Measure::Off).unwrap();
        for case in &report.cases {
            for difference in &case.differences {
                wrong.push(format!("{name}\n    {difference}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} differences over the 113 JOB queries on {}\n\n{}",
        wrong.len(),
        at.display(),
        wrong.join("\n\n")
    );
}
