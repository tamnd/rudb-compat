//! The harness command line.
//!
//! The subcommands are named in `spec/14-rudb-compat.md` and the CI job in the rudb repository
//! calls them by name, so the names are a decision rather than an afterthought. Three of them work
//! now. `reduce` and `report` do not, and they say so rather than printing something empty.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use std::path::Path;

use rudb_compat::Level;
use rudb_compat::compare::MessageMatch;
use rudb_compat::conform::Summary;
use rudb_compat::duckdb::{Duckdb, PINNED};
use rudb_compat::engine::{Engine, HarnessError};
use rudb_compat::rudb::Rudb;
use rudb_compat::suite::{Report, run, run_parse, statements};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strict = args.iter().any(|a| a == "--strict-messages");
    let slow = args.iter().any(|a| a == "--slow");
    let refresh = args.iter().any(|a| a == "--refresh");
    let rest: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--strict") && *a != "--slow" && *a != "--refresh")
        .collect();
    let messages = if strict { MessageMatch::Headline } else { MessageMatch::Kind };

    match rest.first().copied() {
        Some("--version" | "-V") => {
            println!("rudb-compat {VERSION}");
            ExitCode::SUCCESS
        }
        Some("levels") => {
            levels();
            ExitCode::SUCCESS
        }
        Some("duckdb") => report_on_duckdb(),
        Some("parse") => match rest.get(1) {
            Some(path) => suite(path, messages, Mode::Parse),
            None => {
                eprintln!("rudb-compat: parse needs a file of SQL");
                ExitCode::FAILURE
            }
        },
        Some("query") => match rest.get(1) {
            Some(sql) => one(sql, messages),
            None => {
                eprintln!("rudb-compat: query needs a statement");
                ExitCode::FAILURE
            }
        },
        Some("run") => match rest.get(1) {
            Some(path) => suite(path, messages, Mode::Run),
            None => {
                eprintln!("rudb-compat: run needs a file of SQL");
                ExitCode::FAILURE
            }
        },
        Some("slt") => slt(rest.get(1).copied(), slow, refresh),
        Some("vendor") => fetch(refresh),
        Some("reduce" | "report") => {
            eprintln!("rudb-compat: not built yet, see spec/14-rudb-compat.md in tamnd/rudb");
            ExitCode::FAILURE
        }
        Some("--help" | "-h") | None => {
            help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("rudb-compat: unknown argument {other}");
            eprintln!("rudb-compat: try `rudb-compat --help`");
            ExitCode::FAILURE
        }
    }
}

