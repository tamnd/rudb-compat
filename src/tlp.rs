//! Ternary logic partitioning, which is an oracle that needs no second engine.
//!
//! The paper is Rigger and Su, "Finding Bugs in Database Systems via Query Partitioning", OOPSLA
//! 2020, which is the one after their "Testing Database Engines via Pivoted Query Synthesis" and
//! is the cheaper of the two to implement. Take a query and a predicate `p`. Every row the
//! query can see is in exactly one of three buckets: the rows where `p` is true, the rows where it
//! is false, and the rows where it is neither because something in it was NULL. So the rows of the
//! three queries `WHERE p`, `WHERE NOT (p)` and `WHERE (p) IS NULL` put together are the rows of
//! the query with no predicate at all, and that has to hold for every `p` an engine will accept.
//!
//! `spec/sql/duckdb/09-the-harness.md` asks for this in the generation block and it is worth saying
//! what it buys over the differential loop, since the differential loop is already there. Three
//! things. It runs on rudb alone, so it is a check that works on a machine with no DuckDB on it and
//! a check CI can run on every commit. It localises: a failure names the predicate rather than the
//! query, because the three parts are the same query four times over and only the predicate moved.
//! And it tests three valued logic directly, which is the part of SQL a young engine gets wrong
//! quietly, since a predicate that returns false where it should return unknown gives the right
//! answer for almost every query anybody writes and the wrong answer for the rest.
//!
//! ## Why the union is done here and not by the engine
//!
//! The paper writes the three parts as one `UNION ALL` and compares that against the original. Here
//! the three parts are run as three queries and the rows are put together in the harness. It is the
//! same property and it fails in fewer places: a bug in `UNION ALL` would break every case in the
//! run and say nothing about any predicate, which is exactly the outcome an oracle is supposed to
//! avoid. The one statement form is printed beside every failure anyway, because that is what a
//! person wants to paste into a shell, and `crate::reduce` takes it from there.
//!
//! ## Why the rows are fixed
//!
//! The table is a constant in this file rather than something generated from the seed. A run is
//! then replayed by the seed alone, a failure is reproduced by the seed and this page, and the
//! rows can be chosen for the job instead of sampled. What the job needs is rows that straddle
//! every boundary a generated predicate might draw, and one row per column whose value in that
//! column is NULL and whose other values are not, because that is the row a predicate on one column
//! is unknown about while everything else about it is ordinary.
//!
//! It is a different fixture from `crate::sqlsmith::CATALOG` and that is not duplication. That one
//! is shaped so the generator finds a column of every type group to write about, and three rows is
//! enough for it. This one is shaped so a predicate over it can come out true for some rows, false
//! for others and unknown for the rest, and three rows cannot do that.

use std::collections::BTreeMap;
use std::fmt;

use crate::compare::{Difference, MessageMatch, Ordering, Rules, compare};
use crate::engine::{Engine, HarnessError, Outcome, Table};
use crate::sqlsmith::generalised;

/// How many predicates a run tries when nobody said a number.
///
/// Four queries per predicate against a table of thirteen rows, so a thousand of these is a second
/// and the number is about how much of the generator's output a reader wants to think about rather
/// than about what a run can afford.
pub const CASES: usize = 1000;

/// How deep a generated predicate is allowed to nest.
///
/// Three is enough for `NOT ((a) AND (b))` with a comparison at each leaf, which is where the
/// interesting three valued cases live, and it is shallow enough that the predicate printed beside
/// a failure is one a person can read without rewriting it first.
const DEPTH: usize = 3;

/// The table every partitioning is written over.
pub const TABLE: &str = "t";

