//! Shrinking a failing statement to the smallest one that still fails the same way.
//!
//! `spec/14-rudb-compat.md` section 14.2 says every failure is reduced automatically, because a
//! forty line generated query that returns the wrong answer tells you nothing about why. It is the
//! difference between a failure list somebody works through and a failure list somebody closes.
//!
//! Reduction here is over tokens and clauses rather than over characters. Character reduction on
//! SQL spends its whole budget producing text that does not parse, and what comes out the far end
//! of it, if anything does, is unreadable. So the moves are the ones a person would make: drop a
//! clause, drop an item from a comma separated list, drop a conjunct, replace a parenthesised group
//! by a constant, shrink a literal, and delete a run of tokens. Every candidate is put through
//! [`rudb::parses`] before either engine sees it, which costs nothing and throws away most of what a
//! naive reducer would spend its budget on.
//!
//! It is not the tree aware reducer that section asks for, and the reason is worth writing down.
//! The tree is there, rudb parses into an arena AST, but the nodes carry no spans and there is no
//! printer, so there is no way from outside the parser to say which bytes a node came from or to
//! turn a node back into SQL. Until one of those exists a reducer outside rudb cannot cut on node
//! boundaries. Tokens and paren depth get most of the way there, because the cuts that matter are
//! clause boundaries and list items at a depth, and both of those are visible without a tree. What
//! they do not reach is anything that needs to know a node is a node, which is tamnd/rudb#519.
//!
//! The rule every step is held to is that the statement still fails the same way. Not that it still
//! fails: a cut that turns a wrong answer into a parse error has thrown one bug away and found
//! another, and a reducer that accepts it walks off the bug it was pointed at. That is what
//! [`Difference::signature`] is for.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ask::differences;
use crate::compare::{Difference, MessageMatch, Rules};
use crate::engine::{Engine, HarnessError};
use crate::rudb::ordering_of;

/// How many candidates one reduction may put to the engines before it stops.
///
/// Every candidate is two engine runs and one of them is a subprocess, so this is a clock and not a
/// memory limit. Two thousand is about four minutes against a live DuckDB, which is the right order
/// for something a person is waiting on, and a reduction that runs out says so rather than
/// presenting what it happened to have reached as a minimum.
pub const BUDGET: usize = 2000;

/// What is left of a statement when the reducer has finished with it.
#[derive(Debug, Clone)]
pub struct Reduced {
    /// The smallest statement that still failed the same way.
    pub sql: String,
    /// How large the statement was to start with, in bytes.
    pub before: usize,
    /// How large it is now.
    pub after: usize,
    /// How many candidates were put to the engines.
    pub tried: usize,
    /// The move each kept step was, in the order they were taken.
    pub steps: Vec<&'static str>,
    /// Whether it stopped because it ran out of budget rather than because it was finished.
    pub gave_up: bool,
}

impl fmt::Display for Reduced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.sql)?;
        writeln!(f)?;
        writeln!(
            f,
            "{} bytes down to {}, in {} steps out of {} candidates",
            self.before,
            self.after,
            self.steps.len(),
            self.tried
        )?;
        let mut moves: BTreeMap<&str, usize> = BTreeMap::new();
        for step in &self.steps {
            *moves.entry(step).or_default() += 1;
        }
        for (what, count) in moves {
            writeln!(f, "    {count:>4}  {what}")?;
        }
        if self.gave_up {
            writeln!(f)?;
            writeln!(
                f,
                "It ran out of candidates before it ran out of things to try, so this is the smallest one it reached and not the smallest one there is."
            )?;
        }
        Ok(())
    }
}