/// Say which DuckDB is on the machine and whether it is the one this project tracks.
fn report_on_duckdb() -> ExitCode {
    match Duckdb::discover() {
        Ok(db) => {
            println!("binary   {}", db.binary().display());
            println!("version  {}", db.version());
            println!("pinned   {PINNED}");
            if db.is_pinned_version() {
                println!();
                println!("This is the version the grammar is vendored from.");
                ExitCode::SUCCESS
            } else {
                println!();
                println!("This is not the version the grammar is vendored from, so a number that");
                println!("comes out of it is about this DuckDB and not about the one rudb tracks.");
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Which question the run asks of both engines.
#[derive(Clone, Copy)]
enum Mode {
    /// Whether the text is SQL, which is the one both engines can answer today.
    Parse,
    /// What the text returns, which rudb cannot answer yet.
    Run,
}

/// Compare one statement and print the differences.
fn one(sql: &str, messages: MessageMatch) -> ExitCode {
    let statements = vec![sql.to_owned()];
    match go(&statements, messages, Mode::Run) {
        Ok(report) => {
            print(&report);
            verdict(&report)
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Compare every statement in a file.
fn suite(path: &str, messages: MessageMatch, mode: Mode) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rudb-compat: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let statements = statements(&text);
    if statements.is_empty() {
        eprintln!("rudb-compat: {path} has no statements in it");
        return ExitCode::FAILURE;
    }
    match go(&statements, messages, mode) {
        Ok(report) => {
            print(&report);
            verdict(&report)
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Build both engines and put the statements to them.
fn go(statements: &[String], messages: MessageMatch, mode: Mode) -> Result<Report, HarnessError> {
    let mut duckdb = Duckdb::discover()?;
    let mut rudb = Rudb::new();
    match mode {
        Mode::Parse => run_parse(&mut duckdb, &mut rudb, statements, messages),
        Mode::Run => run(&mut duckdb, &mut rudb, statements, messages),
    }
}

/// Print the report.
///
/// Every case that disagreed is printed in full, with the statement above the differences, because
/// a report that summarizes is a report somebody has to run again with a flag to make useful.
fn print(report: &Report) {
    println!("left   {}", report.left);
    println!("right  {}", report.right);
    println!();
    for case in &report.cases {
        if case.agreed() {
            continue;
        }
        println!("{}", case.sql);
        for difference in &case.differences {
            println!("    {difference}");
        }
        println!();
    }
    println!(
        "{} of {} agreed, which is {:.1} percent",
        report.agreed(),
        report.cases.len(),
        report.share() * 100.0
    );
}

/// A run where anything disagreed exits nonzero, so CI does not have to read the text.
fn verdict(report: &Report) -> ExitCode {
    if report.agreed() == report.cases.len() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Run the sqllogictest corpus, either a path that was given or the upstream one.
fn slt(path: Option<&str>, slow: bool, refresh: bool) -> ExitCode {
    match path {
        Some(path) => corpus(Path::new(path), slow),
        // No path means the upstream corpus, fetched if it is not already there. That is the run
        // CI does and it is the number the project publishes, so it is the one that takes no
        // argument. Pointing it at a directory is for narrowing down a failure by hand.
        None => match rudb_compat::vendor::corpus(Path::new(root()), refresh) {
            Ok(dir) => corpus(&dir, slow),
            Err(e) => {
                eprintln!("rudb-compat: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Fetch the corpus and say where it went.
fn fetch(refresh: bool) -> ExitCode {
    match rudb_compat::vendor::corpus(Path::new(root()), refresh) {
        Ok(dir) => {
            let files = count(&dir);
            println!("corpus  {}", dir.display());
            println!("ref     {}", rudb_compat::vendor::REF);
            match rudb_compat::vendor::commit(Path::new(root())) {
                Ok(commit) => println!("commit  {commit}"),
                Err(e) => eprintln!("commit  unknown: {e}"),
            }
            println!("files   {files}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// How many test files are under a directory, for the line `vendor` prints.
fn count(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut total = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += count(&path);
        } else if path.extension().is_some_and(|e| e == "test" || e == "test_slow") {
            total += 1;
        }
    }
    total
}

/// The crate root, which is where the fetched corpus hangs off.
///
/// Cargo sets this at compile time and it is right for `cargo run`. A binary that has been copied
/// somewhere else gets the path it was built at, which is wrong, but the only caller is CI and a
/// developer running the harness out of the checkout it came from.
const fn root() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

/// Run a sqllogictest file or a directory of them against rudb and print the pass rate.
///
/// This exits zero whatever the pass rate is, which is the opposite of what `run` does and is
/// deliberate. The corpus is thousands of statements against a database that is being built, so a
/// nonzero exit would mean the job is red every day until the day it is finished and nobody would
/// read it. What CI watches is the number going down, and the number is what this prints.
fn corpus(path: &Path, slow: bool) -> ExitCode {
    let mut rudb = Rudb::new();
    let summary = match rudb_compat::conform::run_path(&mut rudb, path, slow) {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    print_corpus(&rudb, &summary);
    ExitCode::SUCCESS
}

/// Print a corpus run.
///
/// Every failure in full, then the counts. The failures come first because a report whose useful
/// part is above the fold is a report people read.
fn print_corpus(engine: &Rudb, summary: &Summary) {
    println!("engine  {} {}", engine.name(), engine.version());
    println!();
    for failure in &summary.failures {
        println!("{failure}");
    }
    if !summary.skipped_files.is_empty() {
        println!("files not run");
        for (name, why) in &summary.skipped_files {
            println!("    {name}: {why}");
        }
        println!();
    }
    println!(
        "{} files, {} passed, {} failed, which is {:.1} percent of what was attempted",
        summary.files,
        summary.passed,
        summary.failed,
        summary.rate() * 100.0
    );
    println!("{}", summary.skipped);
}

/// The four levels and where each one currently stands, which is nowhere.
fn levels() {
    println!("level  name                    coverage  against");
    for level in Level::all() {
        println!("{:>5}  {:<22}  {:>8}  nothing", level.number(), level.name(), "not run");
    }
    println!();
    println!("No suite has run. A level with no test behind it is not a claim, per section 14.1.");
}

fn help() {
    println!("rudb-compat {VERSION}");
    println!("The DuckDB compatibility harness for rudb.");
    println!();
    println!("Usage: rudb-compat <command> [options]");
    println!();
    println!("  duckdb        print which DuckDB is on this machine and whether it is the pin");
    println!("  parse <file>  ask both engines which statements in a file are SQL");
    println!("  query <sql>   run one statement on both engines and print the differences");
    println!("  run <file>    run every statement in a file of SQL and print the differences");
    println!("  slt [path]    run sqllogictest and print the pass rate, upstream if no path");
    println!("  vendor        fetch the upstream sqllogictest corpus and say where it went");
    println!("  levels        print the four compatibility levels and their current status");
    println!("  reduce        shrink a failing query to a minimal reproduction");
    println!("  report        write the published status page from the last run");
    println!("  -V, --version print the version and exit");
    println!();
    println!("  --strict-messages  require error text to match and not only the error kind");
    println!("  --slow             include the .test_slow files, which slt leaves out by default");
    println!("  --refresh          fetch the corpus again even if it is already there");
    println!();
    println!("The slt command needs no DuckDB on the machine, because a sqllogictest file already");
    println!("carries what every statement is supposed to produce. Everything else here compares");
    println!("two live engines and needs a duckdb binary on the path.");
    println!("The design is spec/14-rudb-compat.md in https://github.com/tamnd/rudb.");
}
