//! Fetching DuckDB's sqllogictest corpus.
//!
//! Four thousand files and thirty six megabytes, which is why it is fetched rather than committed.
//! A corpus in the repository would be a copy of somebody else's tests that goes stale the moment
//! they change one, and the number we publish has to be against a named DuckDB release or it does
//! not mean anything, so the release is the thing to pin and the files follow from it.
//!
//! The clone lands under `target`, which is already ignored and already the place a build puts
//! things it can recreate. Nothing here is committed and nothing here needs to be.
//!
//! The small corpus in `corpus/slt` is a different thing and it is committed. Those files are
//! ours, they cover what rudb is supposed to do today, and the test suite requires every one of
//! them to pass. The vendored corpus is a measurement and this one is a gate.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::engine::HarnessError;

/// Where the corpus is fetched from.
pub const UPSTREAM: &str = "https://github.com/duckdb/duckdb.git";

/// The release the numbers are published against.
///
/// The same tag `crate::duckdb::PINNED` names and the same one rudb vendors its grammar from. A
/// pass rate against one version and a grammar from another would be two claims about two
/// different databases printed as though they were one.
pub const REF: &str = "v2.0-cyanoptera";

/// Where a fetched corpus goes, relative to the crate root.
pub const DEST: &str = "target/corpus";

/// The corpus directory, fetching it first if it is not already there.
///
/// # Errors
///
/// When git is not on the path, when the clone fails, or when upstream has moved the tests.
pub fn corpus(root: &Path, refresh: bool) -> Result<PathBuf, HarnessError> {
    let dest = root.join(DEST);
    let tests = dest.join("test").join("sql");
    if tests.is_dir() && !refresh {
        return Ok(tests);
    }

    // Everything is fetched into place under one directory and the whole directory is removed
    // first, because a clone that dies halfway leaves a corpus that is part of one release and
    // part of another, and the pass rate from that is a number about nothing.
    let _ = std::fs::remove_dir_all(&dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| HarnessError::new(format!("cannot make {}: {e}", parent.display())))?;
    }

    println!("fetching {UPSTREAM} at {REF}");
    git(&[
        "clone",
        "--quiet",
        "--depth",
        "1",
        "--branch",
        REF,
        "--filter=blob:none",
        "--sparse",
        UPSTREAM,
        &dest.to_string_lossy(),
    ])?;
    // The tests are thirty six megabytes and the repository is not. A blobless clone still walks
    // the whole tree, and a sparse checkout is what keeps the working copy to the part we read.
    git(&["-C", &dest.to_string_lossy(), "sparse-checkout", "set", "test/sql"])?;

    if !tests.is_dir() {
        return Err(HarnessError::new(format!(
            "upstream layout changed, test/sql is missing at {REF}"
        )));
    }
    Ok(tests)
}

/// The commit the corpus is at, for the report.
///
/// # Errors
///
/// When the directory is not a clone.
pub fn commit(root: &Path) -> Result<String, HarnessError> {
    let dest = root.join(DEST);
    let out = Command::new("git")
        .args(["-C", &dest.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .map_err(|e| HarnessError::new(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(HarnessError::new(format!("{} is not a clone", dest.display())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Run git and fail with what it said.
fn git(args: &[&str]) -> Result<(), HarnessError> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| HarnessError::new(format!("cannot run git: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(HarnessError::new(format!(
        "git {} failed\n{}",
        args.first().copied().unwrap_or(""),
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}
