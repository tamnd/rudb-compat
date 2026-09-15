//! Our own generator, pointed at both engines, which is the level two statement number.
//!
//! `rudb::generate` walks the 1088 rule grammar table in the other direction from the matcher and
//! writes statements the grammar can produce. Run on its own it is a parser exerciser and it says
//! nothing about DuckDB. Run here it answers the question the whole milestone is about: a statement
//! DuckDB parses and rudb does not is a query somebody cannot run at all, and it is the `Syntax`
//! reason in `spec/sql/duckdb/11-the-number.md`.
//!
//! This is the other half of `sqlsmith`. That one is upstream's generator and writes queries out of
//! a catalog, so what it produces is realistic and narrow: deep joins, correlated subqueries and
//! casts, all of them `SELECT`. This one is written out of the grammar itself, so what it produces
//! is unrealistic and wide: all thirty six statement kinds, in proportion to how cheaply the
//! grammar can write each one, reaching clauses nobody has ever typed. Neither is a substitute for
//! the other and the two find different gaps.
//!
//! # Why it only asks whether the statement parses
//!
//! Because running it would be a different question and a destructive one. A walk of the whole
//! grammar writes `DROP`, `ATTACH`, `EXPORT DATABASE`, `COPY t TO 'out.csv'` and `INSTALL`, and a
//! harness that executes those is a harness that writes files into whatever directory it was
//! started in. Both engines are asked `accepts`, which parses and stops, so the run touches
//! nothing. The generated statements also refer to a catalog that does not have to exist, which is
//! what makes the answer a fact about the two grammars rather than about two schemas.
//!
//! What that gives up is real. A statement both engines parse can still be bound by one and refused
//! by the other, and this will never see it. That is the level three number and it has its own
//! sources, the corpus and `sqlsmith`, which run statements somebody meant.
//!
//! # Why both refusing is counted apart
//!
//! Same argument as in [`crate::sqlsmith`] and for a different reason. Ordered choice means the
//! generator can write the fifth alternative of a choice and the matcher settle on the second,
//! which leaves the rest of the sequence with nothing to match, so a share of what comes out is
//! text the grammar can produce and no PEG over that grammar accepts. `EXPLAIN ANALYZE` is the two
//! word example. DuckDB's parser is not a PEG at all and refuses those for its own reasons, so the
//! two complaints put side by side say nothing about whether the two engines agree. They are
//! counted and set aside.
//!
//! This is also why the two answers are compared here rather than read out of [`crate::compare`].
//! That comparison is built for records that run, so two engines refusing the same statement with
//! the same error kind is no difference at all to it, and a mode where most of what comes out is
//! refused by both would read every one of those as agreement. Four cases, written out.
//!
//! # Which direction this can actually find something in
//!
//! Not the gap, and that is worth saying plainly before somebody quotes the number. The generator
//! writes from the same table the matcher reads, so almost everything it writes is text our own
//! parser accepts by construction, and a source that cannot write a statement rudb refuses cannot
//! measure what rudb refuses. The first two runs of 2000 statements found nothing in the gap, which
//! is what the construction predicts rather than a compatibility result. What the column is worth
//! here is as a check on that construction: a number above zero would mean the generator and the
//! matcher disagree about the grammar they share, which is a bug in one of them.
//!
//! The other direction is what this finds, and it found a lot of it: 300 statements from
//! `Statement` and 1264 from `SelectStatement` that rudb parses and DuckDB does not. They say one
//! thing repeatedly, which is that DuckDB checks things while it parses that its own grammar
//! allows. `SELECT` with no select list, a CTE body that is a `CHECKPOINT`, an empty subscript, two
//! aliases on one table reference. rudb has no transformer, so it takes all of them. That surface
//! mostly closes with the binder rather than with the parser, which is why they are grouped and
//! printed rather than filed one at a time.

use std::collections::BTreeMap;
use std::fmt;

use rudb::generate::{Catalog, Table};

use crate::engine::{Acceptance, Engine, EngineError, HarnessError};
use crate::sqlsmith::generalised;

/// How many statements a run writes when nobody said a number.
///
/// Generation is in process and costs microseconds, and parsing them on our side costs about as
/// much again, so the whole cost of a run is DuckDB, which is one process per statement. Two
/// thousand is a few minutes and is enough that the groups at the bottom stop being one offs.
pub const STATEMENTS: usize = 2000;

/// The rule statements are written from when nobody names one.
///
/// The whole statement grammar rather than the query rule, because the thing being counted is the
/// statement surface and `SELECT` is one of thirty six alternatives in it. Somebody who wants
/// queries asks for `SelectStatement`, which is worth doing on its own run: from `Statement` only a
/// small share of what comes out contains a query at all.
pub const RULE: &str = "Statement";

