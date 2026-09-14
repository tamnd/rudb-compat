//! The table and the predicates the oracles are written over.
//!
//! Two oracles use this, `crate::tlp` and `crate::norec`, and both of them are the same shape: put
//! a generated predicate to one engine in two forms that have to agree. What varies is the pair of
//! forms. So the table, the generator and the seeded random source are here, and each oracle is
//! the few lines that say what it asks.
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
//!
//! ## What the predicates are made of
//!
//! Comparisons, `IS NULL`, `BETWEEN`, `IN`, `IS DISTINCT FROM`, `LIKE` and a bare boolean column at
//! the leaves, `AND`, `OR`, `NOT` and a nested `IS NULL` above them, and either a column or a
//! function of it on the left of every comparison. That is a narrower surface than
//! `crate::sqlsmith` reaches and it is narrow on purpose, because what these oracles are about is
//! three valued logic rather than grammar coverage. A generated form nothing implements produces a
//! refusal, and a run of refusals says nothing about logic.

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
    pub fn predicate(&mut self) -> String {
        self.expr(DEPTH)
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

#[cfg(test)]
mod tests {
    use super::{Predicates, fixture};

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
        let first: Vec<String> = (0..50).map(|_| one.predicate()).collect();
        let second: Vec<String> = (0..50).map(|_| two.predicate()).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn two_seeds_do_not_generate_the_same_predicates() {
        let mut one = Predicates::from_seed(7);
        let mut two = Predicates::from_seed(8);
        let first: Vec<String> = (0..50).map(|_| one.predicate()).collect();
        let second: Vec<String> = (0..50).map(|_| two.predicate()).collect();
        assert_ne!(first, second);
    }

    #[test]
    fn every_generated_predicate_names_a_column_of_the_table_and_nothing_else() {
        // A predicate that named something not in the fixture would come back as a refusal on every
        // engine, and a run of refusals looks like a clean run from the summary line down.
        let mut predicates = Predicates::from_seed(11);
        for _ in 0..2000 {
            let text = predicates.predicate();
            for word in unquoted(&text).split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
                let known = word.is_empty()
                    || word.chars().next().is_some_and(|c| c.is_ascii_digit())
                    || KNOWN.split(' ').any(|known| known == word);
                assert!(known, "{word} is not a column, a keyword or a function: {text}");
            }
        }
    }

    /// The predicate with what is inside the string literals taken out.
    ///
    /// A pattern like `'a%'` is a value and not a name, and reading it as one would have the test
    /// complain about a column called a.
    fn unquoted(text: &str) -> String {
        let mut outside = String::with_capacity(text.len());
        let mut inside = false;
        for c in text.chars() {
            if c == '\'' {
                inside = !inside;
            } else if !inside {
                outside.push(c);
            }
        }
        outside
    }

    #[test]
    fn taking_the_literals_out_leaves_the_names_and_nothing_from_inside_the_quotes() {
        assert_eq!(unquoted("s LIKE 'a%' AND d = DATE '2000-01-01'"), "s LIKE  AND d = DATE ");
    }

    /// Every word a generated predicate is allowed to contain, other than a number or a literal.
    const KNOWN: &str = "i j x s b d ts NOT AND OR IS NULL BETWEEN IN DISTINCT FROM \
                         LIKE abs coalesce upper lower true false DATE TIMESTAMP";
}