/// Shrink a statement for as long as it keeps failing the same way.
///
/// `keep` is the question asked of every candidate, and everything that makes this useful is in
/// there rather than here. This half only knows how to make a statement smaller and how to tell
/// that it did.
///
/// A trailing semicolon comes off first. Both drivers take one statement at a time, and a semicolon
/// left on the end turns every later cut into a candidate that has to be special cased.
///
/// # Errors
///
/// When the question cannot be answered, which is an engine that could not be run.
pub fn shrink(
    sql: &str,
    budget: usize,
    keep: &mut dyn FnMut(&str) -> Result<bool, HarnessError>,
) -> Result<Reduced, HarnessError> {
    let start = sql.trim().trim_end_matches(';').trim().to_owned();
    // Only filter on the grammar when the statement started out inside it. A statement rudb rejects
    // is a failure worth reducing, it is the difference this harness was built to find first, and
    // filtering its candidates through the parser that rejects it would throw away every one of
    // them.
    let grammatical = rudb::parses(&start);
    let before = start.len();
    let mut best = start;
    let mut tried = 0;
    let mut steps = Vec::new();
    let mut gave_up = false;
    'again: loop {
        for cut in cuts(&best) {
            let candidate = apply(&best, &cut);
            if candidate.trim().is_empty() || candidate == best {
                continue;
            }
            if grammatical && !rudb::parses(&candidate) {
                continue;
            }
            if tried >= budget {
                gave_up = true;
                break 'again;
            }
            tried += 1;
            if keep(&candidate)? {
                best = candidate;
                steps.push(cut.what);
                continue 'again;
            }
        }
        break;
    }
    // The whitespace the cuts left behind, taken out at the end rather than after every step, so
    // that the offsets a step works over are the offsets of the text it was given.
    let tidied = tidy(&best);
    if tidied != best {
        tried += 1;
        if keep(&tidied)? {
            best = tidied;
        }
    }
    Ok(Reduced { after: best.len(), sql: best, before, tried, steps, gave_up })
}

/// The two engines, and the failure the reducer is keeping alive.
pub struct Alive<'a> {
    left: &'a mut dyn Engine,
    right: &'a mut dyn Engine,
    messages: MessageMatch,
    wanted: BTreeSet<String>,
}

impl fmt::Debug for Alive<'_> {
    /// The engines are behind a trait that says nothing about itself, so this prints the part that
    /// is worth seeing, which is what the reduction is holding on to.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Alive")
            .field("messages", &self.messages)
            .field("wanted", &self.wanted)
            .finish_non_exhaustive()
    }
}

impl<'a> Alive<'a> {
    /// Read how the statement fails now, which is how every step has to leave it failing.
    ///
    /// # Errors
    ///
    /// When either engine could not be run, or when the two of them agree about the statement, in
    /// which case there is no failure to shrink and the answer to give is that rather than a
    /// reduction of something to nothing.
    pub fn of(
        left: &'a mut dyn Engine,
        right: &'a mut dyn Engine,
        sql: &str,
        messages: MessageMatch,
    ) -> Result<Self, HarnessError> {
        let mut one = Self { left, right, messages, wanted: BTreeSet::new() };
        one.wanted = one.signatures(sql)?;
        if one.wanted.is_empty() {
            return Err(HarnessError::new(
                "the two engines agree about this statement, so there is nothing to reduce"
                    .to_owned(),
            ));
        }
        Ok(one)
    }

    /// Whether this statement still fails the way the one it came from failed.
    ///
    /// Any one of the original differences surviving is enough. A cut that removes a column removes
    /// the difference about that column and that is the reduction working, not the bug going away,
    /// so requiring every original difference to survive would stop the reducer at the first useful
    /// step.
    ///
    /// # Errors
    ///
    /// When either engine could not be run.
    pub fn keeps(&mut self, sql: &str) -> Result<bool, HarnessError> {
        Ok(!self.signatures(sql)?.is_disjoint(&self.wanted))
    }

    /// Every way the two engines disagree about a statement, for the report at the end.
    ///
    /// # Errors
    ///
    /// When either engine could not be run.
    pub fn differences(&mut self, sql: &str) -> Result<Vec<Difference>, HarnessError> {
        let rules = Rules { ordering: ordering_of(sql), messages: self.messages };
        differences(self.left, self.right, sql, rules)
    }

