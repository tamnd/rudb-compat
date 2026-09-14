//! What the real query corpus costs on both engines.
//!
//! The project's claim is a tenth of DuckDB's time and a tenth of its memory, and `crate::resource`
//! has been able to measure a pair of processes for a while. What was missing was something to
//! measure. The sqllogictest corpus cannot supply it, because a record there only means anything
//! under a session that replays every statement before it, so timing a record would time the
//! replay. The benchmark corpus in `crate::queries` is independent by construction: each file
//! carries its own `load` and one query, and nothing in it depends on the file before it.
//!
//! ## What a number here is
//!
//! It is the load and the query together, not the query alone, and that is worth being blunt
//! about. rudb has no storage format yet, tamnd/rudb#103, so there is no way to build a table once
//! and then time a query against it. Both engines are given the load as setup statements and the
//! query after it, in one process, and the process is what gets measured. So a benchmark whose load
//! builds a million rows and whose query sums a column is mostly reporting the ingestion.
//!
//! The load is therefore measured on its own as well, with a trivial statement after it, and the
//! share it takes is printed beside every ratio. A page that said rudb was three times slower and
//! did not say that nine tenths of the number was `CREATE TABLE` would be a page that sends people
//! to the wrong file.
//!
//! ## The row counts are cut down
//!
//! The suite builds tables of a hundred million rows, because it is a benchmark suite for a
//! finished database and a run of it is an afternoon. Every `range` and `generate_series` argument
//! above [`CAP`] is cut to [`CAP`] before either engine sees it. That changes what the benchmark
//! measures and it does not change what this measures, because both engines are handed the same
//! text and the ratio is the answer. What it does mean is that a number here is a ratio at a
//! million rows and says nothing about a ratio at a hundred million, which is the kind of claim
//! `tamnd/rudb-bench` exists to make properly.
//!
//! Nothing else is rewritten. A literal outside those two functions is left exactly as it was, so a
//! modulus or a seed or a hash constant still says what its author wrote.
//!
//! ## A benchmark one engine cannot answer
//!
//! It is refused and the run carries on. The caller puts a wall clock limit on both shells with
//! `Shell::within`, and a process that goes over it is stopped and comes back as a `Timeout`, which
//! reads in the report beside a parser error and a missing function as one more reason a benchmark
//! has no ratio. Without that the first unanswerable query ends the run and every benchmark after
//! it goes unmeasured, which is how a corpus of eight hundred queries produces nothing.

use std::collections::BTreeMap;

use crate::engine::{Engine, HarnessError};
use crate::queries::Query;
use crate::resource::{Ratios, Usage, middle};
use crate::shell::Shell;

/// The largest number of rows a load is allowed to build.
pub const CAP: u64 = 1_000_000;

/// Why a benchmark was never put to either engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// It says it needs an extension, which neither engine here loads.
    Requires(String),
    /// It calls the TPC-H or TPC-DS data generator, which is an extension the pin does not ship
    /// loaded and rudb does not have at all.
    Generator,
    /// It reads or writes a file. The file is not in the sparse checkout and a run that made one up
    /// would be measuring the file rather than the engines.
    File,
}

impl Skipped {
    /// The word this reason is counted under.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Requires(what) => format!("requires {what}"),
            Self::Generator => "needs the data generator".to_owned(),
            Self::File => "reads or writes a file".to_owned(),
        }
    }
}

/// What one benchmark cost on both engines.
#[derive(Debug, Clone, PartialEq)]
pub struct Measured {
    /// What the benchmark calls itself.
    pub name: String,
    /// The suite it came from, which is the per feature granularity.
    pub group: String,
    /// The load and the query together on rudb.
    pub ours: Usage,
    /// The load and the query together on the pinned binary.
    pub theirs: Usage,
    /// The load on its own on rudb, with a trivial statement after it.
    pub our_load: Usage,
    /// The load on its own on the pinned binary.
    pub their_load: Usage,
}

impl Measured {
    /// The pair `crate::resource::Ratios` wants.
    #[must_use]
    pub const fn pair(&self) -> (Usage, Usage) {
        (self.ours, self.theirs)
    }

