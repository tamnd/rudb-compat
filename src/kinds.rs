//! Statement coverage, which is the first of the three level two denominators.
//!
//! `spec/sql/duckdb/01-what-compatible-means.md` section 1.1 puts it over the 36 alternatives of the
//! `Statement` rule in the vendored grammar, and section 1.2 says what makes one of them count: a
//! statement kind is supported when every form of it in the corpus either works or fails the way the
//! file said it fails, not when the parser stops refusing it. That definition is the whole reason
//! this is computed out of a corpus run rather than out of a list of statements somebody ticked.
//!
//! # Why the parser sorts the records and not the first word
//!
//! `WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x` begins with the word `WITH` and is an
//! `InsertStatement`. `rudb::statement_kind` asks the matcher which alternative of `Statement`
//! matched, so a record goes in the bucket it belongs in rather than the bucket its first token
//! suggests, and the bucket names are the grammar's own.
//!
//! The grammar doing the sorting is DuckDB's, vendored whole, and that matters more than it looks.
//! It accepts a great deal that rudb cannot run: `MERGE INTO` parses here and there is no AST for
//! it. So a record being sorted into a bucket is a fact about DuckDB's dialect and not about how far
//! rudb has got, which is what keeps the denominator from shrinking to whatever we have already
//! built.
//!
//! # What is not counted
//!
//! A record whose text the vendored grammar does not accept cannot be sorted at all, and those are
//! counted on their own as [`Kinds::unclassified`] rather than dropped. There are two ways to get
//! there and they mean opposite things. Most of them are the corpus doing it on purpose, since a
//! file full of `statement error` records has syntax errors in it by design. The rest are a hole in
//! the vendored grammar or in the tokenizer, which is a bug in us, and the count going up without
//! the corpus changing is how it shows.
//!
//! Skipped records are not counted either way, the same as they are left out of the pass rate. A
//! kind whose every record was behind a `require json` has nothing measured about it and printing it
//! as unsupported would be a number with nothing behind it.

use std::collections::BTreeMap;

use rudb::{statement_kind, statement_kinds};

/// How one statement kind did over the records that are one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Records of this kind that ran and did what the file said.
    pub passed: usize,
    /// Records of this kind that ran and did something else.
    pub failed: usize,
}

impl Tally {
    /// How many records of this kind ran.
    #[must_use]
    pub const fn records(self) -> usize {
        self.passed + self.failed
    }

    /// Whether this kind counts as supported, per section 1.2.
    ///
    /// Every record of it passed and there was at least one. The second half is not a formality: a
    /// kind with no records is not a kind that works, and a rule that counted an empty bucket would
    /// report the whole surface as supported on an empty corpus.
    #[must_use]
    pub const fn supported(self) -> bool {
        self.failed == 0 && self.passed > 0
    }
}

/// What a run saw of the statement surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kinds {
    seen: BTreeMap<String, Tally>,
    /// Records the vendored grammar does not accept, so nothing can say which kind they are.
    pub unclassified: usize,
}

impl Kinds {
    /// Charge one record that ran to whichever kind it is.
    pub fn charge(&mut self, sql: &str, passed: bool) {
        let Some(kind) = statement_kind(sql) else {
            self.unclassified += 1;
            return;
        };
        let tally = self.seen.entry(kind.to_owned()).or_default();
        if passed {
            tally.passed += 1;
        } else {
            tally.failed += 1;
        }
    }

    /// Charge one record that ran, to a kind that has already been decided.
    ///
    /// For the reader on the other side of the pipe in [`crate::isolate`], which is handed a name
    /// rather than the SQL it came from. A name is taken as it arrives rather than checked against
    /// the grammar, because a child built from a different tree than the parent is a broken harness
    /// and quietly dropping its counts would look like a corpus with fewer statements in it.
    pub fn charged(&mut self, kind: &str, passed: usize, failed: usize) {
        let tally = self.seen.entry(kind.to_owned()).or_default();
        tally.passed += passed;
        tally.failed += failed;
    }

    /// Fold another run's counts in, for the per file children.
    pub fn absorb(&mut self, other: &Self) {
        for (kind, tally) in &other.seen {
            self.charged(kind, tally.passed, tally.failed);
        }
        self.unclassified += other.unclassified;
    }

    /// Every kind that has a record, in name order, with what it did.
    pub fn rows(&self) -> impl Iterator<Item = (&str, Tally)> {
        self.seen.iter().map(|(kind, tally)| (kind.as_str(), *tally))
    }

    /// What one kind did, which is nothing at all for a kind with no records.
    #[must_use]
    pub fn tally(&self, kind: &str) -> Tally {
        self.seen.get(kind).copied().unwrap_or_default()
    }

