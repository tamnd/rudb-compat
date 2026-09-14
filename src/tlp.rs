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
//! ## The three forms
//!
//! The paper has three and this has all three, because they are not the same test. The `WHERE` form
//! partitions the rows of a query and catches a filter that loses the rows it is unknown about. The
//! aggregate form puts the same three predicates under `count`, `sum`, `min` and `max` and requires
//! the three answers to add back up, which catches an aggregate that treats the rows it was handed
//! differently depending on how many of them there were. The `HAVING` form partitions the groups
//! instead of the rows, where the predicate is unknown because a whole group was NULL rather than
//! because a row was, and that is a different piece of code in every engine.
//!
//! The aggregate form is the one where the harness does arithmetic rather than concatenation, and
//! that is the reason its select list is seven aggregates over the two integer columns and nothing
//! else. Adding three floating point numbers in an order the engine did not is a difference in the
//! last bits that is nobody's bug, and deciding which of two strings sorts first is the engine's
//! collation and not the harness's business.
//!
//! The table the predicates are written over and the generator that writes them are in
//! `crate::predicate`, which `crate::norec` shares, and the reasoning about why the rows are fixed
//! and what the predicates are made of is there with them.

use std::collections::BTreeMap;
use std::fmt;

use crate::compare::{Difference, MessageMatch, Ordering, Rules, compare};
use crate::engine::{Cell, Engine, HarnessError, Outcome, Table};
use crate::predicate::{Predicates, TABLE, fixture};
use crate::sqlsmith::generalised;

/// How many predicates a run tries when nobody said a number.
///
/// Four queries per predicate against a table of thirteen rows, so a thousand of these is a second
/// and the number is about how much of the generator's output a reader wants to think about rather
/// than about what a run can afford.
pub const CASES: usize = 1000;

/// Which of the three forms a case is in.
///
/// The paper has all three and they are not the same test. The `WHERE` form partitions the rows of
/// a query and catches a filter that loses the rows it is unknown about. The aggregate form
/// partitions the input of an aggregate and catches an aggregate that counts, adds or ordered those
/// rows differently depending on which of the three parts they landed in. The `HAVING` form
/// partitions the groups and catches the same thing one level up, where the predicate is unknown
/// because a whole group was NULL rather than because a row was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// The predicate in a `WHERE` clause, and the rows of the three parts have to be the rows of
    /// the query with no predicate on it.
    Where,
    /// The predicate in a `WHERE` clause underneath an aggregate, and the three aggregates have to
    /// add back up to the aggregate over the whole table.
    Aggregate,
    /// The predicate in a `HAVING` clause, and the groups of the three parts have to be the groups
    /// of the query with no `HAVING` on it.
    Having,
}

impl Form {
    /// The word for what the three parts are dividing, for the report.
    #[must_use]
    pub const fn divided(self) -> &'static str {
        match self {
            Self::Where | Self::Aggregate => "rows",
            Self::Having => "groups",
        }
    }

    /// What the run is called, for the first line of the report.
    #[must_use]
    pub const fn headline(self) -> &'static str {
        match self {
            Self::Where => "ternary logic partitioning over the rows",
            Self::Aggregate => "ternary logic partitioning under an aggregate",
            Self::Having => "ternary logic partitioning over the groups",
        }
    }

    /// The name the command line spells it with.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            Self::Where => "where",
            Self::Aggregate => "aggregate",
            Self::Having => "having",
        }
    }

    /// The form that name spells, or nothing when it is not one of the three.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        [Self::Where, Self::Aggregate, Self::Having].into_iter().find(|f| f.spelling() == name)
    }
}

/// The aggregates the aggregate form asks for.
///
/// All seven are over an integer column, and that is the whole of the reason the list looks like
/// this. The harness has to add the three parts back up itself, so it has to know how, and it can
/// do that for a count, a sum, a minimum and a maximum of whole numbers and cannot honestly do it
/// for anything else. A `sum` over the double column would have the harness add three floating point
/// numbers in an order the engine did not, and the last bits would come out different for a reason
/// that is nobody's bug. A `min` over the text column would have the harness decide which of two
/// strings sorts first, which is the engine's collation and not the harness's business.
const AGGREGATES: &str = "count(*), sum(i), min(i), max(i), sum(j), min(j), max(j)";

