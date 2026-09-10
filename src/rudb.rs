//! rudb, as an engine the harness can point at.
//!
//! rudb cannot run a query yet. That is not a reason to leave this side of the harness unwritten,
//! because there is one thing it can already answer and it is the thing the compatibility claim
//! rests on hardest: whether a piece of text is SQL. `rudb-parse` has a tokenizer, a rule table
//! generated from DuckDB's own grammar, a matcher and a transformer, so every statement in a
//! corpus can be put to both engines and the two answers compared today.
//!
//! That comparison already earns its keep. Three claims in the rudb specification about the
//! dialect turned out to be wrong and each was found by reading DuckDB's source rather than by
//! running it, which is exactly the method this harness exists to replace.

use rudb_common::{Error, ErrorCode};
use rudb_parse::ast::Statement;
use rudb_parse::{parse_ast, tokenize};

use crate::compare::Ordering;
use crate::engine::{Acceptance, Engine, EngineError, HarnessError, Outcome};

/// The rudb build this harness was linked against.
#[derive(Debug, Clone)]
pub struct Rudb {
    version: String,
}

impl Default for Rudb {
    fn default() -> Self {
        Self::new()
    }
}

impl Rudb {
    /// The rudb this crate is built against.
    #[must_use]
    pub fn new() -> Self {
        Self { version: format!("rudb-parse {}", rudb_parse_version()) }
    }
}

/// What version of the parser crate we linked, which is the version of rudb we are testing.
fn rudb_parse_version() -> &'static str {
    // There is no `rudb` to ask yet, so the version comes from the one crate that does work. When
    // the embedding API arrives this asks that instead, and the string in every report changes
    // from naming a crate to naming a database.
    option_env!("RUDB_VERSION").unwrap_or("from git")
}

impl Engine for Rudb {
    fn name(&self) -> &str {
        "rudb"
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn run(&mut self, sql: &str) -> Result<Outcome, HarnessError> {
        match parse_ast(sql) {
            Ok(_) => Ok(Outcome::Error(EngineError {
                kind: ErrorCode::NotImplemented.duckdb_name().to_owned(),
                message: "rudb parses this and cannot run it yet".to_owned(),
            })),
            Err(e) => Ok(Outcome::Error(engine_error(&e))),
        }
    }

    fn accepts(&mut self, sql: &str) -> Result<Acceptance, HarnessError> {
        // The matcher and not the transformer, because acceptance is a question about the grammar.
        // A statement the transformer has not reached yet still parses, and counting it as a
        // rejection would make the harness report a hole in the dialect where there is a hole in
        // the AST.
        Ok(match rudb_parse::parse(sql) {
            Ok(_) => Acceptance::Accepted,
            Err(e) => Acceptance::Rejected(engine_error(&e)),
        })
    }
}

/// Turn a rudb error into the form the comparison reads.
///
/// The code comes across as itself rather than through its printed form, because `ErrorCode`
/// already carries DuckDB's exact spelling including the parts that look like typos, and going
/// through text would mean parsing back out something we have in hand.
fn engine_error(error: &Error) -> EngineError {
    EngineError { kind: error.code().duckdb_name().to_owned(), message: error.message().to_owned() }
}

/// Whether the query fixes its own row order.
///
/// Section 14.2 says results are compared in order when the query has an `ORDER BY` and sorted
/// when it does not. Deciding which needs a parser, and rudb has one, so the AST answers it: a
/// query orders itself when its top level has an order clause, and an `ORDER BY` inside a
/// subquery does not count because it does not survive into the outer result.
///
/// When rudb cannot parse the text at all, the fallback is a token scan for the word `ORDER`,
/// using rudb's tokenizer so that the word inside a string literal or a comment does not count.
/// The scan cannot tell a top level clause from a nested one, so it says `AsWritten` whenever it
/// sees the word, which makes an unstable order show up as a difference rather than disappear into
/// a sort. A false failure costs someone a look at a report. A false pass costs a user their data
/// coming back in an order the query said it would not.
#[must_use]
pub fn ordering_of(sql: &str) -> Ordering {
    if let Ok(ast) = parse_ast(sql) {
        let ordered = ast.statements.iter().any(|statement| {
            let Statement::Query(at) = statement;
            let query = ast.query(*at);
            query.order_by_all || !query.order_by.is_empty()
        });
        return if ordered { Ordering::AsWritten } else { Ordering::Sorted };
    }

    let Ok(tokens) = tokenize(sql) else {
        return Ordering::AsWritten;
    };
    if tokens.iter().any(|t| t.text(sql).eq_ignore_ascii_case("order")) {
        Ordering::AsWritten
    } else {
        Ordering::Sorted
    }
}

#[cfg(test)]
mod tests {
    use super::{Rudb, ordering_of};
    use crate::compare::Ordering;
    use crate::engine::{Engine, Outcome};

    #[test]
    fn text_that_is_not_sql_gets_duckdbs_own_error_kind() {
        let mut rudb = Rudb::new();
        let Outcome::Error(e) = rudb.run("SELECT FROM WHERE").unwrap() else {
            panic!("that is not valid SQL");
        };
        assert_eq!(e.kind, "Parser Error");
    }

    #[test]
    fn text_that_is_sql_says_so_and_says_it_cannot_run_it() {
        let mut rudb = Rudb::new();
        let Outcome::Error(e) = rudb.run("SELECT 1").unwrap() else {
            panic!("nothing here can return rows yet");
        };
        assert_eq!(e.kind, "Not implemented Error");
    }

    #[test]
    fn a_top_level_order_by_makes_the_order_part_of_the_answer() {
        assert_eq!(ordering_of("SELECT x FROM t ORDER BY x"), Ordering::AsWritten);
        assert_eq!(ordering_of("SELECT x FROM t"), Ordering::Sorted);
    }

    #[test]
    fn an_order_by_inside_a_subquery_does_not_order_the_outer_result() {
        let sql = "SELECT x FROM (SELECT x FROM t ORDER BY x)";
        assert_eq!(ordering_of(sql), Ordering::Sorted);
    }

    #[test]
    fn the_word_order_in_a_string_is_not_an_order_by() {
        // This one has to go through the fallback to be worth anything, so it is deliberately
        // something the transformer does not handle yet. When it does, the AST answers it and the
        // answer is the same.
        assert_eq!(ordering_of("VALUES ('order by x')"), Ordering::Sorted);
    }

    #[test]
    fn text_that_does_not_even_tokenize_is_treated_as_ordered() {
        assert_eq!(ordering_of("SELECT 'unterminated"), Ordering::AsWritten);
    }
}
