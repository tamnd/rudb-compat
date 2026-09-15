//! NoREC, which asks the engine the same question twice and takes the optimizer out of one of them.
//!
//! The paper is Rigger, Rui and Su, "Detecting Optimization Bugs in Database Engines via
//! Non-Optimizing Reference Engine Construction", ESEC/FSE 2020. Take a predicate `p`. The
//! ordinary way to ask how many rows satisfy it is `SELECT * FROM t WHERE (p)`, and that is the
//! query an optimizer works on: it pushes the predicate down, it uses an index, it rewrites the
//! expression, it decides a whole block of rows cannot match and skips it. Now move the predicate
//! out of the `WHERE` and into the select list, `SELECT (p) IS TRUE FROM t`, and none of that is
//! available any more. There is no filter to push, nothing to skip, and the engine has to evaluate
//! the expression once per row and hand every answer back. Count the true ones and you have the
//! same number by a route the optimizer cannot take.
//!
//! So two counts that have to be equal, computed by two paths that share the expression evaluator
//! and share nothing else. The paper's word for the second query is the non optimizing reference
//! engine, and the point of it is that it is a reference implementation the engine is already
//! obliged to have rather than one anybody had to write.
//!
//! ## Why this is worth having beside TLP
//!
//! `crate::tlp` tests three valued logic, which is a semantics question, and this tests the
//! optimizer, which is a plan question. They fail on different bugs. A filter that drops the rows
//! where the predicate is unknown is a TLP failure and a NoREC pass, because both of its queries
//! are filters and both drop the same rows. A pushdown that loses rows is a NoREC failure and a TLP
//! pass, because the three parts are all wrong in the same direction and still add up.
//!
//! ## Why it pairs with the pass bisector
//!
//! `spec/sql/duckdb/10-generation-and-fuzzing.md` section 10.4 asks for this next to the pass
//! bisector in section 9.6, and the pairing is the useful part rather than a coincidence. When the
//! two counts disagree, the same predicate is put to a second engine with every optimizer pass
//! turned off. If it agrees there, a rewrite did it and the bisector names which one. If it
//! disagrees there too, the optimizer is not where to look at all and the answer is wrong in the
//! binder or the executor, which is the finding that saves a day of reading plans.

use std::collections::BTreeMap;
use std::fmt;

use crate::engine::{Cell, Engine, HarnessError, Outcome, Table};
use crate::predicate::{Predicates, TABLE, fixture};
use crate::replay::{Run, Shape};
use crate::sqlsmith::generalised;

/// How many predicates a run tries when nobody said a number.
///
/// Two queries per predicate rather than four, so this is cheaper than a TLP run of the same size.
/// The number is the same one for the same reason: it is about how much of a generator's output a
/// reader wants to think about.
pub const CASES: usize = 1000;

/// One predicate, and the two queries that have to give the same number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// The predicate, exactly as it is written into both queries.
    pub predicate: String,
}

impl Case {
    /// The query the optimizer gets to work on.
    #[must_use]
    pub fn optimized(&self) -> String {
        format!("SELECT * FROM {TABLE} WHERE ({})", self.predicate)
    }

    /// The same question with the predicate moved where no rewrite can reach it.
    ///
    /// `IS TRUE` rather than the bare predicate, because the answer wanted is one value per row
    /// that is never NULL, and counting the trues of a column that is true, false or NULL means the
    /// harness has to decide what an unknown is worth. `IS TRUE` is the engine's own answer to that
    /// question and it is the one `WHERE` uses, so asking for it keeps the two sides comparing the
    /// same thing.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("SELECT ({}) IS TRUE FROM {TABLE}", self.predicate)
    }
}

/// What one predicate came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The engine would not run one of the two queries, and this is what it said.
    Refused(String),
    /// The two counts agree.
    Held {
        /// Whether the predicate matched some rows and not all of them. A predicate that matches
        /// everything or nothing agrees on both sides of any bug that drops or keeps rows in
        /// blocks, so a run wants to know how many of its cases were like that.
        sharp: bool,
    },
    /// They do not.
    Split(Broken),
}