/// The rows every partitioning runs against.
///
/// Seven columns, one per type group that has its own comparison code in both engines, and thirteen
/// rows. Six of them are ordinary rows with values spread across each column so that a comparison
/// against a literal has rows on both sides of it. Six more are the same ordinary row with exactly
/// one column set to NULL, which is the row that makes a predicate on that column unknown while
/// leaving every other predicate in the same expression well defined. The last is NULL everywhere.
pub const FIXTURE: &str = "\
CREATE TABLE t(i INTEGER, j BIGINT, x DOUBLE, s VARCHAR, b BOOLEAN, d DATE, ts TIMESTAMP);
INSERT INTO t VALUES
  (-3, -300, -1.5, 'alpha', true, DATE '1999-12-31', TIMESTAMP '1999-12-31 23:59:59'),
  (0, 0, 0.0, '', false, DATE '1970-01-01', TIMESTAMP '1970-01-01 00:00:00'),
  (1, 1, 0.5, 'a', true, DATE '2000-01-01', TIMESTAMP '2000-01-01 00:00:00'),
  (2, 100, 1.5, 'abc', false, DATE '2020-02-29', TIMESTAMP '2020-02-29 12:00:00'),
  (3, 300, 2.5, 'ABC', true, DATE '2024-06-30', TIMESTAMP '2024-06-30 06:30:00'),
  (7, 7000, 3.25, 'one', false, DATE '2038-01-19', TIMESTAMP '2038-01-19 03:14:07'),
  (NULL, 1, 1.0, 'two', true, DATE '2000-01-01', TIMESTAMP '2000-01-01 00:00:00'),
  (1, NULL, 1.0, 'two', true, DATE '2000-01-01', TIMESTAMP '2000-01-01 00:00:00'),
  (1, 1, NULL, 'two', true, DATE '2000-01-01', TIMESTAMP '2000-01-01 00:00:00'),
  (1, 1, 1.0, NULL, true, DATE '2000-01-01', TIMESTAMP '2000-01-01 00:00:00'),
  (1, 1, 1.0, 'two', NULL, DATE '2000-01-01', TIMESTAMP '2000-01-01 00:00:00'),
  (1, 1, 1.0, 'two', true, NULL, NULL),
  (NULL, NULL, NULL, NULL, NULL, NULL, NULL);";

/// The fixture as a list of statements, in the order they have to run.
#[must_use]
pub fn fixture() -> Vec<String> {
    FIXTURE
        .split(";\n")
        .map(|statement| statement.trim().trim_end_matches(';').to_owned())
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// One predicate, and the four queries it turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// The predicate, exactly as it is written into the three parts.
    pub predicate: String,
}

impl Case {
    /// The query with no predicate on it, which is what the three parts have to add up to.
    #[must_use]
    pub fn whole(&self) -> String {
        format!("SELECT * FROM {TABLE}")
    }

    /// The three parts, true first, then false, then unknown.
    ///
    /// The predicate is parenthesised in all three rather than in the two that need it, so that the
    /// text of the predicate is the same string in each and a difference between the parts cannot
    /// be a difference in how they were spelled.
    #[must_use]
    pub fn parts(&self) -> [String; 3] {
        let p = &self.predicate;
        [
            format!("SELECT * FROM {TABLE} WHERE ({p})"),
            format!("SELECT * FROM {TABLE} WHERE NOT ({p})"),
            format!("SELECT * FROM {TABLE} WHERE ({p}) IS NULL"),
        ]
    }

    /// The three parts as the one statement the paper writes, for reproducing a failure by hand.
    #[must_use]
    pub fn together(&self) -> String {
        self.parts().join(" UNION ALL ")
    }
}

/// A seeded source of predicates over [`FIXTURE`].
#[derive(Debug, Clone)]
pub struct Predicates {
    rng: Rng,
}

impl Predicates {
    /// The generator a seed names. The same seed gives the same predicates in the same order.
    #[must_use]
    pub const fn from_seed(seed: u64) -> Self {
        Self { rng: Rng::seeded(seed) }
    }

    /// The next predicate.
    #[must_use]
    pub fn case(&mut self) -> Case {
        Case { predicate: self.expr(DEPTH) }
    }

