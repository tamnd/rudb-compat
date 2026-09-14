//! The real query corpus, and what it says about which of the catalog is worth anything.
//!
//! `spec/sql/duckdb/11-the-number.md` says every percentage in the spec folder is unweighted until
//! there is a corpus of queries somebody meant to write, and that the weights have to come from
//! such a corpus rather than from anybody's opinion about which functions matter. This is that
//! corpus, and its pass rate is the least interesting thing about it. What it is for is the
//! histogram: which of the eleven hundred names in the catalog appear in a query a person wrote,
//! how often, and which of them appear nowhere at all.
//!
//! ## Where the queries come from
//!
//! DuckDB's own benchmark suite, which is already in the vendored clone. It is ClickBench, the
//! join order benchmark over IMDB, TPC-H, TPC-DS, h2oai, LDBC, the JSON benchmarks, the taxi data
//! and several hundred micro benchmarks, and every one of those is a query somebody wrote to
//! measure something rather than a query somebody wrote to test something. That distinction is the
//! whole point. A sqllogictest file is written to break an engine and weights `SELECT 1` the same
//! as a seven way join. A benchmark is written because somebody cared how fast it was.
//!
//! Nothing here is run. The loads in that suite build tables of a hundred million rows and the
//! histogram does not need them, so this reads and counts and stops there. A pass rate over these
//! queries wants loads cut down to a size a test can carry, which is a separate job and a later
//! one.
//!
//! ## The file format
//!
//! A `.benchmark` file is directives at the left margin, each either taking the rest of its line or
//! introducing a block that runs to the next blank line. The ones that matter here are `name`,
//! `group`, `require`, and then `run` and `load`, each of which is either the SQL itself as a block
//! or the path of a `.sql` file holding it.
//!
//! Rather more than half of them are not files but instances of one: `template <path>` followed by
//! `KEY=VALUE` lines, against a `.benchmark.in` that has `${KEY}` in it and may `include` another
//! one. Expanding those is what takes the corpus from five hundred queries to eleven hundred, and
//! the five hundred it adds are the benchmark suites proper, so leaving them out would leave the
//! corpus made mostly of micro benchmarks and give a histogram about the wrong thing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::engine::HarnessError;
use crate::functions::Overload;

/// Where the benchmark suite sits inside the vendored clone.
pub const BENCHMARKS: &str = "benchmark";

/// One query somebody wrote on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// What the benchmark calls itself, or the file stem when it says nothing.
    pub name: String,
    /// The suite it belongs to, which is the `group` directive or the directory it sits in.
    pub group: String,
    /// The file it came from, relative to the clone.
    pub file: String,
    /// The query.
    pub sql: String,
    /// What has to be in the database before the query means anything, one statement at a time.
    /// Empty for a query that builds its own rows out of `range`.
    pub load: Vec<String>,
    /// What the benchmark says it needs before it will run, which is extensions and settings.
    pub requires: Vec<String>,
}

/// Read every benchmark under a vendored DuckDB clone.
///
/// A file with no `run` is not a query and is skipped rather than reported. The suite has fragments
/// and load scripts in it that are there to be included by something else, and a reader that
/// complained about each of them would complain about a third of the directory.
///
/// A query that appears twice is counted once, which matters more than it sounds. TPC-H is
/// twenty two queries and there are a hundred and eight benchmark files holding them, because the
/// same query is run at several scale factors and against several storage settings. Those are five
/// measurements of one query, and counting them five times would weight the histogram towards the
/// two suites that happen to be parameterised that way.
///
/// # Errors
///
/// When the benchmark directory is not there, which means the clone was made before this was in the
/// sparse checkout and wants a `--refresh`.
pub fn read(clone: &Path) -> Result<Vec<Query>, HarnessError> {
    let root = clone.join(BENCHMARKS);
    if !root.is_dir() {
        return Err(HarnessError::new(format!(
            "no benchmark suite at {}. The clone is older than this reader, so fetch it again with --refresh",
            root.display()
        )));
    }
    let mut files = Vec::new();
    walk(&root, &mut files)?;
    files.sort();
    let mut found = Vec::new();
    for file in files {
        if let Some(query) = one(clone, &file) {
            found.push(query);
        }
    }
    Ok(distinct(found))
}

