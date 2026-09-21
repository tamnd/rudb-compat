//! A file the engine has to stop, run the way the corpus run runs it.
//!
//! The upstream corpus has files that ask for more rows than exist and more memory than the
//! machine has, on purpose, because giving up is one of the behaviours being tested. Seven of them
//! were killed from outside by the runner, which left them counted in neither column: not a pass,
//! not a failure, just a file nobody knows anything about.
//!
//! This is the two ways that goes wrong, written small enough to run in a second. Each one gets its
//! own run with the other limit set out of reach, because a run that gives a query a clock and a
//! budget it can both reach is measuring which one the machine got to first. That is what this test
//! used to do and on a loaded box the clock always won, so the file that was there to prove the
//! memory cap works proved the clock works twice.
//!
//! The other engine is here too, at the bottom. DuckDB is a subprocess rather than a library, so
//! nothing in this harness was stopping it, and a generated call to `sleep_ms` is all it takes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rudb_compat::conform::Reason;
use rudb_compat::duckdb::Duckdb;
use rudb_compat::engine::{Engine, Outcome};
use rudb_compat::isolate::{Limits, run_corpus};
use rudb_compat::shard::Shard;

#[test]
fn a_query_that_is_too_slow_is_a_failed_record_and_not_a_killed_process() {
    // A second, which is twelve before this runner would step in, against a cap no query in this
    // file is ever going to reach.
    let limits = Limits { time: Duration::from_secs(1), memory: 64 * 1024 * 1024 * 1024 };
    let detail = stopped_by(
        "clock",
        "query I\nSELECT count(*) FROM range(100000000000);\n----\n100000000000\n",
        limits,
    );
    assert!(detail.contains("Interrupt Error"), "{detail}");
}

#[test]
fn a_query_that_is_too_large_is_a_failed_record_and_not_a_killed_process() {
    // A cap the engine gets a quarter of, against a clock the sort underneath it finishes well
    // inside on any machine this runs on. The cap is not small: it is the size of the process and
    // the process is a database, so a cap the engine can reach has to leave room for the binary
    // underneath it and for everything the engine allocates without charging itself for it. A
    // quarter of 512 MB is a 128 MB budget, and the sort below was measured holding 408 MB of
    // process while it was inside that budget, so the room this leaves is real and it is deliberate.
    //
    // This was 256 MB until rudb's vector went from 1024 to 8192 in tamnd/rudb#480. The sort still
    // stops itself at its budget, which is the thing under test, but the process it stops in got
    // bigger: at a 64 MB budget it went from 205 MB to 306 MB and started tripping the old cap, so
    // the file this test is about was killed from outside instead of failing. The overshoot is
    // rudb's to bring down and is filed as tamnd/rudb#735. The table in `Limits::budget` has what
    // each budget costs now.
    let limits = Limits { time: Duration::from_secs(120), memory: 512 * 1024 * 1024 };
    let detail = stopped_by(
        "budget",
        "query I\nSELECT count(*) FROM (SELECT * FROM range(10000000) ORDER BY range) t;\n\
         ----\n10000000\n",
        limits,
    );
    assert!(detail.contains("Out of Memory Error"), "{detail}");
}

/// Run one file on its own and give back what the engine said when it stopped.
fn stopped_by(name: &str, text: &str, limits: Limits) -> String {
    let dir = scratch(name);
    write(&dir, &format!("{name}.test"), text);

    let exe = Path::new(env!("CARGO_BIN_EXE_rudb-compat"));
    let run = run_corpus(exe, &dir, false, limits, Shard::whole()).expect("the file is there");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(run.stopped.is_empty(), "a file was killed from outside: {:?}", run.stopped);
    assert_eq!(run.files, 1);
    assert_eq!(run.failed, 1, "{:?}", run.failures);
    let failure = &run.failures[0];
    assert_eq!(failure.reason, Reason::Stopped, "{failure}");
    failure.detail.clone()
}

#[test]
fn a_duckdb_that_is_never_going_to_finish_is_killed_and_comes_back_as_a_record() {
    let Ok(duckdb) = Duckdb::discover() else {
        eprintln!("skipping, no DuckDB");
        return;
    };
    if !duckdb.is_pinned() {
        eprintln!("skipping, sleep_ms being in the catalog is a property of the pin");
        return;
    }
    // The other engine in this harness is a subprocess and nothing was stopping it. The function
    // sweep generated this exact call, DuckDB went to sleep for about nine billion seconds, and the
    // run sat behind it for an hour before anybody looked.
    let mut duckdb = duckdb.within(Duration::from_secs(1));
    let started = Instant::now();
    let outcome = duckdb.run("SELECT sleep_ms(9223372036854775807::BIGINT)").expect("it answers");
    let elapsed = started.elapsed();
    let Outcome::Error(e) = outcome else { panic!("that call was never going to return rows") };
    assert_eq!(e.kind, "Timeout Error", "{e}");
    assert!(elapsed < Duration::from_secs(30), "it waited {elapsed:?}, so nothing killed it");
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rudb-compat-runaway-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).expect("a file in the scratch directory");
}