    /// What the original statement did, in the words the report uses.
    #[must_use]
    pub fn keeping(&self) -> Vec<String> {
        self.wanted.iter().cloned().collect()
    }

    fn signatures(&mut self, sql: &str) -> Result<BTreeSet<String>, HarnessError> {
        Ok(self.differences(sql)?.iter().map(Difference::signature).collect())
    }
}

/// One thing to try taking out of a statement, or replacing in it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cut {
    at: usize,
    end: usize,
    with: String,
    what: &'static str,
}

impl Cut {
    fn out(at: usize, end: usize, what: &'static str) -> Self {
        Self { at, end, with: String::new(), what }
    }

    fn to(at: usize, end: usize, with: &str, what: &'static str) -> Self {
        Self { at, end, with: with.to_owned(), what }
    }
}

/// A statement with one cut made in it.
fn apply(sql: &str, cut: &Cut) -> String {
    let mut out = String::with_capacity(sql.len());
    out.push_str(&sql[..cut.at]);
    out.push_str(&cut.with);
    out.push_str(&sql[cut.end..]);
    out
}

/// Everything worth trying on a statement, largest first.
///
/// Largest first because the loop restarts from the top after every step it keeps, so the order of
/// this list is the order the reduction happens in. A run that finds the whole `WHERE` clause is
/// removable on its first candidate does in one step what dropping one conjunct at a time does in
/// six, and each of those steps is two engine runs.
fn cuts(sql: &str) -> Vec<Cut> {
    let tokens = scan(sql);
    let mut out = Vec::new();
    clauses(sql, &tokens, &mut out);
    groups(&tokens, &mut out);
    runs(&tokens, &mut out);
    separated(sql, &tokens, &mut out);
    literals(sql, &tokens, &mut out);
    out
}

/// The clause keywords a top level cut may start at.
///
/// `SELECT` is not one of them, because a statement with its select list removed is not a smaller
/// statement, it is not a statement. The set operators are, because the halves of a union are the
/// largest thing in a query that can be dropped whole.
const CLAUSES: [&str; 12] = [
    "from",
    "where",
    "group",
    "having",
    "order",
    "limit",
    "offset",
    "qualify",
    "window",
    "union",
    "except",
    "intersect",
];

/// Drop one whole clause, and everything from one clause to the end of the statement.
///
/// Both, because one step that takes `GROUP BY`, `HAVING`, `ORDER BY` and `LIMIT` off together is
/// four steps of the first kind, and every step of either kind is two engine runs. The tail cut is
/// the one that turns a generated forty line query into something readable in one move.
fn clauses(sql: &str, tokens: &[Token], out: &mut Vec<Cut>) {
    let at: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.depth == 0 && clause(sql, token))
        .map(|(n, _)| n)
        .collect();
    for (which, start) in at.iter().enumerate() {
        if which + 1 < at.len() {
            out.push(Cut::out(tokens[*start].at, sql.len(), "dropped a clause and the ones after"));
        }
        let end = at.get(which + 1).map_or(sql.len(), |next| tokens[*next].at);
        out.push(Cut::out(tokens[*start].at, end, "dropped a clause"));
    }
}

/// Whether a token is a clause keyword.
fn clause(sql: &str, token: &Token) -> bool {
    token.kind == Kind::Word && CLAUSES.contains(&text(sql, token).to_ascii_lowercase().as_str())
}