    /// A predicate that is allowed to nest that many more times.
    ///
    /// The leaf arms are weighted up rather than left at one in six, because every other arm makes
    /// two of whatever it is applied to and an even weighting produces expressions that are almost
    /// all at the depth limit and almost all unreadable.
    fn expr(&mut self, depth: usize) -> String {
        if depth == 0 {
            return self.leaf();
        }
        match self.rng.below(8) {
            0..=3 => self.leaf(),
            4 => format!("NOT ({})", self.expr(depth - 1)),
            5 => {
                let left = self.expr(depth - 1);
                let right = self.expr(depth - 1);
                format!("({left}) AND ({right})")
            }
            6 => {
                let left = self.expr(depth - 1);
                let right = self.expr(depth - 1);
                format!("({left}) OR ({right})")
            }
            _ => format!("({}) IS NULL", self.expr(depth - 1)),
        }
    }

    /// A predicate with nothing else inside it.
    fn leaf(&mut self) -> String {
        let field = self.rng.pick(&FIELDS);
        match self.rng.below(9) {
            0 | 1 => {
                let term = self.term(field);
                format!("{term} {} {}", self.operator(), self.literal(field.kind))
            }
            2 => {
                let term = self.term(field);
                let other = self.other(field.kind);
                format!("{term} {} {other}", self.operator())
            }
            3 => {
                let term = self.term(field);
                format!("{term} IS {}NULL", self.negation())
            }
            4 => {
                let term = self.term(field);
                let low = self.literal(field.kind);
                let high = self.literal(field.kind);
                format!("{term} BETWEEN {low} AND {high}")
            }
            5 => {
                let term = self.term(field);
                let one = self.literal(field.kind);
                let two = self.literal(field.kind);
                format!("{term} IN ({one}, {two})")
            }
            6 => {
                let term = self.term(field);
                let value = self.literal(field.kind);
                format!("{term} IS {}DISTINCT FROM {value}", self.negation())
            }
            7 => format!("s LIKE {}", self.rng.pick(&PATTERNS)),
            // A boolean column on its own, which is the only leaf whose value is the column and not
            // a comparison of it. It is the shortest way to get an unknown into an expression and
            // it is the shape an engine that models a predicate as a filter mask tends to special
            // case, so it is worth having on its own rather than only as `b = true`.
            _ => "b".to_owned(),
        }
    }

    /// A column, or something computed from it that is still of the same kind.
    ///
    /// Half of them are the bare column, because a comparison against a column is the shape that
    /// has to work and the rest is stress. The other half wrap it in arithmetic or in a function,
    /// which is worth doing here and not only in the differential loop for two reasons. It puts the
    /// predicate through the constant folder and the expression rewriter rather than only through
    /// the comparison, and every one of these functions returns NULL for a NULL argument, so the
    /// unknown that TLP is about now has to survive a call on its way to the comparison.
    ///
    /// Kind preserving on purpose. `length(s)` is text in and integer out, and a term whose kind is
    /// not its column's kind would have to be compared against a literal of the other kind, which
    /// is a cast question rather than a three valued logic one.
    fn term(&mut self, field: Field) -> String {
        let name = field.name;
        if self.rng.below(2) == 0 {
            return name.to_owned();
        }
        match field.kind {
            Kind::Int | Kind::Big => match self.rng.below(5) {
                0 => format!("abs({name})"),
                1 => format!("-{name}"),
                2 => format!("{name} % 3"),
                3 => format!("{name} + {}", self.rng.pick(&["1", "0", "100"])),
                _ => format!("coalesce({name}, {})", self.rng.pick(&["0", "1", "-1"])),
            },
            Kind::Real => match self.rng.below(4) {
                0 => format!("abs({name})"),
                1 => format!("-{name}"),
                2 => format!("{name} * 2"),
                _ => format!("coalesce({name}, {})", self.rng.pick(&["0.0", "1.0"])),
            },
            Kind::Text => match self.rng.below(4) {
                0 => format!("upper({name})"),
                1 => format!("lower({name})"),
                2 => format!("{name} || 'x'"),
                _ => format!("coalesce({name}, {})", self.rng.pick(&["''", "'two'"])),
            },
            Kind::Bool => match self.rng.below(2) {
                0 => format!("NOT {name}"),
                _ => format!("coalesce({name}, {})", self.rng.pick(&["true", "false"])),
            },
            Kind::Date | Kind::Stamp => format!("coalesce({name}, {})", self.literal(field.kind)),
        }
    }

