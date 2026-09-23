//! The `<REGEX>:` and `<!REGEX>:` expectations the corpus writes, matched the way DuckDB's runner
//! matches them.
//!
//! Upstream hands the text after the prefix to RE2 with `dot_nl` on and asks for a full match, so
//! `.` crosses newlines and the pattern has to cover the whole value, not find itself somewhere in
//! it. `<!REGEX>:` is the same test with the answer turned round. That is `MatchesRegex` in
//! `result_helper.cpp`, and it is used both for a cell of a result and for the message of an error.
//!
//! This is a small backtracking matcher rather than the `regex` crate, because the manifest keeps
//! this repository to the one dependency on rudb. It reads the part of the syntax the corpus uses,
//! which was counted over the whole of it at the pin: literals, `.`, the `\d \s \w` classes and
//! their capitals, bracket classes with ranges and negation, groups with alternation, the `* + ?`
//! and `{n,m}` counts in their greedy and lazy forms, `^`, `$`, and the `(?s)` and `(?i)` flags. A
//! `{` that does not start a count is a literal, which is how RE2 reads one. Anything else is
//! refused when the pattern is read, so a pattern this cannot follow is reported as that and never
//! quietly passes or fails.

/// A pattern that has been read and can be matched.
#[derive(Debug, Clone)]
pub struct Pattern {
    nodes: Vec<Node>,
    fold: bool,
}

/// What an expected value asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect<'a> {
    /// The text is compared the ordinary way.
    Plain(&'a str),
    /// The value has to match this pattern.
    Match(&'a str),
    /// The value must not match this pattern.
    Refuse(&'a str),
}

impl<'a> Expect<'a> {
    /// Sort an expected value by its prefix.
    #[must_use]
    pub fn of(wanted: &'a str) -> Self {
        if let Some(rest) = wanted.strip_prefix("<REGEX>:") {
            Self::Match(rest)
        } else if let Some(rest) = wanted.strip_prefix("<!REGEX>:") {
            Self::Refuse(rest)
        } else {
            Self::Plain(wanted)
        }
    }
}

/// Whether a value satisfies a `<REGEX>:` or `<!REGEX>:` expectation, or `None` when the expected
/// text is plain and the caller should compare it its own way.
///
/// # Errors
///
/// When the pattern uses syntax this matcher does not read, with a sentence saying what.
pub fn satisfies(wanted: &str, got: &str) -> Option<Result<bool, String>> {
    match Expect::of(wanted) {
        Expect::Plain(_) => None,
        Expect::Match(source) => Some(Pattern::new(source).map(|p| p.matches(got))),
        Expect::Refuse(source) => Some(Pattern::new(source).map(|p| !p.matches(got))),
    }
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Class),
    Group(Vec<Vec<Node>>),
    Repeat { node: Box<Node>, min: usize, max: Option<usize>, greedy: bool },
    Start,
    End,
}

#[derive(Debug, Clone)]
struct Class {
    negated: bool,
    ranges: Vec<(char, char)>,
    named: Vec<(char, bool)>,
}

impl Class {
    fn has(&self, c: char, fold: bool) -> bool {
        let hit = |c: char| {
            self.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi)
                || self.named.iter().any(|&(name, negated)| named(name, c) != negated)
        };
        let found =
            hit(c) || (fold && (hit(c.to_ascii_lowercase()) || hit(c.to_ascii_uppercase())));
        found != self.negated
    }
}

/// Whether a character is in one of the `\d \s \w` classes, named by its lower case letter.
fn named(name: char, c: char) -> bool {
    match name {
        'd' => c.is_ascii_digit(),
        's' => matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'),
        _ => c.is_ascii_alphanumeric() || c == '_',
    }
}

struct Reader<'a> {
    chars: Vec<char>,
    at: usize,
    fold: bool,
    source: &'a str,
}

impl Pattern {
    /// Read a pattern.
    ///
    /// # Errors
    ///
    /// When the pattern uses syntax this matcher does not read, or does not close a group or a
    /// class it opened.
    pub fn new(source: &str) -> Result<Self, String> {
        let mut reader = Reader { chars: source.chars().collect(), at: 0, fold: false, source };
        let nodes = reader.alternatives()?;
        if reader.at != reader.chars.len() {
            return Err(reader.refuse("an unmatched `)`"));
        }
        let nodes = match nodes.len() {
            1 => nodes.into_iter().next().unwrap_or_default(),
            _ => vec![Node::Group(nodes)],
        };
        Ok(Self { nodes, fold: reader.fold })
    }

    /// Whether the pattern covers the whole of this text.
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let text: Vec<char> = text.chars().collect();
        let run = Run { text: &text, fold: self.fold };
        run.seq(&self.nodes, 0, &mut |end| end == text.len())
    }
}