    /// Whether anything was counted, which is false for a run that measured no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty() && self.unclassified == 0
    }

    /// How many records were sorted into a kind.
    #[must_use]
    pub fn records(&self) -> usize {
        self.seen.values().map(|tally| tally.records()).sum()
    }

    /// The whole statement surface, which is the denominator.
    #[must_use]
    pub fn surface() -> Vec<&'static str> {
        statement_kinds()
    }

    /// The kinds that are supported, in the order the grammar has them.
    #[must_use]
    pub fn supported(&self) -> Vec<&'static str> {
        Self::surface().into_iter().filter(|kind| self.tally(kind).supported()).collect()
    }

    /// The kinds with a record that failed, in the order the grammar has them.
    #[must_use]
    pub fn failing(&self) -> Vec<&'static str> {
        Self::surface().into_iter().filter(|kind| self.tally(kind).failed > 0).collect()
    }

    /// The kinds with no record at all, in the order the grammar has them.
    ///
    /// The figure to read beside the coverage number, the way section 11.3 reads the zero weight
    /// names beside function coverage. A kind in here is not failing and is not working, and a
    /// corpus that never writes one is a corpus that cannot say anything about it.
    #[must_use]
    pub fn untouched(&self) -> Vec<&'static str> {
        Self::surface().into_iter().filter(|kind| self.tally(kind).records() == 0).collect()
    }

    /// The share of the whole surface that is supported, between zero and one.
    ///
    /// Over all 36 and not over the ones the corpus reaches, which is what document 13 closes
    /// milestone 5 on. Dividing by the kinds the corpus happens to exercise would be a number that
    /// goes up when the corpus shrinks.
    #[must_use]
    pub fn rate(&self) -> f64 {
        let surface = Self::surface().len();
        if surface == 0 {
            return 0.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "thirty six of them")]
        {
            self.supported().len() as f64 / surface as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Kinds, Tally};

    #[test]
    fn the_surface_is_the_thirty_six_the_grammar_has() {
        let surface = Kinds::surface();
        assert_eq!(surface.len(), 36);
        assert!(surface.contains(&"SelectStatement"));
    }

    #[test]
    fn a_kind_every_record_of_which_passed_is_supported() {
        let mut kinds = Kinds::default();
        kinds.charge("SELECT 1", true);
        kinds.charge("SELECT 2", true);
        assert_eq!(kinds.tally("SelectStatement"), Tally { passed: 2, failed: 0 });
        assert_eq!(kinds.supported(), vec!["SelectStatement"]);
        assert!(kinds.failing().is_empty());
    }

    #[test]
    fn one_failing_record_takes_the_whole_kind_out() {
        // Section 1.2, read strictly. Thirty passes and one failure is a kind that does not work.
        let mut kinds = Kinds::default();
        for _ in 0..30 {
            kinds.charge("SELECT 1", true);
        }
        kinds.charge("SELECT 2", false);
        assert!(kinds.supported().is_empty());
        assert_eq!(kinds.failing(), vec!["SelectStatement"]);
    }

    #[test]
    fn a_kind_with_no_records_is_untouched_rather_than_either_answer() {
        let mut kinds = Kinds::default();
        kinds.charge("SELECT 1", true);
        let untouched = kinds.untouched();
        assert_eq!(untouched.len(), 35);
        assert!(!untouched.contains(&"SelectStatement"));
        assert!(untouched.contains(&"MergeIntoStatement"));
    }

    #[test]
    fn a_record_goes_in_the_bucket_the_parser_says_and_not_the_one_its_first_word_suggests() {
        let mut kinds = Kinds::default();
        kinds.charge("WITH x AS (SELECT 1 AS a) INSERT INTO t SELECT a FROM x", true);
        assert_eq!(kinds.tally("InsertStatement").passed, 1);
        assert_eq!(kinds.tally("SelectStatement").passed, 0);
    }

    #[test]
    fn a_record_the_grammar_does_not_accept_is_counted_apart_rather_than_dropped() {
        let mut kinds = Kinds::default();
        kinds.charge("SELECT FROM WHERE", false);
        assert_eq!(kinds.unclassified, 1);
        assert_eq!(kinds.records(), 0);
        assert!(kinds.supported().is_empty());
    }

    #[test]
    fn the_rate_is_over_the_whole_surface_and_not_over_what_the_corpus_reaches() {
        let mut kinds = Kinds::default();
        kinds.charge("SELECT 1", true);
        // One of thirty six, not one of one.
        assert!((kinds.rate() - 1.0 / 36.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_run_has_nothing_supported_rather_than_everything() {
        let kinds = Kinds::default();
        assert!(kinds.is_empty());
        assert!(kinds.supported().is_empty());
        assert_eq!(kinds.untouched().len(), 36);
        assert!(kinds.rate().abs() < f64::EPSILON);
    }

    #[test]
    fn the_counts_of_two_children_fold_into_one() {
        let mut one = Kinds::default();
        one.charge("SELECT 1", true);
        one.charge("SELECT FROM WHERE", false);
        let mut two = Kinds::default();
        two.charge("SELECT 2", false);
        two.charge("DROP TABLE t", true);
        one.absorb(&two);
        assert_eq!(one.tally("SelectStatement"), Tally { passed: 1, failed: 1 });
        assert_eq!(one.tally("DropStatement"), Tally { passed: 1, failed: 0 });
        assert_eq!(one.unclassified, 1);
        assert_eq!(one.supported(), vec!["DropStatement"]);
    }

    #[test]
    fn the_rows_come_back_in_name_order_so_a_page_written_twice_reads_the_same() {
        let mut kinds = Kinds::default();
        kinds.charge("SELECT 1", true);
        kinds.charge("DROP TABLE t", true);
        kinds.charge("INSERT INTO t VALUES (1)", true);
        let names: Vec<&str> = kinds.rows().map(|(kind, _)| kind).collect();
        assert_eq!(names, vec!["DropStatement", "InsertStatement", "SelectStatement"]);
    }
}
