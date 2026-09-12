//! The committed sqllogictest corpus, which every commit has to pass in full.
//!
//! These files are ours and not DuckDB's. They cover what rudb is supposed to be able to do today,
//! written in the format the upstream corpus uses, so a feature that lands in the executor gets a
//! test here in the same shape as the four thousand files it is eventually measured against.
//!
//! The upstream corpus is a measurement and this one is a gate. The pass rate against upstream goes
//! up over the milestones and CI only watches it for a fall, but nothing here is allowed to fail,
//! ever, which is why it is a test and not a printed number.

use std::path::Path;
use std::time::Duration;

use rudb_compat::conform::run_path;
use rudb_compat::isolate::{Limits, run_corpus};
use rudb_compat::rudb::Rudb;
use rudb_compat::shell::{Session, Shell};

#[test]
fn every_file_in_the_committed_corpus_passes() {
    let mut rudb = Rudb::new();
    let summary = run_path(&mut rudb, Path::new("corpus/slt"), false).expect("the corpus is there");

    assert!(
        summary.failures.is_empty(),
        "{} of {} records failed\n{}",
        summary.failed,
        summary.attempted(),
        summary.failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    );
    assert_eq!(summary.failed, 0, "a failure was counted without being recorded");
    assert!(summary.skipped_files.is_empty(), "{:?}", summary.skipped_files);
    assert_eq!(summary.skipped.total(), 0, "nothing here should need a directive we do not have");
    // A corpus that silently stopped being read would pass every assertion above it, so the count
    // is pinned. Raise it when a file is added, which is the point at which somebody is looking.
    assert_eq!(summary.files, 7, "the corpus gained or lost a file");
    assert!(summary.passed > 100, "only {} records ran, which is too few", summary.passed);
}

/// The same corpus through the shell, which is the rudb somebody who is not this harness runs.
///
/// The library is one of the two ways into the engine and the binary is the other, and the drop in
/// claim is about the binary. So the corpus that says what rudb can do today has to come out the
/// same way when a shell is the thing answering, or the claim is about a library nobody has.
///
/// This needs a built `rudb` and skips without one, which is the same rule the rest of the harness
/// follows for a binary it does not build itself. CI in this repository has the library and not the
/// binary, so the run that makes this a gate is the one in the engine's own CI, which builds the
/// shell from the commit under test and points this corpus at it.
#[test]
fn every_file_in_the_committed_corpus_passes_through_the_shell_too() {
    let shell = match Shell::rudb() {
        Ok(shell) => shell,
        Err(e) => {
            eprintln!("skipping, no rudb shell: {e}");
            return;
        }
    };
    let mut library = Rudb::new();
    let through_library =
        run_path(&mut library, Path::new("corpus/slt"), false).expect("the corpus is there");

    let mut session = Session::new(shell);
    let summary = run_path(&mut session, Path::new("corpus/slt"), false).expect("the corpus is on");

    assert!(
        summary.failures.is_empty(),
        "{} of {} records failed through the shell\n{}",
        summary.failed,
        summary.attempted(),
        summary.failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    );
    assert!(summary.skipped_files.is_empty(), "{:?}", summary.skipped_files);
    // The same corpus and the same engine, so the two ways in have to agree on the count as well
    // as on the outcome. A file the shell quietly did not read would otherwise pass every
    // assertion above this one.
    assert_eq!(summary.files, through_library.files);
    assert_eq!(summary.passed, through_library.passed);
    assert_eq!(summary.failed, through_library.failed);
}

/// The same corpus through the isolating runner, which is how the upstream run works.
///
/// Two runners that can disagree about what passed is worse than one runner, so this pins them
/// together on a corpus where the answer is known. It also exercises the spawning, the temporary
/// files and the decoding, none of which the unit tests can reach because they do not run a
/// process.
#[test]
fn the_isolating_runner_gets_the_same_answer_as_the_one_in_this_process() {
    let mut rudb = Rudb::new();
    let inline = run_path(&mut rudb, Path::new("corpus/slt"), false).expect("the corpus is there");

    let exe = Path::new(env!("CARGO_BIN_EXE_rudb-compat"));
    let limits = Limits { time: Duration::from_secs(60), ..Limits::default() };
    let apart =
        run_corpus(exe, Path::new("corpus/slt"), false, limits).expect("the corpus is there");

    assert!(apart.stopped.is_empty(), "{:?}", apart.stopped);
    assert_eq!(apart.files, inline.files);
    assert_eq!(apart.passed, inline.passed);
    assert_eq!(apart.failed, inline.failed);
    assert_eq!(apart.skips, inline.skipped);
    assert_eq!(apart.failures, inline.failures);
}
