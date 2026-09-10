//! The harness command line.
//!
//! At this commit it reports what it would run and says plainly that it cannot run it yet. The
//! subcommands are named now because `spec/14-rudb-compat.md` describes them and because the CI
//! job in the rudb repository will call them by name, so the names are a decision rather than an
//! afterthought.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use rudb_compat::Level;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("rudb-compat {VERSION}");
            ExitCode::SUCCESS
        }
        Some("levels") => {
            levels();
            ExitCode::SUCCESS
        }
        Some("run" | "reduce" | "report") => {
            eprintln!("rudb-compat: there is no engine to compare against yet");
            eprintln!("rudb-compat: this arrives with M2, see spec/17-milestones.md in tamnd/rudb");
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
    println!("  levels        print the four compatibility levels and their current status");
    println!("  run           run a suite against a real DuckDB and a rudb build");
    println!("  reduce        shrink a failing query to a minimal reproduction");
    println!("  report        write the published status page from the last run");
    println!("  -V, --version print the version and exit");
    println!();
    println!("Only `levels` works. The rest arrives with M2, when there is an engine to compare");
    println!("against. The design is spec/14-rudb-compat.md in https://github.com/tamnd/rudb.");
}