/// Replace a parenthesised group by a constant, and unwrap one that has no need of its brackets.
///
/// The constant is what turns a correlated subquery into a number without anybody knowing what the
/// subquery was for, which is the move that shrinks a generated query fastest.
fn groups(tokens: &[Token], out: &mut Vec<Cut>) {
    for (n, open) in tokens.iter().enumerate() {
        if open.kind != Kind::Open {
            continue;
        }
        let Some(close) = tokens[n + 1..]
            .iter()
            .find(|token| token.kind == Kind::Close && token.depth == open.depth)
        else {
            continue;
        };
        out.push(Cut::to(open.at, close.end, "1", "replaced a bracketed group by a constant"));
        out.push(Cut::out(close.at, close.end, "took a group's brackets off"));
        out.push(Cut::out(open.at, open.end, "took a group's brackets off"));
    }
}

/// Delete a run of tokens, halving the length until it is one.
///
/// This is the part that guarantees progress. Everything else in here is a shortcut to a shape a
/// person would recognise, and a shortcut that does not apply leaves the statement where it was.
fn runs(tokens: &[Token], out: &mut Vec<Cut>) {
    let mut width = tokens.len() / 2;
    while width >= 1 {
        for start in 0..tokens.len().saturating_sub(width - 1) {
            let Some(last) = tokens.get(start + width - 1) else { continue };
            out.push(Cut::out(tokens[start].at, last.end, "deleted a run of tokens"));
        }
        width /= 2;
    }
}

/// The words an item of a list can never begin with, on top of the clause keywords.
///
/// The walk to the far end of an item has to stop somewhere. It stops at a bracket the item is
/// inside, at the separator on the other side of it, and at one of these, which are the words that
/// introduce a list rather than sit in one. Without `select` in here the first item of a select list
/// takes the `SELECT` with it when it goes and `SELECT a, b` reduces to `b`, which parses and is not
/// a smaller version of the statement. Without `by`, `ORDER BY a, b` reduces to `ORDER b`. Anything
/// else missing from this list produces text that does not parse and is thrown away before either
/// engine is asked about it, which costs nothing.
const INTRODUCES: [&str; 6] = ["select", "by", "distinct", "values", "set", "using"];

/// Drop one item of a comma separated list, or one side of an `AND` or an `OR`.
///
/// The same shape twice. A separator has an item on each side of it, and either side can go as long
/// as the separator goes with it.
fn separated(sql: &str, tokens: &[Token], out: &mut Vec<Cut>) {
    let boundary = |n: usize| {
        let token = &tokens[n];
        clause(sql, token)
            || (token.kind == Kind::Word && INTRODUCES.contains(&word(sql, token).as_str()))
    };
    let separator = |n: usize| {
        let token = &tokens[n];
        token.kind == Kind::Comma
            || boundary(n)
            || (token.kind == Kind::Word && matches!(word(sql, token).as_str(), "and" | "or"))
    };
    for n in 0..tokens.len() {
        if !separator(n) || boundary(n) {
            continue;
        }
        let what = if tokens[n].kind == Kind::Comma {
            "dropped an item from a list"
        } else {
            "dropped one side of an and or an or"
        };
        let (left, right) = bounds(tokens, n, &separator);
        if left < n {
            out.push(Cut::out(tokens[left].at, tokens[n].end, what));
        }
        if right > n {
            out.push(Cut::out(tokens[n].at, tokens[right].end, what));
        }
    }
}

/// The first token of the item before a separator and the last token of the item after it.
///
/// Bounded by the separators on either side and by the brackets the separator sits inside, so an
/// item never reaches out of its own list into the one around it.
fn bounds(tokens: &[Token], at: usize, separator: &dyn Fn(usize) -> bool) -> (usize, usize) {
    let depth = tokens[at].depth;
    let mut left = at;
    while left > 0 {
        let before = left - 1;
        if tokens[before].depth < depth || (tokens[before].depth == depth && separator(before)) {
            break;
        }
        left = before;
    }
    let mut right = at;
    while right + 1 < tokens.len() {
        let after = right + 1;
        if tokens[after].depth < depth || (tokens[after].depth == depth && separator(after)) {
            break;
        }
        right = after;
    }
    (left, right)
}

