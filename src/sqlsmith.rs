//! Upstream's own query generator, pointed at both engines.
//!
//! DuckDB ships a `sqlsmith` extension and runs it against itself as a public fuzzer, so there is
//! a generated query source here with no generator to write. `spec/sql/duckdb/09-the-harness.md`
//! says it should be running before anything else in the generation block for exactly that reason:
//! every other generator on that list is weeks of work and this one is an extension load.
//!
//! What it produces is not a realistic query and does not need to be. It is deep joins, correlated
//! subqueries in select lists, `tablesample` clauses and casts to types nobody would write, which
//! is a good description of the part of the grammar a corpus of hand written tests does not reach.
//!
//! ## Why the generator is not the pinned binary
//!
//! The pin is a development commit and `extensions.duckdb.org` publishes binaries for releases, so
//! `INSTALL sqlsmith` against the pin is a 404 and there is nothing to be done about it from here.
//! That is fine, and it is worth saying why rather than leaving it as a workaround. Generation and
//! comparison are two different jobs. What comes out of the generator is SQL text, and text has no
//! version. The comparison that follows still puts every one of those statements to the pinned
//! binary and to rudb, which is where the version matters, so nothing about the answer depends on
//! which build wrote the query down.
//!
//! What does depend on the generator is the shape of what it writes, because sqlsmith builds
//! queries out of the catalog and the function list of the database it is running in. A generator
//! with the spatial extension loaded writes queries about geometries. So the tables are fixed here
//! rather than left to whatever database somebody points it at, and the generator is asked for
//! nothing else.
//!
//! ## Why the log goes to standard output
//!
//! `CALL sqlsmith(log => ...)` rewrites that file for every query rather than appending to it,
//! because the file is there so that a build which crashed leaves the query that crashed behind.
//! Pointed at a file, it holds one query at the end of a run of a thousand. Pointed at the process
//! standard output it is a stream, each rewrite lands after the last one, and the whole run comes
//! back. `dump_all_queries` sounds like the option for this and is not: it writes the queries it
//! decided to keep, which is a different and much smaller set.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;

use crate::compare::{Difference, Side};
use crate::engine::HarnessError;
use crate::suite::Report;

/// How many queries a run asks for when nobody said a number.
///
/// Enough that the shapes start repeating, which is what tells a reader that the list at the
/// bottom is the whole of what the generator is finding rather than the first few of it, and few
/// enough that a run is a couple of minutes rather than an afternoon. Both engines get a process
/// per query, so the cost is linear in this and there is no reason to guess high.
pub const QUERIES: usize = 500;

/// The tables every generated query is written against.
///
/// Fixed here rather than taken from a database somebody points at, because sqlsmith writes
/// queries out of the catalog it finds and a run whose queries depend on which database was open
/// is a run nobody can repeat. The columns cover the type groups that have their own code paths in
/// both engines, which is the integers, the floating point and fixed point numbers, text and
/// bytes, the temporal types, and booleans. There are rows in each, because a generator that is
/// only ever asked about empty tables produces queries whose answer is empty whatever either
/// engine thinks.
pub const CATALOG: &str = "\
CREATE TABLE ints(a INTEGER, b BIGINT, c SMALLINT, d TINYINT, e HUGEINT);
CREATE TABLE reals(a DOUBLE, b FLOAT, c DECIMAL(18,3));
CREATE TABLE words(a VARCHAR, b BLOB);
CREATE TABLE times(a DATE, b TIME, c TIMESTAMP, d INTERVAL);
CREATE TABLE flags(a BOOLEAN, b VARCHAR);
INSERT INTO ints VALUES (1, 2, 3, 4, 5), (-1, -2, -3, -4, -5), (NULL, NULL, NULL, NULL, NULL);
INSERT INTO reals VALUES (1.5, 2.5, 3.125), (-0.0, 0.0, -1.000), (NULL, NULL, NULL);
INSERT INTO words VALUES ('one', 'abc'), ('', ''), (NULL, NULL);
INSERT INTO times VALUES (DATE '2020-02-29', TIME '12:00:00', TIMESTAMP '1970-01-01 00:00:00', INTERVAL 1 DAY), (NULL, NULL, NULL, NULL);
INSERT INTO flags VALUES (true, 'yes'), (false, 'no'), (NULL, NULL);";