/// The queries with the repeats dropped, keeping the first file that held each one.
#[must_use]
pub fn distinct(found: Vec<Query>) -> Vec<Query> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    found.into_iter().filter(|query| seen.insert(query.sql.clone())).collect()
}

/// Every `.benchmark` under a directory, including the ones below it.
fn walk(dir: &Path, into: &mut Vec<PathBuf>) -> Result<(), HarnessError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| HarnessError::new(format!("cannot read {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, into)?;
        } else if path.extension().is_some_and(|ext| ext == "benchmark") {
            into.push(path);
        }
    }
    Ok(())
}

/// One file into the query it holds, when it holds one.
fn one(clone: &Path, file: &Path) -> Option<Query> {
    let text = std::fs::read_to_string(file).ok()?;
    let text = expanded(clone, &text)?;
    let sql = part(clone, &text, "run")?;
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        return None;
    }
    let relative = file.strip_prefix(clone).unwrap_or(file).to_string_lossy().into_owned();
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Some(Query {
        name: directive(&text, "name").unwrap_or(stem),
        group: directive(&text, "group").unwrap_or_else(|| suite(&relative)),
        file: relative,
        sql: sql.to_owned(),
        load: statements(&part(clone, &text, "load").unwrap_or_default()),
        requires: every(&text, "require"),
    })
}

/// A block of SQL as the statements it holds.
///
/// `rudb::split` first, because it is a real splitter and knows a semicolon inside a string is not
/// the end of anything. A load it cannot parse falls back to splitting on the line ends, which is
/// how the rest of this file reads the format anyway.
fn statements(text: &str) -> Vec<String> {
    let split = crate::suite::statements(text);
    if !split.is_empty() {
        return split;
    }
    text.split(";\n")
        .map(|one| one.trim().trim_end_matches(';').trim().to_owned())
        .filter(|one| !one.is_empty())
        .collect()
}

/// Every line of a directive that can appear more than once.
fn every(text: &str, name: &str) -> Vec<String> {
    let wanted = format!("{name} ");
    text.lines()
        .filter_map(|line| line.strip_prefix(&wanted))
        .map(|rest| rest.trim().to_owned())
        .collect()
}

/// The suite a file belongs to when it does not say, which is the directory under `benchmark`.
fn suite(relative: &str) -> String {
    relative.split('/').nth(1).map_or_else(|| "benchmark".to_owned(), ToOwned::to_owned)
}

/// A file with its template filled in, or itself when it is not an instance of one.
///
/// Nothing recurses here beyond one template and the fragments it includes, because the suite does
/// not nest them and a reader that allowed it would need a cycle check for a case that does not
/// exist.
fn expanded(clone: &Path, text: &str) -> Option<String> {
    let Some(path) = directive(text, "template") else {
        return Some(text.to_owned());
    };
    let template = std::fs::read_to_string(clone.join(path)).ok()?;
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if !key.is_empty() && key.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                values.insert(key.to_owned(), value.trim().to_owned());
            }
        }
    }
    let mut filled = fill(&included(clone, &template), &values);
    // A template can name a value that the instance did not set, and what upstream's runner does
    // with one is leave it empty. Leaving the `${...}` in would put it in a path or in the SQL.
    while let Some(at) = filled.find("${") {
        match filled[at..].find('}') {
            Some(end) => filled.replace_range(at..=at + end, ""),
            None => break,
        }
    }
    Some(filled)
}

