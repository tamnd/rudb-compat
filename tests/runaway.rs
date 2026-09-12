//! A file the engine has to stop, run the way the corpus run runs it.
//!
//! The upstream corpus has files that ask for more rows than exist and more memory than the
//! machine has, on purpose, because giving up is one of the behaviours being tested. Seven of them
//! were killed from outside by the runner, which left them counted in neither column: not a pass,
//! not a failure, just a file nobody knows anything about.
//!
//! This is the two ways that goes wrong, written small enough to run in a second. The engine is
//! given a one second clock and a one megabyte budget, and both files come back as records with an
//! outcome rather than as processes that had to be killed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rudb_compat::conform::Reason;
use rudb_compat::isolate::{Limits, run_corpus};

#[test]
fn a_query_the_engine_stops_is_a_failed_record_and_not_a_killed_process() {
    let dir = scratch();
    write(
        &dir,
        "clock.test",
        "query I\nSELECT count(*) FROM range(100000000000);\n----\n100000000000\n",
    );
    write(
        &dir,
        "budget.test",
        "query I\nSELECT count(*) FROM (SELECT * FROM range(10000000) ORDER BY range) t;\n\
         ----\n10000000\n",
    );

    let exe = Path::new(env!("CARGO_BIN_EXE_rudb-compat"));
    // A second, which is four before this runner would step in, and a cap the engine gets half of.
    // The cap is not small: it is the size of the process and the process is a database, so a cap
    // the engine could actually reach first has to leave room for the binary underneath it.
    let limits = Limits { time: Duration::from_secs(1), memory: 128 * 1024 * 1024 };
    let run = run_corpus(exe, &dir, false, limits).expect("the files are there");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(run.stopped.is_empty(), "a file was killed from outside: {:?}", run.stopped);
    assert_eq!(run.files, 2);
    assert_eq!(run.failed, 2, "{:?}", run.failures);
    for failure in &run.failures {
        assert_eq!(failure.reason, Reason::Stopped, "{failure}");
    }

    // And it says which limit did it, because the two are different problems. One is a query that
    // is too slow and the other is a query that is too large.
    let details: Vec<&str> = run.failures.iter().map(|f| f.detail.as_str()).collect();
    assert!(details.iter().any(|d| d.contains("Interrupt Error")), "{details:?}");
    assert!(details.iter().any(|d| d.contains("Out of Memory Error")), "{details:?}");
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rudb-compat-runaway-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).expect("a file in the scratch directory");
}