/// How each of [`AGGREGATES`] adds back up, in the same order.
const FOLDS: [Fold; 7] =
    [Fold::Total, Fold::Total, Fold::Least, Fold::Most, Fold::Total, Fold::Least, Fold::Most];

/// The aggregates the `HAVING` form asks for beside the group key.
///
/// These are not folded by the harness, since the three parts of that form are sets of groups and
/// they are put together by concatenating them. So there is no reason to stay on the integer columns
/// and every reason not to, and a group whose double and text aggregates are NULL is a group this
/// form should have an opinion about.
const GROUPED: &str = "count(*), sum(i), min(x), max(s)";

/// What the harness does with the three answers to one aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    /// Add them, which is what a count and a sum do.
    Total,
    /// Keep the smallest.
    Least,
    /// Keep the largest.
    Most,
}

/// One predicate, and the four queries it turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Which of the three forms this is.
    pub form: Form,
    /// The expression the query groups by, which is read only by [`Form::Having`].
    pub grouping: String,
    /// The predicate, exactly as it is written into the three parts.
    pub predicate: String,
}

impl Case {
    /// A case in the `WHERE` form, which is the one that has no grouping expression.
    #[must_use]
    pub fn rows(predicate: impl Into<String>) -> Self {
        Self { form: Form::Where, grouping: String::new(), predicate: predicate.into() }
    }

    /// A case in the aggregate form.
    #[must_use]
    pub fn aggregate(predicate: impl Into<String>) -> Self {
        Self { form: Form::Aggregate, grouping: String::new(), predicate: predicate.into() }
    }

    /// A case in the `HAVING` form, which groups by that expression.
    #[must_use]
    pub fn groups(grouping: impl Into<String>, predicate: impl Into<String>) -> Self {
        Self { form: Form::Having, grouping: grouping.into(), predicate: predicate.into() }
    }

    /// The query with no predicate on it, which is what the three parts have to add up to.
    #[must_use]
    pub fn whole(&self) -> String {
        match self.form {
            Form::Where => format!("SELECT * FROM {TABLE}"),
            Form::Aggregate => format!("SELECT {AGGREGATES} FROM {TABLE}"),
            Form::Having => {
                let g = &self.grouping;
                format!("SELECT {g}, {GROUPED} FROM {TABLE} GROUP BY {g}")
            }
        }
    }

    /// The three parts, true first, then false, then unknown.
    ///
    /// The predicate is parenthesised in all three rather than in the two that need it, so that the
    /// text of the predicate is the same string in each and a difference between the parts cannot
    /// be a difference in how they were spelled.
    #[must_use]
    pub fn parts(&self) -> [String; 3] {
        let p = &self.predicate;
        let each = ["({p})", "NOT ({p})", "({p}) IS NULL"];
        each.map(|shape| {
            let clause = shape.replace("{p}", p);
            match self.form {
                Form::Where => format!("SELECT * FROM {TABLE} WHERE {clause}"),
                Form::Aggregate => format!("SELECT {AGGREGATES} FROM {TABLE} WHERE {clause}"),
                Form::Having => {
                    let g = &self.grouping;
                    format!("SELECT {g}, {GROUPED} FROM {TABLE} GROUP BY {g} HAVING {clause}")
                }
            }
        })
    }