/// A predicate the two routes to the same number disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    /// The predicate both queries were written from.
    pub predicate: String,
    /// How many rows the filter returned.
    pub filtered: usize,
    /// How many rows the select list said were true.
    pub counted: usize,
    /// What the same predicate does with every optimizer pass turned off, when there was a second
    /// engine to ask. This is the part that says where to look.
    pub without: Option<Verdict>,
}

/// What an engine with no optimizer says about a predicate the optimized one got wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The two counts agree with the passes off, so a rewrite did it and `bisect` names which.
    Optimizer,
    /// They disagree with the passes off as well, so this is the binder or the executor and the
    /// optimizer is not where to look.
    Deeper,
    /// The engine with the passes off would not run it, so there is nothing to say.
    Unknown,
}

impl Verdict {
    /// The sentence the report prints for it.
    #[must_use]
    pub const fn said(self) -> &'static str {
        match self {
            Self::Optimizer => {
                "with every optimizer pass off the two agree, so a rewrite did this and bisect names it"
            }
            Self::Deeper => {
                "with every optimizer pass off the two still disagree, so this is the binder or the executor"
            }
            Self::Unknown => "the engine with the passes off would not run it",
        }
    }
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  {}", self.predicate)?;
        writeln!(
            f,
            "    the filter returned {} rows and the select list found {} true",
            self.filtered, self.counted
        )?;
        if let Some(verdict) = self.without {
            writeln!(f, "    {}", verdict.said())?;
        }
        let case = Case { predicate: self.predicate.clone() };
        writeln!(f, "    {}", case.optimized())?;
        writeln!(f, "    {}", case.reference())
    }
}

/// Put one predicate to one engine both ways and say what happened.
///
/// The engine has to have the fixture loaded already, which [`run`] does once for the whole run.
///
/// # Errors
///
/// When the engine could not be run at all, which is a harness failure and not an answer.
pub fn check(engine: &mut dyn Engine, case: &Case) -> Result<Checked, HarnessError> {
    let Some((filtered, counted, rows)) = both(engine, case)? else {
        return Ok(Checked::Refused(refusal(engine, case)?));
    };
    if filtered == counted {
        return Ok(Checked::Held { sharp: counted > 0 && counted < rows });
    }
    Ok(Checked::Split(Broken {
        predicate: case.predicate.clone(),
        filtered,
        counted,
        without: None,
    }))
}

/// Both counts, and how many rows the reference query saw, or nothing when the engine refused.
fn both(
    engine: &mut dyn Engine,
    case: &Case,
) -> Result<Option<(usize, usize, usize)>, HarnessError> {
    let Outcome::Rows(filtered) = engine.run(&case.optimized())? else { return Ok(None) };
    let Outcome::Rows(counted) = engine.run(&case.reference())? else { return Ok(None) };
    Ok(Some((filtered.height(), trues(&counted), counted.height())))
}

/// What the engine said about the predicate, for the refusal count.
///
/// The queries are run again rather than the error being carried out of [`both`], because the
/// interesting error is the first one either query produced and there are two of them. This is the
/// refusal path, which is rare on an engine that has the features and constant on one that does
/// not, and either way two more statements against a thirteen row table is nothing.
fn refusal(engine: &mut dyn Engine, case: &Case) -> Result<String, HarnessError> {
    for sql in [case.optimized(), case.reference()] {
        if let Outcome::Error(e) = engine.run(&sql)? {
            return Ok(format!("{}: {}", e.kind, generalised(e.headline())));
        }
    }
    Ok("the engine refused once and then did not".to_owned())
}

