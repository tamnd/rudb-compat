//! The sqllogictest file format, as DuckDB writes it.
//!
//! `spec/14-rudb-compat.md` section 14.3 calls DuckDB's `sqllogictest` corpus the highest value
//! first step in the compatibility work, and it is right for a reason that has nothing to do with
//! effort. The corpus is tens of thousands of statements with expected results written by the
//! people who know where the edges are, and every one of them is a claim about behaviour that
//! somebody cared enough about to write down. Nothing we could generate is worth as much.
//!
//! The format is line oriented and older than most of what runs it. A record is a directive line,
//! then the SQL, then optionally a `----` and what the SQL is supposed to produce, and records are
//! separated by blank lines. Comments start with `#`. That is nearly all of it, and the rest is
//! the directives listed on [`Directive`].
//!
//! Two things about this parser are decisions rather than shortcuts.
//!
//! Loops are expanded here rather than interpreted by the runner. A `loop i 0 10` and its
//! `endloop` become ten copies of the records between them with `${i}` substituted, which means
//! the runner sees a flat list and a failure report can name the iteration it came from by naming
//! the SQL it actually ran. Interpreting them instead would put a variable environment in the
//! runner for the sole benefit of not copying some strings.
//!
//! An unknown directive is an error and not a skip. The corpus is vendored from a known DuckDB
//! release, so a directive we have never seen means either the format moved or we are reading the
//! file wrong, and both of those want a person rather than a silently smaller pass rate.

use std::fmt;

/// One record from a `.test` file, after loops are expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The line the directive was on, for the failure report.
    pub line: usize,
    /// Whether this record runs on this engine at all.
    pub condition: Condition,
    /// What it says to do.
    pub directive: Directive,
}

/// What a record says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// `statement ok`, `statement error` or `statement maybe`, then the SQL.
    Statement {
        /// What the statement is supposed to do.
        expected: StatementResult,
        /// The SQL, with the newlines it was written with.
        sql: String,
    },
    /// `query <types> [sortmode] [label]`, then the SQL, then `----` and the results.
    Query {
        /// One character per column, from the set `TIR`, which says how to render a value before
        /// comparing it. Section 14.2's rule that values are compared as text starts here.
        types: String,
        /// How the rows are put in a canonical order before comparing.
        sort: Sort,
        /// The label two queries share when the file says their results must be equal, or empty.
        label: String,
        /// The SQL.
        sql: String,
        /// What it is supposed to produce.
        expected: QueryResult,
    },
    /// `halt`, which stops reading the file where it stands.
    ///
    /// Present in the corpus at the point where the rest of a file is known not to work, so it is
    /// a deliberate end and not a failure. Everything after it is not counted at all, in either
    /// direction.
    Halt,
    /// `hash-threshold N`, the number of values above which results are given as a digest.
    ///
    /// Only relevant when writing a file, since a record that is hashed says so. It is carried
    /// because a run that rewrites expectations needs it and dropping it would lose it.
    HashThreshold(usize),
    /// `require <feature>`, which skips the whole file when the feature is missing.
    Require(String),
    /// `mode <name>`, which sets a parser or runner mode for the rest of the file.
    Mode(String),
    /// Something the format has and this runner does not do: `sleep`, `restart`, `load`, `unzip`
    /// and the rest.
    ///
    /// Carried rather than dropped, because a file that reconnects in the middle is a file whose
    /// later records are about persistence, and running them against a database that never
    /// restarted would report passes that mean nothing.
    Unsupported(String),
}

/// Whether a record runs on this engine.
///
/// The labels in the corpus name engines: `skipif postgresql`, `onlyif duckdb`. rudb answers to
/// both its own name and to `duckdb`, which is not a trick. The whole claim is that rudb is
/// DuckDB, so a record DuckDB is expected to pass is a record rudb is expected to pass, and a
/// record DuckDB is excused from is one we are excused from for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Condition {
    /// No condition, which is nearly every record.
    #[default]
    Always,
    /// `skipif <label>`.
    SkipIf(String),
    /// `onlyif <label>`.
    OnlyIf(String),
    /// A condition on a loop variable that came out false when the loop was expanded.
    ///
    /// The corpus writes `onlyif threadid=0` inside a `concurrentloop threadid 0 20` and means the
    /// record belongs to one iteration. That is not a question about the engine, so it is settled
    /// during expansion and the runner never has to know a loop was involved.
    Never,
}