    /// The three parts as the one statement the paper writes, for reproducing a failure by hand.
    ///
    /// Two of the forms are a `UNION ALL` of the parts and read as one. The aggregate form is a
    /// `UNION ALL` with the folding aggregate over the top of it, which needs a subquery, and that
    /// is the reason the harness does the folding itself rather than asking for this: a subquery is
    /// a feature, and an oracle that only works where that feature already does is an oracle that
    /// stops working exactly when it would have been useful.
    #[must_use]
    pub fn together(&self) -> String {
        let parts = self.parts().join(" UNION ALL ");
        match self.form {
            Form::Where | Form::Having => parts,
            Form::Aggregate => format!(
                "SELECT sum(n), sum(si), min(ni), max(xi), sum(sj), min(nj), max(xj) FROM ({}) AS parts(n, si, ni, xi, sj, nj, xj)",
                parts
            ),
        }
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
    /// The case, which carries the form, the grouping expression and the predicate, and which is
    /// what writes the four queries again for the report.
    pub case: Case,
    /// How many rows or groups the query with no predicate returned. In the aggregate form there is
    /// only ever one row, so this is what `count(*)` said instead.
    pub whole: usize,
    /// The same number for each of the three parts, true then false then unknown.
    pub parts: [usize; 3],
    /// Where the two results stopped agreeing.
    pub differences: Vec<Difference>,
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.case.form == Form::Having {
            writeln!(f, "  grouped by {}", self.case.grouping)?;
        }
        writeln!(f, "  {}", self.case.predicate)?;
        writeln!(
            f,
            "    {} {} in the whole, {} true, {} false, {} unknown, {} in the parts",
            self.whole,
            self.case.form.divided(),
            self.parts[0],
            self.parts[1],
            self.parts[2],
            self.parts.iter().sum::<usize>()
        )?;
        for difference in &self.differences {
            writeln!(f, "    {difference}")?;
        }
        writeln!(f, "    {}", self.case.together())
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

    let counts =
        [reach(case.form, &parts[0])?, reach(case.form, &parts[1])?, reach(case.form, &parts[2])?];
    let together = match case.form {
        Form::Where | Form::Having => united(&parts),
        Form::Aggregate => folded(&parts)?,
    };
    let mut differences = shapes(&parts);
    differences.extend(compare(
        &Outcome::Rows(whole.clone()),
        &Outcome::Rows(together),
        Rules { ordering: Ordering::Sorted, messages: MessageMatch::Kind },
    ));

    if differences.is_empty() {
        return Ok(Checked::Held { sharp: counts.iter().filter(|n| **n > 0).count() > 1 });
    }
    Ok(Checked::Split(Broken {
        case: case.clone(),
        whole: reach(case.form, &whole)?,
        parts: counts,
        differences,
    }))
}

/// How many rows or groups a result covers.
///
/// The aggregate form always comes back with one row whatever the predicate did, so the number that
/// says whether a part was empty is the count in it rather than the height of the table. Without
/// that, every case in an aggregate run would look like it divided the table three ways and the
/// sharp count would say nothing.
///
/// # Errors
///
/// When the first aggregate came back as something that is not a whole number, which is the harness
/// and the engine disagreeing about what `count(*)` is rather than a finding about a predicate.
fn reach(form: Form, table: &Table) -> Result<usize, HarnessError> {
    if form != Form::Aggregate {
        return Ok(table.height());
    }
    match table.rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.parse().map_err(|_| {
            HarnessError::new(format!("count(*) came back as {text}, which is not a number"))
        }),
        _ => Ok(0),
    }
}