    /// Wall clock, rudb over the pin. One is even and below one is faster.
    #[must_use]
    pub fn time(&self) -> f64 {
        let theirs = self.theirs.wall.as_secs_f64();
        if theirs == 0.0 { f64::INFINITY } else { self.ours.wall.as_secs_f64() / theirs }
    }

    /// How much of this benchmark is the load rather than the query, on the pinned binary.
    ///
    /// Taken on the pin rather than on rudb because it is the side that answers everything, so it
    /// is the side whose split between building the table and reading it is trustworthy.
    #[must_use]
    pub fn load_share(&self) -> f64 {
        let whole = self.theirs.wall.as_secs_f64();
        if whole == 0.0 { 0.0 } else { (self.their_load.wall.as_secs_f64() / whole).min(1.0) }
    }
}

/// Everything a run found.
#[derive(Debug, Clone, Default)]
pub struct Costs {
    /// The benchmarks both engines answered, which are the only ones with a ratio.
    pub measured: Vec<Measured>,
    /// The benchmarks nobody put to an engine, with the reason.
    pub skipped: Vec<(String, Skipped)>,
    /// The benchmarks an engine refused, with what it said. A failing record is never timed,
    /// because timing an error path measures the error path.
    pub refused: Vec<(String, String)>,
}

impl Costs {
    /// The three ratios over everything measured.
    #[must_use]
    pub fn overall(&self) -> Option<Ratios> {
        let pairs: Vec<(Usage, Usage)> = self.measured.iter().map(Measured::pair).collect();
        Ratios::of(&pairs)
    }

    /// The three ratios per suite, worst median wall clock first.
    ///
    /// A suite with fewer than two benchmarks in it is left out. One benchmark is an anecdote and
    /// putting it in a table beside a suite of a hundred invites somebody to read it as a trend.
    #[must_use]
    pub fn per_group(&self) -> Vec<(String, Ratios)> {
        let mut by: BTreeMap<&str, Vec<(Usage, Usage)>> = BTreeMap::new();
        for one in &self.measured {
            by.entry(one.group.as_str()).or_default().push(one.pair());
        }
        let mut rows: Vec<(String, Ratios)> = by
            .into_iter()
            .filter(|(_, pairs)| pairs.len() > 1)
            .filter_map(|(group, pairs)| Ratios::of(&pairs).map(|r| (group.to_owned(), r)))
            .collect();
        rows.sort_by(|a, b| {
            b.1.time.median.total_cmp(&a.1.time.median).then_with(|| a.0.cmp(&b.0))
        });
        rows
    }

    /// The benchmarks rudb is worst on, slowest first.
    ///
    /// This is the column that gets read. An engine that is even on average and two hundred times
    /// slower on one shape has a bug rather than a distribution, and an average hides that by
    /// construction.
    #[must_use]
    pub fn worst(&self, how_many: usize) -> Vec<&Measured> {
        let mut all: Vec<&Measured> = self.measured.iter().collect();
        all.sort_by(|a, b| b.time().total_cmp(&a.time()));
        all.truncate(how_many);
        all
    }
}

/// Why this benchmark cannot be put to either engine, if it cannot.
#[must_use]
pub fn skipped(query: &Query) -> Option<Skipped> {
    if let Some(what) = query.requires.first() {
        return Some(Skipped::Requires(what.clone()));
    }
    let all = || query.load.iter().chain(std::iter::once(&query.sql));
    for sql in all() {
        let lower = sql.to_ascii_lowercase();
        if lower.contains("dbgen(") || lower.contains("dsdgen(") {
            return Some(Skipped::Generator);
        }
        if names_a_file(&lower) {
            return Some(Skipped::File);
        }
    }
    None
}

/// Whether a statement mentions something on disk.
///
/// A path in single quotes and the reader functions that take one. It is a text rule and it is
/// meant to be a cautious one: a benchmark wrongly skipped is one missing row on a page and a
/// benchmark wrongly run is a number that measures a filesystem.
fn names_a_file(lower: &str) -> bool {
    const READERS: [&str; 6] =
        ["read_csv", "read_parquet", "read_json", "read_ndjson", "copy ", "attach "];
    if READERS.iter().any(|reader| lower.contains(reader)) {
        return true;
    }
    lower.split('\'').skip(1).step_by(2).any(|text| {
        text.contains('/')
            || text.ends_with(".csv")
            || text.ends_with(".parquet")
            || text.ends_with(".json")
            || text.ends_with(".db")
    })
}