/// The names the generator writes into a statement.
///
/// The same five tables `sqlsmith` creates, so the two generated sources talk about one schema and
/// a finding from either reads the same way. Nothing here is created in either engine, because
/// parsing does not need a catalog, and that is the point: the answer is about the two grammars.
#[must_use]
pub fn catalog() -> Catalog {
    Catalog {
        tables: vec![
            table("ints", &["a", "b", "c", "d", "e"]),
            table("reals", &["a", "b", "c"]),
            table("words", &["a", "b"]),
            table("times", &["a", "b", "c", "d"]),
            table("flags", &["a", "b"]),
        ],
        functions: names(&["abs", "length", "upper", "count", "coalesce", "least", "nullif"]),
        table_functions: names(&["range", "generate_series", "read_csv", "read_parquet"]),
        types: names(&["INTEGER", "BIGINT", "VARCHAR", "DOUBLE", "DECIMAL(18,3)", "DATE", "BLOB"]),
        schemas: names(&["main"]),
        catalogs: names(&["memory"]),
        pragmas: names(&["database_list", "show_tables", "table_info"]),
        settings: names(&["threads", "memory_limit", "default_null_order"]),
        files: names(&["'data.parquet'", "'data.csv'"]),
        variables: names(&["v"]),
    }
}

fn table(name: &str, columns: &[&str]) -> Table {
    Table { name: name.to_owned(), columns: names(columns) }
}

fn names(from: &[&str]) -> Vec<String> {
    from.iter().map(|name| (*name).to_owned()).collect()
}

/// Write that many statements from that rule, starting at that seed.
///
/// One seed per statement and consecutive, so the run is named by its first seed and its count and
/// any statement in it can be had back on its own. Duplicates are kept rather than folded, because
/// two seeds writing the same text is a fact about the weights and a run that quietly deduplicated
/// would report a smaller denominator than the one it measured.
///
/// # Errors
///
/// When the rule is not in the grammar table, which is the only thing the generator refuses.
pub fn generate(how_many: usize, seed: u64, rule: &str) -> Result<Vec<String>, HarnessError> {
    let generator = rudb::generate::Generator::with_catalog(catalog());
    let mut written = Vec::with_capacity(how_many);
    for at in 0..how_many as u64 {
        written.push(
            generator
                .from_rule(rule, seed.saturating_add(at))
                .map_err(|e| HarnessError::new(e.to_string()))?,
        );
    }
    Ok(written)
}

/// What the two engines said about one generated statement.
#[derive(Debug, Clone)]
pub struct Answered {
    /// The statement, as it was written.
    pub sql: String,
    /// Whether DuckDB parsed it, and what it said when it did not.
    pub duckdb: Acceptance,
    /// Whether rudb parsed it, and what it said when it did not.
    pub rudb: Acceptance,
}

/// Ask both engines whether each statement parses.
///
/// Nothing is run and nothing is created, so neither engine has any state to carry from one
/// statement to the next and the order they are asked in does not matter.
///
/// # Errors
///
/// When either engine could not be run at all, which is a broken harness rather than a finding.
pub fn ask(
    duckdb: &mut dyn Engine,
    rudb: &mut dyn Engine,
    statements: &[String],
) -> Result<Vec<Answered>, HarnessError> {
    let mut answers = Vec::with_capacity(statements.len());
    for sql in statements {
        answers.push(Answered {
            sql: sql.clone(),
            duckdb: duckdb.accepts(sql)?,
            rudb: rudb.accepts(sql)?,
        });
    }
    Ok(answers)
}

/// One way a set of statements came apart, with the shortest of them.
#[derive(Debug, Clone)]
pub struct Group {
    /// What the engine that refused said, with the part of it that is this statement taken out.
    pub key: String,
    /// How many statements landed here.
    pub found: usize,
    /// The shortest one that did, which is the one worth reading.
    pub example: String,
}

/// What a generated run came to.
#[derive(Debug, Clone)]
pub struct Found {
    /// The first seed, which with the count replays the run exactly.
    pub seed: u64,
    /// The rule the statements were written from.
    pub rule: String,
    /// How many were written.
    pub statements: usize,
    /// How many both engines parsed.
    pub agreed: usize,
    /// How many neither engine parsed, which says nothing about either.
    pub refused: usize,
    /// DuckDB parsed it and rudb did not, grouped by what rudb said. The gap.
    pub gap: Vec<Group>,
    /// rudb parsed it and DuckDB did not, grouped by what DuckDB said. A dialect we invented.
    pub invented: Vec<Group>,
}

