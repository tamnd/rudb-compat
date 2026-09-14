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
use std::fs;
use std::path::Path;

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
    /// `require <what> [argument]`, which can turn the whole file off.
    ///
    /// The words are kept apart rather than joined, because several of them take an argument and
    /// the argument is what decides the answer. `require vector_size 2048` and `require
    /// vector_size 64` are the same requirement of two very different sizes, and a runner that
    /// reads the line as one string cannot tell them apart.
    Require {
        /// Whether the line said `require-env`, which asks about the environment the run is in
        /// rather than about the engine or the build.
        env: bool,
        /// The words after the directive, lowercased nowhere, because the argument of a
        /// `require-env` is an environment variable name and case matters in one.
        params: Vec<String>,
    },
    /// `mode <name>`, which sets a parser or runner mode for the rest of the file.
    Mode(String),
    /// `reset label <name>`, which forgets a result two queries were told to share.
    ///
    /// A file writes this when it reuses a label across iterations of a loop, so that the second
    /// iteration compares its two queries against each other rather than against the first
    /// iteration's. Dropping it would turn that into a wrong answer report on a record that is
    /// fine.
    ResetLabel(String),
    /// `continue`, which ends the iteration of the loop it is in.
    ///
    /// Only ever written under a condition, because a `continue` that always fires would make the
    /// rest of the loop body dead. The loop is expanded by this parser, so the usual case is
    /// settled there and this is what is left when the condition is about the engine rather than
    /// about the loop.
    Continue,
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
    /// Either is fine, and when it fails the error has to be the one named.
    ///
    /// The corpus uses this where the answer depends on a build option or on the order two
    /// concurrent things happened in. It carries a `----` the same way `error` does, and upstream's
    /// parser refuses a `maybe` without one, so reading the directive and leaving the `----` behind
    /// is how twenty nine files in the corpus came back as unreadable.
    Maybe(Option<String>),
}

/// What a `query` record expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResult {
    /// The result block as the file wrote it, one entry per line.
    ///
    /// Not split into values here, because a line is a row in some files and a value in others and
    /// nothing in the file says which. DuckDB decides it against the result that came back, so the
    /// decision belongs where the result is. See [`crate::conform::wanted`].
    Lines(Vec<String>),
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
    parse_under(None, name, text)
}

/// Read a `.test` file that may pull another one in with `include`.
///
/// The root is the directory an `include` path is relative to, which is the top of the corpus and
/// not the directory the file is in. With no root an `include` is an error, the same as any other
/// directive this parser cannot carry out, because a parse with no corpus around it has nowhere to
/// look and quietly dropping the line would run a file that is missing its setup.
///
/// # Errors
///
/// Everything [`parse`] fails on, and an `include` whose file is not there or does not parse.
pub fn parse_under(root: Option<&Path>, name: &str, text: &str) -> Result<TestFile, ParseError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0usize;
    let records = block(root, &lines, &mut at, false, 0)?;
    Ok(TestFile { name: name.to_owned(), records })
}

/// How deep one `include` may reach through another.
///
/// The corpus goes one deep. The limit is here so that a file that includes itself is an error with
/// a line number on it rather than a stack overflow in a test runner.
const NESTING: usize = 8;

