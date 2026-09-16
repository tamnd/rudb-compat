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
use rudb_compat::isolate::{Isolated, Limits, decode_run, encode_run, run_corpus};
use rudb_compat::rudb::Rudb;
use rudb_compat::shard::Shard;
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
    assert_eq!(summary.files, 35, "the corpus gained or lost a file");
    assert!(summary.passed > 100, "only {} records ran, which is too few", summary.passed);
}

/// The same corpus with every optimizer pass turned off, which has to answer the same.
///
/// The strongest property in the project, and it needs no new expected outputs. Every pass is a
/// rewrite that is supposed to keep the answer, so the plan the binder produced is the right answer
/// by construction and a file that passes with the passes off and fails with them on is a pass that
/// changed an answer. `spec/09-optimizer.md` section 9.1 asks for exactly this.
///
/// It is checked both ways round. The unoptimized run has to pass the corpus on its own, which is
/// what catches a pass covering for a hole in the executor, and the two summaries have to match
/// record for record, which is what catches a pass changing an answer the corpus checks.
#[test]
fn the_corpus_answers_the_same_with_every_optimizer_pass_turned_off() {
    let mut on = Rudb::new();
    let optimized = run_path(&mut on, Path::new("corpus/slt"), false).expect("the corpus is there");

    let mut off = Rudb::unoptimized();
    let unoptimized =
        run_path(&mut off, Path::new("corpus/slt"), false).expect("the corpus is there");

    assert!(
        unoptimized.failures.is_empty(),
        "{} of {} records failed with the optimizer off\n{}",
        unoptimized.failed,
        unoptimized.attempted(),
        unoptimized.failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    );
    assert!(unoptimized.skipped_files.is_empty(), "{:?}", unoptimized.skipped_files);
    assert_eq!(unoptimized.files, optimized.files);
    assert_eq!(unoptimized.passed, optimized.passed);
    assert_eq!(unoptimized.failed, optimized.failed);
    assert_eq!(unoptimized.failures, optimized.failures);
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
    let apart = run_corpus(exe, Path::new("corpus/slt"), false, limits, Shard::whole())
        .expect("the corpus is there");

    assert!(apart.stopped.is_empty(), "{:?}", apart.stopped);
    assert_eq!(apart.files, inline.files);
    assert_eq!(apart.passed, inline.passed);
    assert_eq!(apart.failed, inline.failed);
    assert_eq!(apart.skips, inline.skipped);
    assert_eq!(apart.failures, inline.failures);
}

/// Four shards of the same corpus add up to the whole of it.
///
/// This is the property the whole sharding scheme rests on, so it is checked against a run that was
/// not sharded rather than against the sum of the parts, which would pass on a split that dropped
/// the same file from every shard.
#[test]
fn the_four_shards_of_a_corpus_add_up_to_the_corpus() {
    let exe = Path::new(env!("CARGO_BIN_EXE_rudb-compat"));
    let limits = Limits { time: Duration::from_secs(60), ..Limits::default() };
    let whole = run_corpus(exe, Path::new("corpus/slt"), false, limits, Shard::whole())
        .expect("the corpus is there");

    let mut merged = Isolated::default();
    for at in 1..=4 {
        let shard = Shard::parse(&format!("{at}/4")).expect("a shard");
        let part = run_corpus(exe, Path::new("corpus/slt"), false, limits, shard)
            .expect("the corpus is there");
        assert!(part.files < whole.files, "a quarter of eight files is not eight files");
        // Through the encoding, because that is the trip a real shard makes, over scp and back.
        merged.absorb(decode_run(&encode_run(&part)));
    }

    assert_eq!(merged.files, whole.files);
    assert_eq!(merged.passed, whole.passed);
    assert_eq!(merged.failed, whole.failed);
    assert_eq!(merged.skips, whole.skips);
    assert_eq!(merged.failures.len(), whole.failures.len());
    assert_eq!(merged.kinds, whole.kinds);
}