/// How many cells in a one column result are the word true.
///
/// The value is compared as the text the engine printed, the same way every other comparison in
/// this crate works, so a NULL is `Cell::Null` and is not the text of one. `IS TRUE` should never
/// produce a NULL, and a row that is one is counted as not true rather than as an error, because
/// the count then disagrees and the disagreement is the finding.
fn trues(table: &Table) -> usize {
    table
        .rows
        .iter()
        .filter(|row| matches!(row.first(), Some(Cell::Text(t)) if t == "true"))
        .count()
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
    /// How many of them the two counts agreed on.
    pub held: usize,
    /// How many of those matched some rows and not all of them.
    pub sharp: usize,
    /// Each distinct thing the engine refused with, and how many predicates it refused that way.
    pub refused: BTreeMap<String, usize>,
    /// Every predicate the two counts disagree about.
    pub broken: Vec<Broken>,
}

impl Found {
    /// This run as a row for the generated series.
    ///
    /// Same shape as the TLP row and for the same reasons: nothing to group a finding by yet, and
    /// the engine matters because `--pinned` puts it to DuckDB instead.
    #[must_use]
    pub fn recorded(&self, engines: &'static str) -> Run {
        Run {
            mode: "norec",
            shape: Shape::Only,
            seed: self.seed,
            cases: self.cases,
            usable: self.cases - self.refusals(),
            findings: self.broken.len(),
            groups: self.broken.len(),
            engines,
        }
    }

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
        writeln!(f, "the optimizer against itself on {}", self.engine)?;
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
            "{} of those count the same filtered as they do evaluated a row at a time, {} of which matched some rows and not all of them",
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
        writeln!(f, "where the two counts disagree, against this table")?;
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

/// Load the fixture and put that many generated predicates to the engine, both ways.
///
/// `without` is the same engine with every optimizer pass turned off, and it is only asked about
/// the predicates the first one got wrong. Passing nothing is allowed and costs the run the
/// sentence that says where to look, which is what happens when the engine is a binary rather than
/// a library and there is no second one to open.
///
/// # Errors
///
/// When an engine could not be run at all, and when one of them will not load the fixture.
pub fn run(
    engine: &mut dyn Engine,
    without: Option<&mut dyn Engine>,
    cases: usize,
    seed: u64,
) -> Result<Found, HarnessError> {
    load(engine)?;
    let mut without = match without {
        Some(engine) => {
            load(engine)?;
            Some(engine)
        }
        None => None,
    };

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
        let case = Case { predicate: predicates.predicate() };
        match check(engine, &case)? {
            Checked::Refused(said) => *found.refused.entry(said).or_insert(0) += 1,
            Checked::Held { sharp } => {
                found.held += 1;
                found.sharp += usize::from(sharp);
            }
            Checked::Split(mut broken) => {
                if let Some(plain) = without.as_deref_mut() {
                    broken.without = Some(verdict(plain, &case)?);
                }
                found.broken.push(broken);
            }
        }
    }
    Ok(found)
}

/// What the engine with no optimizer says about a predicate the optimized one got wrong.
fn verdict(engine: &mut dyn Engine, case: &Case) -> Result<Verdict, HarnessError> {
    let Some((filtered, counted, _)) = both(engine, case)? else { return Ok(Verdict::Unknown) };
    Ok(if filtered == counted { Verdict::Optimizer } else { Verdict::Deeper })
}