/// Make a literal as small as its type allows.
///
/// A thousand character string and an empty one reach the same code path nearly always, and when
/// they do not the reducer finds out by the candidate failing to keep the difference alive.
fn literals(sql: &str, tokens: &[Token], out: &mut Vec<Cut>) {
    for token in tokens {
        match token.kind {
            Kind::Text if token.end - token.at > 2 && sql.as_bytes()[token.at] == b'\'' => {
                out.push(Cut::to(token.at, token.end, "''", "emptied a string"));
            }
            Kind::Number if text(sql, token) != "0" => {
                out.push(Cut::to(token.at, token.end, "0", "shrank a number"));
            }
            _ => {}
        }
    }
}

/// One piece of a statement, as far as the reducer needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Token {
    at: usize,
    end: usize,
    depth: usize,
    kind: Kind,
}

/// What a token is, which is only ever asked to decide where a cut may land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A keyword or an identifier. Nothing here tells the two apart.
    Word,
    /// A number.
    Number,
    /// A quoted string or a quoted identifier, in one piece.
    Text,
    /// An opening bracket of any of the three kinds.
    Open,
    /// A closing one.
    Close,
    /// A comma.
    Comma,
    /// A run of operator characters, or one character that is none of the above.
    Symbol,
}

/// The text of a token.
fn text<'a>(sql: &'a str, token: &Token) -> &'a str {
    &sql[token.at..token.end]
}

/// The text of a token, folded for comparison against a keyword.
fn word(sql: &str, token: &Token) -> String {
    text(sql, token).to_ascii_lowercase()
}

/// The characters that run together into one operator.
///
/// Together, because `::` and `<=` and `||` are one thing each and a reducer that deletes half of
/// one produces a candidate that cannot parse, which costs a slot in the budget for nothing.
const OPERATOR: [u8; 15] = *b"+-*/%<>=!~^|&:.";

/// Break a statement into tokens, with the bracket depth of each one.
///
/// Comments are not tokens, so nothing is ever cut out of the middle of one, and the tidying pass at
/// the end drops them along with the whitespace they sit in. A reduced statement is a reproduction
/// rather than a piece of somebody's file, and a comment on it is a comment about the query it came
/// from.
fn scan(sql: &str) -> Vec<Token> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    let mut depth = 0usize;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte == b'-' && bytes.get(at + 1) == Some(&b'-') {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(at + 1) == Some(&b'*') {
            at += 2;
            while at < bytes.len() && !(bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/')) {
                at += 1;
            }
            at = bytes.len().min(at + 2);
            continue;
        }
        let start = at;
        let kind = match byte {
            b'\'' | b'"' | b'`' => {
                at += 1;
                while at < bytes.len() {
                    if bytes[at] != byte {
                        at += 1;
                        continue;
                    }
                    // A doubled quote is one quote inside the string rather than the end of it.
                    if bytes.get(at + 1) == Some(&byte) {
                        at += 2;
                        continue;
                    }
                    at += 1;
                    break;
                }
                Kind::Text
            }
            b'(' | b'[' | b'{' => {
                at += 1;
                Kind::Open
            }
            b')' | b']' | b'}' => {
                at += 1;
                Kind::Close
            }
            b',' => {
                at += 1;
                Kind::Comma
            }
            b'0'..=b'9' => {
                while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'.') {
                    at += 1;
                }
                Kind::Number
            }
            _ if name(byte) => {
                while at < bytes.len() && name(bytes[at]) {
                    at += 1;
                }
                Kind::Word
            }
            _ if OPERATOR.contains(&byte) => {
                while at < bytes.len() && OPERATOR.contains(&bytes[at]) {
                    at += 1;
                }
                Kind::Symbol
            }
            _ => {
                at += 1;
                Kind::Symbol
            }
        };
        // An opening bracket belongs to the level it was written at and its contents belong to the
        // one inside it, and a closing bracket belongs to the same level as its opening one. Without
        // that, a comma in a function call reads as a comma in the select list around it.
        let here = match kind {
            Kind::Open => {
                depth += 1;
                depth - 1
            }
            Kind::Close => {
                depth = depth.saturating_sub(1);
                depth
            }
            _ => depth,
        };
        out.push(Token { at: start, end: at, depth: here, kind });
    }
    out
}

