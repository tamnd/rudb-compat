//! Fetching DuckDB's own tests and benchmarks.
//!
//! Four thousand files and thirty six megabytes, which is why it is fetched rather than committed.
//! A corpus in the repository would be a copy of somebody else's tests that goes stale the moment
//! they change one, and the number we publish has to be against a named DuckDB release or it does
//! not mean anything, so the release is the thing to pin and the files follow from it.
//!
//! The clone lands under `target`, which is already ignored and already the place a build puts
//! things it can recreate. Nothing here is committed and nothing here needs to be. That choice has
//! two sharp edges and between them they cost every conformance number CI ever published. One is
//! that a Rust build cache saves and restores `target`, and what comes back is not what went in.
//! The other is that `target` is inside this repository, so a git command pointed at the corpus
//! answers from this repository when the corpus is not a clone. The private `filled` and `own`
//! below are the two halves of not being fooled by either, and they carry the measurements.
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
        // Both halves have to hold: the directory has to be our own clone, and the widening has to
        // work. Neither is an error, because the clone below is the answer to both.
        if own(&dest) && git(&sparse(&dest)).is_ok() && here(&dest) {
            return Ok(dest);
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
/// Guarded by `own` below, because a `rev-parse HEAD` that walks up answers with a commit, and a
/// corpus commit that is really this repository's own HEAD is a worse report than no commit at all.
/// CI published exactly that on every run it ever made.
///
/// # Errors
///
/// When the directory is not a clone of its own.
pub fn commit(root: &Path) -> Result<String, HarnessError> {
    let dest = root.join(DEST);
    if !own(&dest) {
        return Err(HarnessError::new(format!("{} is not a clone", dest.display())));
    }
    let out = Command::new("git")
        .args(["-C", &dest.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .map_err(|e| HarnessError::new(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(HarnessError::new(format!("{} is not a clone", dest.display())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Whether the directory is the root of a clone of its own, rather than a directory inside one.
///
/// Git looks for a repository by walking up from where it is pointed, so `git -C target/corpus`
/// answers from this repository whenever `target/corpus` is not a repository itself. Every git
/// command here runs against a path under `target`, which is always inside this repository, so
/// every one of them has somewhere to walk up to and none of them fail the way the code expected.
///
/// Two things came of that, and they are the same mistake read twice. The gentler one is that the
/// corpus commit CI published on every run was this repository's own HEAD: the run on the pull
/// request before this one reported `2016100497f1`, which is not a DuckDB commit, it is that pull
/// request's merge commit. The other one is what this function was written for. When the corpus
/// directory came back from the build cache emptied, the code saw a `.git` under it and tried to
/// widen the sparse checkout, git walked up, and `sparse-checkout set test/sql benchmark ...` was
/// applied to the working tree of rudb-compat. That leaves the root files and removes every
/// directory outside the cone, so `Cargo.toml` survived and `src` did not, and the next step said
/// `no targets specified in the manifest`.
///
/// Comparing paths rather than trusting an exit code, because the exit code is zero either way.
fn own(dest: &Path) -> bool {
    let Ok(out) = Command::new("git")
        .args(["-C", &dest.to_string_lossy(), "rev-parse", "--show-toplevel"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    // Through `canonicalize` because a temporary directory on macOS is reached by a symlink and
    // git answers with the resolved path, so the two spellings are the same directory.
    match (top.canonicalize(), dest.canonicalize()) {
        (Ok(top), Ok(dest)) => top == dest,
        _ => top == dest,
    }
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

    use super::{PARTS, filled, own};

    /// Make a directory a git repository, or say why the test cannot ask this question.
    fn init(dir: &Path) -> bool {
        std::process::Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "init", "--quiet"])
            .status()
            .is_ok_and(|s| s.success())
    }

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

    /// The shape that took CI's workspace apart. `target` is inside this repository, so a git
    /// command pointed at a corpus that is not a clone walks up and answers from the repository
    /// that contains it, with an exit code of zero and somebody else's working tree under it.
    #[test]
    fn a_directory_inside_a_repository_is_not_a_clone_of_its_own() {
        let outer = scratch("outer");
        if !init(&outer) {
            eprintln!("skipping, no git");
            return;
        }
        let inner = outer.join("target/corpus");
        std::fs::create_dir_all(&inner).expect("a directory");
        assert!(own(&outer), "the repository is its own clone");
        assert!(!own(&inner), "a directory inside one is not, however far down it is");

        // And the half of it that actually did the damage: a `.git` that is a directory is not a
        // repository, which is what the build cache handed back and what the old check believed.
        let gutted = inner.join(".git");
        std::fs::create_dir_all(gutted.join("refs/heads")).expect("a directory");
        assert!(gutted.is_dir(), "the old check asked this and stopped there");
        assert!(!own(&inner), "and git still answers from the repository above it");
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn a_directory_that_is_not_in_a_repository_at_all_is_not_a_clone_either() {
        let dir = scratch("loose");
        // Only meaningful when the temp directory is not itself inside somebody's checkout, which
        // is the normal case and the reason this is an assertion about `own` and not about git.
        if own(&dir) {
            eprintln!("skipping, the temp directory is a repository root");
            return;
        }
        assert!(!own(&dir));
        let _ = std::fs::remove_dir_all(&dir);
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