impl Reader<'_> {
    fn refuse(&self, what: &str) -> String {
        format!("the pattern {:?} has {what} at character {}", self.source, self.at)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn alternatives(&mut self) -> Result<Vec<Vec<Node>>, String> {
        let mut all = vec![self.sequence()?];
        while self.peek() == Some('|') {
            self.at += 1;
            all.push(self.sequence()?);
        }
        Ok(all)
    }

    fn sequence(&mut self) -> Result<Vec<Node>, String> {
        let mut nodes = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let Some(atom) = self.atom()? else { continue };
            let node = self.counted(atom)?;
            nodes.push(node);
        }
        Ok(nodes)
    }

    /// One thing to match, or `None` for a flag group that only changes how the rest is read.
    fn atom(&mut self) -> Result<Option<Node>, String> {
        let Some(c) = self.peek() else { return Err(self.refuse("nothing")) };
        self.at += 1;
        Ok(Some(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '[' => Node::Class(self.class()?),
            '\\' => self.escape()?,
            '(' => {
                if self.peek() == Some('?') {
                    self.at += 1;
                    if self.peek() == Some(':') {
                        self.at += 1;
                    } else {
                        while let Some(flag) = self.peek() {
                            self.at += 1;
                            match flag {
                                // Upstream turns `dot_nl` on for every pattern, so this says
                                // nothing new.
                                's' => {}
                                'i' => self.fold = true,
                                ')' => return Ok(None),
                                _ => return Err(self.refuse("a flag this does not read")),
                            }
                        }
                        return Err(self.refuse("a flag group that does not close"));
                    }
                }
                let inner = self.alternatives()?;
                if self.peek() != Some(')') {
                    return Err(self.refuse("a group that does not close"));
                }
                self.at += 1;
                Node::Group(inner)
            }
            '*' | '+' | '?' => return Err(self.refuse("a count with nothing before it")),
            c => Node::Char(c),
        }))
    }

    fn escape(&mut self) -> Result<Node, String> {
        let Some(c) = self.peek() else { return Err(self.refuse("a `\\` at the end")) };
        self.at += 1;
        Ok(match c {
            'd' | 's' | 'w' | 'D' | 'S' | 'W' => Node::Class(Class {
                negated: false,
                ranges: Vec::new(),
                named: vec![(c.to_ascii_lowercase(), c.is_ascii_uppercase())],
            }),
            'n' => Node::Char('\n'),
            't' => Node::Char('\t'),
            'r' => Node::Char('\r'),
            c if c.is_ascii_alphanumeric() => {
                return Err(self.refuse("an escape this does not read"));
            }
            c => Node::Char(c),
        })
    }

    fn class(&mut self) -> Result<Class, String> {
        let mut class = Class { negated: false, ranges: Vec::new(), named: Vec::new() };
        if self.peek() == Some('^') {
            self.at += 1;
            class.negated = true;
        }
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.refuse("a class that does not close"));
            };
            self.at += 1;
            if c == ']' && !first {
                return Ok(class);
            }
            first = false;
            let lo = if c == '\\' {
                match self.escape()? {
                    Node::Char(c) => c,
                    Node::Class(inner) => {
                        class.named.extend(inner.named);
                        continue;
                    }
                    _ => return Err(self.refuse("an escape this does not read")),
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.at + 1).is_some_and(|&c| c != ']') {
                self.at += 1;
                let Some(mut hi) = self.peek() else {
                    return Err(self.refuse("a class that does not close"));
                };
                self.at += 1;
                if hi == '\\' {
                    let Node::Char(c) = self.escape()? else {
                        return Err(self.refuse("a range that ends in a class"));
                    };
                    hi = c;
                }
                class.ranges.push((lo, hi));
            } else {
                class.ranges.push((lo, lo));
            }
        }
    }

    fn counted(&mut self, node: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => match self.bounds() {
                Some((min, max, len)) => {
                    self.at += len - 1;
                    (min, max)
                }
                None => return Ok(node),
            },
            _ => return Ok(node),
        };
        self.at += 1;
        let greedy = if self.peek() == Some('?') {
            self.at += 1;
            false
        } else {
            true
        };
        if matches!(self.peek(), Some('*' | '+')) {
            return Err(self.refuse("a possessive count"));
        }
        Ok(Node::Repeat { node: Box::new(node), min, max, greedy })
    }

    /// `{n}`, `{n,}` or `{n,m}` at the reader, with how many characters it takes, or `None` when
    /// the `{` is a literal.
    fn bounds(&self) -> Option<(usize, Option<usize>, usize)> {
        let rest: String = self.chars[self.at..].iter().take(32).collect();
        let close = rest.find('}')?;
        let inside = &rest[1..close];
        let number = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        };
        let (min, max) = match inside.split_once(',') {
            None => {
                let n = number(inside)?;
                (n, Some(n))
            }
            Some((lo, "")) => (number(lo)?, None),
            Some((lo, hi)) => (number(lo)?, Some(number(hi)?)),
        };
        Some((min, max, rest[..=close].chars().count()))
    }
}

struct Run<'a> {
    text: &'a [char],
    fold: bool,
}