impl Condition {
    /// Whether an engine answering to these names should run the record.
    #[must_use]
    pub fn applies_to(&self, names: &[&str]) -> bool {
        match self {
            Self::Always => true,
            Self::SkipIf(label) => !names.iter().any(|n| n.eq_ignore_ascii_case(label)),
            Self::OnlyIf(label) => names.iter().any(|n| n.eq_ignore_ascii_case(label)),
            Self::Never => false,
        }
    }
}

/// What a `statement` record expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementResult {
    /// It has to succeed.
    Ok,
    /// It has to fail, and if there is text after the `----` the error has to contain it.
    Error(Option<String>),
    /// Either is fine.
    ///
    /// The corpus uses this where the answer depends on a build option or on the order two
    /// concurrent things happened in. A `maybe` that fails is not a pass, it is not counted.
    Maybe,
}

/// What a `query` record expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResult {
    /// The values in full, already flattened row by row and left to right.
    Values(Vec<String>),
    /// `N values hashing to H`, which is what a result too long to write out becomes.
    Hash {
        /// How many values, which is checked before the digest is.
        count: usize,
        /// The lowercase hex MD5 from [`crate::hash::hash_values`].
        digest: String,
    },
    /// The query has to fail. DuckDB spells this `query <types> error`.
    Error(Option<String>),
}

/// How rows are put in a canonical order before they are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// The order the engine returned them in is the answer. The default, and the only correct
    /// choice when the query has an `ORDER BY`.
    #[default]
    NoSort,
    /// Sort whole rows, comparing column by column as text.
    RowSort,
    /// Sort every value on its own, which loses which row it came from.
    ValueSort,
}

/// A `.test` file that has been read but not run.
#[derive(Debug, Clone)]
pub struct TestFile {
    /// Where it came from, for the report.
    pub name: String,
    /// Every record, in order, with loops already expanded.
    pub records: Vec<Record>,
}

/// A problem with the file itself, which is never a failing test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The line it was found on.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Read a `.test` file.
///
/// # Errors
///
/// When a directive is not one this parser knows, when a record is missing the part after its
/// `----`, or when a loop is not closed.
pub fn parse(name: &str, text: &str) -> Result<TestFile, ParseError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0usize;
    let records = block(&lines, &mut at, false)?;
    Ok(TestFile { name: name.to_owned(), records })
}

/// Read records until the end of the input, or until an `endloop` when one is expected.
///
/// The recursion is the loop nesting, which the corpus does go three deep on.
fn block(lines: &[&str], at: &mut usize, inside_loop: bool) -> Result<Vec<Record>, ParseError> {
    let mut out = Vec::new();
    while *at < lines.len() {
        let line = lines[*at];
        let number = *at + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            *at += 1;
            continue;
        }

        let mut words = trimmed.split_whitespace();
        let first = words.next().unwrap_or("");
        match first {
            "endloop" => {
                if !inside_loop {
                    return Err(ParseError {
                        line: number,
                        message: "an endloop with no loop above it".to_owned(),
                    });
                }
                *at += 1;
                return Ok(out);
            }
            "loop" | "concurrentloop" => {
                // A concurrent loop is run in order here. It exists in the corpus to find races,
                // and rudb has one thread today, so running it sequentially is honest about what
                // was checked rather than pretending to have checked concurrency.
                let name = words.next().unwrap_or("").to_owned();
                let from = number_arg(words.next(), number)?;
                let to = number_arg(words.next(), number)?;
                *at += 1;
                let body = block(lines, at, true)?;
                for i in from..to {
                    out.extend(substitute(&body, &name, &i.to_string()));
                }
            }
            "foreach" | "concurrentforeach" => {
                let name = words.next().unwrap_or("").to_owned();
                let values: Vec<String> = words.flat_map(expand_collection).collect();
                *at += 1;
                let body = block(lines, at, true)?;
                for value in &values {
                    out.extend(substitute(&body, &name, value));
                }
            }
            _ => {
                let record = one(lines, at)?;
                if let Some(record) = record {
                    out.push(record);
                }
            }
        }
    }

    if inside_loop {
        return Err(ParseError {
            line: lines.len(),
            message: "a loop that reaches the end of the file without an endloop".to_owned(),
        });
    }
    Ok(out)
}