/// The three parts of an aggregate case added back up, one row wide.
///
/// Each column is folded by its own rule from [`FOLDS`], and a column the harness has no rule for is
/// added, which is what the seven real ones mostly are and is only ever reached by a test engine
/// that answers with a different shape. A part that is NULL in a column contributes nothing to it,
/// since that is what an aggregate over no rows says, and a column that is NULL in all three parts
/// stays NULL.
///
/// # Errors
///
/// When a cell is neither NULL nor a whole number, which means the select list and [`FOLDS`] have
/// drifted apart and the harness would otherwise quietly compare the wrong thing.
fn folded(parts: &[Table]) -> Result<Table, HarnessError> {
    let columns = parts.first().map(|first| first.columns.clone()).unwrap_or_default();
    let mut row = Vec::with_capacity(columns.len());
    for at in 0..columns.len() {
        let fold = FOLDS.get(at).copied().unwrap_or(Fold::Total);
        let mut so_far: Option<i128> = None;
        for part in parts {
            let Some(Cell::Text(text)) = part.rows.first().and_then(|first| first.get(at)) else {
                continue;
            };
            let value: i128 = text.parse().map_err(|_| {
                HarnessError::new(format!(
                    "an aggregate over a whole number came back as {text}, which cannot be added up"
                ))
            })?;
            so_far = Some(match (so_far, fold) {
                (None, _) => value,
                (Some(had), Fold::Total) => had.saturating_add(value),
                (Some(had), Fold::Least) => had.min(value),
                (Some(had), Fold::Most) => had.max(value),
            });
        }
        row.push(so_far.map_or(Cell::Null, |value| Cell::Text(value.to_string())));
    }
    Ok(Table { columns, rows: vec![row] })
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
    /// Which of the three forms ran.
    pub form: Form,
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
        writeln!(f, "{} against {}", self.form.headline(), self.engine)?;
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
            "{} of those split into three parts that add back up to the whole, {} of which divided the {} rather than putting everything in one part",
            self.held,
            self.sharp,
            self.form.divided()
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
pub fn run(
    engine: &mut dyn Engine,
    form: Form,
    cases: usize,
    seed: u64,
) -> Result<Found, HarnessError> {
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
        form,
        seed,
        engine: engine.version().to_owned(),
        cases,
        held: 0,
        sharp: 0,
        refused: BTreeMap::new(),
        broken: Vec::new(),
    };
    for _ in 0..cases {
        let case = match form {
            Form::Where => Case::rows(predicates.predicate()),
            Form::Aggregate => Case::aggregate(predicates.predicate()),
            Form::Having => {
                let (grouping, predicate) = predicates.groups();
                Case::groups(grouping, predicate)
            }
        };
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
    use super::{CASES, Case, Checked, Form, Found, fixture, run};
    use crate::engine::{Acceptance, Cell, Column, Engine, Outcome, Table};
    use crate::rudb::Rudb;
    use std::collections::BTreeMap;

    #[test]
    fn the_three_parts_are_the_predicate_its_negation_and_the_case_where_it_is_neither() {
        let case = Case::rows("i < 3");
        let parts = case.parts();
        assert_eq!(parts[0], "SELECT * FROM t WHERE (i < 3)");
        assert_eq!(parts[1], "SELECT * FROM t WHERE NOT (i < 3)");
        assert_eq!(parts[2], "SELECT * FROM t WHERE (i < 3) IS NULL");
        assert_eq!(case.together(), parts.join(" UNION ALL "));
    }

    #[test]
    fn the_aggregate_form_puts_the_same_three_predicates_under_the_aggregates() {
        let case = Case::aggregate("i < 3");
        let parts = case.parts();
        assert!(parts[0].starts_with("SELECT count(*), sum(i), min(i), max(i), sum(j), min(j)"));
        assert!(parts[0].ends_with("FROM t WHERE (i < 3)"));
        assert!(parts[1].ends_with("FROM t WHERE NOT (i < 3)"));
        assert!(parts[2].ends_with("FROM t WHERE (i < 3) IS NULL"));
        assert!(case.whole().ends_with("FROM t"));
        // The single statement form needs a subquery, which is the reason the harness does not use
        // it and prints it only for the person reproducing the failure somewhere that has one.
        assert!(case.together().contains("AS parts(n, si, ni, xi, sj, nj, xj)"));
    }

    #[test]
    fn the_having_form_groups_by_the_expression_and_partitions_on_the_having_clause() {
        let case = Case::groups("i % 3", "count(*) > 1");
        assert_eq!(
            case.whole(),
            "SELECT i % 3, count(*), sum(i), min(x), max(s) FROM t GROUP BY i % 3"
        );
        let parts = case.parts();
        assert!(parts[0].ends_with("GROUP BY i % 3 HAVING (count(*) > 1)"));
        assert!(parts[1].ends_with("GROUP BY i % 3 HAVING NOT (count(*) > 1)"));
        assert!(parts[2].ends_with("GROUP BY i % 3 HAVING (count(*) > 1) IS NULL"));
        assert_eq!(case.together(), parts.join(" UNION ALL "));
    }

    #[test]
    fn the_three_forms_are_spelled_the_way_the_command_line_takes_them() {
        for form in [Form::Where, Form::Aggregate, Form::Having] {
            assert_eq!(Form::named(form.spelling()), Some(form));
        }
        assert_eq!(Form::named("group by"), None);
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
        let case = Case::rows("i < 3");
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
        let case = Case::rows("1 = 1");
        assert_eq!(
            super::check(&mut rudb, &case).expect("rudb runs"),
            Checked::Held { sharp: false }
        );
    }

    #[test]
    fn a_predicate_the_engine_will_not_run_is_a_refusal_and_not_a_failure() {
        let mut rudb = loaded();
        let case = Case::rows("nosuchcolumn = 1");
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
        for form in [Form::Where, Form::Aggregate, Form::Having] {
            let found = run(&mut rudb, form, 200, 20250915).expect("rudb runs");
            assert!(found.is_clean(), "{found}");
            assert_eq!(found.refusals(), 0, "{found}");
            assert!(
                found.sharp * 4 > found.cases,
                "a run where almost nothing divided the table is testing almost nothing:\n{found}"
            );
        }
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
        let case = Case::rows("a = 1");
        let Checked::Split(broken) = super::check(&mut two, &case).expect("the fake engine runs")
        else {
            panic!("one row three times is not two rows");
        };
        assert_eq!(broken.whole, 2);
        assert_eq!(broken.parts, [1, 1, 1]);
        assert!(!broken.differences.is_empty());
        assert!(broken.to_string().contains("UNION ALL"));
    }

    #[test]
    fn the_same_engine_is_caught_by_the_aggregate_form_where_the_harness_does_the_adding() {
        // One column, which the fold rules treat as a count, so three parts of one row each add up
        // to three against a whole of two. The row counts are all one in this form whatever the
        // predicate did, so what has to be wrong here is the value and not the height.
        let mut two = Two;
        let case = Case::aggregate("a = 1");
        let Checked::Split(broken) = super::check(&mut two, &case).expect("the fake engine runs")
        else {
            panic!("one plus one plus one is not two");
        };
        assert_eq!(broken.whole, 0);
        assert!(!broken.differences.is_empty());
    }

    #[test]
    fn three_parts_of_one_aggregate_row_each_fold_by_their_own_rule() {
        let parts = [
            aggregate(&[Some(2), Some(5), Some(1), Some(4)]),
            aggregate(&[Some(3), None, Some(9), Some(9)]),
            aggregate(&[Some(1), Some(7), Some(0), Some(2)]),
        ];
        let folded = super::folded(&parts).expect("whole numbers fold");
        assert_eq!(folded.rows.len(), 1);
        // count and sum add, min keeps the smallest, max keeps the largest, and the part that was
        // NULL in a column is an aggregate over no rows and contributes nothing to it.
        assert_eq!(
            folded.rows[0],
            vec![
                Cell::Text("6".to_owned()),
                Cell::Text("12".to_owned()),
                Cell::Text("0".to_owned()),
                Cell::Text("9".to_owned()),
            ]
        );
    }

    #[test]
    fn a_column_no_part_had_a_value_for_folds_back_to_null() {
        let parts = [aggregate(&[None]), aggregate(&[None]), aggregate(&[None])];
        let folded = super::folded(&parts).expect("nothing folds to nothing");
        assert_eq!(folded.rows[0], vec![Cell::Null]);
    }

    #[test]
    fn an_aggregate_that_came_back_as_something_other_than_a_number_is_a_harness_failure() {
        let odd = Table {
            columns: vec![Column { name: "count_star()".to_owned(), ty: "BIGINT".to_owned() }],
            rows: vec![vec![Cell::Text("some".to_owned())]],
        };
        assert!(super::folded(&[odd]).is_err());
    }

    /// One row of aggregates, in the order [`super::FOLDS`] reads them.
    fn aggregate(values: &[Option<i64>]) -> Table {
        Table {
            columns: values
                .iter()
                .enumerate()
                .map(|(at, _)| Column { name: format!("a{at}"), ty: "BIGINT".to_owned() })
                .collect(),
            rows: vec![
                values
                    .iter()
                    .map(|v| v.map_or(Cell::Null, |n| Cell::Text(n.to_string())))
                    .collect(),
            ],
        }
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
            form: Form::Where,
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
        let case = Case::rows("i < 3");
        let Checked::Held { .. } = super::check(&mut rudb, &case).expect("rudb runs") else {
            panic!("that one holds");
        };
        let broken = Found {
            broken: vec![super::Broken {
                case: Case::rows("i < 3"),
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
