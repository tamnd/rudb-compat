//! The files rudb does not pass yet, held to the one thing that is true of them today.
//!
//! `corpus/pending` is for a file whose every expected answer came from the pin and which rudb
//! cannot pass until a milestone lands. It is not a place to park a failure. What this test asks of
//! each file is that the runner reaches every record in it, so a file here is a real measurement
//! and not one that quietly stopped at its first second connection. When rudb passes a file in
//! full, the file moves to `corpus/slt` and the gate there takes it over, which this test says when
//! it happens.

use std::path::Path;

use rudb_compat::conform::run_path;
use rudb_compat::rudb::Rudb;

#[test]
fn every_pending_file_is_scored_to_its_end() {
    let mut rudb = Rudb::new();
    let summary =
        run_path(&mut rudb, Path::new("corpus/pending"), false).expect("the files are there");

    assert!(summary.files > 0, "the pending directory was not read");
    assert!(summary.skipped_files.is_empty(), "{:?}", summary.skipped_files);
    assert_eq!(summary.skipped.total(), 0, "a record was not attempted");
    assert!(
        summary.failed > 0,
        "every pending record passes now, so move the files to corpus/slt, where they are a gate"
    );
}