/// A template with the fragments it includes spliced in where the `include` line was.
fn included(clone: &Path, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match line.strip_prefix("include ") {
            Some(path) => match std::fs::read_to_string(clone.join(path.trim())) {
                Ok(fragment) => out.push_str(&fragment),
                Err(_) => out.push_str(line),
            },
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// Every `${KEY}` replaced by what the instance said it was.
fn fill(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = text.to_owned();
    for (key, value) in values {
        out = out.replace(&format!("${{{key}}}"), value);
    }
    out
}

/// The SQL a directive carries, whether the file wrote it out or named the file holding it.
fn part(clone: &Path, text: &str, name: &str) -> Option<String> {
    if let Some(path) = directive(text, name) {
        return std::fs::read_to_string(clone.join(path.trim())).ok();
    }
    block(text, name)
}

/// The rest of the line after a directive, when the directive has one.
fn directive(text: &str, name: &str) -> Option<String> {
    let wanted = format!("{name} ");
    text.lines()
        .find(|line| line.starts_with(&wanted))
        .map(|line| line[wanted.len()..].trim().to_owned())
        .filter(|rest| !rest.is_empty())
}

/// The block a directive on a line of its own introduces, which runs to the next blank line.
fn block(text: &str, name: &str) -> Option<String> {
    let mut lines = text.lines().skip_while(|line| line.trim_end() != name);
    lines.next()?;
    let body: Vec<&str> = lines.take_while(|line| !line.trim().is_empty()).collect();
    Some(body.join("\n"))
}

/// How often each catalog name is called, and by how many distinct queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    /// How many queries were read.
    pub queries: usize,
    /// Each name that appears, with the calls and then the queries that hold them, most used
    /// first.
    pub used: Vec<(String, usize, usize)>,
    /// How many catalog names appear in no query at all.
    pub unused: usize,
    /// How many distinct names the catalog has. Not the overload count, which is three times
    /// larger, because a histogram over text cannot tell one overload of a name from another.
    pub names: usize,
    /// Each suite, with how many queries came from it.
    pub groups: Vec<(String, usize)>,
}

/// Count which of the catalog the corpus actually calls.
///
/// A name counts when it is followed by an opening bracket, which is what tells a call from a
/// column that happens to share a name with a function. That rule undercounts on purpose in two
/// places and it is worth saying where. An operator is a function in the catalog and is never
/// written as a call, so `+` and `~~` score nothing here. And a function called through a keyword,
/// which is `EXTRACT` and `CAST` and `SUBSTRING FROM FOR`, scores nothing either. Both of those
/// want the parser rather than a scan over text, and a histogram that guessed at them would be a
/// histogram nobody could check by reading a file.
///
/// Matching is case insensitive because the corpus writes `COUNT` and `count` about equally, and
/// the catalog is lowercase.
#[must_use]
pub fn histogram(queries: &[Query], catalog: &[Overload]) -> Histogram {
    let known: BTreeMap<String, ()> =
        catalog.iter().map(|overload| (overload.name.to_ascii_lowercase(), ())).collect();
    let mut calls: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
    for query in queries {
        *groups.entry(query.group.as_str()).or_default() += 1;
        let mut here: BTreeMap<String, usize> = BTreeMap::new();
        for name in called(&query.sql) {
            if known.contains_key(&name) {
                *here.entry(name).or_default() += 1;
            }
        }
        for (name, found) in here {
            let known = known.get_key_value(&name).map_or("", |(key, ())| key.as_str());
            let row = calls.entry(known).or_default();
            row.0 += found;
            row.1 += 1;
        }
    }
    let mut used: Vec<(String, usize, usize)> = calls
        .iter()
        .map(|(name, (found, queries))| ((*name).to_owned(), *found, *queries))
        .collect();
    used.sort_by_key(|(name, found, _)| (std::cmp::Reverse(*found), name.clone()));
    let mut groups: Vec<(String, usize)> =
        groups.into_iter().map(|(name, found)| (name.to_owned(), found)).collect();
    groups.sort_by_key(|(name, found)| (std::cmp::Reverse(*found), name.clone()));
    Histogram {
        queries: queries.len(),
        unused: known.len() - calls.len(),
        names: known.len(),
        used,
        groups,
    }
}

/// Every name in a statement that is written as a call, lowercased.
fn called(sql: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut word = String::new();
    let mut in_text = false;
    for byte in sql.as_bytes() {
        let c = *byte as char;
        if c == '\'' {
            in_text = !in_text;
            word.clear();
            continue;
        }
        if in_text {
            continue;
        }
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c.to_ascii_lowercase());
            continue;
        }
        if c == '(' && !word.is_empty() && !word.starts_with(|c: char| c.is_ascii_digit()) {
            found.push(word.clone());
        }
        // The bracket has to come straight after the name. A space between the two is legal SQL and
        // this misses it, and the corpus was checked before that was written down: every name it
        // writes with a space before a bracket is a keyword, `as` and `in` and `from` and `select`,
        // and not one of them is a function. So the strict rule loses nothing here and the loose
        // one would count `IN (` and `EXISTS (` as calls.
        word.clear();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{Histogram, Query, called, distinct, histogram};
    use crate::functions::{Kind, Overload};

    fn overload(name: &str) -> Overload {
        Overload {
            name: name.to_owned(),
            kind: Kind::Scalar,
            returns: "INTEGER".to_owned(),
            parameters: Vec::new(),
            varargs: None,
            internal: true,
        }
    }

    fn query(group: &str, sql: &str) -> Query {
        Query {
            name: "q".to_owned(),
            group: group.to_owned(),
            file: "benchmark/x/q.benchmark".to_owned(),
            sql: sql.to_owned(),
            load: Vec::new(),
            requires: Vec::new(),
        }
    }

    #[test]
    fn a_name_with_a_bracket_after_it_is_a_call_and_a_bare_name_is_not() {
        assert_eq!(called("SELECT count(*) FROM count"), vec!["count".to_owned()]);
    }

    #[test]
    fn the_bracket_has_to_come_straight_after_the_name_so_a_keyword_is_never_a_call() {
        assert!(called("SELECT sum (a)").is_empty());
        assert!(called("WHERE x IN (1, 2)").is_empty());
    }

    #[test]
    fn a_name_inside_a_string_is_not_a_call_however_much_it_looks_like_one() {
        assert!(called("SELECT 'count(x)'").is_empty(), "{:?}", called("SELECT 'count(x)'"));
    }

    #[test]
    fn a_bracket_after_something_that_is_not_a_name_is_not_a_call() {
        assert!(called("SELECT (a + b) * 2").is_empty());
        assert!(called("SELECT x[1](2)").is_empty());
    }

    #[test]
    fn only_names_the_pinned_catalog_has_are_counted() {
        let counted =
            histogram(&[query("a", "SELECT count(x), notafunction(y)")], &[overload("count")]);
        assert_eq!(counted.used.len(), 1, "{:?}", counted.used);
        assert_eq!(counted.used[0].0, "count");
    }

    #[test]
    fn a_name_used_twice_in_one_query_is_two_calls_and_one_query() {
        let counted = histogram(&[query("a", "SELECT count(x), count(y)")], &[overload("count")]);
        assert_eq!(counted.used[0], ("count".to_owned(), 2, 1));
    }

    #[test]
    fn the_names_are_ordered_by_how_often_they_are_called_and_not_alphabetically() {
        let queries = vec![query("a", "SELECT sum(a), sum(b), count(c)")];
        let counted = histogram(&queries, &[overload("count"), overload("sum")]);
        assert_eq!(counted.used[0].0, "sum", "{:?}", counted.used);
        assert_eq!(counted.used[1].0, "count");
    }

    #[test]
    fn a_catalog_name_no_query_calls_is_counted_as_unused_because_that_is_the_point_of_this() {
        let counted = histogram(
            &[query("a", "SELECT count(x)")],
            &[overload("count"), overload("nobody_calls_this"), overload("nor_this")],
        );
        assert_eq!(counted.unused, 2);
    }

    #[test]
    fn one_query_in_five_benchmark_files_is_one_query_and_the_first_file_is_the_one_kept() {
        let mut twice = query("tpch", "SELECT 1");
        twice.file = "benchmark/tpch/sf100/q01.benchmark".to_owned();
        let kept = distinct(vec![query("tpch", "SELECT 1"), twice, query("tpch", "SELECT 2")]);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(kept[0].file, "benchmark/x/q.benchmark");
    }

    #[test]
    fn the_suites_are_counted_with_the_largest_first() {
        let queries =
            vec![query("micro", "SELECT 1"), query("tpch", "SELECT 2"), query("micro", "SELECT 3")];
        let Histogram { groups, queries: how_many, .. } = histogram(&queries, &[]);
        assert_eq!(how_many, 3);
        assert_eq!(groups, vec![("micro".to_owned(), 2), ("tpch".to_owned(), 1)]);
    }

    #[test]
    fn the_matching_is_case_insensitive_because_the_corpus_shouts_and_the_catalog_does_not() {
        let counted = histogram(&[query("a", "SELECT COUNT(x)")], &[overload("count")]);
        assert_eq!(counted.used[0].0, "count", "{:?}", counted.used);
    }
}