impl Found {
    /// Read a run into the groups it came apart into.
    ///
    /// The four cases are written out rather than folded, because the interesting two are the ones
    /// where the engines disagree and the boring two are both large. Grouping the gap on the error
    /// kind as well as the text is what keeps a parser complaint and a binder complaint about the
    /// same feature in separate rows, and the text is generalised first so the part of it that was
    /// the statement does not make every row a row of one.
    #[must_use]
    pub fn of(answers: &[Answered], seed: u64, rule: &str) -> Self {
        let mut gap: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut invented: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut agreed = 0;
        let mut refused = 0;
        for answer in answers {
            match (&answer.duckdb, &answer.rudb) {
                (Acceptance::Accepted, Acceptance::Accepted) => agreed += 1,
                (Acceptance::Accepted, Acceptance::Rejected(error)) => {
                    keep(
                        &mut gap,
                        &format!("{}: {}", error.kind, generalised(&headline(error))),
                        &answer.sql,
                    );
                }
                (Acceptance::Rejected(error), Acceptance::Accepted) => {
                    keep(&mut invented, &headline(error), &answer.sql);
                }
                // Both refused it. The PEG artefacts live here and they say nothing about either
                // engine, so they are counted and kept out of every other number.
                (Acceptance::Rejected(_), Acceptance::Rejected(_)) => refused += 1,
            }
        }
        Self {
            seed,
            rule: rule.to_owned(),
            statements: answers.len(),
            agreed,
            refused,
            gap: sorted(gap),
            invented: sorted(invented),
        }
    }

    /// How many statements said anything about rudb, which is the denominator worth reading.
    #[must_use]
    pub const fn usable(&self) -> usize {
        self.statements - self.refused
    }

    /// How many landed in the gap, which is the number this run exists to produce.
    #[must_use]
    pub fn missing(&self) -> usize {
        self.gap.iter().map(|group| group.found).sum()
    }
}

/// Add one statement to a group, keeping the shortest example seen for it.
fn keep(groups: &mut BTreeMap<String, (usize, String)>, key: &str, sql: &str) {
    let entry = groups.entry(key.to_owned()).or_insert_with(|| (0, sql.to_owned()));
    entry.0 += 1;
    if sql.len() < entry.1.len() {
        entry.1 = sql.to_owned();
    }
}

/// The groups, most found first, with ties broken by the key so a run is reproducible.
fn sorted(groups: BTreeMap<String, (usize, String)>) -> Vec<Group> {
    let mut rows: Vec<Group> =
        groups.into_iter().map(|(key, (found, example))| Group { key, found, example }).collect();
    rows.sort_by_key(|group| (std::cmp::Reverse(group.found), group.key.clone()));
    rows
}

/// The first line of what an engine said, which is the part that names the feature.
fn headline(error: &EngineError) -> String {
    error.message.lines().next().unwrap_or_default().to_owned()
}

/// How much of a generated statement is printed as the example for its group.
///
/// The ninetieth percentile from the whole statement rule is 76 words, so a report that printed
/// them in full would be a report nobody reads. The seed and the count at the top are what get any
/// of them back whole.
const SHOWN: usize = 160;

/// One generated statement on one line, cut to something a report can print.
fn short(sql: &str) -> String {
    let one_line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(SHOWN) {
        Some((at, _)) => {
            format!("{} ... and {} more characters", &one_line[..at], one_line.len() - at)
        }
        None => one_line,
    }
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "written out of the rudb grammar table, from the rule {}", self.rule)?;
        writeln!(
            f,
            "seeds {} to {}, which is what replays this run exactly",
            self.seed,
            self.seed + self.statements.saturating_sub(1) as u64
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "{} statements, {} neither engine parses, {} left",
            self.statements,
            self.refused,
            self.usable()
        )?;
        writeln!(f, "{} of those both engines parse", self.agreed)?;
        writeln!(f, "{} DuckDB parses and rudb does not, which is the gap", self.missing())?;
        let invented: usize = self.invented.iter().map(|group| group.found).sum();
        writeln!(f, "{invented} rudb parses and DuckDB does not, which is a dialect we invented")?;
        section(f, "what rudb could not parse, most found first", &self.gap)?;
        section(f, "what DuckDB could not parse, most found first", &self.invented)
    }
}

