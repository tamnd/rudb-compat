//! Naming the optimizer pass that changed an answer.
//!
//! A wrong answer is a sentence and a plan, and the plan is the part nobody wants to read. rudb has
//! `SET disabled_optimizers`, which takes DuckDB's comma separated spelling and turns a rewrite off
//! for the statements that follow, so the question "which pass did this" can be asked by running
//! the statement again rather than by reading anything.
//!
//! The search is linear over the passes and not a binary search over subsets. `spec/09-optimizer.md`
//! section 9.1 describes the bisector as a binary search, which is the right shape when the list is
//! long, and the list is a handful. That many runs of a statement that already ran is nothing, a
//! binary search over subsets finds one pass and quietly picks a side when two of them are involved,
//! and the linear search answers the question the report actually asks, which is which passes each
//! on their own make the difference go away.
//!
//! There is an eighth run with every pass off, and it is the one that earns the module. Most wrong
//! answers are not the optimizer at all, and a run that says so costs one statement and saves
//! somebody an afternoon in the wrong file. `spec/09-optimizer.md` section 9.1 is what makes that
//! answer sound: the unoptimized plan is the right answer by construction, so a query that is still
//! wrong with every rewrite off is wrong in the binder or the executor and there is nothing here to
//! find.
//!
//! This asks the engine rather than the process it happens to be in, so it works through the
//! library driver and through a shell that keeps a session, and it cannot work through a driver
//! that forgets between statements, because a `SET` that is forgotten is a `SET` that did nothing.

use std::fmt;

use crate::compare::{Rules, compare};
use crate::engine::{Engine, HarnessError, Outcome};

/// What the optimizer had to do with a difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blame {
    /// Turning these off, each on its own, makes the engine answer the way the other one does.
    ///
    /// Usually one name. More than one means several passes each make the difference go away
    /// alone, which happens when one of them is what feeds the one that is wrong.
    Passes(Vec<String>),
    /// No single pass does it and every pass off does, so it takes two of them together.
    Together,
    /// Every pass off answers the same way, so the optimizer is not where to look.
    ///
    /// The unoptimized plan is the right answer by construction, so this says the bug is in the
    /// binder or the executor and no amount of reading plans will find it.
    Elsewhere,
}

impl fmt::Display for Blame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Passes(names) if names.len() == 1 => {
                write!(f, "{} is the pass that changed the answer", names[0])
            }
            Self::Passes(names) => write!(
                f,
                "any one of {} off makes the answer agree, so the one to look at is whichever of them the others feed",
                names.join(", ")
            ),
            Self::Together => write!(
                f,
                "no single pass off makes the answer agree and every pass off does, so it takes two of them together"
            ),
            Self::Elsewhere => write!(
                f,
                "every pass off answers the same way, so this is the binder or the executor and not the optimizer"
            ),
        }
    }
}

/// Which pass changed the answer, by running the statement again with each one turned off.
///
/// `want` is what the statement should have produced, which in a differential run is what the other
/// engine produced and in a conformance run is what the file says. The comparison is the same one
/// the run that found the difference used, because a bisector that compared differently could
/// report a pass as the cause of a difference nobody was looking at.
///
/// The passes come from the crate this is built against rather than from the engine, because there
/// is nothing to ask an engine for. rudb has no `duckdb_optimizers()` table function yet. A shell of
/// a different build than this one is therefore a shell that might not have every name, and a name
/// it does not have is a `SET` that fails, which comes back as a harness error rather than as a
/// wrong verdict.
///
/// # Errors
///
/// When the engine could not be run at all, and when it refused one of the `SET` statements, which
/// means the engine and the pass list do not agree and every answer after that would be about a
/// pass that was never turned off.
pub fn blame(
    engine: &mut dyn Engine,
    sql: &str,
    want: &Outcome,
    rules: Rules,
) -> Result<Blame, HarnessError> {
    let passes = rudb::optimizers();
    let mut found = Vec::new();
    for pass in &passes {
        if agrees(engine, sql, want, rules, pass)? {
            found.push((*pass).to_string());
        }
    }
    if !found.is_empty() {
        return Ok(Blame::Passes(found));
    }
    if agrees(engine, sql, want, rules, &passes.join(","))? {
        return Ok(Blame::Together);
    }
    Ok(Blame::Elsewhere)
}

/// Whether the statement agrees with what was wanted while those passes are off.
///
/// The setting is put back whatever happened, including when the statement itself failed, because
/// the engine in a differential run is the one the next record uses and a run that left a pass off
/// would report the rest of the file against a different optimizer.
fn agrees(
    engine: &mut dyn Engine,
    sql: &str,
    want: &Outcome,
    rules: Rules,
    off: &str,
) -> Result<bool, HarnessError> {
    set(engine, &format!("SET disabled_optimizers = '{off}'"))?;
    let got = engine.run(sql);
    set(engine, "RESET disabled_optimizers")?;
    Ok(compare(&got?, want, rules).is_empty())
}