/// A statement with its row counts cut down to something a test run can carry.
///
/// Only the arguments of `range` and `generate_series` are touched, because those are where the
/// suite says how big a table is. Every other literal is somebody's modulus or seed and changing
/// one would change what the query means without making it any cheaper.
#[must_use]
pub fn cut(sql: &str, cap: u64) -> String {
    let lower = sql.to_ascii_lowercase();
    let mut out = String::with_capacity(sql.len());
    let mut at = 0;
    while at < sql.len() {
        let Some(found) = call(&lower[at..]) else {
            out.push_str(&sql[at..]);
            break;
        };
        let opens = at + found;
        let Some(closes) = closing(sql, opens) else {
            out.push_str(&sql[at..]);
            break;
        };
        out.push_str(&sql[at..=opens]);
        out.push_str(&shrunk(&sql[opens + 1..closes], cap));
        out.push(')');
        at = closes + 1;
    }
    out
}

/// Where the bracket of the next `range` or `generate_series` call is.
///
/// The whole name has to match and not the end of it. `list_range(` is a different function and
/// cutting its argument would be cutting something this has not read.
fn call(lower: &str) -> Option<usize> {
    lower.bytes().enumerate().filter(|(_, byte)| *byte == b'(').map(|(at, _)| at).find(|at| {
        let before = &lower[..*at];
        let start =
            before.rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_').map_or(0, |end| end + 1);
        matches!(&before[start..], "range" | "generate_series")
    })
}