/// Read records until the end of the input, or until an `endloop` when one is expected.
///
/// The recursion is the loop nesting, which the corpus does go three deep on, and the `include`
/// nesting, which it does not.
fn block(
    root: Option<&Path>,
    lines: &[&str],
    at: &mut usize,
    inside_loop: bool,
    depth: usize,
) -> Result<Vec<Record>, ParseError> {
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
                let body = block(root, lines, at, true, depth)?;
                for i in from..to {
                    out.extend(iteration(&body, &name, &i.to_string()));
                }
            }
            "foreach" | "concurrentforeach" => {
                let name = words.next().unwrap_or("").to_owned();
                let values: Vec<String> = words.flat_map(expand_collection).collect();
                *at += 1;
                let body = block(root, lines, at, true, depth)?;
                for value in &values {
                    out.extend(iteration(&body, &name, value));
                }
            }
            // Labels that pick out a subset of the corpus to run. They say nothing about what the
            // file does, so the line carries no record.
            "tags" => *at += 1,
            "include" => {
                let path = words.next().unwrap_or("");
                *at += 1;
                out.extend(include(root, path, number, depth)?);
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

/// Read the file an `include` names and hand back its records to be spliced in where it stood.
///
/// The path is relative to the top of the corpus, not to the file doing the including, which is
/// what DuckDB's own parser does in `IncludeFile`. Every use of it in the corpus today points at
/// the same tpch setup template, which is a `require tpch` and a `CALL dbgen`, so what this buys
/// is fifteen files moving from unreadable, which reads as a bug in this parser, to requiring
/// something rudb does not have, which is what is actually true of them.
fn include(
    root: Option<&Path>,
    path: &str,
    number: usize,
    depth: usize,
) -> Result<Vec<Record>, ParseError> {
    let fail = |message: String| ParseError { line: number, message };
    if depth >= NESTING {
        return Err(fail(format!("an include nested more than {NESTING} deep")));
    }
    let Some(root) = root else {
        return Err(fail("an include with no corpus to look in".to_owned()));
    };
    let full = root.join(path);
    let text = fs::read_to_string(&full)
        .map_err(|e| fail(format!("the included {} could not be read, {e}", full.display())))?;
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0usize;
    // The included file is read as a whole file rather than as a continuation, so a loop it opens
    // has to close inside it. That is upstream's rule too and the template obeys it.
    block(Some(root), &lines, &mut at, false, depth + 1)
        .map_err(|e| fail(format!("the included {path} does not parse, {e}")))
}

/// One turn of a loop, with the variable put in and a `continue` taken at its word.
///
/// `continue` ends the iteration it is in. The loop is expanded here rather than interpreted, so an
/// iteration is a stretch of records this function is holding and ending it is a truncation. The
/// condition on the `continue` is the loop variable in every use of it in the corpus, and
/// [`substitute`] has already settled that into [`Condition::Always`] or [`Condition::Never`], so
/// by this point the question is answerable. A condition naming an engine rather than the loop is
/// left alone for the runner, because this is the wrong place to know which engine is running.
fn iteration(body: &[Record], name: &str, value: &str) -> Vec<Record> {
    let mut out = substitute(body, name, value);
    let stop = out.iter().position(|record| {
        record.directive == Directive::Continue && record.condition == Condition::Always
    });
    if let Some(stop) = stop {
        out.truncate(stop);
    }
    out
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
        "require" | "require-env" => Directive::Require {
            env: first == "require-env",
            params: rest.iter().map(|word| (*word).to_owned()).collect(),
        },
        "mode" => Directive::Mode(rest.join(" ")),
        "continue" => Directive::Continue,
        "reset" => match (rest.first().copied(), rest.get(1).copied()) {
            (Some("label"), Some(name)) => Directive::ResetLabel(name.to_owned()),
            _ => {
                return Err(ParseError {
                    line: number,
                    message: "a reset is reset label followed by a name".to_owned(),
                });
            }
        },
        "sleep" | "restart" | "reconnect" | "load" | "unzip" | "set" | "test-env" => {
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
        "maybe" => StatementResult::Maybe(tail(lines, at)),
        "error" => StatementResult::Error(tail(lines, at)),
        other => {
            return Err(ParseError {
                line: number,
                message: format!("a statement is ok, error or maybe, not {other}"),
            });
        }
    };
    if sql.is_empty() {
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
    if sql.is_empty() {
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

    let raw = result_lines(lines, at);
    Ok(Directive::Query { types, sort, label, sql, expected: expectation(raw) })
}

/// Turn the lines under a `----` into what the query is supposed to produce.
fn expectation(raw: Vec<String>) -> QueryResult {
    if raw.len() == 1 {
        if let Some(hash) = parse_hash(&raw[0]) {
            return hash;
        }
    }
    QueryResult::Lines(raw)
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

/// A line the way upstream's parser sees it, which is without the carriage return a Windows
/// checkout leaves behind and with nothing else taken off.
fn plain(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// Whether a line ends the record it is inside.
///
/// Empty means empty, and not blank. `sqllogic_parser.cpp` asks `line.empty()`, so a line of eighty
/// spaces is content and a line of nothing is a separator. That is not a detail. `test_bar.test`
/// draws bar charts and its first bar is the empty one, so a reader that ends the result at a blank
/// line reads the second bar as a directive and throws the file away.
fn ends_record(line: &str) -> bool {
    plain(line).is_empty()
}

/// The SQL under a directive, which runs to a `----` or an empty line or a comment or the end.
///
/// A comment ends it because upstream's `ExtractStatement` stops at `EmptyOrComment`, so a `#` at
/// the start of a line inside a statement is the end of that statement and not part of the SQL.
///
/// A line that is exactly `endloop` also ends it, which is ours rather than upstream's. The corpus
/// almost always leaves a blank line before one, and where it does not, swallowing the `endloop`
/// into the SQL turns the rest of the file into one unclosed loop. No statement in any dialect is a
/// single line reading `endloop`, so there is nothing this costs.
fn sql_body(lines: &[&str], at: &mut usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    while let Some(line) = lines.get(*at) {
        let line = plain(line);
        if line == "----" || line.is_empty() || line.starts_with('#') || line.trim() == "endloop" {
            break;
        }
        out.push(line);
        *at += 1;
    }
    out.join("\n")
}

/// The lines after a `----`, or none when there is no `----`.
fn result_lines(lines: &[&str], at: &mut usize) -> Vec<String> {
    if lines.get(*at).map(|l| plain(l)) != Some("----") {
        return Vec::new();
    }
    *at += 1;
    let mut out = Vec::new();
    while let Some(line) = lines.get(*at) {
        if ends_record(line) {
            break;
        }
        out.push(plain(line).to_owned());
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
                        QueryResult::Lines(values) => {
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
        assert_eq!(*expected, QueryResult::Lines(vec!["1".to_owned(), "2".to_owned()]));
    }

    #[test]
    fn a_result_block_is_kept_as_lines_because_the_parser_cannot_tell_a_row_from_a_value() {
        let tabbed = parse("x.test", "query II\nSELECT 1, 2\n----\n1\t2\n").unwrap();
        let stacked = parse("x.test", "query II\nSELECT 1, 2\n----\n1\n2\n").unwrap();
        let blocks = [vec!["1\t2".to_owned()], vec!["1".to_owned(), "2".to_owned()]];
        for (file, lines) in [tabbed, stacked].into_iter().zip(blocks) {
            let Directive::Query { expected: got, .. } = &file.records[0].directive else {
                panic!("a query");
            };
            assert_eq!(*got, QueryResult::Lines(lines));
        }
    }

    #[test]
    fn a_result_that_is_not_a_whole_number_of_rows_is_still_a_file_this_reader_can_read() {
        // It is one record that cannot be right rather than a file with no outcome at all, which
        // is the same call DuckDB makes and it makes it against the result rather than the text.
        let file = parse("x.test", "query II\nSELECT 1, 2\n----\n1\n2\n3\n").unwrap();
        assert_eq!(file.records.len(), 1);
    }

    #[test]
    fn a_line_of_spaces_is_a_value_and_only_a_line_of_nothing_ends_the_record() {
        // `test_bar.test` draws bar charts and the first bar is the empty one, so this is the
        // difference between reading the file and throwing it away.
        let text = "query I\nSELECT bar(x)\n----\n   \n###\n\nstatement ok\nSELECT 1\n";
        let file = parse("x.test", text).unwrap();
        assert_eq!(file.records.len(), 2);
        let Directive::Query { expected, .. } = &file.records[0].directive else {
            panic!("a query");
        };
        assert_eq!(*expected, QueryResult::Lines(vec!["   ".to_owned(), "###".to_owned()]));
    }

    #[test]
    fn sql_that_is_nothing_but_a_space_no_keyboard_has_is_still_sql() {
        // `invisible_spaces.test` writes a statement whose whole body is U+2000, and asks for it to
        // be accepted. Trimming the body the way Rust trims it makes that an empty statement and
        // loses the file, and the line upstream draws is between nothing and anything at all.
        let file = parse("x.test", "statement ok\n\u{2000}\n").unwrap();
        let Directive::Statement { sql, .. } = &file.records[0].directive else {
            panic!("a statement");
        };
        assert_eq!(sql, "\u{2000}");
    }

    #[test]
    fn a_comment_ends_the_sql_above_it_the_way_upstream_ends_it() {
        let file = parse("x.test", "statement ok\nSELECT 1\n# and that is all\n").unwrap();
        let Directive::Statement { sql, .. } = &file.records[0].directive else {
            panic!("a statement");
        };
        assert_eq!(sql, "SELECT 1");
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
        assert_eq!(*expected, QueryResult::Lines(vec!["INTEGER".to_owned(), "INTEGER".to_owned()]));
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
    fn a_requirement_keeps_its_argument_apart_from_its_name() {
        let file = parse("x.test", "require vector_size 2048\n").unwrap();
        let Directive::Require { env, params } = &file.records[0].directive else {
            panic!("a require");
        };
        assert!(!env);
        assert_eq!(params, &["vector_size".to_owned(), "2048".to_owned()]);

        let file = parse("x.test", "require-env LOCAL_EXTENSION_REPO\n").unwrap();
        let Directive::Require { env, params } = &file.records[0].directive else {
            panic!("a require");
        };
        assert!(env);
        assert_eq!(params, &["LOCAL_EXTENSION_REPO".to_owned()]);
    }

    #[test]
    fn a_comment_and_a_blank_line_carry_no_record() {
        let file = parse("x.test", "# a note\n\n# another\n").unwrap();
        assert!(file.records.is_empty());
    }

    #[test]
    fn a_maybe_takes_its_result_block_the_same_way_an_error_does() {
        // Twenty nine files in the corpus came back as unreadable because this did not. The
        // directive was read, the `----` under it was left where it was, and the next pass over the
        // file found a line reading `----` and called it a directive it did not know.
        let file = parse("x.test", "statement maybe\nINSERT INTO t VALUES (1)\n----\n\n").unwrap();
        assert_eq!(file.records.len(), 1);
        let Directive::Statement { expected, .. } = &file.records[0].directive else {
            panic!("a statement");
        };
        assert_eq!(*expected, StatementResult::Maybe(None));

        let text = "statement maybe\nINSERT INTO t VALUES (1)\n----\nConstraint Error\n";
        let file = parse("x.test", text).unwrap();
        let Directive::Statement { expected, .. } = &file.records[0].directive else {
            panic!("a statement");
        };
        assert_eq!(*expected, StatementResult::Maybe(Some("Constraint Error".to_owned())));
    }

    #[test]
    fn a_tags_line_is_about_which_files_to_run_and_carries_no_record() {
        let file = parse("x.test", "tags release\n\nstatement ok\nSELECT 1\n").unwrap();
        assert_eq!(file.records.len(), 1);
    }

    #[test]
    fn a_reset_names_the_label_it_forgets() {
        let file = parse("x.test", "reset label expected_res\n").unwrap();
        assert_eq!(file.records[0].directive, Directive::ResetLabel("expected_res".to_owned()));
        assert!(parse("x.test", "reset something_else\n").is_err());
    }

    #[test]
    fn a_continue_ends_the_turn_of_the_loop_it_fires_on_and_leaves_the_others_whole() {
        // The corpus writes this under a condition on the loop variable, which is settled when the
        // loop is expanded, so by the time the runner sees the records the skipped turn is simply
        // not in them.
        let text = "\
foreach col a b

onlyif col=b
continue

statement ok
SELECT {col}

endloop
";
        let file = parse("x.test", text).unwrap();
        let sql: Vec<&str> = file
            .records
            .iter()
            .filter_map(|record| match &record.directive {
                Directive::Statement { sql, .. } => Some(sql.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(sql, ["SELECT a"]);
    }

    #[test]
    fn an_include_with_no_corpus_to_look_in_is_an_error_and_not_a_line_that_is_dropped() {
        // Dropping it would run a file that is missing its setup and then report on what happened,
        // which is a worse answer than saying the parser could not do it.
        let e = parse("x.test", "include test/sql/tpch/tpch_setup.test_template\n").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.message.contains("include"), "{}", e.message);
    }

    #[test]
    fn an_include_puts_the_records_of_the_named_file_where_the_line_stood() {
        // Under the crate's own target directory rather than the machine's temporary one, so a run
        // leaves nothing behind anywhere the next run does not already clean.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("include");
        let inner = root.join("test").join("sql").join("setup");
        std::fs::create_dir_all(&inner).unwrap();
        let template = inner.join("template.test_template");
        std::fs::write(&template, "require tpch\n\nstatement ok\nCALL dbgen(sf=0)\n").unwrap();

        let text = "statement ok\nSELECT 1\n\ninclude test/sql/setup/template.test_template\n";
        let file = super::parse_under(Some(&root), "x.test", text).unwrap();
        assert_eq!(file.records.len(), 3);
        assert!(matches!(file.records[1].directive, Directive::Require { .. }));
        let Directive::Statement { sql, .. } = &file.records[2].directive else {
            panic!("a statement");
        };
        assert_eq!(sql, "CALL dbgen(sf=0)");

        let e = super::parse_under(Some(&root), "x.test", "include nowhere.test\n").unwrap_err();
        assert!(e.message.contains("could not be read"), "{}", e.message);
        std::fs::remove_file(&template).unwrap();
    }
}
