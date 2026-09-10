//! The harness command line.
//!
//! The subcommands are named in `spec/14-rudb-compat.md` and the CI job in the rudb repository
//! calls them by name, so the names are a decision rather than an afterthought. Three of them work
//! now. `reduce` and `report` do not, and they say so rather than printing something empty.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use rudb_compat::Level;
use rudb_compat::compare::MessageMatch;
use rudb_compat::duckdb::{Duckdb, PINNED};
use rudb_compat::engine::{Engine, HarnessError};
use rudb_compat::rudb::Rudb;
use rudb_compat::suite::{Report, run, run_parse, statements};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strict = args.iter().any(|a| a == "--strict-messages");
    let rest: Vec<&str> =
        args.iter().map(String::as_str).filter(|a| !a.starts_with("--strict")).collect();
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
    println!("  levels        print the four compatibility levels and their current status");
    println!("  reduce        shrink a failing query to a minimal reproduction");
    println!("  report        write the published status page from the last run");
    println!("  -V, --version print the version and exit");
    println!();
    println!("  --strict-messages  require error text to match and not only the error kind");
    println!();
    println!("rudb cannot run a query yet, so every statement it accepts comes back as a not");
    println!("implemented error. What the comparison can already settle is which text is SQL.");
    println!("The design is spec/14-rudb-compat.md in https://github.com/tamnd/rudb.");
}