/// Run one statement that has to work, and say so loudly when it does not.
fn set(engine: &mut dyn Engine, sql: &str) -> Result<(), HarnessError> {
    match engine.run(sql)? {
        Outcome::Rows(_) => Ok(()),
        Outcome::Error(error) => Err(HarnessError::new(format!(
            "{} refused `{sql}`, with {}: {}. The pass list this was built against and the one the engine has do not agree, so nothing below it would be about the pass it names.",
            engine.name(),
            error.kind,
            error.message
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Blame, blame};
    use crate::compare::Rules;
    use crate::engine::{
        Acceptance, Cell, Column, Engine, EngineError, HarnessError, Outcome, Table,
    };

    /// An engine that answers from a script rather than by running anything.
    ///
    /// It keeps the passes that are off the way a real one does, so the thing under test here is
    /// the sweep and not a mock that agrees with whatever it is asked.
    struct Canned {
        /// Which passes have to be off for the answer to be the right one. Empty means it is right
        /// with everything on.
        needs: Vec<&'static str>,
        /// Whether no arrangement of passes makes it right.
        hopeless: bool,
        /// What is off right now.
        off: Vec<String>,
        /// Every statement it was asked, in order, which is what counts the runs.
        asked: Vec<String>,
    }

    impl Canned {
        fn new(needs: &[&'static str]) -> Self {
            Self { needs: needs.to_vec(), hopeless: false, off: Vec::new(), asked: Vec::new() }
        }

        fn hopeless() -> Self {
            Self { needs: Vec::new(), hopeless: true, off: Vec::new(), asked: Vec::new() }
        }
    }

    impl Engine for Canned {
        fn name(&self) -> &str {
            "canned"
        }

        fn version(&self) -> &str {
            "0"
        }

        fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
            self.asked.push(sql.to_owned());
            if let Some(rest) = sql.strip_prefix("SET disabled_optimizers = '") {
                let names = rest.trim_end_matches('\'');
                self.off = names.split(',').filter(|n| !n.is_empty()).map(str::to_owned).collect();
                return Ok(Outcome::Rows(Table::default()));
            }
            if sql == "RESET disabled_optimizers" {
                self.off.clear();
                return Ok(Outcome::Rows(Table::default()));
            }
            let right = !self.hopeless
                && self.needs.iter().all(|need| self.off.iter().any(|name| name == need));
            Ok(Outcome::Rows(one(if right { "right" } else { "wrong" })))
        }

        fn accepts(&mut self, _sql: &str) -> Result<Acceptance, HarnessError> {
            Ok(Acceptance::Accepted)
        }
    }

    /// An engine that will not have the setting at all.
    struct Refuses;

    impl Engine for Refuses {
        fn name(&self) -> &str {
            "refuses"
        }

        fn version(&self) -> &str {
            "0"
        }

        fn run(&mut self, _sql: &str) -> Result<Outcome, HarnessError> {
            Ok(Outcome::Error(EngineError {
                kind: "Parser Error".to_owned(),
                message: "Optimizer type \"top_n\" not recognized".to_owned(),
            }))
        }

        fn accepts(&mut self, _sql: &str) -> Result<Acceptance, HarnessError> {
            Ok(Acceptance::Accepted)
        }
    }

    /// A one row one column table holding that text.
    fn one(text: &str) -> Table {
        Table {
            columns: vec![Column { name: "a".to_owned(), ty: "VARCHAR".to_owned() }],
            rows: vec![vec![Cell::Text(text.to_owned())]],
        }
    }

    #[test]
    fn the_pass_that_has_to_be_off_for_the_answer_to_agree_is_the_one_named() {
        let mut engine = Canned::new(&["top_n"]);
        let want = Outcome::Rows(one("right"));
        let blamed = blame(&mut engine, "SELECT 1", &want, Rules::default()).expect("it ran");
        assert_eq!(blamed, Blame::Passes(vec!["top_n".to_owned()]), "{blamed}");
    }

    #[test]
    fn a_difference_no_single_pass_accounts_for_and_all_of_them_do_takes_two_together() {
        let mut engine = Canned::new(&["top_n", "limit_pushdown"]);
        let want = Outcome::Rows(one("right"));
        let blamed = blame(&mut engine, "SELECT 1", &want, Rules::default()).expect("it ran");
        assert_eq!(blamed, Blame::Together, "{blamed}");
    }

    #[test]
    fn a_difference_that_is_there_with_every_pass_off_is_not_the_optimizer() {
        let mut engine = Canned::hopeless();
        let want = Outcome::Rows(one("right"));
        let blamed = blame(&mut engine, "SELECT 1", &want, Rules::default()).expect("it ran");
        assert_eq!(blamed, Blame::Elsewhere, "{blamed}");
        assert!(blamed.to_string().contains("binder or the executor"), "{blamed}");
    }

    #[test]
    fn the_setting_is_put_back_after_every_run_so_the_next_record_gets_the_whole_optimizer() {
        let mut engine = Canned::new(&["top_n"]);
        let want = Outcome::Rows(one("right"));
        blame(&mut engine, "SELECT 1", &want, Rules::default()).expect("it ran");
        assert!(engine.off.is_empty(), "{:?} was left off", engine.off);
        let resets = engine.asked.iter().filter(|sql| *sql == "RESET disabled_optimizers").count();
        let sets = engine.asked.iter().filter(|sql| sql.starts_with("SET ")).count();
        assert_eq!(resets, sets, "every set is put back");
    }

    #[test]
    fn it_is_one_run_per_pass_and_one_more_with_all_of_them_off() {
        let mut engine = Canned::hopeless();
        let want = Outcome::Rows(one("right"));
        blame(&mut engine, "SELECT 1", &want, Rules::default()).expect("it ran");
        let runs = engine.asked.iter().filter(|sql| *sql == "SELECT 1").count();
        assert_eq!(runs, rudb::optimizers().len() + 1, "{:?}", engine.asked);
    }

    #[test]
    fn it_stops_early_rather_than_naming_a_pass_an_engine_never_turned_off() {
        let mut engine = Refuses;
        let want = Outcome::Rows(one("right"));
        let error = blame(&mut engine, "SELECT 1", &want, Rules::default()).expect_err("refused");
        assert!(error.to_string().contains("do not agree"), "{error}");
    }
}