/// The catalog as a list of statements, in the order they have to run.
#[must_use]
pub fn catalog() -> Vec<String> {
    CATALOG
        .split(";\n")
        .map(|statement| statement.trim().trim_end_matches(';').to_owned())
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// A DuckDB that can load the sqlsmith extension.
#[derive(Debug, Clone)]
pub struct Generator {
    binary: PathBuf,
    version: String,
}

impl Generator {
    /// Find a DuckDB to generate with.
    ///
    /// `RUDB_COMPAT_SQLSMITH` names it when it is set, and otherwise the binary the rest of the
    /// harness drives is tried. That order is the way round it is because the usual case on a
    /// machine that has the pin is that the pin cannot load the extension, so the variable is how
    /// somebody says which of the two DuckDBs on the box is the one with extensions.
    ///
    /// # Errors
    ///
    /// When the binary is missing or does not answer `--version`.
    pub fn find() -> Result<Self, HarnessError> {
        let binary = std::env::var_os("RUDB_COMPAT_SQLSMITH")
            .or_else(|| std::env::var_os("RUDB_COMPAT_DUCKDB"))
            .map_or_else(|| PathBuf::from("duckdb"), PathBuf::from);
        let out = Command::new(&binary).arg("--version").output().map_err(|e| {
            HarnessError::new(format!(
                "cannot run {}: {e}. Set RUDB_COMPAT_SQLSMITH to a DuckDB that can load extensions",
                binary.display()
            ))
        })?;
        if !out.status.success() {
            return Err(HarnessError::new(format!(
                "{} --version exited {}",
                binary.display(),
                out.status
            )));
        }
        Ok(Self { binary, version: String::from_utf8_lossy(&out.stdout).trim().to_owned() })
    }

    /// What the generator calls itself, for the run header.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Ask for that many queries against [`CATALOG`], from that seed.
    ///
    /// Fewer than asked for is a normal answer. The generator stops early on a query it could not
    /// build and the count is a ceiling rather than a promise.
    ///
    /// # Errors
    ///
    /// When the extension cannot be installed or loaded, which on a development build is the
    /// ordinary case and says so, and when the generator exits without writing anything.
    pub fn generate(&self, how_many: usize, seed: u32) -> Result<Vec<String>, HarnessError> {
        let script = format!(
            "INSTALL sqlsmith; LOAD sqlsmith;\n{CATALOG}\nCALL sqlsmith(max_queries={how_many}, seed={seed}, verbose_output=0, log='/dev/stdout');"
        );
        let out =
            Command::new(&self.binary).arg("-c").arg(&script).output().map_err(|e| {
                HarnessError::new(format!("cannot run {}: {e}", self.binary.display()))
            })?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let found = statements(&text);
        if found.is_empty() {
            let said = String::from_utf8_lossy(&out.stderr);
            let said = said.trim();
            return Err(HarnessError::new(format!(
                "{} generated nothing. {}. A development build of DuckDB has no published extension binaries, so set RUDB_COMPAT_SQLSMITH to a release build",
                self.binary.display(),
                if said.is_empty() { "It said nothing either" } else { said }
            )));
        }
        Ok(found)
    }
}

/// Split what the generator wrote into one statement per query.
///
/// A query is everything up to a line that ends in a semicolon, which is how sqlsmith lays them
/// out and is a rule that survives the semicolons inside a generated string literal, since those
/// are never at the end of a line. Anything after the last one is a query the generator was part
/// way through when it stopped and is thrown away rather than run.
#[must_use]
pub fn statements(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        current.push_str(line);
        current.push('\n');
        if line.trim_end().ends_with(';') {
            let query = current.trim().trim_end_matches(';').trim().to_owned();
            if !query.is_empty() {
                found.push(query);
            }
            current.clear();
        }
    }
    found
}

/// What a generated run came to.
#[derive(Debug, Clone)]
pub struct Found {
    /// The seed it was generated from, which is what replays it.
    pub seed: u32,
    /// What the generator called itself.
    pub generator: String,
    /// How many queries came back.
    pub queries: usize,
    /// How many of them the two engines answered the same way.
    pub agreed: usize,
    /// How many of them the pinned binary could not run either.
    pub unusable: usize,
    /// Each distinct shape of difference, with how many queries had it and one of those queries.
    pub shapes: Vec<(String, usize, String)>,
}

