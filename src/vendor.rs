//! Fetching DuckDB's own tests and benchmarks.
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

/// The parts of upstream the checkout holds.
///
/// The tests are what the pass rate is measured over. The benchmarks are the queries somebody wrote
/// because they cared how fast something was rather than because they wanted to break an engine,
/// and `crate::queries` reads them for the weights. The last two are there because the TPC-H and
/// TPC-DS benchmarks do not hold their own queries: each one is four lines naming a template, and
/// the template runs a `.sql` file that lives beside the generator. Without those two directories
/// the corpus is missing a hundred and twenty one of the best known queries in the business.
pub const PARTS: [&str; 4] =
    ["test/sql", "benchmark", "extension/tpch/dbgen/queries", "extension/tpcds/dsdgen/queries"];

/// The corpus directory, fetching it first if it is not already there.
///
/// # Errors
///
/// When git is not on the path, when the clone fails, or when upstream has moved the tests.
pub fn corpus(root: &Path, refresh: bool) -> Result<PathBuf, HarnessError> {
    let dest = checkout(root, refresh)?;
    let tests = dest.join("test").join("sql");
    if !tests.is_dir() {
        return Err(HarnessError::new(format!(
            "upstream layout changed, test/sql is missing at {REF}"
        )));
    }
    Ok(tests)
}

/// The whole checkout, fetching it first if it is not already there.
///
/// A checkout made before a part was added to [`PARTS`] is widened rather than thrown away, because
/// the expensive half of this is the clone and the parts come out of the same commit either way.
///
/// # Errors
///
/// When git is not on the path, when the clone fails, or when upstream has moved the tests.
pub fn checkout(root: &Path, refresh: bool) -> Result<PathBuf, HarnessError> {
    let dest = root.join(DEST);
    let here = |dest: &Path| PARTS.iter().all(|part| dest.join(part).is_dir());
    if !refresh {
        if here(&dest) {
            return Ok(dest);
        }
        if dest.join(".git").is_dir() {
            git(&sparse(&dest))?;
            if here(&dest) {
                return Ok(dest);
            }
        }
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
    // the whole tree, and a sparse checkout is what keeps the working copy to the parts we read.
    git(&sparse(&dest))?;

    for part in PARTS {
        if !dest.join(part).is_dir() {
            return Err(HarnessError::new(format!(
                "upstream layout changed, {part} is missing at {REF}"
            )));
        }
    }
    Ok(dest)
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

/// The sparse checkout command for every part, as git wants it.
fn sparse(dest: &Path) -> Vec<String> {
    let mut args = vec![
        "-C".to_owned(),
        dest.to_string_lossy().into_owned(),
        "sparse-checkout".to_owned(),
        "set".to_owned(),
    ];
    args.extend(PARTS.iter().map(|part| (*part).to_owned()));
    args
}

/// Run git and fail with what it said.
fn git<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<(), HarnessError> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| HarnessError::new(format!("cannot run git: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(HarnessError::new(format!(
        "git {} failed\n{}",
        args.first().map(|a| a.as_ref().to_string_lossy()).unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}