/// Read one record, starting at a line that is neither blank nor a comment.
///
/// Returns `None` for a line that carries no record of its own, which is what a bare condition
/// followed by nothing is.
fn one(lines: &[&str], at: &mut usize) -> Result<Option<Record>, ParseError> {
    let mut condition = Condition::Always;

    // The conditions stack in front of the directive they apply to. The corpus only ever writes
    // one, and when there are two the last one wins, which is what DuckDB's own runner does.
    loop {
        let Some(line) = lines.get(*at) else {
            return Ok(None);
        };
        let trimmed = line.trim();
        let mut words = trimmed.split_whitespace();
        match words.next() {
            Some("skipif") => condition = Condition::SkipIf(words.next().unwrap_or("").to_owned()),
            Some("onlyif") => condition = Condition::OnlyIf(words.next().unwrap_or("").to_owned()),
            _ => break,
        }
        *at += 1;
    }

    let number = *at + 1;
    let Some(line) = lines.get(*at) else {
        return Ok(None);
    };
    let trimmed = line.trim();
    let mut words = trimmed.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    *at += 1;

    let directive = match first {
        "statement" => statement(lines, at, &rest, number)?,
        "query" => query(lines, at, &rest, number)?,
        "halt" => Directive::Halt,
        "hash-threshold" => Directive::HashThreshold(number_arg(rest.first().copied(), number)?),
        "require" | "require-env" => Directive::Require(rest.join(" ")),
        "mode" => Directive::Mode(rest.join(" ")),
        "sleep" | "restart" | "reconnect" | "load" | "unzip" | "set" => {
            Directive::Unsupported(trimmed.to_owned())
        }
        other => {
            return Err(ParseError {
                line: number,
                message: format!("{other} is not a directive this parser knows"),
            });
        }
    };

    Ok(Some(Record { line: number, condition, directive }))
}

/// `statement ok`, `statement error` or `statement maybe`, then the SQL.
fn statement(
    lines: &[&str],
    at: &mut usize,
    rest: &[&str],
    number: usize,
) -> Result<Directive, ParseError> {
    let kind = rest.first().copied().unwrap_or("");
    let sql = sql_body(lines, at);
    let expected = match kind {
        "ok" => StatementResult::Ok,
        "maybe" => StatementResult::Maybe,
        "error" => StatementResult::Error(tail(lines, at)),
        other => {
            return Err(ParseError {
                line: number,
                message: format!("a statement is ok, error or maybe, not {other}"),
            });
        }
    };
    if sql.trim().is_empty() {
        return Err(ParseError {
            line: number,
            message: "a statement with no SQL under it".to_owned(),
        });
    }
    Ok(Directive::Statement { expected, sql })
}