/// Whether a byte can be part of an unquoted name.
///
/// Anything above ASCII counts, because DuckDB takes unicode identifiers and this is not the place
/// to decide which ones.
fn name(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// Close up the gaps the cuts left, without respelling anything that is still there.
///
/// One space wherever there was any space, and nothing where there was none, so `t.x` stays `t.x`
/// and a string keeps every character between its quotes. Rebuilding the statement from the tokens
/// with a rule about where spaces go would be shorter and would quietly rewrite the query.
///
/// The one exception is a space against a bracket or in front of a comma. Dropping the first item of
/// a list leaves `f( b)` behind, which is the reduced statement somebody has to paste into a bug
/// report, and none of those three characters can run into what is next to it, so taking the space
/// out cannot change where a token ends.
fn tidy(sql: &str) -> String {
    let tokens = scan(sql);
    let mut out = String::with_capacity(sql.len());
    let mut last: Option<&Token> = None;
    for token in &tokens {
        let against_a_bracket = last.is_some_and(|before| before.kind == Kind::Open)
            || matches!(token.kind, Kind::Close | Kind::Comma);
        if last.is_some_and(|before| before.end < token.at) && !against_a_bracket {
            out.push(' ');
        }
        out.push_str(text(sql, token));
        last = Some(token);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{BUDGET, Kind, cuts, scan, shrink, tidy};
    use crate::engine::HarnessError;

    fn kinds(sql: &str) -> Vec<(Kind, &str)> {
        scan(sql).into_iter().map(|token| (token.kind, &sql[token.at..token.end])).collect()
    }

    #[test]
    fn an_operator_written_out_of_two_characters_is_one_token() {
        assert_eq!(
            kinds("1::INT"),
            [(Kind::Number, "1"), (Kind::Symbol, "::"), (Kind::Word, "INT")]
        );
        assert_eq!(kinds("a<=b")[1], (Kind::Symbol, "<="));
        assert_eq!(kinds("a||b")[1], (Kind::Symbol, "||"));
    }

    #[test]
    fn a_string_is_one_token_whatever_is_inside_it() {
        assert_eq!(kinds("'a, b (c'"), [(Kind::Text, "'a, b (c'")]);
        assert_eq!(kinds("'it''s'"), [(Kind::Text, "'it''s'")]);
        assert_eq!(kinds("\"a b\""), [(Kind::Text, "\"a b\"")]);
    }

    #[test]
    fn a_comment_is_not_a_token_and_does_not_survive_tidying() {
        assert_eq!(
            kinds("1 -- two\n+ 3"),
            [(Kind::Number, "1"), (Kind::Symbol, "+"), (Kind::Number, "3")]
        );
        assert_eq!(tidy("SELECT /* why */ 1"), "SELECT 1");
    }

    #[test]
    fn the_depth_of_a_token_is_the_brackets_it_is_inside() {
        let sql = "SELECT f(a, b), c";
        let depths: Vec<usize> = scan(sql).into_iter().map(|token| token.depth).collect();
        //          SELECT f  (  a  ,  b  )  ,  c
        assert_eq!(depths, [0, 0, 0, 1, 1, 1, 0, 0, 0]);
    }

    #[test]
    fn tidying_closes_a_gap_up_without_making_one_that_was_not_there() {
        assert_eq!(tidy("SELECT   x    FROM  t"), "SELECT x FROM t");
        assert_eq!(tidy("SELECT t.x FROM t"), "SELECT t.x FROM t");
        assert_eq!(tidy("SELECT 'a  b'"), "SELECT 'a  b'");
        assert_eq!(tidy("SELECT f(a, b)"), "SELECT f(a, b)");
        // And the space a cut leaves against a bracket, which is the only one it takes out.
        assert_eq!(tidy("SELECT f( b)"), "SELECT f(b)");
        assert_eq!(tidy("SELECT f(a )"), "SELECT f(a)");
        assert_eq!(tidy("SELECT a , b"), "SELECT a, b");
    }

    #[test]
    fn an_item_of_an_order_by_goes_without_taking_the_by_with_it() {
        let sql = "SELECT x FROM t ORDER BY a, b";
        let mut cuts = Vec::new();
        super::separated(sql, &scan(sql), &mut cuts);
        let made: Vec<String> =
            cuts.iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT x FROM t ORDER BY b".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT x FROM t ORDER BY a".to_owned()), "{made:#?}");
        for one in &made {
            assert!(rudb::parses(one), "{one} came out of the list generator and is not SQL");
        }
    }

    #[test]
    fn a_whole_clause_is_one_of_the_things_it_offers_to_take_out() {
        let sql = "SELECT x FROM t WHERE a = 1 ORDER BY x";
        let made: Vec<String> =
            cuts(sql).iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT x FROM t ORDER BY x".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT x FROM t WHERE a = 1".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT x WHERE a = 1 ORDER BY x".to_owned()), "{made:#?}");
        // And the tail, which is the three of them in one step rather than in three.
        assert!(made.contains(&"SELECT x".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT x FROM t".to_owned()), "{made:#?}");
    }

    #[test]
    fn an_item_of_a_list_goes_without_taking_the_list_with_it() {
        let sql = "SELECT a, b, c FROM t";
        let made: Vec<String> =
            cuts(sql).iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT a, c FROM t".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT a, b FROM t".to_owned()), "{made:#?}");
    }

    #[test]
    fn a_conjunct_goes_without_taking_the_predicate_with_it() {
        let sql = "SELECT x FROM t WHERE a = 1 AND b = 2";
        let made: Vec<String> =
            cuts(sql).iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT x FROM t WHERE a = 1".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT x FROM t WHERE b = 2".to_owned()), "{made:#?}");
    }

    #[test]
    fn a_subquery_can_become_a_constant() {
        let sql = "SELECT (SELECT max(y) FROM u) FROM t";
        let made: Vec<String> =
            cuts(sql).iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT 1 FROM t".to_owned()), "{made:#?}");
    }

    #[test]
    fn a_comma_inside_a_call_is_not_a_comma_in_the_list_around_it() {
        let sql = "SELECT f(a, b), c FROM t";
        // This one asks the list generator on its own rather than everything at once. Deleting a
        // run of tokens produces text that reaches out of the brackets all the time, on purpose,
        // and the parser throws it away for nothing. What is being checked here is that the
        // generator which is supposed to know where a list ends does know.
        let mut cuts = Vec::new();
        super::separated(sql, &scan(sql), &mut cuts);
        let made: Vec<String> =
            cuts.iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT f(a), c FROM t".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT f(b), c FROM t".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT f(a, b) FROM t".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT c FROM t".to_owned()), "{made:#?}");
        for one in &made {
            assert!(rudb::parses(one), "{one} came out of the list generator and is not SQL");
        }
    }

    #[test]
    fn a_long_string_and_a_large_number_can_both_get_smaller() {
        let sql = "SELECT length('abcdefgh') + 12345";
        let made: Vec<String> =
            cuts(sql).iter().map(|cut| super::apply(sql, cut)).map(|one| tidy(&one)).collect();
        assert!(made.contains(&"SELECT length('') + 12345".to_owned()), "{made:#?}");
        assert!(made.contains(&"SELECT length('abcdefgh') + 0".to_owned()), "{made:#?}");
    }

    /// A reduction whose question is whether the statement is still a select around one call.
    ///
    /// It stands in for two engines disagreeing, and it is the same shape: something about the
    /// statement is true, everything else is negotiable, and the reducer has to find the smallest
    /// statement the thing is still true of. It asks for the call whole because a real difference is
    /// about a call and not about a name that happens to appear, and it asks for the select because
    /// rudb takes a bare expression as a statement, so without it the smallest answer to every
    /// question here is a fragment nobody would file.
    fn about(call: &'static str) -> impl FnMut(&str) -> Result<bool, HarnessError> {
        move |sql: &str| Ok(sql.starts_with("SELECT") && sql.contains(call))
    }

    #[test]
    fn it_shrinks_a_statement_down_to_the_part_that_still_fails() {
        let sql = "SELECT a, b, badcall(c) FROM t WHERE a = 1 AND b = 2 ORDER BY a LIMIT 10";
        let out =
            shrink(sql, BUDGET, &mut about("badcall(c)")).expect("the question is answerable");
        assert_eq!(out.sql, "SELECT badcall(c)", "{out}");
        assert!(out.before > out.after, "{out}");
        assert!(!out.gave_up, "{out}");
        assert!(!out.steps.is_empty(), "{out}");
    }

    #[test]
    fn a_statement_that_is_already_minimal_is_left_where_it_is() {
        let out =
            shrink("SELECT badcall(1)", BUDGET, &mut about("badcall(1)")).expect("answerable");
        assert_eq!(out.sql, "SELECT badcall(1)");
        assert!(out.steps.is_empty(), "{out}");
    }

    #[test]
    fn a_trailing_semicolon_comes_off_before_anything_else_does() {
        let out =
            shrink("SELECT badcall(1);", BUDGET, &mut about("badcall(1)")).expect("answerable");
        assert_eq!(out.sql, "SELECT badcall(1)");
    }

    #[test]
    fn it_never_puts_up_a_candidate_the_grammar_would_not_take() {
        let sql = "SELECT a, b FROM t WHERE a = 1 AND b = 2";
        let mut seen = Vec::new();
        let out = shrink(sql, BUDGET, &mut |candidate: &str| {
            seen.push(candidate.to_owned());
            Ok(candidate.contains('b'))
        })
        .expect("answerable");
        assert!(!seen.is_empty());
        for candidate in &seen {
            assert!(rudb::parses(candidate), "{candidate} is not SQL and it was asked about");
        }
        assert!(rudb::parses(&out.sql), "{out}");
    }

    #[test]
    fn a_statement_the_grammar_rejects_is_still_reduced() {
        // The candidates cannot be filtered through a parser that rejects the statement they came
        // from, and this is the failure the harness was built to find first, so it has to work.
        let sql = "SELECT a, b BADKEYWORD c, d FROM t";
        assert!(!rudb::parses(sql), "this test needs a statement rudb does not take");
        let out = shrink(sql, BUDGET, &mut about("BADKEYWORD")).expect("answerable");
        assert!(out.sql.contains("BADKEYWORD"), "{out}");
        assert!(out.after < out.before, "{out}");
    }

    #[test]
    fn a_reduction_that_runs_out_of_candidates_says_so_rather_than_claiming_a_minimum() {
        let sql = "SELECT a, b, c, badcall(d), e, f FROM t WHERE a = 1 AND b = 2 AND c = 3";
        let out = shrink(sql, 3, &mut about("badcall(d)")).expect("answerable");
        assert!(out.gave_up, "{out}");
        assert!(out.tried <= 4, "the budget is a budget, it tried {}", out.tried);
        assert!(out.to_string().contains("ran out of candidates"), "{out}");
    }

    #[test]
    fn the_question_failing_ends_the_reduction_rather_than_being_read_as_a_no() {
        let out = shrink("SELECT a, b FROM t", BUDGET, &mut |_: &str| {
            Err(HarnessError::new("the engine is gone".to_owned()))
        });
        assert!(out.is_err(), "an engine that cannot answer is not an engine that said no");
    }
}