impl Run<'_> {
    fn seq(&self, nodes: &[Node], at: usize, next: &mut dyn FnMut(usize) -> bool) -> bool {
        let Some((first, rest)) = nodes.split_first() else { return next(at) };
        self.one(first, at, &mut |after| self.seq(rest, after, next))
    }

    /// Whether one character matches a node that always takes exactly one.
    fn single(&self, node: &Node, at: usize) -> Option<bool> {
        let c = self.text.get(at).copied();
        Some(match node {
            Node::Any => c.is_some(),
            Node::Char(want) => {
                c.is_some_and(|c| c == *want || (self.fold && c.eq_ignore_ascii_case(want)))
            }
            Node::Class(class) => c.is_some_and(|c| class.has(c, self.fold)),
            _ => return None,
        })
    }

    fn one(&self, node: &Node, at: usize, next: &mut dyn FnMut(usize) -> bool) -> bool {
        if let Some(hit) = self.single(node, at) {
            return hit && next(at + 1);
        }
        match node {
            Node::Group(alternatives) => alternatives.iter().any(|alt| self.seq(alt, at, next)),
            Node::Repeat { node, min, max, greedy } => {
                if self.single(node, at).is_some() {
                    return self.run(node, (*min, *max, *greedy), at, next);
                }
                self.repeat(node, (*min, *max, *greedy), at, 0, next)
            }
            Node::Start => at == 0 && next(at),
            Node::End => at == self.text.len() && next(at),
            Node::Any | Node::Char(_) | Node::Class(_) => unreachable!("taken by single above"),
        }
    }

    /// A count of a node that takes one character, tried without recursing per character, so a
    /// `.*` over a long plan does not go as deep as the plan is long.
    fn run(
        &self,
        node: &Node,
        (min, max, greedy): (usize, Option<usize>, bool),
        at: usize,
        next: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let limit = max.unwrap_or(usize::MAX);
        let mut most = 0;
        while most < limit && self.single(node, at + most) == Some(true) {
            most += 1;
        }
        if most < min {
            return false;
        }
        if greedy {
            (min..=most).rev().any(|n| next(at + n))
        } else {
            (min..=most).any(|n| next(at + n))
        }
    }

    fn repeat(
        &self,
        node: &Node,
        (min, max, greedy): (usize, Option<usize>, bool),
        at: usize,
        count: usize,
        next: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let bounds = (min, max, greedy);
        if count < min {
            return self
                .one(node, at, &mut |after| self.repeat(node, bounds, after, count + 1, next));
        }
        let more = max.is_none_or(|max| count < max);
        let again = |next: &mut dyn FnMut(usize) -> bool| {
            more && self.one(node, at, &mut |after| {
                after != at && self.repeat(node, bounds, after, count + 1, next)
            })
        };
        // A greedy count tries one more before it tries stopping, and a lazy one the other way.
        if greedy && again(next) {
            return true;
        }
        next(at) || (!greedy && again(next))
    }
}

#[cfg(test)]
mod tests {
    use super::{Pattern, satisfies};

    fn full(pattern: &str, text: &str) -> bool {
        Pattern::new(pattern).expect("reads").matches(text)
    }

    #[test]
    fn a_pattern_has_to_cover_the_whole_value() {
        assert!(full("Conversion Error:.*out of range.*", "Conversion Error: 3 is out of range"));
        assert!(!full("out of range", "Conversion Error: 3 is out of range"));
        assert!(full("\\d+", "123"));
        assert!(!full("\\d+", "12a"));
    }

    #[test]
    fn a_dot_crosses_a_newline_the_way_dot_nl_has_it() {
        assert!(full(".*IN.*PRF.*", "a\nIN\nb PRF c\n"));
        assert!(full("(?s).*IN.*", "x\nIN\n"));
        assert!(full("[\\s\\S]*EMPTY_RESULT[\\s\\S]*", "┌─\n│EMPTY_RESULT│\n└─"));
    }

    #[test]
    fn classes_groups_and_counts_read_the_way_re2_reads_them() {
        assert!(full("[a-zA-Z0-9-]{36}.parquet", "0123456789abcdef0123456789abcdef-abc.parquet"));
        assert!(full("{row_group_count=\\d+}", "{row_group_count=7}"));
        assert!(full("(.*Parser Error.*|Binder Error.*bound.*)", "Binder Error: no bound"));
        assert!(full("\"[^\"]*x[^\"]*\"", "\"a x b\""));
        assert!(full("a.*?b", "aXbYb"));
        assert!(full("(ab)+c", "ababc"));
        assert!(!full("(ab){3}c", "ababc"));
        assert!(full("(?i)hello", "HeLLo"));
    }

    #[test]
    fn a_negated_pattern_passes_only_when_it_does_not_match() {
        assert_eq!(satisfies("<!REGEX>:.*Error.*", "fine"), Some(Ok(true)));
        assert_eq!(satisfies("<!REGEX>:.*Error.*", "an Error"), Some(Ok(false)));
        assert_eq!(satisfies("plain", "plain"), None);
    }

    #[test]
    fn a_pattern_this_cannot_follow_is_refused_rather_than_guessed_at() {
        assert!(Pattern::new("\\bword").is_err());
        assert!(Pattern::new("(open").is_err());
        assert!(Pattern::new("[open").is_err());
    }
}
