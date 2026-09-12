//! The harness command line.
//!
//! The subcommands are named in `spec/14-rudb-compat.md` and the CI job in the rudb repository
//! calls them by name, so the names are a decision rather than an afterthought. Three of them work
//! now. `reduce` and `report` do not, and they say so rather than printing something empty.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use std::path::Path;
use std::time::Duration;

use rudb_compat::Level;
use rudb_compat::compare::MessageMatch;
use rudb_compat::conform::{Reason, Skipped, Summary};
use rudb_compat::duckdb::{Duckdb, PINNED, PINNED_COMMIT, Pin};
use rudb_compat::engine::{Engine, HarnessError};
use rudb_compat::isolate::{Isolated, Limits};
use rudb_compat::rudb::Rudb;
use rudb_compat::shell::Shell;
use rudb_compat::suite::{Report, run, run_parse, statements};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strict = args.iter().any(|a| a == "--strict-messages");
    let slow = args.iter().any(|a| a == "--slow");
    let refresh = args.iter().any(|a| a == "--refresh");
    let pinned = args.iter().any(|a| a == "--pinned");
    let through_shells = args.iter().any(|a| a == "--shell");
    let default = Limits::default();
    let limits = Limits {
        time: valued(&args, "--limit").map_or(default.time, Duration::from_secs),
        memory: valued(&args, "--memory")
            .map_or(default.memory, |mb| mb.saturating_mul(1024 * 1024)),
    };
    let rest = positional(&args);
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
        Some("duckdb") => report_on_duckdb(pinned),
        Some("parse") => match rest.get(1) {
            Some(path) => suite(path, messages, Mode::Parse, through_shells),
            None => {
                eprintln!("rudb-compat: parse needs a file of SQL");
                ExitCode::FAILURE
            }
        },
        Some("query") => match rest.get(1) {
            Some(sql) => one(sql, messages, through_shells),
            None => {
                eprintln!("rudb-compat: query needs a statement");
                ExitCode::FAILURE
            }
        },
        Some("run") => match rest.get(1) {
            Some(path) => suite(path, messages, Mode::Run, through_shells),
            None => {
                eprintln!("rudb-compat: run needs a file of SQL");
                ExitCode::FAILURE
            }
        },
        Some("slt") => slt(rest.get(1).copied(), slow, refresh, limits),
        // Not in the help. This is the harness re-running itself for one file, which is how the
        // corpus run survives a query that does not stop, and running it by hand is only ever
        // debugging the runner rather than debugging the engine.
        Some("slt-one") => match (rest.get(1), rest.get(2)) {
            // The seconds and the bytes after the name are the limits the engine is opened with.
            // They are optional so that running this by hand still works, and the runner always
            // passes them.
            (Some(path), Some(name)) => {
                let (timeout, memory) = child_limits(&rest[3..]);
                one_file(Path::new(path), name, timeout, memory)
            }
            _ => {
                eprintln!("rudb-compat: slt-one needs a file and the name to report it under");
                ExitCode::FAILURE
            }
        },
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

/// The flags that take a number, in either the `--flag 30` or the `--flag=30` spelling.
///
/// A flag that is there with something after it that is not a number reads as absent rather than
/// as an error, which is the same thing the rest of this command line does with an argument it did
/// not expect.
fn valued(args: &[String], flag: &str) -> Option<u64> {
    for (at, arg) in args.iter().enumerate() {
        if let Some(rest) = arg.strip_prefix(flag) {
            if let Some(number) = rest.strip_prefix('=') {
                return number.parse().ok();
            }
            if rest.is_empty() {
                return args.get(at + 1).and_then(|next| next.parse().ok());
            }
        }
    }
    None
}

/// The two numbers the runner passes a child after the file and the name.
///
/// A timeout in seconds and a memory budget in bytes, both worked out by the parent out of its own
/// limits, so all that happens here is reading them. Either one missing or unreadable falls back to
/// what the default limits would have given, which is what somebody running this by hand gets.
fn child_limits(rest: &[&str]) -> (Duration, u64) {
    let default = Limits::default();
    let seconds = rest
        .first()
        .and_then(|arg| arg.parse().ok())
        .unwrap_or_else(|| default.statement().as_secs());
    let memory = rest.get(1).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| default.budget());
    (Duration::from_secs(seconds), memory)
}

