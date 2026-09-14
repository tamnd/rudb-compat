//! The harness command line.
//!
//! The subcommands are named in `spec/14-rudb-compat.md` and the CI job in the rudb repository
//! calls them by name, so the names are a decision rather than an afterthought. All of them work.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rudb_compat::Level;
use rudb_compat::compare::{MessageMatch, Ordering, Rules};
use rudb_compat::conform::{Reason, Skipped, Summary};
use rudb_compat::cost::{Costs, Measured};
use rudb_compat::duckdb::{Duckdb, PINNED, PINNED_COMMIT, Pin};
use rudb_compat::engine::{Engine, HarnessError};
use rudb_compat::isolate::{Isolated, Limits};
use rudb_compat::oracles::{Split, Verdict};
use rudb_compat::queries::{Histogram, Query, histogram};
use rudb_compat::reduce::{Alive, BUDGET, Distinct, Reduced, shrink};
use rudb_compat::report::{Page, Provenance, Sweep};
use rudb_compat::resource::{RUNS, Ratios, Spread};
use rudb_compat::rudb::Rudb;
use rudb_compat::shell::{Session, Shell};
use rudb_compat::sqlsmith::QUERIES;
use rudb_compat::suite::{Measure, Report, run, run_parse, statements};
use rudb_compat::tlp::CASES;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strict = args.iter().any(|a| a == "--strict-messages");
    let slow = args.iter().any(|a| a == "--slow");
    let refresh = args.iter().any(|a| a == "--refresh");
    let pinned = args.iter().any(|a| a == "--pinned");
    let through_shells = args.iter().any(|a| a == "--shell");
    // Only the shell driver measures, because it is the only place both engines are processes
    // reached the same way. Asking for numbers without asking for shells would produce a run with
    // nothing in the resource block and no explanation, so it says so instead.
    let measure = if args.iter().any(|a| a == "--measure") {
        if !through_shells {
            eprintln!("rudb-compat: --measure needs --shell, because only the shells are measured");
            return ExitCode::FAILURE;
        }
        Measure::On
    } else {
        Measure::Off
    };
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
            Some(path) => suite(path, messages, Mode::Parse, through_shells, measure),
            None => {
                eprintln!("rudb-compat: parse needs a file of SQL");
                ExitCode::FAILURE
            }
        },
        Some("query") => match rest.get(1) {
            Some(sql) => one(sql, messages, through_shells, measure),
            None => {
                eprintln!("rudb-compat: query needs a statement");
                ExitCode::FAILURE
            }
        },
        Some("run") => match rest.get(1) {
            Some(path) => suite(path, messages, Mode::Run, through_shells, measure),
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
        Some("functions") => functions(rest.get(1).copied(), pinned),
        Some("coverage") => coverage(rest.get(1).copied(), pinned, messages, text(&args, "--out")),
        Some("oracles") => oracles(rest.get(1).copied(), slow, refresh),
        Some("bisect") => match rest.get(1) {
            Some(sql) => bisect(sql, messages),
            None => {
                eprintln!("rudb-compat: bisect needs a statement");
                ExitCode::FAILURE
            }
        },
        Some("sqlsmith") => sqlsmith(
            valued(&args, "--count").map_or(QUERIES, |n| usize::try_from(n).unwrap_or(QUERIES)),
            valued(&args, "--seed").and_then(|n| u32::try_from(n).ok()),
            messages,
        ),
        Some("tlp") => tlp(
            valued(&args, "--count").map_or(CASES, |n| usize::try_from(n).unwrap_or(CASES)),
            valued(&args, "--seed"),
            pinned,
        ),
        Some("queries") => queries(
            refresh,
            pinned,
            valued(&args, "--limit").map_or(NAMES, |n| usize::try_from(n).unwrap_or(NAMES)),
        ),
        Some("cost") => cost(
            refresh,
            valued(&args, "--count").and_then(|n| usize::try_from(n).ok()),
            valued(&args, "--runs").map_or(RUNS, |n| usize::try_from(n).unwrap_or(RUNS)),
            text(&args, "--group"),
            valued(&args, "--seconds").unwrap_or(SECONDS),
        ),
        Some("vendor") => fetch(refresh),
        Some("report") => report(rest.get(1).copied(), slow, refresh, limits, text(&args, "--out")),
        Some("reduce") => reduce(
            rest.get(1).copied(),
            text(&args, "--file"),
            messages,
            through_shells,
            valued(&args, "--budget").map_or(BUDGET, |n| usize::try_from(n).unwrap_or(BUDGET)),
        ),
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

/// The flags that take a word rather than a number, in either spelling.
fn text<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    for (at, arg) in args.iter().enumerate() {
        if let Some(rest) = arg.strip_prefix(flag) {
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value);
            }
            if rest.is_empty() {
                return args.get(at + 1).map(String::as_str);
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
    const VALUED: [&str; 10] = [
        "--limit",
        "--memory",
        "--out",
        "--budget",
        "--file",
        "--count",
        "--seed",
        "--runs",
        "--group",
        "--seconds",
    ];
    const PLAIN: [&str; 6] =
        ["--strict-messages", "--slow", "--refresh", "--pinned", "--shell", "--measure"];
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
fn one(sql: &str, messages: MessageMatch, through_shells: bool, measure: Measure) -> ExitCode {
    let statements = vec![sql.to_owned()];
    match go(&statements, messages, Mode::Run, through_shells, measure) {
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

/// Name the optimizer pass that changed the answer to one statement.
///
/// It runs the statement on the pinned DuckDB once to find out what the answer should be, and then
/// runs it on rudb once per pass with that pass turned off, and once more with all of them off. The
/// answer is which passes off make rudb agree, or that none of them do, which says the optimizer is
/// not where to look at all and is the answer most often wanted.
///
/// It exits successfully whatever it finds, including when the two engines already agree, because
/// it is asked which pass rather than whether there is a difference and `query` is the command that
/// answers the second question by failing.
///
/// It drives rudb as the linked library rather than as a shell. A `SET` only sticks in an engine
/// that has a session, the library driver holds one connection open for the life of the run, and a
/// bare shell would forget the `SET` before the statement after it. The DuckDB side is the pinned
/// binary either way, since nothing is being turned off there.
fn bisect(sql: &str, messages: MessageMatch) -> ExitCode {
    let mut duckdb = match Duckdb::discover() {
        Ok(duckdb) => duckdb,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let want = match duckdb.run(sql) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rules = Rules { ordering: Ordering::Sorted, messages };
    let mut rudb = Rudb::new();
    let got = match rudb.run(sql) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    if rudb_compat::compare::compare(&got, &want, rules).is_empty() {
        println!("the two engines already agree about this statement, so there is no pass to name");
        return ExitCode::SUCCESS;
    }
    match rudb_compat::bisect::blame(&mut rudb, sql, &want, rules) {
        Ok(blame) => {
            println!("{blame}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Shrink one failing statement down to the smallest one that still fails the same way.
///
/// It exits successfully when it reduced something, which is the opposite of what `query` does with
/// the same difference. The two commands are asked different questions. `query` is asked whether
/// these engines agree and answers no by failing. `reduce` is pointed at a disagreement somebody
/// already has and asked to make it small, and it did that or it did not.
///
/// A `--file` with more than one statement in it goes to [`triage`] instead, which is the same
/// reduction over each of them with the results grouped.
fn reduce(
    sql: Option<&str>,
    file: Option<&str>,
    messages: MessageMatch,
    through_shells: bool,
    budget: usize,
) -> ExitCode {
    let from_file = match file.map(std::fs::read_to_string) {
        Some(Ok(text)) => Some(text),
        Some(Err(e)) => {
            eprintln!("rudb-compat: cannot read {}: {e}", file.unwrap_or_default());
            return ExitCode::FAILURE;
        }
        None => None,
    };
    let many = from_file.as_deref().map(statements).unwrap_or_default();
    if many.len() > 1 {
        return triage(&many, messages, through_shells, budget);
    }
    let Some(sql) = many.first().map(String::as_str).or(sql) else {
        eprintln!("rudb-compat: reduce needs a statement, or a file of them behind --file");
        return ExitCode::FAILURE;
    };
    let (mut left, mut right) = match engines(through_shells) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut alive = match Alive::of(&mut *left, &mut *right, sql, messages) {
        Ok(alive) => alive,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("keeping this alive, and a step that loses all of it is not kept");
    for one in alive.keeping() {
        println!("    {one}");
    }
    println!();
    let reduced = match shrink(sql, budget, &mut |candidate| alive.keeps(candidate)) {
        Ok(reduced) => reduced,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{reduced}");
    match alive.differences(&reduced.sql) {
        Ok(differences) => {
            for difference in &differences {
                println!("    {difference}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reduce every statement in a file that the two engines disagree about, and group the results.
///
/// This is what the reducer is for at scale. A corpus produces thousands of failing statements and
/// they are not thousands of bugs, they are a few dozen bugs each found by every query that happens
/// to touch one, so each failure is shrunk and then the shrunken ones are counted by hash. What
/// comes out is a list somebody can work through.
///
/// A statement the two engines agree about is counted and skipped, because reducing something that
/// does not fail has nothing to keep alive.
///
/// Progress goes to standard error under `RUDB_COMPAT_WATCH`, the same as everywhere else here,
/// because a thousand reductions at up to a budget of engine runs each is a long time to print
/// nothing.
fn triage(all: &[String], messages: MessageMatch, through_shells: bool, budget: usize) -> ExitCode {
    let watching = std::env::var_os("RUDB_COMPAT_WATCH").is_some();
    let (mut left, mut right) = match engines(through_shells) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut distinct = Distinct::new();
    for (at, sql) in all.iter().enumerate() {
        match reduce_one(&mut *left, &mut *right, sql, messages, budget) {
            Ok(Some((reduced, signatures))) => distinct.add(sql, &reduced.sql, &signatures),
            Ok(None) => distinct.agreed(),
            Err(e) => {
                eprintln!("rudb-compat: {e}");
                return ExitCode::FAILURE;
            }
        }
        if watching {
            eprintln!("{} of {}, {} distinct", at + 1, all.len(), distinct.cases().len());
        }
    }
    print!("{distinct}");
    ExitCode::SUCCESS
}

/// Reduce one statement, or say that there was nothing to reduce.
///
/// `None` is the two engines agreeing, which is not an error and not a case.
fn reduce_one(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    sql: &str,
    messages: MessageMatch,
    budget: usize,
) -> Result<Option<(Reduced, Vec<String>)>, HarnessError> {
    let Some(mut alive) = Alive::at(left, right, sql, messages)? else { return Ok(None) };
    let signatures = alive.keeping();
    let reduced = shrink(sql, budget, &mut |candidate| alive.keeps(candidate))?;
    Ok(Some((reduced, signatures)))
}

/// The left engine and the right one, in the order everything here puts them in.
type Pair = (Box<dyn Engine>, Box<dyn Engine>);

/// Both engines, as the pair of things a comparison needs rather than as their own types.
///
/// `go` builds the same two without boxing because it hands them straight to a function that takes
/// them by concrete type. Anything that keeps hold of both for longer than one call wants them
/// behind the trait, because the only difference between the two ways of building them is which
/// process the SQL ends up in.
fn engines(through_shells: bool) -> Result<Pair, HarnessError> {
    if through_shells {
        return Ok((Box::new(Shell::duckdb()?), Box::new(Shell::rudb()?)));
    }
    Ok((Box::new(Duckdb::discover()?), Box::new(Rudb::new())))
}

/// Both engines as shells that remember what they were told.
///
/// A test file is a session. It makes a table and then asks questions about it, so a driver that
/// forgets between statements fails every record after the first one for a reason that has nothing
/// to do with the record. `crate::shell::Session` is the only thing here that does not forget,
/// because it replays the statements that left something behind in front of the next one, and it is
/// the only driver a whole file can be run through and mean anything.
fn sessions() -> Result<Pair, HarnessError> {
    Ok((Box::new(Session::new(Shell::duckdb()?)), Box::new(Session::new(Shell::rudb()?))))
}

/// Compare every statement in a file.
fn suite(
    path: &str,
    messages: MessageMatch,
    mode: Mode,
    through_shells: bool,
    measure: Measure,
) -> ExitCode {
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
    match go(&statements, messages, mode, through_shells, measure) {
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
    measure: Measure,
) -> Result<(Report, Pin), HarnessError> {
    if through_shells {
        let mut duckdb = Shell::duckdb()?;
        let pin = rudb_compat::duckdb::pin_of(duckdb.version());
        let mut rudb = Shell::rudb()?;
        let report = match mode {
            Mode::Parse => run_parse(&mut duckdb, &mut rudb, statements, messages)?,
            Mode::Run => run(&mut duckdb, &mut rudb, statements, messages, measure)?,
        };
        return Ok((report, pin));
    }
    let mut duckdb = Duckdb::discover()?;
    let pin = duckdb.pin();
    let mut rudb = Rudb::new();
    let report = match mode {
        Mode::Parse => run_parse(&mut duckdb, &mut rudb, statements, messages)?,
        Mode::Run => run(&mut duckdb, &mut rudb, statements, messages, Measure::Off)?,
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
    resources(report);
}

/// Print what the run cost, when it measured anything.
///
/// Three ratios and not one, and a median with the quartiles beside it and never a minimum, per
/// `spec/sql/duckdb/11-the-number.md` section 11.2. The worst records are printed under them
/// because the median is the statistic that hides the one shape that is two hundred times slower,
/// and that shape is a bug rather than a distribution.
fn resources(report: &Report) {
    let Some(ratios) = report.ratios() else { return };
    println!();
    println!("rudb over duckdb, median with the quartiles, over {} records", ratios.time.count);
    let line = |what: &str, spread: &Spread| {
        println!("  {what:6} {:.2}  [{:.2} {:.2}]", spread.median, spread.low, spread.high);
    };
    line("time", &ratios.time);
    line("cpu", &ratios.cpu);
    line("memory", &ratios.memory);
    println!("  the goal is {:.1} on all three", Ratios::GOAL);
    let worst = report.worst(5);
    if !worst.is_empty() {
        println!();
        println!("slowest records, worst first");
        for (sql, ratio) in worst {
            println!("  {ratio:8.2}  {}", sql.replace('\n', " "));
        }
    }
}

/// A run where anything disagreed exits nonzero, so CI does not have to read the text.
fn verdict(report: &Report) -> ExitCode {
    if report.agreed() == report.cases.len() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Run the sqllogictest corpus, either a path that was given or the upstream one.
fn slt(path: Option<&str>, slow: bool, refresh: bool, limits: Limits) -> ExitCode {
    let run = corpus_dir(path, refresh).and_then(|dir| corpus(&dir, slow, limits));
    match run {
        Ok(total) => {
            print_corpus(&Rudb::new(), &total);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run the corpus against both oracles, the file and the pinned binary, and print what they split
/// on.
///
/// It exits nonzero when there is a harness bug in the answer, and only then. A stale file is not
/// this project's problem and an engine gap is what the ordinary run already reports, but a record
/// rudb passes that the pinned binary fails is a pass this harness has not earned, and that is
/// exactly the thing a run like this exists to catch.
///
/// It always drives both engines through `sessions`, which is not a preference. A test file makes a
/// table and then asks questions about it, so a driver that forgets between statements fails every
/// record after the first one for a reason that has nothing to do with the record. The library pair
/// forgets on one side only, because the linked rudb keeps a connection open and the DuckDB driver
/// spawns a fresh in memory process per statement, so under it every record after the first
/// `CREATE TABLE` passes on rudb and fails on the binary and the whole corpus reads as a harness
/// bug. That is one harness bug reported thousands of times and it hides everything else. Two bare
/// shells forget on both sides instead, which cancels out into a stale file rather than a harness
/// bug, and a run where four fifths of the records are the pin failing to find a table it was never
/// told about is a run that measures nothing.
fn oracles(path: Option<&str>, slow: bool, refresh: bool) -> ExitCode {
    let dir = match corpus_dir(path, refresh) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (mut duckdb, mut rudb) = match sessions() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let both = match rudb_compat::oracles::over(&mut *rudb, &mut *duckdb, &dir, slow) {
        Ok(both) => both,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{both}");
    // The harness bugs in full, because the whole point of finding them is fixing them and a count
    // is not something anybody can fix. The other two kinds are listed by file and line only,
    // because there are tens of thousands of them and they are already on the corpus report.
    let bugs: Vec<&Split> =
        both.splits.iter().filter(|split| split.verdict == Verdict::Harness).collect();
    if !bugs.is_empty() {
        println!();
        println!("the records this runner gets wrong");
        for split in &bugs {
            println!();
            println!("{split}");
        }
    }
    if bugs.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Generate queries with upstream's own generator and put every one of them to both engines.
///
/// The generator is a DuckDB with the sqlsmith extension loaded, which on a machine that has the
/// pin is usually not the pin, because extension binaries are published for releases and the pin is
/// a development commit. That is not a compromise. What comes out of a generator is SQL text and
/// text has no version, and every statement it writes is still put to the pinned binary and to rudb
/// here, which is the run that decides anything.
///
/// Both engines are driven through sessions, because the tables have to be there when the query
/// runs and a driver that forgets between statements would put every generated query to an empty
/// catalog and find one difference over and over.
///
/// The seed is printed whether it was given or not, because a generated run whose findings cannot
/// be replayed is a generated run whose findings do not get fixed.
fn sqlsmith(how_many: usize, seed: Option<u32>, messages: MessageMatch) -> ExitCode {
    let generator = match rudb_compat::sqlsmith::Generator::find() {
        Ok(found) => found,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let seed = seed.unwrap_or_else(fresh_seed);
    let generated = match generator.generate(how_many, seed) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let setup = rudb_compat::sqlsmith::catalog();
    let mut statements = setup.clone();
    statements.extend(generated);
    let (mut duckdb, mut rudb) = match sessions() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let report = match run(&mut *duckdb, &mut *rudb, &statements, messages, Measure::Off) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", rudb_compat::sqlsmith::Found::of(&report, seed, generator.version(), setup.len()));
    ExitCode::SUCCESS
}

/// Split generated predicates three ways and require the parts to add up.
///
/// This is the one check here that needs no DuckDB on the machine, because the property it tests is
/// a property of SQL rather than an agreement between two engines. It drives rudb as the linked
/// library for the same reason `bisect` does: the fixture is created once and every predicate after
/// it asks about the same table, so the driver has to be one that remembers. `--pinned` runs the
/// whole thing against the pinned binary instead, which is how somebody checks that a predicate
/// this reports about is really rudb being wrong and not this generator writing SQL that does not
/// mean what it looks like. That run is a process per query with the fixture replayed in front of
/// each one, so it does a few hundred predicates in the time rudb does twenty thousand, and it is
/// for checking a finding rather than for a sweep.
///
/// It exits nonzero when anything did not add up, unlike `sqlsmith`, because there is no second
/// engine here to be the reason for a difference. Every finding is a bug in the engine or a bug in
/// this file, and both of those are somebody's job before the next merge.
fn tlp(count: usize, seed: Option<u64>, pinned: bool) -> ExitCode {
    let mut engine: Box<dyn Engine> = if pinned {
        match Shell::duckdb() {
            Ok(shell) => Box::new(Session::new(shell)),
            Err(e) => {
                eprintln!("rudb-compat: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        Box::new(Rudb::new())
    };
    let seed = seed.unwrap_or_else(|| u64::from(fresh_seed()));
    match rudb_compat::tlp::run(&mut *engine, count, seed) {
        Ok(found) => {
            print!("{found}");
            if found.is_clean() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A seed for a run nobody gave one for.
///
/// The clock, because this is a starting point for a search and not a key, and the only property it
/// needs is that two runs a second apart look somewhere different. It is printed either way, which
/// is what turns it back into a run somebody can repeat.
fn fresh_seed() -> u32 {
    let since =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    u32::try_from(since.as_nanos() % u128::from(u32::MAX)).unwrap_or_default()
}

/// Run the corpus and write the published page from it.
///
/// The same run `slt` does, because the page has to be a measurement of something that happened and
/// not a summary of a file somebody kept. It prints the page as well as writing it, so that a CI log
/// has the numbers in it without anybody fetching an artifact.
fn report(
    path: Option<&str>,
    slow: bool,
    refresh: bool,
    limits: Limits,
    out: Option<&str>,
) -> ExitCode {
    let dir = match corpus_dir(path, refresh) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let total = match corpus(&dir, slow, limits) {
        Ok(total) => total,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let provenance = Provenance::gather(Path::new(root()), &dir, Rudb::new().version());
    let into = out.map_or_else(|| Path::new(root()).join(rudb_compat::report::DEST), PathBuf::from);
    // The function coverage number is a forty minute sweep and this is not one, so it is read back
    // off the most recent sweep recorded here rather than measured again. No sweep on this machine
    // leaves the page saying so, which is what it said before any of them existed.
    let sweep = Sweep::latest(&into);
    println!("{}", Page::of(&total, &provenance).with(sweep.as_ref()));
    match rudb_compat::report::write(&into, &total, &provenance, sweep.as_ref()) {
        Ok(page) => {
            println!("written to {}", page.display());
            println!("appended to {}", into.join(rudb_compat::report::SERIES).display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Read the function table off the pinned DuckDB and print it.
///
/// With no name it prints the inventory, which is the denominator the function coverage number is
/// over and the list of types nothing can generate a call for yet. With a name it prints that name's
/// overloads and every call the generator would put to each of them, which is how a case gets read
/// before a run puts thousands of them to two engines.
fn functions(name: Option<&str>, require: bool) -> ExitCode {
    let mut duckdb = match Duckdb::discover() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let pin = duckdb.pin();
    if pin != Pin::Pinned {
        eprintln!("rudb-compat: this is not the pinned DuckDB, so this catalog is another one's");
        if require {
            return ExitCode::FAILURE;
        }
    }
    let catalog = match rudb_compat::functions::catalog(&mut duckdb) {
        Ok(catalog) => catalog,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    match name {
        None => print!("{}", rudb_compat::functions::inventory(&catalog)),
        Some(name) => {
            let wanted: Vec<_> = catalog.iter().filter(|o| o.name == name).collect();
            if wanted.is_empty() {
                eprintln!("rudb-compat: this DuckDB has no function called {name}");
                return ExitCode::FAILURE;
            }
            for overload in wanted {
                println!(
                    "{}({}) -> {}, {}",
                    overload.name,
                    overload.parameters.join(", "),
                    overload.returns,
                    overload.kind.name()
                );
                let calls = rudb_compat::functions::calls(overload);
                if calls.is_empty() {
                    println!("    no calls, because nothing here has a boundary set for one of");
                    println!("    its parameter types or the generator does not call this kind");
                }
                for call in calls {
                    println!("    {}", call.sql);
                }
                println!();
            }
        }
    }
    ExitCode::SUCCESS
}

/// How many names a histogram prints before it stops, unless `--limit` says otherwise.
const NAMES: usize = 40;

/// Read the real query corpus and print what it calls.
///
/// Nothing is run here. The benchmark loads in that suite build tables of a hundred million rows,
/// and what this is for is the histogram rather than a pass rate, so it reads the queries and counts
/// what is in them and stops. The corpus is upstream's own benchmark suite, which is the nearest
/// thing available to a thousand queries somebody wrote because they wanted an answer rather than
/// because they wanted to break an engine.
///
/// It wants the pinned binary for the same reason `functions` does. The histogram is over the
/// catalog, so a catalog from another build would count a name this one does not have and would
/// report as unused a name it does.
fn queries(refresh: bool, require: bool, limit: usize) -> ExitCode {
    let mut duckdb = match Duckdb::discover() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    if duckdb.pin() != Pin::Pinned {
        eprintln!("rudb-compat: this is not the pinned DuckDB, so this catalog is another one's");
        if require {
            return ExitCode::FAILURE;
        }
    }
    let read = rudb_compat::vendor::checkout(Path::new(root()), refresh)
        .and_then(|dir| rudb_compat::queries::read(&dir));
    let (corpus, catalog) = match (read, rudb_compat::functions::catalog(&mut duckdb)) {
        (Ok(corpus), Ok(catalog)) => (corpus, catalog),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    print_queries(&histogram(&corpus, &catalog), catalog.len(), limit);
    ExitCode::SUCCESS
}

/// Print a histogram, most used name first.
fn print_queries(found: &Histogram, overloads: usize, limit: usize) {
    println!("queries   {}", found.queries);
    println!("called    {} of {} names", found.used.len(), found.names);
    println!("unused    {}", found.unused);
    println!("overloads {overloads}, which is what those names come to when the types are counted");
    println!();
    println!("suites");
    for (group, how_many) in &found.groups {
        println!("  {how_many:>6}  {group}");
    }
    println!();
    println!("calls   queries  name");
    for (name, calls, queries) in found.used.iter().take(limit) {
        println!("{calls:>5}  {queries:>8}  {name}");
    }
    if found.used.len() > limit {
        println!("and {} more names, which --limit will show", found.used.len() - limit);
    }
    println!();
    println!("Nothing here was run. These are the queries as written, counted, because what this");
    println!("corpus is for is saying which of the catalog is worth anything rather than saying");
    println!("what passes. A name counts when it is written as a call, so an operator and a");
    println!("function spelled as a keyword score nothing and read here as unused.");
}

/// How many of the worst benchmarks the cost page prints.
const WORST: usize = 20;

/// How long one process of one engine gets before the benchmark is called refused.
///
/// A minute is long for a benchmark cut down to a million rows and short next to a run of the
/// whole corpus. The number matters because the corpus contains joins one engine answers in a
/// second and the other does not answer at all, and without a limit the first of those takes the
/// rest of the run with it.
const SECONDS: u64 = 60;

/// Measure the real query corpus on both engines and print the three ratios.
///
/// This is the only place in the project where the claim of a tenth of DuckDB's time and a tenth of
/// its memory can be checked against queries nobody wrote for us. Both engines are driven as shells,
/// because that is the only way both of them are processes reached the same way and the only way
/// either of them can be measured from outside.
///
/// A benchmark is the load and the query together, and the reason is in `crate::cost`: rudb has no
/// storage format yet, so there is no way to build a table once and time a query against it. The
/// load is measured on its own as well and its share is printed beside every row, because a number
/// that is nine tenths `CREATE TABLE` and does not say so sends people to the wrong file.
fn cost(
    refresh: bool,
    count: Option<usize>,
    runs: usize,
    group: Option<&str>,
    seconds: u64,
) -> ExitCode {
    let limit = Duration::from_secs(seconds);
    let (ours, theirs) = match (Shell::rudb(), Shell::duckdb()) {
        (Ok(ours), Ok(theirs)) => (ours.within(limit), theirs.within(limit)),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !ours.can_stop() {
        eprintln!(
            "rudb-compat: no timeout on this machine, so a benchmark that hangs hangs the run"
        );
    }
    let corpus = rudb_compat::vendor::checkout(Path::new(root()), refresh)
        .and_then(|dir| rudb_compat::queries::read(&dir));
    let corpus = match corpus {
        Ok(corpus) => corpus,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let wanted: Vec<&Query> = corpus
        .iter()
        .filter(|query| group.is_none_or(|group| query.group == group))
        .take(count.unwrap_or(usize::MAX))
        .collect();
    let mut costs = Costs::default();
    let watching = rudb_compat::suite::watching();
    for (at, query) in wanted.iter().enumerate() {
        if watching {
            eprintln!("{} of {}  {}", at + 1, wanted.len(), query.name);
        }
        if let Some(why) = rudb_compat::cost::skipped(query) {
            costs.skipped.push((query.name.clone(), why));
            continue;
        }
        match rudb_compat::cost::measure(&ours, &theirs, query, runs) {
            Ok(Ok(measured)) => costs.measured.push(measured),
            Ok(Err(said)) => costs.refused.push((query.name.clone(), said)),
            Err(e) => {
                eprintln!("rudb-compat: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    print_costs(&costs, wanted.len(), runs, seconds);
    ExitCode::SUCCESS
}

/// Print a cost run at the three granularities the milestone asks for.
fn print_costs(costs: &Costs, asked: usize, runs: usize, seconds: u64) {
    let mut reasons: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (_, why) in &costs.skipped {
        *reasons.entry(why.reason()).or_default() += 1;
    }
    println!("benchmarks {asked}");
    println!("measured   {}", costs.measured.len());
    println!(
        "refused    {}, which is one engine declining it or taking too long",
        costs.refused.len()
    );
    for (why, how_many) in grouped(&costs.refused) {
        println!("    {how_many:>5}  {why}");
    }
    println!("skipped    {}", costs.skipped.len());
    for (why, how_many) in &reasons {
        println!("    {how_many:>5}  {why}");
    }
    println!();
    println!(
        "Each number below is rudb over the pinned binary, so one is even and the goal is 0.1."
    );
    println!("The median of {runs} runs with the quartiles beside it, never the minimum.");
    println!();
    match costs.overall() {
        None => {
            println!("no benchmark was measured on both engines, so there is no ratio to print")
        }
        Some(ratios) => {
            println!("whole corpus");
            print_ratios(&ratios);
        }
    }
    let groups = costs.per_group();
    if !groups.is_empty() {
        println!();
        println!("per suite, worst first, and a suite with one benchmark in it is left out");
        println!("{:<24}{:>10}{:>10}{:>10}{:>7}", "suite", "time", "cpu", "memory", "n");
        for (group, ratios) in &groups {
            println!(
                "{group:<24}{:>10.2}{:>10.2}{:>10.2}{:>7}",
                ratios.time.median, ratios.cpu.median, ratios.memory.median, ratios.time.count
            );
        }
    }
    let worst = costs.worst(WORST);
    if !worst.is_empty() {
        println!();
        println!("the worst {}, which is the column to read", worst.len());
        println!("{:<44}{:>9}{:>9}{:>12}", "benchmark", "time", "memory", "load share");
        for one in worst {
            println!(
                "{:<44}{:>9.2}{:>9.2}{:>11.0}%",
                short(&one.name, 43),
                one.time(),
                memory(one),
                one.load_share() * 100.0
            );
        }
    }
    println!();
    println!("A benchmark here is the load and the query in one process, because rudb has no");
    println!("storage format yet and there is no way to build a table once and then time a query");
    println!("against it. The load share says how much of each number is the ingestion. The row");
    println!("counts are cut down to a million, so these are ratios at a million rows. A process");
    println!("that has not answered in {seconds} seconds is stopped and the benchmark is refused.");
    println!();
    println!("A benchmark stopped on our side is one rudb was losing badly, so it leaves the");
    println!("ratios above rather than making them worse. Read the timeout count as part of the");
    println!("result and not as a footnote: those are the shapes to fix first.");
}

/// What the refusals were, the most common first.
fn grouped(refused: &[(String, String)]) -> Vec<(String, usize)> {
    let mut by: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for (_, said) in refused {
        *by.entry(said.as_str()).or_default() += 1;
    }
    let mut rows: Vec<(String, usize)> =
        by.into_iter().map(|(said, how_many)| (said.to_owned(), how_many)).collect();
    rows.sort_by_key(|(said, how_many)| (std::cmp::Reverse(*how_many), said.clone()));
    rows
}

/// The three medians with their quartiles under a heading.
fn print_ratios(ratios: &Ratios) {
    let line = |what: &str, spread: &Spread| {
        println!(
            "  {what:<8}{:>8.2}   quartiles {:.2} to {:.2} over {} benchmarks",
            spread.median, spread.low, spread.high, spread.count
        );
    };
    line("time", &ratios.time);
    line("cpu", &ratios.cpu);
    line("memory", &ratios.memory);
    if ratios.at_goal() {
        println!("  all three are at the goal of a tenth");
    }
}

/// Peak resident set, rudb over the pin.
#[expect(
    clippy::cast_precision_loss,
    reason = "a resident set a double cannot count does not exist"
)]
fn memory(one: &Measured) -> f64 {
    if one.theirs.peak == 0 { 0.0 } else { one.ours.peak as f64 / one.theirs.peak as f64 }
}

/// A name cut to something a column can hold.
fn short(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_owned();
    }
    name.chars().take(width - 3).chain("...".chars()).collect()
}

/// Run the generated calls on both engines and print the function coverage number.
///
/// A name narrows it to that name's overloads, which is how somebody works on one function without
/// waiting for the whole catalog. With no name it is the full sweep, which is about fifteen thousand
/// calls and takes a while, so `RUDB_COMPAT_WATCH` makes it say where it is.
///
/// A full sweep against the pinned binary is written down beside the published pages, where the next
/// `report` run reads it back. A sweep over one name is not, and neither is one against some other
/// DuckDB, because both of those are numbers about something narrower than what the page claims and
/// a row that looks like the published one is worse than no row.
fn coverage(
    name: Option<&str>,
    require: bool,
    messages: MessageMatch,
    out: Option<&str>,
) -> ExitCode {
    let mut duckdb = match Duckdb::discover() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let pinned_binary = duckdb.pin() == Pin::Pinned;
    if !pinned_binary {
        eprintln!(
            "rudb-compat: this is not the pinned DuckDB, so this number is about another one"
        );
        if require {
            return ExitCode::FAILURE;
        }
    }
    let catalog = match rudb_compat::functions::catalog(&mut duckdb) {
        Ok(catalog) => catalog,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    let catalog: Vec<_> = match name {
        Some(name) => catalog.into_iter().filter(|o| o.name == name).collect(),
        None => catalog,
    };
    if catalog.is_empty() {
        eprintln!("rudb-compat: this DuckDB has no function by that name");
        return ExitCode::FAILURE;
    }
    let mut rudb = Rudb::new();
    let scored = match rudb_compat::coverage::score(&mut duckdb, &mut rudb, &catalog, messages) {
        Ok(scored) => scored,
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Every failing call in full and above the counts, the same way the differential report does it,
    // because a report whose useful part is below the fold is a report people stop reading.
    for one in &scored {
        if one.failures.is_empty() {
            continue;
        }
        println!("{}", one.signature());
        for failure in &one.failures {
            print!("{failure}");
        }
        println!();
    }
    let coverage = rudb_compat::coverage::coverage(&scored);
    print!("{coverage}");
    println!();
    let into = out.map_or_else(|| Path::new(root()).join(rudb_compat::report::DEST), PathBuf::from);
    if let Some(name) = name {
        println!("not recorded, this was {name} on its own and not the whole catalog");
        return ExitCode::SUCCESS;
    }
    if !pinned_binary {
        println!("not recorded, this DuckDB is not the pinned one");
        return ExitCode::SUCCESS;
    }
    let provenance = Provenance::of_machine(Path::new(root()), rudb.version());
    match rudb_compat::report::record(&into, &coverage, &provenance) {
        Ok(file) => {
            println!("recorded in {}, where report reads it back", file.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rudb-compat: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Which directory a corpus run reads.
///
/// No path means the upstream corpus, fetched if it is not already there. That is the run CI does
/// and it is the number the project publishes, so it is the one that takes no argument. Pointing it
/// at a directory is for narrowing down a failure by hand.
fn corpus_dir(path: Option<&str>, refresh: bool) -> Result<PathBuf, HarnessError> {
    match path {
        Some(path) => Ok(PathBuf::from(path)),
        None => rudb_compat::vendor::corpus(Path::new(root()), refresh),
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
    // An `include` path is written from the top of the corpus, and this process was handed one file
    // rather than the directory, so the top is found from where the file sits.
    let top = path.parent().and_then(rudb_compat::conform::corpus_top);
    let summary = match String::from_utf8(bytes) {
        Ok(text) => match rudb_compat::conform::run_under(&mut rudb, top.as_deref(), name, &text) {
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

/// Run a sqllogictest file or a directory of them against rudb.
///
/// Whoever called this exits zero whatever the pass rate is, which is the opposite of what `run`
/// does and is deliberate. The corpus is thousands of statements against a database that is being
/// built, so a nonzero exit would mean the job is red every day until the day it is finished and
/// nobody would read it. What CI watches is the number going down.
fn corpus(path: &Path, slow: bool, limits: Limits) -> Result<Isolated, HarnessError> {
    let exe = std::env::current_exe()
        .map_err(|e| HarnessError::new(format!("cannot find this binary to re-run it: {e}")))?;
    rudb_compat::isolate::run_corpus(&exe, path, slow, limits)
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
    println!();
    print!("{}", total.skips);
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
    println!("  functions [n] print the function catalog off the pinned DuckDB, or the calls the");
    println!("                generator would put to the overloads of one name");
    println!("  coverage [n]  run those calls on both engines and print the function coverage");
    println!("                number, over one name if given and over the whole catalog if not.");
    println!("                A full sweep against the pinned binary is also written down where");
    println!("                report reads it back onto the published page.");
    println!("  oracles [path]");
    println!("                run the corpus against both oracles, the file and the pinned");
    println!("                binary, and print the records they split on. A record rudb passes");
    println!("                that the binary fails is a bug in this runner and not in the");
    println!("                engine, and one oracle cannot see it. Exits nonzero on those and");
    println!("                on nothing else. It always drives both engines as shells that");
    println!("                remember what they were told, because a test file makes a table and");
    println!("                then asks about it, so it needs a built rudb on PATH or in");
    println!("                RUDB_COMPAT_RUDB.");
    println!("  bisect <sql>  name the optimizer pass that changed the answer, by running the");
    println!("                statement again once per pass with that pass turned off, and once");
    println!("                more with all of them off. That last run is the one to read: an");
    println!("                answer that is still wrong with every rewrite off is wrong in the");
    println!("                binder or the executor and the optimizer is not where to look.");
    println!("  sqlsmith      generate queries with upstream's own generator and put every one of");
    println!("                them to both engines, grouped by what rudb said about them. Takes");
    println!("                --count and --seed, and prints the seed either way, because a");
    println!("                generated run that cannot be replayed is one nobody can fix.");
    println!("  tlp           generate predicates and split a query on each of them three ways,");
    println!("                the rows where it is true, where it is false and where it is");
    println!("                neither, and require the three to add back up to the query with no");
    println!("                predicate on it. Needs no DuckDB, since the property is a property");
    println!("                of SQL rather than an agreement between two engines. Takes --count");
    println!("                and --seed, and --pinned to run the whole thing against DuckDB");
    println!("                instead, which is how a finding is checked against the generator");
    println!("                and is slow enough to be for a handful of predicates rather than");
    println!("                for a sweep. Exits nonzero on anything that did not add up.");
    println!("  queries       read upstream's benchmark suite, which is a thousand queries");
    println!("                somebody wrote because they wanted an answer, and print how often");
    println!("                each name in the catalog is called and how many of them are called");
    println!("                nowhere. Nothing is run, because what this corpus is for is the");
    println!("                weights rather than a pass rate. Takes --limit.");
    println!("  cost          measure that same corpus on both engines and print the three");
    println!("                ratios, time, processor time and peak memory, at three");
    println!("                granularities: the whole corpus, per suite, and the worst twenty.");
    println!("                The median of five runs with the quartiles beside it. Takes");
    println!("                --count, --runs, --group and --seconds.");
    println!("  vendor        fetch the upstream sqllogictest corpus and say where it went");
    println!("  levels        print the four compatibility levels and their current status");
    println!(
        "  reduce <sql>  shrink a failing query to a minimal reproduction, keeping it failing"
    );
    println!("                the way it failed rather than only keeping it failing. Given a file");
    println!(
        "                with more than one statement in it, it reduces every one of them and"
    );
    println!("                prints the distinct cases they came down to, the most found first.");
    println!("  report [path] run the corpus and write the published status page from that run");
    println!("  -V, --version print the version and exit");
    println!();
    println!("  --strict-messages  require error text to match and not only the error kind");
    println!("  --slow             include the .test_slow files, which slt leaves out by default");
    println!("  --refresh          fetch the corpus again even if it is already there");
    println!("  --pinned           make `duckdb`, `functions` and `coverage` fail when the binary");
    println!("                     is not the pinned commit");
    println!("  --shell            drive both engines as command line binaries rather than one");
    println!("                     binary and one linked library, which is what tests the drop in");
    println!("                     claim. Needs a built rudb on PATH or in RUDB_COMPAT_RUDB.");
    println!("  --measure          also record what each record cost on both engines, which is");
    println!("                     wall clock, processor time and peak resident set. Needs");
    println!("                     --shell, and needs GNU time on the machine. Records that");
    println!("                     failed, disagreed, or took under ten milliseconds on both");
    println!("                     engines are not timed, and the reasons are in");
    println!("                     spec/sql/duckdb/09-the-harness.md section 9.7.");
    println!("  --file <path>      where reduce reads its statements from, for the generated ones");
    println!("                     that are too long to paste onto a command line and for whole");
    println!("                     runs of them at once");
    println!("  --budget <n>       how many candidates one reduce may put to the engines, 2000 by");
    println!("                     default. Every candidate is two engine runs and one of them is");
    println!("                     a subprocess, so this is a clock rather than a memory limit.");
    println!("  --limit <seconds>  how long one statement may run, 10 by default");
    println!("  --seconds <n>      how long one engine gets on one benchmark in a cost run, 60 by");
    println!("                     default. A process that goes over it is stopped and the");
    println!("                     benchmark is refused rather than measured, which is what keeps");
    println!("                     one query nobody can answer from ending the whole run.");
    println!("  --memory <mb>      how large one file may get before it is cut off, 2048 default");
    println!("  --out <dir>        where report writes its page, target/report by default. Each");
    println!("                     run is a new file and one row appended to series.tsv beside");
    println!("                     it, because a page that is overwritten cannot go down in");
    println!("                     front of anybody. coverage takes the same flag and appends a");
    println!("                     row to coverage.tsv in the same place, which is the file the");
    println!("                     page carries the function coverage number off.");
    println!();
    println!("Each file in an slt run gets a process of its own, because the corpus contains");
    println!("queries that are meant to be enormous. Both limits are handed to the engine, which");
    println!("stops the statement itself and leaves a failure the report can count. This process");
    println!("keeps a clock of its own at twelve times the statement limit and a cap on the size");
    println!("the child, and a file that reaches either of those is killed and named instead.");
    println!();
    println!("DuckDB gets ten seconds on every statement everywhere, whatever --limit says, and");
    println!("the process is killed when it runs out. It is a subprocess rather than a library");
    println!("here, so nothing else was going to stop it, and sleep_ms is a real function that a");
    println!("generated call will eventually reach.");
    println!();
    println!("The slt command needs no DuckDB on the machine, because a sqllogictest file already");
    println!("carries what every statement is supposed to produce. Everything else here compares");
    println!("two live engines and needs a duckdb binary on the path.");
    println!("The design is spec/14-rudb-compat.md in https://github.com/tamnd/rudb.");
}