fn section(f: &mut fmt::Formatter<'_>, title: &str, groups: &[Group]) -> fmt::Result {
    if groups.is_empty() {
        return Ok(());
    }
    writeln!(f)?;
    writeln!(f, "{title}")?;
    for group in groups {
        writeln!(f)?;
        writeln!(f, "  {:6}  {}", group.found, group.key)?;
        writeln!(f, "          {}", short(&group.example))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Answered, Found, catalog, generate};
    use crate::engine::{Acceptance, EngineError};

    fn answered(sql: &str, duckdb: Acceptance, rudb: Acceptance) -> Answered {
        Answered { sql: sql.to_owned(), duckdb, rudb }
    }

    fn rejected(kind: &str, message: &str) -> Acceptance {
        Acceptance::Rejected(EngineError { kind: kind.to_owned(), message: message.to_owned() })
    }

    #[test]
    fn the_same_seed_writes_the_same_run() {
        let once = generate(20, 7, "Statement").expect("a rule that exists");
        let again = generate(20, 7, "Statement").expect("a rule that exists");
        assert_eq!(once, again);
        assert_eq!(once.len(), 20);
    }

    #[test]
    fn a_rule_that_is_not_in_the_grammar_says_so() {
        let error = generate(1, 1, "NotARule").expect_err("no such rule");
        assert!(error.to_string().contains("NotARule"), "{error}");
    }

    #[test]
    fn every_name_it_writes_is_one_of_the_five_tables() {
        // The generator is handed the same schema sqlsmith creates, so a lower case word in a
        // generated statement is a name from that schema. A generated name would mean the two
        // sources are talking about different databases, which is a thing a later mode that runs
        // the statements would find out about the hard way.
        let catalog = catalog();
        let known: Vec<&str> = catalog
            .tables
            .iter()
            .flat_map(|table| {
                std::iter::once(table.name.as_str()).chain(table.columns.iter().map(String::as_str))
            })
            .collect();
        let mut seen = false;
        for text in generate(200, 1, "SelectStatement").expect("a rule that exists") {
            for piece in text.split(' ') {
                if piece.chars().next().is_some_and(char::is_lowercase) && known.contains(&piece) {
                    seen = true;
                }
            }
        }
        assert!(seen, "no generated statement named a table or a column");
    }

    #[test]
    fn the_gap_is_what_duckdb_parses_and_we_do_not() {
        let sample = || rejected("Parser Error", "no, the grammar rule is Sample");
        let found = Found::of(
            &[
                answered("SELECT 1", Acceptance::Accepted, Acceptance::Accepted),
                answered("SELECT 1 TABLESAMPLE (1)", Acceptance::Accepted, sample()),
                answered("SELECT 2 TABLESAMPLE (2) FROM ints", Acceptance::Accepted, sample()),
                answered(
                    "EXPLAIN ANALYZE",
                    rejected("Parser Error", "syntax"),
                    Acceptance::Accepted,
                ),
            ],
            1,
            "Statement",
        );
        assert_eq!(found.statements, 4);
        assert_eq!(found.agreed, 1);
        assert_eq!(found.refused, 0);
        assert_eq!(found.missing(), 2);
        // Two statements, one group, because the part of the complaint that was the statement is
        // taken out before it is counted.
        assert_eq!(found.gap.len(), 1);
        assert_eq!(found.gap[0].key, "Parser Error: the grammar rule is Sample");
        // And the example is the shorter of the two.
        assert_eq!(found.gap[0].example, "SELECT 1 TABLESAMPLE (1)");
        assert_eq!(found.invented.len(), 1);
    }

    #[test]
    fn a_statement_neither_engine_parses_is_counted_apart() {
        let found = Found::of(
            &[
                answered("SELECT 1", Acceptance::Accepted, Acceptance::Accepted),
                answered(
                    "EXPLAIN ANALYZE",
                    rejected("Parser Error", "syntax error at or near ANALYZE"),
                    rejected("Parser Error", "no, the grammar rule is ExplainStatement"),
                ),
            ],
            1,
            "Statement",
        );
        assert_eq!(found.refused, 1);
        assert_eq!(found.usable(), 1);
        assert_eq!(found.missing(), 0);
    }

    #[test]
    fn two_engines_refusing_a_statement_the_same_way_is_still_not_agreement() {
        // This is the bug the four cases exist to stop. Comparing the two answers through the
        // record comparison would call this one agreement, because two engines refusing a statement
        // with the same error kind is no difference at all to a comparison built for records that
        // run, and most of what a walk of the whole grammar writes is refused by both.
        let same = || rejected("Parser Error", "syntax error at or near ANALYZE");
        let found = Found::of(&[answered("EXPLAIN ANALYZE", same(), same())], 1, "Statement");
        assert_eq!(found.agreed, 0);
        assert_eq!(found.refused, 1);
        assert_eq!(found.usable(), 0);
    }
}