/// Everything on the command line except the options this file has already read.
///
/// Anything else is left alone, including a flag nobody knows, so that a typed flag still reaches
/// the arm that says it is not a flag rather than being quietly dropped here.
fn positional(args: &[String]) -> Vec<&str> {
    const VALUED: [&str; 2] = ["--limit", "--memory"];
    const PLAIN: [&str; 4] = ["--strict-messages", "--slow", "--refresh", "--pinned"];
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if VALUED.contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        if PLAIN.contains(&arg.as_str())
            || VALUED.iter().any(|flag| arg.starts_with(&format!("{flag}=")))
        {
            continue;
        }
        out.push(arg.as_str());
    }
    out
}

/// Say which DuckDB is on the machine and whether it is the one this project tracks.
///
/// `require` is `--pinned`, which turns anything other than the pinned commit into a failure. The
/// machines that produce published numbers run it that way and CI does not, because CI has a
/// released binary and a fallback run is still worth having.
fn report_on_duckdb(require: bool) -> ExitCode {
    match Duckdb::discover() {
        Ok(db) => {
            let short = &PINNED_COMMIT[..10];
            println!("binary   {}", db.binary().display());
            println!("version  {}", db.version());
            println!("commit   {}", db.commit().unwrap_or("unknown"));
            println!("pinned   {PINNED} at {short}");
            println!();
            match db.pin() {
                Pin::Pinned => {
                    println!("This is the commit the grammar is vendored from.");
                    ExitCode::SUCCESS
                }
                Pin::OtherCommit => {
                    println!(
                        "This is a {PINNED} alpha built at some other commit. The branch moves"
                    );
                    println!(
                        "every day, so this binary and the vendored grammar are two languages"
                    );
                    println!("and a number out of this run is about neither of them on its own.");
                    println!("`scripts/oracle` in the rudb repository builds the pinned one.");
                    if require { ExitCode::FAILURE } else { ExitCode::SUCCESS }
                }
                Pin::Fallback => {
                    println!(
                        "This is not the DuckDB the grammar is vendored from, so a number that"
                    );
                    println!(
                        "comes out of it is about this DuckDB and not about the one rudb tracks."
                    );
                    println!("`scripts/oracle` in the rudb repository builds the pinned one.");
                    if require { ExitCode::FAILURE } else { ExitCode::SUCCESS }
                }
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
fn one(sql: &str, messages: MessageMatch, through_shells: bool) -> ExitCode {
    let statements = vec![sql.to_owned()];
    match go(&statements, messages, Mode::Run, through_shells) {
        Ok((report, pin)) => {
            print(&report, pin);
            verdict(&report)
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Compare every statement in a file.
fn suite(path: &str, messages: MessageMatch, mode: Mode, through_shells: bool) -> ExitCode {
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
    match go(&statements, messages, mode, through_shells) {
        Ok((report, pin)) => {
            print(&report, pin);
            verdict(&report)
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Build both engines and put the statements to them.
///
/// The pin comes back with the report because it belongs to every number in it. A run against a
/// binary that is not the vendored commit is a measurement of a different DuckDB, and the place to
/// say so is next to the percentage rather than in a paragraph somebody has to remember.
fn go(
    statements: &[String],
    messages: MessageMatch,
    mode: Mode,
    through_shells: bool,
) -> Result<(Report, Pin), HarnessError> {
    if through_shells {
        let mut duckdb = Shell::duckdb()?;
        let pin = rudb_compat::duckdb::pin_of(duckdb.version());
        let mut rudb = Shell::rudb()?;
        let report = match mode {
            Mode::Parse => run_parse(&mut duckdb, &mut rudb, statements, messages)?,
            Mode::Run => run(&mut duckdb, &mut rudb, statements, messages)?,
        };
        return Ok((report, pin));
    }
    let mut duckdb = Duckdb::discover()?;
    let pin = duckdb.pin();
    let mut rudb = Rudb::new();
    let report = match mode {
        Mode::Parse => run_parse(&mut duckdb, &mut rudb, statements, messages)?,
        Mode::Run => run(&mut duckdb, &mut rudb, statements, messages)?,
    };
    Ok((report, pin))
}

/// Print the report.
///
/// Every case that disagreed is printed in full, with the statement above the differences, because
/// a report that summarizes is a report somebody has to run again with a flag to make useful.
fn print(report: &Report, pin: Pin) {
    println!("left   {}", report.left);
    println!("right  {}", report.right);
    if pin != Pin::Pinned {
        println!("note   not the pinned commit, so this run is not comparable to a pinned one");
    }
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
fn slt(path: Option<&str>, slow: bool, refresh: bool, limits: Limits) -> ExitCode {
    match path {
        Some(path) => corpus(Path::new(path), slow, limits),
        // No path means the upstream corpus, fetched if it is not already there. That is the run
        // CI does and it is the number the project publishes, so it is the one that takes no
        // argument. Pointing it at a directory is for narrowing down a failure by hand.
        None => match rudb_compat::vendor::corpus(Path::new(root()), refresh) {
            Ok(dir) => corpus(&dir, slow, limits),
            Err(e) => {
                eprintln!("rudb-compat: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Run one file and print what happened in the form the parent process reads back.
///
/// This is the other side of [`rudb_compat::isolate`]. It exits zero whatever the file did, so that
/// a nonzero exit means the process itself came apart and not that a test failed.
fn one_file(path: &Path, name: &str, timeout: Duration, memory: u64) -> ExitCode {
    // Read here rather than letting the runner walk to it, so the file is reported under the name
    // the parent gave it, which is its path inside the corpus and not its basename.
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("rudb-compat: cannot read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let mut rudb = Rudb::limited(timeout, memory);
    let summary = match String::from_utf8(bytes) {
        Ok(text) => match rudb_compat::conform::run_text(&mut rudb, name, &text) {
            Ok(summary) => summary,
            Err(e) => {
                eprintln!("rudb-compat: {e}");
                return ExitCode::FAILURE;
            }
        },
        Err(_) => Summary {
            files: 1,
            skipped_files: vec![(name.to_owned(), Skipped::NotText)],
            ..Summary::default()
        },
    };
    print!("{}", rudb_compat::isolate::encode(&summary));
    ExitCode::SUCCESS
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
fn corpus(path: &Path, slow: bool, limits: Limits) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("rudb-compat: cannot find this binary to re-run it: {e}");
            return ExitCode::FAILURE;
        }
    };
    let total = match rudb_compat::isolate::run_corpus(&exe, path, slow, limits) {
        Ok(total) => total,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    print_corpus(&Rudb::new(), &total);
    ExitCode::SUCCESS
}

/// Print a corpus run.
///
/// Every failure in full, then the counts. The failures come first because a report whose useful
/// part is above the fold is a report people read.
fn print_corpus(engine: &Rudb, total: &Isolated) {
    println!("engine  {} {}", engine.name(), engine.version());
    println!();
    for failure in &total.failures {
        println!("{failure}");
    }
    if !total.skipped_files.is_empty() {
        println!("files not run");
        for (name, why) in &total.skipped_files {
            println!("    {name}: {why}");
        }
        println!();
    }
    if !total.stopped.is_empty() {
        println!("files that were cut off");
        for (name, why) in &total.stopped {
            println!("    {name}: {why}");
        }
        println!();
    }
    println!(
        "{} files, {} passed, {} failed, which is {:.1} percent of what was attempted",
        total.files,
        total.passed,
        total.failed,
        total.rate() * 100.0
    );
    println!("{}", total.skips);
    if !total.stopped.is_empty() {
        println!("{}, and they are counted in neither column", total.cut_off());
    }
    let reasons = total.reasons();
    if reasons.total() > 0 {
        println!();
        print!("{reasons}");
        // Said again on its own line because it is the only row here that is a bug. Everything
        // else on the list is the engine saying it cannot do something, which is a schedule item,
        // and a wrong answer is the engine saying it can and then being wrong. A reader who takes
        // one number away from this report should take this one.
        let wrong = reasons.count(Reason::WrongAnswer);
        println!();
        if wrong == 0 {
            println!("no wrong answers, which is the row that matters");
        } else {
            println!("{wrong} wrong answers, every one of them a bug at priority/p0");
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
    println!("  --pinned           make `duckdb` fail when the binary is not the pinned commit");
    println!("  --shell            drive both engines as command line binaries rather than one");
    println!("                     binary and one linked library, which is what tests the drop in");
    println!("                     claim. Needs a built rudb on PATH or in RUDB_COMPAT_RUDB.");
    println!("  --limit <seconds>  how long one statement may run, 10 by default");
    println!("  --memory <mb>      how large one file may get before it is cut off, 2048 default");
    println!();
    println!("Each file in an slt run gets a process of its own, because the corpus contains");
    println!("queries that are meant to be enormous. Both limits are handed to the engine, which");
    println!("stops the statement itself and leaves a failure the report can count. This process");
    println!("keeps a clock of its own at four times the statement limit and a cap on the size of");
    println!("the child, and a file that reaches either of those is killed and named instead.");
    println!();
    println!("The slt command needs no DuckDB on the machine, because a sqllogictest file already");
    println!("carries what every statement is supposed to produce. Everything else here compares");
    println!("two live engines and needs a duckdb binary on the path.");
    println!("The design is spec/14-rudb-compat.md in https://github.com/tamnd/rudb.");
}