/// The matching close bracket, counting the ones in between.
fn closing(sql: &str, opens: usize) -> Option<usize> {
    let mut depth = 0_i32;
    for (at, byte) in sql.bytes().enumerate().skip(opens) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The arguments of one call, with anything above the cap brought down to it.
///
/// A run of digits that starts straight after a letter or an underscore is part of a name and is
/// left alone, because a column called `a256` is not a row count and rewriting it would rename it.
fn shrunk(args: &str, cap: u64) -> String {
    let mut out = String::with_capacity(args.len());
    let mut number = String::new();
    let mut naming = false;
    for c in args.chars() {
        if !naming && (c.is_ascii_digit() || (c == '_' && !number.is_empty())) {
            number.push(c);
            continue;
        }
        out.push_str(&capped(&number, cap));
        number.clear();
        naming = c.is_ascii_alphanumeric() || c == '_';
        out.push(c);
    }
    out.push_str(&capped(&number, cap));
    out
}

/// One number, brought down to the cap if it is over it.
///
/// The underscores go before the parse and do not come back, because a number that was written
/// `10_000_000` and is over the cap is being replaced anyway, and one that is under it is handed
/// back as it was written.
fn capped(number: &str, cap: u64) -> String {
    match number.replace('_', "").parse::<u64>() {
        Ok(value) if value > cap => cap.to_string(),
        _ => number.to_owned(),
    }
}

/// Run one benchmark on both engines and report what it cost.
///
/// `runs` processes per engine per measurement, and the middle one is the answer, following
/// `spec/15-rudb-bench.md` section 15.1. Never the minimum, which is a number about how quiet the
/// machine got rather than about the engine.
///
/// # Errors
///
/// When a shell cannot be started at all. A statement either engine refuses is not an error here,
/// it is a benchmark with no ratio, and the caller records it as refused.
pub fn measure(
    ours: &Shell,
    theirs: &Shell,
    query: &Query,
    runs: usize,
) -> Result<Result<Measured, String>, HarnessError> {
    let load: Vec<String> = query.load.iter().map(|sql| cut(sql, CAP)).collect();
    let sql = cut(&query.sql, CAP);
    // Four measurements in this order, and every one of them can end the benchmark. The load on its
    // own comes before the query, because a benchmark whose load one engine refuses has no ratio
    // and finding that out first saves five runs of the query. Our side comes before theirs at each
    // step, because ours is the side that refuses, and running the pinned binary five times to
    // learn nothing is most of what a corpus run would otherwise spend its afternoon on.
    let our_load = match cost(ours, &load, TRIVIAL, runs)? {
        Ok(usage) => usage,
        Err(said) => return Ok(Err(format!("the load: {said}"))),
    };
    let their_load = match cost(theirs, &load, TRIVIAL, runs)? {
        Ok(usage) => usage,
        Err(said) => return Ok(Err(format!("the load: {said}"))),
    };
    let our_query = match cost(ours, &load, &sql, runs)? {
        Ok(usage) => usage,
        Err(said) => return Ok(Err(said)),
    };
    let their_query = match cost(theirs, &load, &sql, runs)? {
        Ok(usage) => usage,
        Err(said) => return Ok(Err(said)),
    };
    Ok(Ok(Measured {
        name: query.name.clone(),
        group: query.group.clone(),
        ours: our_query,
        theirs: their_query,
        our_load,
        their_load,
    }))
}

/// The statement that measures a load with as little else in the process as possible.
const TRIVIAL: &str = "SELECT 1";

/// The middle of several runs of one statement on one engine.
fn cost(
    shell: &Shell,
    load: &[String],
    sql: &str,
    runs: usize,
) -> Result<Result<Usage, String>, HarnessError> {
    let mut seen = Vec::with_capacity(runs);
    for _ in 0..runs.max(1) {
        let engine = shell.clone().with_setup(load.to_vec());
        match engine.timed(sql)? {
            Ok(Some(usage)) => seen.push(usage),
            Ok(None) => {
                return Ok(Err("this machine cannot measure, see the resource module".to_owned()));
            }
            Err(e) => return Ok(Err(format!("{} said {}", engine.name(), e.kind))),
        }
    }
    Ok(middle(&seen).map_or_else(|| Err("no run of this produced a measurement".to_owned()), Ok))
}

#[cfg(test)]
mod tests {
    use super::{CAP, Costs, Measured, Skipped, cut, skipped};
    use crate::queries::Query;
    use crate::resource::Usage;
    use std::time::Duration;

    fn query(sql: &str, load: &[&str]) -> Query {
        Query {
            name: "q".to_owned(),
            group: "micro".to_owned(),
            file: "benchmark/micro/q.benchmark".to_owned(),
            sql: sql.to_owned(),
            load: load.iter().map(|one| (*one).to_owned()).collect(),
            requires: Vec::new(),
        }
    }

    fn usage(ms: u64, peak: u64) -> Usage {
        Usage { wall: Duration::from_millis(ms), cpu: Duration::from_millis(ms), peak }
    }

    fn measured(group: &str, ours: u64, theirs: u64) -> Measured {
        Measured {
            name: format!("{group} {ours} over {theirs}"),
            group: group.to_owned(),
            ours: usage(ours, ours * 1024),
            theirs: usage(theirs, theirs * 1024),
            our_load: usage(ours / 2, ours * 512),
            their_load: usage(theirs / 2, theirs * 512),
        }
    }

    #[test]
    fn a_row_count_the_suite_wrote_for_a_finished_database_is_cut_to_something_a_test_can_run() {
        assert_eq!(
            cut("CREATE TABLE t AS SELECT * FROM range(100000000) tbl(i)", CAP),
            "CREATE TABLE t AS SELECT * FROM range(1000000) tbl(i)"
        );
    }

    #[test]
    fn both_ends_of_a_range_are_cut_and_the_small_one_is_left_alone() {
        assert_eq!(
            cut("SELECT * FROM range(10, 500000000)", CAP),
            "SELECT * FROM range(10, 1000000)"
        );
    }

    #[test]
    fn a_row_count_written_with_underscores_in_it_is_still_a_row_count() {
        // The suite writes them that way in about a third of the micro benchmarks, and a cut that
        // does not read them leaves a ten million row load in a run that was meant to take seconds.
        assert_eq!(
            cut("SELECT count(*) FROM range(10_000_000)", CAP),
            "SELECT count(*) FROM range(1000000)"
        );
    }

    #[test]
    fn digits_that_are_part_of_a_name_are_not_a_row_count_and_the_name_survives() {
        assert_eq!(
            cut("SELECT a256 FROM range(10_000_000) t(a256)", CAP),
            "SELECT a256 FROM range(1000000) t(a256)"
        );
    }

    #[test]
    fn a_literal_that_is_not_a_row_count_is_left_exactly_as_its_author_wrote_it() {
        let sql = "SELECT (i * 2654435761) % 4294967296 FROM range(100000000) tbl(i)";
        assert_eq!(
            cut(sql, CAP),
            "SELECT (i * 2654435761) % 4294967296 FROM range(1000000) tbl(i)"
        );
    }

    #[test]
    fn a_function_whose_name_ends_in_range_is_not_range_and_keeps_its_arguments() {
        assert_eq!(cut("SELECT list_range(99999999)", CAP), "SELECT list_range(99999999)");
    }

    #[test]
    fn a_call_inside_a_call_does_not_end_the_one_outside_it_early() {
        assert_eq!(
            cut("SELECT * FROM range(0, (SELECT max(x) FROM t), 900000000)", CAP),
            "SELECT * FROM range(0, (SELECT max(x) FROM t), 1000000)"
        );
    }

    #[test]
    fn generate_series_is_the_other_spelling_and_is_cut_the_same_way() {
        assert_eq!(cut("FROM generate_series(200000000)", CAP), "FROM generate_series(1000000)");
    }

    #[test]
    fn a_benchmark_that_wants_the_data_generator_is_not_put_to_either_engine() {
        assert_eq!(skipped(&query("SELECT 1", &["CALL dbgen(sf=1)"])), Some(Skipped::Generator));
    }

    #[test]
    fn a_benchmark_that_reads_a_file_is_not_put_to_either_engine() {
        assert_eq!(skipped(&query("SELECT * FROM read_csv('x.csv')", &[])), Some(Skipped::File));
        assert_eq!(
            skipped(&query("SELECT 1", &["COPY t FROM 'data/t.parquet'"])),
            Some(Skipped::File)
        );
    }

    #[test]
    fn a_benchmark_that_builds_its_own_rows_is_measurable() {
        assert_eq!(
            skipped(&query("SELECT sum(i) FROM t", &["CREATE TABLE t AS SELECT * FROM range(10)"])),
            None
        );
    }

    #[test]
    fn a_benchmark_that_says_it_needs_an_extension_is_skipped_with_the_name_it_asked_for() {
        let mut wanting = query("SELECT 1", &[]);
        wanting.requires = vec!["httpfs".to_owned()];
        assert_eq!(skipped(&wanting), Some(Skipped::Requires("httpfs".to_owned())));
    }

    #[test]
    fn the_worst_benchmarks_come_back_slowest_first_because_that_is_the_column_anybody_reads() {
        let costs = Costs {
            measured: vec![
                measured("a", 100, 100),
                measured("b", 900, 100),
                measured("c", 50, 100),
            ],
            ..Costs::default()
        };
        let worst = costs.worst(2);
        assert_eq!(worst.len(), 2);
        assert_eq!(worst[0].group, "b");
        assert_eq!(worst[1].group, "a");
    }

    #[test]
    fn a_suite_with_one_benchmark_in_it_is_not_a_row_because_one_is_an_anecdote() {
        let costs = Costs {
            measured: vec![
                measured("alone", 100, 100),
                measured("pair", 100, 100),
                measured("pair", 200, 100),
            ],
            ..Costs::default()
        };
        let groups = costs.per_group();
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].0, "pair");
    }

    #[test]
    fn the_suites_come_back_worst_first_so_the_table_reads_top_down() {
        let costs = Costs {
            measured: vec![
                measured("quick", 10, 100),
                measured("quick", 20, 100),
                measured("slow", 800, 100),
                measured("slow", 900, 100),
            ],
            ..Costs::default()
        };
        let groups = costs.per_group();
        assert_eq!(groups[0].0, "slow");
        assert_eq!(groups[1].0, "quick");
    }

    #[test]
    fn the_share_of_a_benchmark_that_is_the_load_is_taken_on_the_side_that_answers_everything() {
        let one = measured("micro", 400, 200);
        assert!((one.load_share() - 0.5).abs() < 1e-9, "{}", one.load_share());
    }
}