/// `query <types> [sortmode] [label]`, then the SQL, then `----` and the results.
fn query(
    lines: &[&str],
    at: &mut usize,
    rest: &[&str],
    number: usize,
) -> Result<Directive, ParseError> {
    let types = rest.first().copied().unwrap_or("").to_owned();
    if types.is_empty() {
        return Err(ParseError {
            line: number,
            message: "a query with no column types after it".to_owned(),
        });
    }

    // `query I error` is DuckDB's spelling for a query that has to fail, and it is the one place
    // where the word in the sort position is not a sort.
    let failing = rest.get(1).copied() == Some("error");
    let sort = match rest.get(1).copied() {
        Some("rowsort") => Sort::RowSort,
        Some("valuesort") => Sort::ValueSort,
        _ => Sort::NoSort,
    };
    let label = if failing { String::new() } else { rest.get(2).copied().unwrap_or("").to_owned() };

    let sql = sql_body(lines, at);
    if sql.trim().is_empty() {
        return Err(ParseError {
            line: number,
            message: "a query with no SQL under it".to_owned(),
        });
    }

    if failing {
        return Ok(Directive::Query {
            types,
            sort,
            label,
            sql,
            expected: QueryResult::Error(tail(lines, at)),
        });
    }

    let column_count = types.chars().count();
    let raw = result_lines(lines, at);
    let expected = expectation(&raw, column_count, number)?;
    Ok(Directive::Query { types, sort, label, sql, expected })
}

/// Turn the lines under a `----` into what the query is supposed to produce.
fn expectation(
    raw: &[String],
    column_count: usize,
    number: usize,
) -> Result<QueryResult, ParseError> {
    if raw.len() == 1 {
        if let Some(hash) = parse_hash(&raw[0]) {
            return Ok(hash);
        }
    }

    // A row per line with tabs between the values, or a value per line. DuckDB writes the second
    // and the older files in the corpus are the first, so both have to be read, and a tab anywhere
    // decides it. A tab inside a value would be read wrong, and a value with a tab in it cannot be
    // written in this format at all, so there is nothing to lose.
    let values: Vec<String> = if raw.iter().any(|line| line.contains('\t')) {
        raw.iter().flat_map(|line| line.split('\t').map(str::to_owned)).collect()
    } else {
        raw.to_vec()
    };

    if column_count > 0 && values.len() % column_count != 0 {
        return Err(ParseError {
            line: number,
            message: format!(
                "{} values under a query of {column_count} columns, which is not a whole number of rows",
                values.len()
            ),
        });
    }
    Ok(QueryResult::Values(values))
}

/// `N values hashing to H`, or nothing.
fn parse_hash(line: &str) -> Option<QueryResult> {
    let mut words = line.split_whitespace();
    let count: usize = words.next()?.parse().ok()?;
    if words.next()? != "values" || words.next()? != "hashing" || words.next()? != "to" {
        return None;
    }
    let digest = words.next()?.to_owned();
    if words.next().is_some() || digest.len() != 32 {
        return None;
    }
    Some(QueryResult::Hash { count, digest })
}

/// The SQL under a directive, which runs to a `----` or a blank line or the end.
///
/// A line that is exactly `endloop` also ends it. The corpus almost always leaves a blank line
/// before one, and where it does not, swallowing the `endloop` into the SQL turns the rest of the
/// file into one unclosed loop. No statement in any dialect is a single line reading `endloop`, so
/// there is nothing this costs.
fn sql_body(lines: &[&str], at: &mut usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    while let Some(line) = lines.get(*at) {
        let trimmed = line.trim();
        if trimmed == "----" || trimmed.is_empty() || trimmed == "endloop" {
            break;
        }
        out.push(line);
        *at += 1;
    }
    out.join("\n")
}

/// The lines after a `----`, or none when there is no `----`.
fn result_lines(lines: &[&str], at: &mut usize) -> Vec<String> {
    if lines.get(*at).map(|l| l.trim()) != Some("----") {
        return Vec::new();
    }
    *at += 1;
    let mut out = Vec::new();
    while let Some(line) = lines.get(*at) {
        if line.trim().is_empty() {
            break;
        }
        out.push((*line).to_owned());
        *at += 1;
    }
    out
}

/// The text after a `----` on an error record, joined back into one string.
fn tail(lines: &[&str], at: &mut usize) -> Option<String> {
    let out = result_lines(lines, at);
    if out.is_empty() {
        return None;
    }
    Some(out.join("\n").trim().to_owned())
}