    /// One of the six comparisons, which are the ones that are unknown when either side is NULL.
    fn operator(&mut self) -> &'static str {
        self.rng.pick(&["=", "<>", "<", "<=", ">", ">="])
    }

    /// `NOT ` or nothing, for the forms that spell their negation inside themselves.
    fn negation(&mut self) -> &'static str {
        self.rng.pick(&["", "NOT "])
    }

    /// Another column of a kind this one can be compared against.
    ///
    /// The three number columns are interchangeable here and nothing else is. A comparison across
    /// groups is a cast question rather than a three valued logic question, and mixing the two
    /// would mean a run whose refusals are all about casting says nothing about the logic.
    fn other(&mut self, kind: Kind) -> &'static str {
        let same: Vec<&Field> = FIELDS.iter().filter(|f| f.kind.group() == kind.group()).collect();
        self.rng.pick(&same).name
    }

    /// A literal of that kind, or NULL.
    ///
    /// NULL is in every pool because a comparison against it is the cheapest way to write a leaf
    /// that is unknown for every row, and an engine that folds `i = NULL` to false rather than to
    /// unknown passes every query anybody would write by hand and fails this.
    fn literal(&mut self, kind: Kind) -> &'static str {
        let pool: &[&'static str] = match kind {
            Kind::Int => &["NULL", "-3", "-1", "0", "1", "2", "3", "7", "100"],
            Kind::Big => &["NULL", "-300", "0", "1", "100", "300", "7000"],
            Kind::Real => &["NULL", "-1.5", "0.0", "0.5", "1.0", "1.5", "2.5", "3.25"],
            Kind::Text => &["NULL", "''", "'a'", "'abc'", "'ABC'", "'one'", "'two'", "'zzz'"],
            Kind::Bool => &["NULL", "true", "false"],
            Kind::Date => &[
                "NULL",
                "DATE '1970-01-01'",
                "DATE '2000-01-01'",
                "DATE '2020-02-29'",
                "DATE '2038-01-19'",
            ],
            Kind::Stamp => &[
                "NULL",
                "TIMESTAMP '1970-01-01 00:00:00'",
                "TIMESTAMP '2000-01-01 00:00:00'",
                "TIMESTAMP '2020-02-29 12:00:00'",
                "TIMESTAMP '2038-01-19 03:14:07'",
            ],
        };
        self.rng.pick(pool)
    }
}

/// The patterns the text column is matched against.
const PATTERNS: [&str; 6] = ["'a%'", "'%b%'", "'_bc'", "'ABC'", "'%'", "'two'"];

/// One column a predicate can be written about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    name: &'static str,
    kind: Kind,
}

/// The columns of [`FIXTURE`], which are what a generated predicate is allowed to name.
const FIELDS: [Field; 7] = [
    Field { name: "i", kind: Kind::Int },
    Field { name: "j", kind: Kind::Big },
    Field { name: "x", kind: Kind::Real },
    Field { name: "s", kind: Kind::Text },
    Field { name: "b", kind: Kind::Bool },
    Field { name: "d", kind: Kind::Date },
    Field { name: "ts", kind: Kind::Stamp },
];

/// What a column holds, to the extent the generator has to care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Big,
    Real,
    Text,
    Bool,
    Date,
    Stamp,
}

impl Kind {
    /// Which kinds can be compared against each other without the comparison being about casts.
    const fn group(self) -> u8 {
        match self {
            Self::Int | Self::Big | Self::Real => 0,
            Self::Text => 1,
            Self::Bool => 2,
            Self::Date => 3,
            Self::Stamp => 4,
        }
    }
}

