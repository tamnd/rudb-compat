//! Putting one statement to both engines and coming back with the differences.
//!
//! Two callers want this and they want exactly the same thing: the function sweep, which asks
//! fifteen thousand generated calls, and the reducer, which asks the same statement over and over
//! with a little less of it each time. Before this module the sweep had it inline and the reducer
//! would have had a second copy, and the part that is easy to get wrong is not the comparison, it is
//! what happens when an engine does not come back.
//!
//! rudb is linked into this process rather than run beside it, so a panic in rudb is a panic here.
//! It is caught, it is recorded as a difference against the statement that caused it, and the engine
//! that came apart is reset before it is asked anything else, because an unwind leaves whatever it
//! was in the middle of half done. None of that is optional: the first full sweep lost forty minutes
//! of work to one bad call, and a reducer that unwinds on the first candidate reduces nothing.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::compare::{Difference, Rules, Side, compare};
use crate::engine::{Engine, HarnessError, Outcome};

/// Put a statement to both engines and say how they disagreed.
///
/// An empty list means they agreed. A side that came apart is one difference and the other side is
/// not compared against it, because there is nothing to compare: a crash is not an answer.
///
/// # Errors
///
/// When either engine could not be run at all, which is a broken harness rather than a failing
/// statement.
pub fn differences(
    left: &mut dyn Engine,
    right: &mut dyn Engine,
    sql: &str,
    rules: Rules,
) -> Result<Vec<Difference>, HarnessError> {
    let a = ask(left, sql, Side::Left)?;
    let b = ask(right, sql, Side::Right)?;
    Ok(match (a, b) {
        (Answer::Ran(a), Answer::Ran(b)) => compare(&a, &b, rules),
        (a, b) => a.crash().into_iter().chain(b.crash()).collect(),
    })
}

/// What came back from an engine, which is an outcome or the engine coming apart.
#[derive(Debug)]
pub enum Answer {
    /// The engine answered, with rows or with an error of its own.
    Ran(Outcome),
    /// The engine panicked, and this is what it said on the way down.
    Panicked(Side, String),
}

impl Answer {
    /// The difference this answer is, if it is one on its own.
    #[must_use]
    pub fn crash(self) -> Option<Difference> {
        match self {
            Self::Ran(_) => None,
            Self::Panicked(side, message) => Some(Difference::Panicked { side, message }),
        }
    }
}

/// Run one statement on one engine and catch it coming apart.
///
/// The reset after a panic is not optional. An unwind leaves whatever the engine was in the middle
/// of half done, and going on to ask the same object another fifteen thousand questions would turn
/// one crash into a run nobody can read.
///
/// # Errors
///
/// When the engine could not be run, or could not be put back after a panic.
pub fn ask(engine: &mut dyn Engine, sql: &str, side: Side) -> Result<Answer, HarnessError> {
    match catch_unwind(AssertUnwindSafe(|| engine.run(sql))) {
        Ok(outcome) => outcome.map(Answer::Ran),
        Err(payload) => {
            engine.reset()?;
            Ok(Answer::Panicked(side, said(&*payload)))
        }
    }
}

/// What a panic said, out of the box it comes in.
fn said(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_owned();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "a panic that carried no message".to_owned()
}