/// Parse a number out of a directive.
fn number_arg(word: Option<&str>, line: usize) -> Result<usize, ParseError> {
    let word = word.unwrap_or("");
    word.parse()
        .map_err(|_| ParseError { line, message: format!("expected a number and found {word:?}") })
}

/// Expand the named collections a `foreach` can use in place of a list.
///
/// DuckDB's corpus writes `foreach type <numeric>` and means every numeric type. The lists are
/// from `test/sqlite/sqllogic_parser.cpp` and they are copied rather than derived, because a
/// difference between our idea of what is numeric and DuckDB's would quietly change which tests
/// exist.
fn expand_collection(word: &str) -> Vec<String> {
    let named: &[&str] = match word {
        "<integral>" => &["tinyint", "smallint", "integer", "bigint", "hugeint"],
        "<signed>" => &["tinyint", "smallint", "integer", "bigint", "hugeint"],
        "<unsigned>" => &["utinyint", "usmallint", "uinteger", "ubigint", "uhugeint"],
        "<numeric>" => {
            &["tinyint", "smallint", "integer", "bigint", "hugeint", "float", "double", "decimal"]
        }
        "<alltypes>" => &[
            "bool",
            "interval",
            "varchar",
            "tinyint",
            "smallint",
            "integer",
            "bigint",
            "hugeint",
            "utinyint",
            "usmallint",
            "uinteger",
            "ubigint",
            "date",
            "time",
            "timestamp",
            "float",
            "double",
            "decimal",
        ],
        other => return vec![other.to_owned()],
    };
    named.iter().map(|s| (*s).to_owned()).collect()
}

/// Put one iteration's value into every record of a loop body, and settle any condition on the
/// variable.
///
/// Both spellings are replaced. The corpus writes `${type}` in the older files and `{type}` in the
/// newer ones, DuckDB's own runner takes either, and a file using the one we did not handle would
/// come out as several hundred syntax errors that look like a hole in the parser.
///
/// The substitution reaches the expected results and the expected error text as well as the SQL,
/// because a `foreach type <numeric>` whose query returns the type name has the variable on both
/// sides of the `----`.
fn substitute(body: &[Record], name: &str, value: &str) -> Vec<Record> {
    let needles = [format!("${{{name}}}"), format!("{{{name}}}")];
    let put = |text: &str| {
        let mut out = text.to_owned();
        for needle in &needles {
            out = out.replace(needle, value);
        }
        out
    };
    body.iter()
        .map(|record| {
            let mut record = record.clone();
            match &mut record.directive {
                Directive::Statement { sql, expected } => {
                    *sql = put(sql);
                    if let StatementResult::Error(Some(text)) = expected {
                        *text = put(text);
                    }
                }
                Directive::Query { sql, expected, .. } => {
                    *sql = put(sql);
                    match expected {
                        QueryResult::Values(values) => {
                            for value in values.iter_mut() {
                                *value = put(value);
                            }
                        }
                        QueryResult::Error(Some(text)) => *text = put(text),
                        _ => {}
                    }
                }
                _ => {}
            }
            record.condition = resolve(&record.condition, name, value);
            record
        })
        .collect()
}

/// Settle a `skipif name=value` or `onlyif name=value` against one iteration of a loop.
///
/// A condition that names a different variable is left alone, because an inner loop resolves it
/// when its own expansion runs, and a condition that names an engine is not this at all.
fn resolve(condition: &Condition, name: &str, value: &str) -> Condition {
    let (label, negated) = match condition {
        Condition::SkipIf(label) => (label, true),
        Condition::OnlyIf(label) => (label, false),
        other => return other.clone(),
    };
    let Some((variable, wanted)) = label.split_once('=') else {
        return condition.clone();
    };
    if variable != name {
        return condition.clone();
    }
    if (wanted == value) != negated { Condition::Always } else { Condition::Never }
}

#[cfg(test)]
mod tests {
    use super::{Condition, Directive, QueryResult, Sort, StatementResult, parse};