/// A small deterministic random source, so that a seed replays a run exactly.
///
/// splitmix64, which is the one everybody uses to seed something better and is more than good
/// enough on its own for picking between eight arms of a match. A crate would be a second
/// dependency for thirty lines, and this crate having exactly one dependency is a rule.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

    const fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(Self::GOLDEN);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below that one. Zero when asked for a number below zero, which cannot happen here
    /// because every caller passes the length of a list it is about to index.
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(1).max(1);
        usize::try_from(self.next() % n).unwrap_or(0)
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len())]
    }
}

/// What one predicate came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The engine would not run one of the four queries, and this is what it said. A refusal is a
    /// gap in the engine rather than a failure of the property, because a query nobody could run
    /// says nothing about whether the parts add up.
    Refused(String),
    /// The parts added up.
    Held {
        /// Whether the predicate actually divided the table, which is at least two of the three
        /// parts having rows in them. A predicate that puts every row in one part is a case that
        /// passed without testing anything and the run counts those apart.
        sharp: bool,
    },
    /// They did not.
    Split(Broken),
}

/// A predicate whose three parts do not add up to the whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    /// The predicate the query was split on.
    pub predicate: String,
    /// How many rows the query with no predicate returned.
    pub whole: usize,
    /// How many rows each of the three parts returned, true then false then unknown.
    pub parts: [usize; 3],
    /// Where the two results stopped agreeing.
    pub differences: Vec<Difference>,
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  {}", self.predicate)?;
        writeln!(
            f,
            "    {} rows in the whole, {} true, {} false, {} unknown, {} in the parts",
            self.whole,
            self.parts[0],
            self.parts[1],
            self.parts[2],
            self.parts.iter().sum::<usize>()
        )?;
        for difference in &self.differences {
            writeln!(f, "    {difference}")?;
        }
        writeln!(f, "    {}", Case { predicate: self.predicate.clone() }.together())
    }
}

/// Put one predicate to one engine and say what happened.
///
/// The engine has to have the fixture loaded already, which [`run`] does once for the whole run
/// rather than once per predicate.
///
/// # Errors
///
/// When the engine could not be run at all, which is a harness failure and not an answer.
pub fn check(engine: &mut dyn Engine, case: &Case) -> Result<Checked, HarnessError> {
    let whole = match engine.run(&case.whole())? {
        Outcome::Rows(rows) => rows,
        Outcome::Error(e) => return Ok(Checked::Refused(named(&e.kind, e.headline()))),
    };
    let mut parts = Vec::with_capacity(3);
    for sql in case.parts() {
        match engine.run(&sql)? {
            Outcome::Rows(rows) => parts.push(rows),
            Outcome::Error(e) => return Ok(Checked::Refused(named(&e.kind, e.headline()))),
        }
    }

    let counts = [parts[0].height(), parts[1].height(), parts[2].height()];
    let mut differences = shapes(&parts);
    differences.extend(compare(
        &Outcome::Rows(whole.clone()),
        &Outcome::Rows(united(&parts)),
        Rules { ordering: Ordering::Sorted, messages: MessageMatch::Kind },
    ));

    if differences.is_empty() {
        return Ok(Checked::Held { sharp: counts.iter().filter(|n| **n > 0).count() > 1 });
    }
    Ok(Checked::Split(Broken {
        predicate: case.predicate.clone(),
        whole: whole.height(),
        parts: counts,
        differences,
    }))
}

/// The three parts as one result set, with the columns of the first of them.
fn united(parts: &[Table]) -> Table {
    let mut rows = Vec::new();
    for part in parts {
        rows.extend(part.rows.iter().cloned());
    }
    Table { columns: parts.first().map(|first| first.columns.clone()).unwrap_or_default(), rows }
}