impl Found {
    /// Read a run of generated queries into the shapes it came apart into.
    ///
    /// The catalog statements at the front of the run are not queries and are counted apart, so a
    /// `CREATE TABLE` the two engines disagree about is loud rather than filed under one more
    /// difference in a thousand.
    ///
    /// A query both engines refused is counted apart too, and that rule is the one worth arguing
    /// for. The generator is a different build from the pin and it writes queries out of its own
    /// function catalog, so a share of what it produces names something the pin has never heard of.
    /// The pin refuses those, rudb refuses them for a different reason, and the two reasons put
    /// side by side look exactly like an error kind divergence while saying nothing at all about
    /// compatibility. In a corpus of SQL somebody wrote on purpose two different error kinds is a
    /// real finding. Here it is the generator talking to itself, and counting it would make the
    /// largest group on the page the one group nobody can act on.
    ///
    /// The three cases left are all real. The pin answered and rudb refused, which is the gap.
    /// rudb answered and the pin refused, which is a dialect we invented. Both answered and the
    /// values differ, which is the one worth dropping everything for.
    ///
    /// The grouping key is the first difference of each query rather than all of them, because a
    /// query that rudb rejected has one difference and a query that answered wrongly has one per
    /// value, and counting the second by its differences would bury the first. Within a rejection
    /// the key carries the first line of what rudb said, which is what names the feature to build.
    #[must_use]
    pub fn of(report: &Report, seed: u32, generator: &str, setup: usize) -> Self {
        let queries = &report.cases[setup.min(report.cases.len())..];
        let mut counts: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut unusable = 0;
        for case in queries {
            let Some(first) = case.differences.first() else { continue };
            if both_refused(first) {
                unusable += 1;
                continue;
            }
            let entry = counts.entry(shape(first)).or_insert_with(|| (0, case.sql.clone()));
            entry.0 += 1;
        }
        let mut shapes: Vec<(String, usize, String)> =
            counts.into_iter().map(|(key, (found, sql))| (key, found, sql)).collect();
        shapes.sort_by_key(|(key, found, _)| (std::cmp::Reverse(*found), key.clone()));
        Self {
            seed,
            generator: generator.to_owned(),
            queries: queries.len(),
            agreed: queries.iter().filter(|case| case.agreed()).count(),
            unusable,
            shapes,
        }
    }

    /// How many queries said something about rudb, which is the denominator worth reading.
    #[must_use]
    pub const fn usable(&self) -> usize {
        self.queries - self.unusable
    }
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "generated by {}", self.generator)?;
        writeln!(f, "seed {}, which is what replays this run exactly", self.seed)?;
        writeln!(f)?;
        writeln!(
            f,
            "{} queries, {} the pinned binary could not run either, {} left",
            self.queries,
            self.unusable,
            self.usable()
        )?;
        writeln!(f, "{} of those the two engines answered the same way", self.agreed)?;
        if self.shapes.is_empty() {
            return Ok(());
        }
        writeln!(f)?;
        writeln!(f, "what the rest came apart on, most found first")?;
        for (key, found, sql) in &self.shapes {
            writeln!(f)?;
            writeln!(f, "  {found:6}  {key}")?;
            writeln!(f, "          {}", short(sql))?;
        }
        Ok(())
    }
}

/// How much of one generated query is printed as the example for its group.
///
/// They run to several thousand characters and a report that prints one in full is a report nobody
/// scrolls past. The seed at the top is what gets the whole of any of them back, so what is wanted
/// here is enough to recognise the shape rather than enough to run.
const SHOWN: usize = 160;

/// One generated query on one line, cut to something a report can print.
fn short(sql: &str) -> String {
    let one_line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(SHOWN) {
        Some((at, _)) => {
            format!("{} ... and {} more characters", &one_line[..at], one_line.len() - at)
        }
        None => one_line,
    }
}

/// Whether both engines refused the query, which is the case that says nothing about either.
const fn both_refused(difference: &Difference) -> bool {
    matches!(difference, Difference::ErrorKind { .. } | Difference::ErrorMessage { .. })
}

/// The key one difference is counted under.
fn shape(difference: &Difference) -> String {
    match difference {
        Difference::OneRejected { side: Side::Right, error }
        | Difference::OneErrored { side: Side::Right, error } => {
            let said = error.message.lines().next().unwrap_or_default();
            format!("{}: {}", error.kind, generalised(said))
        }
        other => other.signature(),
    }
}