    #[test]
    fn a_statement_and_a_query_read_back_as_what_they_say() {
        let text = "\
statement ok
CREATE TABLE t (a INTEGER)

query I
SELECT a FROM t
----
1
2
";
        let file = parse("x.test", text).unwrap();
        assert_eq!(file.records.len(), 2);
        let Directive::Statement { expected, sql } = &file.records[0].directive else {
            panic!("the first record is a statement");
        };
        assert_eq!(*expected, StatementResult::Ok);
        assert_eq!(sql, "CREATE TABLE t (a INTEGER)");
        let Directive::Query { types, expected, .. } = &file.records[1].directive else {
            panic!("the second record is a query");
        };
        assert_eq!(types, "I");
        assert_eq!(*expected, QueryResult::Values(vec!["1".to_owned(), "2".to_owned()]));
    }

    #[test]
    fn a_row_per_line_with_tabs_is_read_as_values_and_so_is_a_value_per_line() {
        let tabbed = parse("x.test", "query II\nSELECT 1, 2\n----\n1\t2\n").unwrap();
        let stacked = parse("x.test", "query II\nSELECT 1, 2\n----\n1\n2\n").unwrap();
        let expected = QueryResult::Values(vec!["1".to_owned(), "2".to_owned()]);
        for file in [tabbed, stacked] {
            let Directive::Query { expected: got, .. } = &file.records[0].directive else {
                panic!("a query");
            };
            assert_eq!(*got, expected);
        }
    }

    #[test]
    fn a_result_that_is_not_a_whole_number_of_rows_is_a_broken_file_and_not_a_failing_test() {
        let error = parse("x.test", "query II\nSELECT 1, 2\n----\n1\n2\n3\n").unwrap_err();
        assert!(error.message.contains("whole number of rows"), "{}", error.message);
    }

    #[test]
    fn a_hashed_result_carries_the_count_and_the_digest() {
        let text =
            "query I\nSELECT 1\n----\n40 values hashing to 3c13dee48d9356ae19af2515e05e6b54\n";
        let file = parse("x.test", text).unwrap();
        let Directive::Query { expected, .. } = &file.records[0].directive else {
            panic!("a query");
        };
        assert_eq!(
            *expected,
            QueryResult::Hash { count: 40, digest: "3c13dee48d9356ae19af2515e05e6b54".to_owned() }
        );
    }

    #[test]
    fn a_sort_mode_and_a_label_are_read_off_the_directive() {
        let file = parse("x.test", "query I rowsort mylabel\nSELECT 1\n----\n1\n").unwrap();
        let Directive::Query { sort, label, .. } = &file.records[0].directive else {
            panic!("a query");
        };
        assert_eq!(*sort, Sort::RowSort);
        assert_eq!(label, "mylabel");
    }

    #[test]
    fn a_query_that_has_to_fail_does_not_read_error_as_a_sort_mode() {
        let file = parse("x.test", "query I error\nSELECT nope\n----\nno such column\n").unwrap();
        let Directive::Query { expected, sort, .. } = &file.records[0].directive else {
            panic!("a query");
        };
        assert_eq!(*sort, Sort::NoSort);
        assert_eq!(*expected, QueryResult::Error(Some("no such column".to_owned())));
    }

    #[test]
    fn a_condition_attaches_to_the_record_under_it_and_not_to_the_file() {
        let text = "skipif postgresql\nstatement ok\nSELECT 1\n\nstatement ok\nSELECT 2\n";
        let file = parse("x.test", text).unwrap();
        assert_eq!(file.records[0].condition, Condition::SkipIf("postgresql".to_owned()));
        assert_eq!(file.records[1].condition, Condition::Always);
    }

    #[test]
    fn rudb_answers_to_duckdbs_name_because_that_is_the_whole_claim() {
        let names = ["rudb", "duckdb"];
        assert!(!Condition::SkipIf("duckdb".to_owned()).applies_to(&names));
        assert!(Condition::OnlyIf("duckdb".to_owned()).applies_to(&names));
        assert!(Condition::SkipIf("postgresql".to_owned()).applies_to(&names));
    }