/// Whether the three parts are the same shape as each other.
///
/// [`united`] takes its columns from the first part, so a part that came back with a different
/// column name or a different type would otherwise be invisible. The same query with three
/// different predicates on it has to produce the same columns, and an engine that types a column
/// out of the rows that survived the filter is a real bug this catches on the way past.
fn shapes(parts: &[Table]) -> Vec<Difference> {
    let Some(first) = parts.first() else { return Vec::new() };
    let mut found = Vec::new();
    for other in parts.iter().skip(1) {
        found.extend(compare(
            &Outcome::Rows(bare(first)),
            &Outcome::Rows(bare(other)),
            Rules { ordering: Ordering::AsWritten, messages: MessageMatch::Kind },
        ));
    }
    found
}

/// A result set with its rows taken off, which compares as its columns alone.
fn bare(table: &Table) -> Table {
    Table { columns: table.columns.clone(), rows: Vec::new() }
}

/// An error as it is counted, which is the kind and as much of the message as is not this query.
fn named(kind: &str, headline: &str) -> String {
    format!("{kind}: {}", generalised(headline))
}

/// What a whole run came to.
#[derive(Debug, Clone)]
pub struct Found {
    /// The seed it was generated from, which is what replays it.
    pub seed: u64,
    /// The engine it ran against, with its version.
    pub engine: String,
    /// How many predicates were tried.
    pub cases: usize,
    /// How many of them the parts added up for.
    pub held: usize,
    /// How many of those actually divided the table rather than putting every row in one part.
    pub sharp: usize,
    /// Each distinct thing the engine refused with, and how many predicates it refused that way.
    pub refused: BTreeMap<String, usize>,
    /// Every predicate whose parts did not add up.
    pub broken: Vec<Broken>,
}

impl Found {
    /// How many predicates the engine refused, which are the ones that say nothing either way.
    #[must_use]
    pub fn refusals(&self) -> usize {
        self.refused.values().sum()
    }

    /// Whether the run found anything, which is what the exit code is.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.broken.is_empty()
    }
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ternary logic partitioning against {}", self.engine)?;
        writeln!(f, "seed {}, which is what replays this run exactly", self.seed)?;
        writeln!(f)?;
        writeln!(
            f,
            "{} predicates, {} the engine would not run, {} left",
            self.cases,
            self.refusals(),
            self.cases - self.refusals()
        )?;
        writeln!(
            f,
            "{} of those split into three parts that add back up to the whole, {} of which divided the table rather than putting every row in one part",
            self.held, self.sharp
        )?;
        if !self.refused.is_empty() {
            writeln!(f)?;
            writeln!(f, "what the engine would not run, most found first")?;
            let mut by_count: Vec<(&String, &usize)> = self.refused.iter().collect();
            by_count.sort_by_key(|(key, found)| (std::cmp::Reverse(**found), (*key).clone()));
            for (key, found) in by_count {
                writeln!(f, "  {found:6}  {key}")?;
            }
        }
        if self.broken.is_empty() {
            return Ok(());
        }
        writeln!(f)?;
        writeln!(f, "where the parts did not add up, against this table")?;
        writeln!(f)?;
        for statement in fixture() {
            writeln!(f, "  {statement};")?;
        }
        for broken in &self.broken {
            writeln!(f)?;
            write!(f, "{broken}")?;
        }
        Ok(())
    }
}