/// What is left of one complaint when the part of it that is this query is taken out.
///
/// Same rule as `Difference::signature`, for the same reason. rudb quotes the text it could not
/// handle back at the reader, which is right in a message to a person and wrong as a grouping key,
/// because `tablesample system (8.1)` and `tablesample bernoulli (2.6)` are one missing feature and
/// would otherwise be two rows that each look like a one off.
///
/// Two things are taken out. A complaint that names the grammar rule it stopped on keeps only the
/// rule, since that is the feature and the rest is the query. A table name is replaced by the word
/// table, which works because every table the generator writes is qualified with `main` and there
/// is nothing else in a message shaped that way.
fn generalised(message: &str) -> String {
    if let Some(at) = message.find(", the grammar rule is ") {
        return message[at + ", ".len()..].to_owned();
    }
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(at) = rest.find("main.") {
        out.push_str(&rest[..at]);
        out.push_str("a table");
        let after = &rest[at + "main.".len()..];
        let end = after.find(|c: char| !c.is_alphanumeric() && c != '_').unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{Found, catalog, statements};
    use crate::compare::{Difference, Side};
    use crate::engine::{Cell, EngineError};
    use crate::suite::{Case, Report};

    fn case(sql: &str, differences: Vec<Difference>) -> Case {
        Case { sql: sql.to_owned(), differences, cost: None }
    }

    fn rejected(message: &str) -> Difference {
        Difference::OneRejected {
            side: Side::Right,
            error: EngineError { kind: "Parser Error".to_owned(), message: message.to_owned() },
        }
    }

    fn report(cases: Vec<Case>) -> Report {
        Report {
            left: "duckdb".to_owned(),
            right: "rudb".to_owned(),
            ours: Some(crate::suite::Ours::Right),
            cases,
        }
    }

    #[test]
    fn two_spellings_of_one_missing_feature_are_one_group_and_not_two() {
        let cases = vec![
            case(
                "a",
                vec![rejected(
                    "tablesample system (8.1) is not supported yet, the grammar rule is SampleClause",
                )],
            ),
            case(
                "b",
                vec![rejected(
                    "tablesample bernoulli (2.6) is not supported yet, the grammar rule is SampleClause",
                )],
            ),
        ];
        let found = Found::of(&report(cases), 7, "duckdb", 0);
        assert_eq!(found.shapes.len(), 1, "{:?}", found.shapes);
        assert_eq!(found.shapes[0].1, 2);
        assert!(
            found.shapes[0].0.ends_with("the grammar rule is SampleClause"),
            "{}",
            found.shapes[0].0
        );
    }

    #[test]
    fn the_same_statement_against_two_tables_is_one_group() {
        let cases = vec![
            case("a", vec![rejected("update main.flags set ")]),
            case("b", vec![rejected("update main.reals set ")]),
        ];
        let found = Found::of(&report(cases), 7, "duckdb", 0);
        assert_eq!(found.shapes.len(), 1, "{:?}", found.shapes);
        assert!(found.shapes[0].0.contains("a table"), "{}", found.shapes[0].0);
    }

    #[test]
    fn a_query_both_engines_refused_is_counted_apart_from_the_ones_that_say_something() {
        let cases = vec![
            case(
                "a",
                vec![Difference::ErrorKind {
                    left: "Catalog Error".to_owned(),
                    right: "Not implemented Error".to_owned(),
                }],
            ),
            case(
                "b",
                vec![Difference::ErrorMessage {
                    left: "no such function".to_owned(),
                    right: "no such function either".to_owned(),
                }],
            ),
            case("c", vec![rejected("syntax error at or near tablesample")]),
        ];
        let found = Found::of(&report(cases), 7, "duckdb", 0);
        assert_eq!(found.queries, 3);
        assert_eq!(found.unusable, 2);
        assert_eq!(found.usable(), 1);
        assert_eq!(found.shapes.len(), 1, "{:?}", found.shapes);
        assert!(found.shapes[0].0.contains("tablesample"), "{}", found.shapes[0].0);
    }

    #[test]
    fn a_query_only_rudb_answered_is_a_finding_and_not_a_query_the_pin_could_not_run() {
        let only_duckdb_errored = Difference::OneErrored {
            side: Side::Left,
            error: EngineError {
                kind: "Binder Error".to_owned(),
                message: "no function matches".to_owned(),
            },
        };
        let found = Found::of(&report(vec![case("a", vec![only_duckdb_errored])]), 7, "duckdb", 0);
        assert_eq!(found.unusable, 0, "one side answering is not both sides refusing");
        assert_eq!(found.shapes.len(), 1, "{:?}", found.shapes);
    }

    #[test]
    fn a_generated_query_is_cut_down_to_something_a_report_can_print_on_one_line() {
        let long = format!("select {} from t", "verylongcolumnname, ".repeat(40));
        let found = Found::of(&report(vec![case(&long, vec![rejected("no")])]), 7, "duckdb", 0);
        let printed = found.to_string();
        assert!(printed.contains("more characters"), "{printed}");
        assert!(printed.lines().all(|line| line.chars().count() < 220), "{printed}");
    }

    #[test]
    fn a_query_is_everything_up_to_a_line_that_ends_in_a_semicolon() {
        let found = statements("select\n  1;\nselect\n  2;\n");
        assert_eq!(found, vec!["select\n  1".to_owned(), "select\n  2".to_owned()]);
    }

    #[test]
    fn a_semicolon_inside_a_query_does_not_end_it_because_it_is_not_at_a_line_end() {
        let found = statements("select 'a;b'\n  from t;\n");
        assert_eq!(found, vec!["select 'a;b'\n  from t".to_owned()]);
    }

    #[test]
    fn a_query_the_generator_was_part_way_through_is_thrown_away_rather_than_run() {
        let found = statements("select 1;\nselect 2\n");
        assert_eq!(found, vec!["select 1".to_owned()], "the second one has no end");
    }

    #[test]
    fn the_catalog_is_one_statement_per_entry_and_none_of_them_are_empty() {
        let statements = catalog();
        assert_eq!(statements.len(), 10, "{statements:?}");
        assert!(statements.iter().all(|s| !s.contains(";\n")), "{statements:?}");
        assert!(statements[0].starts_with("CREATE TABLE ints"), "{}", statements[0]);
    }

    #[test]
    fn the_setup_statements_are_not_counted_as_generated_queries() {
        let cases = vec![
            case("CREATE TABLE ints(a INTEGER)", vec![rejected("no")]),
            case("select 1", Vec::new()),
        ];
        let found = Found::of(&report(cases), 7, "duckdb 1.5.5", 1);
        assert_eq!(found.queries, 1);
        assert_eq!(found.agreed, 1);
        assert!(found.shapes.is_empty(), "{:?}", found.shapes);
    }

    #[test]
    fn queries_are_grouped_by_what_rudb_said_and_the_largest_group_is_printed_first() {
        let cases = vec![
            case("a", vec![rejected("syntax error at or near tablesample")]),
            case("b", vec![rejected("syntax error at or near tablesample")]),
            case("c", vec![rejected("syntax error at or near lateral")]),
            case("d", Vec::new()),
        ];
        let found = Found::of(&report(cases), 7, "duckdb 1.5.5", 0);
        assert_eq!(found.queries, 4);
        assert_eq!(found.agreed, 1);
        assert_eq!(found.shapes.len(), 2, "{:?}", found.shapes);
        assert_eq!(found.shapes[0].1, 2);
        assert!(found.shapes[0].0.contains("tablesample"), "{}", found.shapes[0].0);
        assert_eq!(found.shapes[1].1, 1);
    }

    #[test]
    fn a_query_with_many_differing_values_is_counted_once_and_not_once_per_value() {
        let value = |row: usize| Difference::Value {
            row,
            column: "c0".to_owned(),
            left: Cell::Text("1".to_owned()),
            right: Cell::Text("2".to_owned()),
        };
        let many = vec![value(0), value(1)];
        let found = Found::of(&report(vec![case("a", many)]), 7, "duckdb", 0);
        assert_eq!(found.shapes.len(), 1, "{:?}", found.shapes);
        assert_eq!(found.shapes[0].1, 1);
    }

    #[test]
    fn the_seed_is_carried_through_because_it_is_the_whole_of_what_replays_a_run() {
        let found = Found::of(&report(Vec::new()), 4294967295, "duckdb", 0);
        assert_eq!(found.seed, 4294967295);
        assert!(found.to_string().contains("seed 4294967295"), "{found}");
    }
}