    #[test]
    fn a_loop_becomes_one_record_per_iteration_with_the_variable_put_in() {
        let text = "loop i 0 3\nstatement ok\nINSERT INTO t VALUES (${i})\nendloop\n";
        let file = parse("x.test", text).unwrap();
        assert_eq!(file.records.len(), 3);
        let sqls: Vec<&str> = file
            .records
            .iter()
            .map(|r| match &r.directive {
                Directive::Statement { sql, .. } => sql.as_str(),
                _ => panic!("a statement"),
            })
            .collect();
        assert_eq!(
            sqls,
            vec![
                "INSERT INTO t VALUES (0)",
                "INSERT INTO t VALUES (1)",
                "INSERT INTO t VALUES (2)"
            ]
        );
    }

    #[test]
    fn a_foreach_over_a_named_collection_expands_to_the_collection() {
        let text = "foreach t <signed>\nstatement ok\nSELECT CAST(1 AS ${t})\nendloop\n";
        let file = parse("x.test", text).unwrap();
        assert_eq!(file.records.len(), 5);
    }

    #[test]
    fn loops_nest() {
        let text = "loop i 0 2\nloop j 0 3\nstatement ok\nSELECT ${i}, ${j}\nendloop\nendloop\n";
        let file = parse("x.test", text).unwrap();
        assert_eq!(file.records.len(), 6);
    }

    #[test]
    fn a_loop_that_is_never_closed_is_an_error_about_the_file() {
        let error = parse("x.test", "loop i 0 2\nstatement ok\nSELECT 1\n").unwrap_err();
        assert!(error.message.contains("endloop"), "{}", error.message);
    }

    #[test]
    fn a_directive_nobody_has_seen_before_stops_the_file_rather_than_being_skipped() {
        let error = parse("x.test", "frobnicate 3\n").unwrap_err();
        assert!(error.message.contains("frobnicate"), "{}", error.message);
    }

    #[test]
    fn both_spellings_of_a_loop_variable_are_replaced_and_the_results_get_it_too() {
        let text = "foreach t INTEGER\nquery T\nSELECT typeof(1::{t}), '${t}'\n----\n{t}\n${t}\n\nendloop\n";
        let file = parse("x.test", text).unwrap();
        let Directive::Query { sql, expected, .. } = &file.records[0].directive else {
            panic!("a query");
        };
        assert_eq!(sql, "SELECT typeof(1::INTEGER), 'INTEGER'");
        assert_eq!(
            *expected,
            QueryResult::Values(vec!["INTEGER".to_owned(), "INTEGER".to_owned()])
        );
    }

    #[test]
    fn a_condition_on_a_loop_variable_is_settled_when_the_loop_is_expanded() {
        let text = "loop i 0 3\nonlyif i=1\nstatement ok\nSELECT ${i}\nendloop\n";
        let file = parse("x.test", text).unwrap();
        let conditions: Vec<&Condition> = file.records.iter().map(|r| &r.condition).collect();
        assert_eq!(conditions, vec![&Condition::Never, &Condition::Always, &Condition::Never]);
    }

    #[test]
    fn a_condition_on_a_variable_from_an_outer_loop_survives_the_inner_one() {
        let text =
            "loop a 0 2\nloop b 0 2\nskipif a=0\nstatement ok\nSELECT ${a}${b}\nendloop\nendloop\n";
        let file = parse("x.test", text).unwrap();
        let conditions: Vec<&Condition> = file.records.iter().map(|r| &r.condition).collect();
        assert_eq!(
            conditions,
            vec![&Condition::Never, &Condition::Never, &Condition::Always, &Condition::Always]
        );
    }

    #[test]
    fn a_comment_and_a_blank_line_carry_no_record() {
        let file = parse("x.test", "# a note\n\n# another\n").unwrap();
        assert!(file.records.is_empty());
    }
}