/// Load the fixture and put that many generated predicates to the engine.
///
/// The fixture is loaded once and the engine is reset first, because a table left over from
/// whatever ran before would be a table the predicates were not written about.
///
/// # Errors
///
/// When the engine could not be run at all, and when it will not load the fixture, which is not a
/// finding about three valued logic and is reported as the harness failing rather than counted as
/// a thousand refusals.
pub fn run(engine: &mut dyn Engine, cases: usize, seed: u64) -> Result<Found, HarnessError> {
    engine.reset()?;
    for statement in fixture() {
        if let Outcome::Error(e) = engine.run(&statement)? {
            return Err(HarnessError::new(format!(
                "{} will not load the fixture: {e}",
                engine.name()
            )));
        }
    }

    let mut predicates = Predicates::from_seed(seed);
    let mut found = Found {
        seed,
        engine: engine.version().to_owned(),
        cases,
        held: 0,
        sharp: 0,
        refused: BTreeMap::new(),
        broken: Vec::new(),
    };
    for _ in 0..cases {
        let case = predicates.case();
        match check(engine, &case)? {
            Checked::Refused(said) => *found.refused.entry(said).or_insert(0) += 1,
            Checked::Held { sharp } => {
                found.held += 1;
                found.sharp += usize::from(sharp);
            }
            Checked::Split(broken) => found.broken.push(broken),
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::{CASES, Case, Checked, Found, Predicates, fixture, run};
    use crate::engine::{Acceptance, Cell, Column, Engine, Outcome, Table};
    use crate::rudb::Rudb;
    use std::collections::BTreeMap;

    #[test]
    fn the_fixture_is_two_statements_and_the_second_one_has_a_row_with_every_column_null() {
        let loaded = fixture();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].starts_with("CREATE TABLE t("));
        assert!(loaded[1].contains("(NULL, NULL, NULL, NULL, NULL, NULL, NULL)"));
    }

    #[test]
    fn a_seed_replays_the_same_predicates_in_the_same_order() {
        let mut one = Predicates::from_seed(7);
        let mut two = Predicates::from_seed(7);
        let first: Vec<Case> = (0..50).map(|_| one.case()).collect();
        let second: Vec<Case> = (0..50).map(|_| two.case()).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn two_seeds_do_not_generate_the_same_predicates() {
        let mut one = Predicates::from_seed(7);
        let mut two = Predicates::from_seed(8);
        let first: Vec<Case> = (0..50).map(|_| one.case()).collect();
        let second: Vec<Case> = (0..50).map(|_| two.case()).collect();
        assert_ne!(first, second);
    }

    #[test]
    fn the_three_parts_are_the_predicate_its_negation_and_the_case_where_it_is_neither() {
        let case = Case { predicate: "i < 3".to_owned() };
        let parts = case.parts();
        assert_eq!(parts[0], "SELECT * FROM t WHERE (i < 3)");
        assert_eq!(parts[1], "SELECT * FROM t WHERE NOT (i < 3)");
        assert_eq!(parts[2], "SELECT * FROM t WHERE (i < 3) IS NULL");
        assert_eq!(case.together(), parts.join(" UNION ALL "));
    }

    fn loaded() -> Rudb {
        let mut rudb = Rudb::new();
        for statement in fixture() {
            let outcome = rudb.run(&statement).expect("rudb runs");
            assert!(outcome.is_rows(), "the fixture has to load: {statement}");
        }
        rudb
    }

    #[test]
    fn a_predicate_that_divides_the_table_holds_and_says_it_divided_it() {
        let mut rudb = loaded();
        let case = Case { predicate: "i < 3".to_owned() };
        assert_eq!(
            super::check(&mut rudb, &case).expect("rudb runs"),
            Checked::Held { sharp: true }
        );
    }

    #[test]
    fn a_predicate_nothing_is_unknown_about_still_holds_and_is_not_counted_as_sharp() {
        // Every row is in the true part, so the property is satisfied by a query that ignored the
        // predicate entirely. That is a pass and it is worth knowing how many of a run are like it,
        // which is what the sharp count is for.
        let mut rudb = loaded();
        let case = Case { predicate: "1 = 1".to_owned() };
        assert_eq!(
            super::check(&mut rudb, &case).expect("rudb runs"),
            Checked::Held { sharp: false }
        );
    }

    #[test]
    fn a_predicate_the_engine_will_not_run_is_a_refusal_and_not_a_failure() {
        let mut rudb = loaded();
        let case = Case { predicate: "nosuchcolumn = 1".to_owned() };
        let Checked::Refused(said) = super::check(&mut rudb, &case).expect("rudb runs") else {
            panic!("there is no such column");
        };
        assert!(said.contains("Error"), "{said}");
    }

    #[test]
    fn a_generated_run_against_rudb_finds_nothing_and_the_predicates_are_not_all_refused() {
        // This is the check running as a test rather than as a subcommand, which is what makes it
        // a pre merge gate. The seed is fixed so that a failure here is one anybody can reproduce,
        // and the count is small so that the test suite stays a few seconds.
        let mut rudb = Rudb::new();
        let found = run(&mut rudb, 200, 20250915).expect("rudb runs");
        assert!(found.is_clean(), "{found}");
        assert!(
            found.sharp * 4 > found.cases,
            "a run where almost nothing divided the table is testing almost nothing:\n{found}"
        );
    }

    #[test]
    fn the_default_count_is_a_number_a_person_asked_for() {
        assert_eq!(CASES, 1000);
    }

    /// An engine that answers every partition with the rows where the predicate is true, which is
    /// what an engine that treats unknown as false does, and the bug this oracle exists to catch.
    #[derive(Debug)]
    struct Two;

    impl Engine for Two {
        fn name(&self) -> &str {
            "an engine that loses the unknown rows"
        }

        fn version(&self) -> &str {
            "two valued logic 1.0"
        }

        fn run(&mut self, sql: &str) -> Result<Outcome, crate::engine::HarnessError> {
            let rows = if sql.contains("WHERE") { 1 } else { 2 };
            Ok(Outcome::Rows(table(&["a"], rows)))
        }

        fn accepts(&mut self, _sql: &str) -> Result<Acceptance, crate::engine::HarnessError> {
            Ok(Acceptance::Accepted)
        }
    }

    #[test]
    fn an_engine_that_answers_every_partition_the_same_way_is_caught() {
        // Without this the oracle is a test of the fixture and the generator and of nothing else,
        // because rudb passes every predicate either of them produces. Three parts of one row each
        // against a whole of two rows is the arithmetic that has to fail.
        let mut two = Two;
        let case = Case { predicate: "a = 1".to_owned() };
        let Checked::Split(broken) = super::check(&mut two, &case).expect("the fake engine runs")
        else {
            panic!("one row three times is not two rows");
        };
        assert_eq!(broken.whole, 2);
        assert_eq!(broken.parts, [1, 1, 1]);
        assert!(!broken.differences.is_empty());
        assert!(broken.to_string().contains("UNION ALL"));
    }

    fn table(names: &[&str], rows: usize) -> Table {
        Table {
            columns: names
                .iter()
                .map(|n| Column { name: (*n).to_owned(), ty: "INTEGER".to_owned() })
                .collect(),
            rows: (0..rows).map(|n| vec![Cell::Text(n.to_string())]).collect(),
        }
    }

    #[test]
    fn a_part_that_came_back_with_a_different_column_than_the_others_is_a_difference() {
        let parts = [table(&["a"], 1), table(&["b"], 1), table(&["a"], 1)];
        assert!(!super::shapes(&parts).is_empty());
    }

    #[test]
    fn the_report_prints_the_table_when_there_is_something_to_reproduce_and_not_otherwise() {
        let clean = Found {
            seed: 1,
            engine: "rudb 0.0.0".to_owned(),
            cases: 1,
            held: 1,
            sharp: 1,
            refused: BTreeMap::new(),
            broken: Vec::new(),
        };
        assert!(!clean.to_string().contains("CREATE TABLE"));
        let mut rudb = loaded();
        let case = Case { predicate: "i < 3".to_owned() };
        let Checked::Held { .. } = super::check(&mut rudb, &case).expect("rudb runs") else {
            panic!("that one holds");
        };
        let broken = Found {
            broken: vec![super::Broken {
                predicate: "i < 3".to_owned(),
                whole: 13,
                parts: [6, 4, 2],
                differences: Vec::new(),
            }],
            ..clean
        };
        let printed = broken.to_string();
        assert!(printed.contains("CREATE TABLE"));
        assert!(printed.contains("UNION ALL"));
    }
}
