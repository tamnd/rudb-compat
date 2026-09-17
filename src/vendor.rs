//! Fetching DuckDB's own tests and benchmarks.
//!
//! Four thousand files and thirty six megabytes, which is why it is fetched rather than committed.
//! A corpus in the repository would be a copy of somebody else's tests that goes stale the moment
//! they change one, and the number we publish has to be against a named DuckDB release or it does
//! not mean anything, so the release is the thing to pin and the files follow from it.
//!
//! The clone lands under `target`, which is already ignored and already the place a build puts
//! things it can recreate. Nothing here is committed and nothing here needs to be. That choice has
//! one sharp edge and it cost every conformance number CI ever published: a Rust build cache saves
//! and restores `target`, and what comes back is not always what went in. See [`filled`].
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
    tests(&checkout(root, refresh)?)
}

/// The tests under a checkout, once it is known to be one.
///
/// Split out from [`corpus`] because the two ways this can be wrong want different sentences and
/// both are worth a test, and a test of [`corpus`] itself is a test that clones DuckDB.
///
/// # Errors
///
/// When upstream has moved the tests, and when the directory they were at holds nothing.
fn tests(dest: &Path) -> Result<PathBuf, HarnessError> {
    let sql = dest.join("test").join("sql");
    if !sql.is_dir() {
        return Err(HarnessError::new(format!(
            "upstream layout changed, test/sql is missing at {REF}"
        )));
    }
    // A corpus with nothing in it is not a corpus that scores zero, it is a fetch that did not
    // happen, and the two print the same way if nobody asks. This is the backstop for that: the
    // caller gets an error it has to say out loud instead of a directory it measures nothing in.
    if !filled(&sql) {
        return Err(HarnessError::new(format!(
            "the corpus at {} has nothing in it, so it was never fetched",
            sql.display()
        )));
    }
    Ok(sql)
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
    let here = |dest: &Path| PARTS.iter().all(|part| filled(&dest.join(part)));
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

/// Whether a part of the checkout is there, which means holding something rather than existing.
///
/// This is the whole of the bug it was written for. The corpus lives under `target`, CI restores
/// `target` from a Rust build cache, and that cache hands back the directory tree with the files
/// taken out of it: `target/corpus/test/sql` comes back as a directory with nothing in it, and
/// enough of `target/corpus/.git` comes back that `git rev-parse` still answers. Asking `is_dir`
/// said the corpus was already fetched, so nothing was fetched, and `slt` measured four thousand
/// one hundred and thirty seven files as zero.
///
/// Every conformance run this repository has ever published said `0 files, 0 passed, 0 failed,
/// which is 0.0 percent`, from the first one on 2026-09-14 to the one that found this. A number
/// that is zero because nothing ran looks exactly like a number that is zero because nothing
/// passed, which is why it survived a fortnight of being on the run summary.
///
/// Emptiness rather than a file count, because this is on the path of every run and the answer only
/// has to separate a checkout from the shape of one. When it says no, the caller re-clones, which
/// is thirty seconds and correct.
fn filled(part: &Path) -> bool {
    std::fs::read_dir(part).is_ok_and(|mut entries| entries.next().is_some())
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{PARTS, filled};

    /// A directory of our own to build checkout shapes in, named so a leftover says whose it was.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rudb-compat-vendor-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory under the temp directory");
        dir
    }

    /// The four parts, as directories and nothing else.
    fn shape(dest: &Path) {
        for part in PARTS {
            std::fs::create_dir_all(dest.join(part)).expect("a directory");
        }
    }

    /// What CI restored from the build cache for a fortnight. Every part is a directory, so asking
    /// `is_dir` said the corpus was fetched, and none of them held a single file.
    #[test]
    fn a_directory_tree_with_the_files_taken_out_is_not_a_fetched_corpus() {
        let dest = scratch("emptied");
        shape(&dest);
        for part in PARTS {
            assert!(
                dest.join(part).is_dir(),
                "{part} is a directory, which is how this got through"
            );
            assert!(!filled(&dest.join(part)), "{part} holds nothing, so it is not there");
        }
        assert!(super::tests(&dest).is_err(), "an empty test/sql is a fetch that did not happen");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn a_part_holding_a_file_is_there() {
        let dest = scratch("filled");
        shape(&dest);
        let sql = dest.join("test/sql");
        std::fs::write(sql.join("select.test"), "statement ok\nSELECT 1\n").expect("a file");
        assert!(filled(&sql));
        assert_eq!(super::tests(&dest).expect("the tests are there"), sql);
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// A subdirectory counts, because upstream keeps the tests in one and an empty check that
    /// recursed would be doing the fetch's job.
    #[test]
    fn a_part_holding_only_directories_is_there_too() {
        let dest = scratch("nested");
        shape(&dest);
        std::fs::create_dir_all(dest.join("test/sql/join")).expect("a directory");
        assert!(filled(&dest.join("test/sql")));
        assert!(super::tests(&dest).is_ok());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn a_directory_that_is_not_there_at_all_is_not_filled() {
        assert!(!filled(&scratch("absent").join("nothing/here")));
    }

    /// The two ways this goes wrong want different sentences: one is upstream moving the tests and
    /// one is the fetch not having run, and somebody reading the second as the first goes looking
    /// for a commit in DuckDB that does not exist.
    #[test]
    fn upstream_moving_the_tests_and_the_fetch_not_running_do_not_say_the_same_thing() {
        let gone = scratch("gone");
        let moved = super::tests(&gone).expect_err("there is no test/sql at all");
        let dest = scratch("never");
        shape(&dest);
        let never = super::tests(&dest).expect_err("test/sql is empty");
        assert!(moved.to_string().contains("upstream layout changed"), "{moved}");
        assert!(never.to_string().contains("never fetched"), "{never}");
        let _ = std::fs::remove_dir_all(&gone);
        let _ = std::fs::remove_dir_all(&dest);
    }
}