/// Put the fixture into an engine, from a clean database.
fn load(engine: &mut dyn Engine) -> Result<(), HarnessError> {
    engine.reset()?;
    for statement in fixture() {
        if let Outcome::Error(e) = engine.run(&statement)? {
            return Err(HarnessError::new(format!(
                "{} will not load the fixture: {e}",
                engine.name()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CASES, Case, Checked, Verdict, run, trues};
    use crate::engine::{Acceptance, Cell, Column, Engine, HarnessError, Outcome, Table};
    use crate::predicate::fixture;
    use crate::rudb::Rudb;

    #[test]
    fn the_two_queries_ask_the_same_thing_and_only_one_of_them_can_be_optimized() {
        let case = Case { predicate: "i < 3".to_owned() };
        assert_eq!(case.optimized(), "SELECT * FROM t WHERE (i < 3)");
        assert_eq!(case.reference(), "SELECT (i < 3) IS TRUE FROM t");
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
    fn a_predicate_that_matches_some_rows_and_not_all_of_them_holds_and_says_so() {
        let mut rudb = loaded();
        let case = Case { predicate: "i < 3".to_owned() };
        assert_eq!(
            super::check(&mut rudb, &case).expect("rudb runs"),
            Checked::Held { sharp: true }
        );
    }

    #[test]
    fn a_predicate_that_matches_everything_holds_and_is_not_counted_as_sharp() {
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
        // The same arrangement as the TLP test and for the same reason: this is the check running
        // as a test rather than as a subcommand, which is what makes it a pre merge gate.
        let mut rudb = Rudb::new();
        let mut plain = Rudb::unoptimized();
        let found = run(&mut rudb, Some(&mut plain), 200, 20250915).expect("rudb runs");
        assert!(found.is_clean(), "{found}");
        assert!(
            found.sharp * 4 > found.cases,
            "a run where almost nothing matched some rows and not all of them is testing almost nothing:\n{found}"
        );
    }

    #[test]
    fn the_default_count_is_a_number_a_person_asked_for() {
        assert_eq!(CASES, 1000);
    }

    #[test]
    fn a_null_where_is_true_should_have_answered_is_counted_as_not_true() {
        let table = Table {
            columns: vec![Column { name: "x".to_owned(), ty: "BOOLEAN".to_owned() }],
            rows: vec![
                vec![Cell::Text("true".to_owned())],
                vec![Cell::Null],
                vec![Cell::Text("false".to_owned())],
            ],
        };
        assert_eq!(trues(&table), 1);
    }

    /// An engine whose filter loses a row the select list finds, which is what a pushdown that
    /// drops rows looks like from outside.
    #[derive(Debug)]
    struct Pushed {
        optimized: bool,
    }

    impl Engine for Pushed {
        fn name(&self) -> &str {
            "an engine with a pushdown that drops a row"
        }

        fn version(&self) -> &str {
            "pushdown 1.0"
        }

        fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
            let trues = if sql.contains("WHERE") && self.optimized { 1 } else { 2 };
            Ok(Outcome::Rows(Table {
                columns: vec![Column { name: "x".to_owned(), ty: "BOOLEAN".to_owned() }],
                rows: (0..trues).map(|_| vec![Cell::Text("true".to_owned())]).collect(),
            }))
        }

        fn accepts(&mut self, _sql: &str) -> Result<Acceptance, HarnessError> {
            Ok(Acceptance::Accepted)
        }
    }

    #[test]
    fn an_engine_whose_filter_loses_a_row_is_caught_and_the_passes_off_run_names_the_optimizer() {
        // rudb passes every predicate the generator writes, so without this the oracle is a test of
        // the generator and of nothing else. The second engine here is the same bug with the
        // pushdown turned off, which is the pairing with the pass bisector that section 10.4 asks
        // for, and the verdict has to come back naming the optimizer.
        let mut engine = Pushed { optimized: true };
        let mut plain = Pushed { optimized: false };
        let found = run(&mut engine, Some(&mut plain), 3, 1).expect("the fake engine runs");
        assert!(!found.is_clean());
        assert_eq!(found.broken.len(), 3);
        assert_eq!(found.broken[0].filtered, 1);
        assert_eq!(found.broken[0].counted, 2);
        assert_eq!(found.broken[0].without, Some(Verdict::Optimizer));
        assert!(found.to_string().contains("IS TRUE"));
    }

    #[test]
    fn the_same_bug_with_the_passes_off_as_well_says_it_is_not_the_optimizer() {
        let mut engine = Pushed { optimized: true };
        let mut plain = Pushed { optimized: true };
        let found = run(&mut engine, Some(&mut plain), 1, 1).expect("the fake engine runs");
        assert_eq!(found.broken[0].without, Some(Verdict::Deeper));
        assert!(found.to_string().contains("the binder or the executor"));
    }
}
