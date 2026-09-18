//! Splitting one corpus run across several machines.
//!
//! The run forks a process per file already, so the files are independent and nothing about
//! splitting them is hard. A shard is written `k/n`, takes every file whose position in the sorted
//! list is `k` modulo `n`, and the pieces are folded back together with [`crate::isolate::decode_run`]
//! into the same [`crate::isolate::Isolated`] a whole run produces.
//!
//! Round robin over the sorted list rather than contiguous blocks, which is what `cargo nextest`
//! does with `--partition count:k/n` and for the same reason. The corpus is sorted by path, so a
//! contiguous block is a directory, and `test/sql/copy` and `test/sql/aggregate` are not the same
//! amount of work. Round robin gives every shard a piece of every directory.
//!
//! # What this actually buys, which today is nothing
//!
//! Measured on the gaming machine, the whole corpus is 177 processor seconds of work over 4140
//! files and the run takes 42 seconds on 32 cores. It was 364 seconds over 4106 files and 121
//! seconds on 32 cores when this was written, and the difference is one file. The 125 seconds that
//! `optimizer/table_filters.test` was of the 364 are five now. tamnd/rudb#866 taught the hash join
//! to see through the cast the binder puts around a join key and tamnd/rudb#883 gave it a residual
//! predicate, which between them are the two things every join in that file needed.
//!
//! The tail is still a tail. Timed 32 at a time, the seven slowest files are 157 of the 177, and
//! the two at the top of that are the two left in `corpus/cutoff.txt`:
//! `catalog/table/create_table_as_abort.test` and `overflow/expression_tree_depth.test`, which are
//! 75 and 46 of the 157. Neither of them is slow work the way `table_filters` was. One builds the
//! whole result of a statement it is about to roll back and goes over the memory cap, the other
//! builds a three thousand term expression tree that DuckDB refuses to parse at all. A run cannot
//! finish before its slowest file does, so the floor is still one file rather than the machine, and
//! splitting the files across four machines still does not move it.
//!
//! So this is here because it is fifty lines, not because it speeds anything up now. Section 9.8 of
//! `spec/sql/duckdb/09-the-harness.md` has the rest of the argument, including why two machines do
//! not currently produce the same page.

use std::fmt;

/// Which slice of the corpus a run takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shard {
    at: usize,
    of: usize,
}

impl Default for Shard {
    fn default() -> Self {
        Self::whole()
    }
}

impl Shard {
    /// Every file, which is what a run with no `--shard` takes.
    #[must_use]
    pub const fn whole() -> Self {
        Self { at: 1, of: 1 }
    }

    /// Whether this is every file, so a caller can leave the shard out of what it prints.
    #[must_use]
    pub const fn is_whole(self) -> bool {
        self.of == 1
    }

    /// Which shard of how many, both one based, which is how they are written and read.
    #[must_use]
    pub const fn numbers(self) -> (usize, usize) {
        (self.at, self.of)
    }

    /// Read a `k/n`, one based, both ends.
    ///
    /// One based because that is how a person says it and how every other tool spells it. The error
    /// is a sentence rather than a code, because the only caller prints it and gives up.
    ///
    /// # Errors
    ///
    /// When it is not two numbers with a slash between them, when either is zero, or when the first
    /// is larger than the second, which is a shard that would take nothing at all.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (left, right) = text
            .split_once('/')
            .ok_or_else(|| format!("a shard is written k/n, and {text} has no slash in it"))?;
        let at: usize = left
            .trim()
            .parse()
            .map_err(|_| format!("{left} is not a number, and a shard is written k/n"))?;
        let of: usize = right
            .trim()
            .parse()
            .map_err(|_| format!("{right} is not a number, and a shard is written k/n"))?;
        if of == 0 || at == 0 {
            return Err(format!("a shard is counted from one, so {text} names nothing"));
        }
        if at > of {
            return Err(format!("{text} asks for shard {at} of {of}, which is no files at all"));
        }
        Ok(Self { at, of })
    }

    /// Whether the file at this position in the sorted list belongs to this shard.
    #[must_use]
    pub const fn takes(self, index: usize) -> bool {
        index % self.of == self.at - 1
    }

    /// Keep the files this shard takes and drop the rest, in the order they came.
    #[must_use]
    pub fn keep<T>(self, files: Vec<T>) -> Vec<T> {
        if self.is_whole() {
            return files;
        }
        files.into_iter().enumerate().filter(|(at, _)| self.takes(*at)).map(|(_, f)| f).collect()
    }
}

impl fmt::Display for Shard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.at, self.of)
    }
}

#[cfg(test)]
mod tests {
    use super::Shard;

    #[test]
    fn a_run_with_no_shard_takes_every_file() {
        let whole = Shard::whole();
        assert!(whole.is_whole());
        assert_eq!(whole.keep((0..10).collect()), (0..10).collect::<Vec<i32>>());
    }

    #[test]
    fn the_four_shards_of_four_are_every_file_once_between_them() {
        let files: Vec<usize> = (0..4106).collect();
        let mut seen: Vec<usize> = Vec::new();
        for at in 1..=4 {
            let shard = Shard::parse(&format!("{at}/4")).expect("a shard");
            seen.extend(shard.keep(files.clone()));
        }
        seen.sort_unstable();
        assert_eq!(seen, files);
    }

    #[test]
    fn the_shards_are_within_one_file_of_each_other_in_size() {
        // Round robin, so a corpus that does not divide by the shard count is off by one and no
        // more. A block split would be off by whatever the last block is short by.
        let files: Vec<usize> = (0..4106).collect();
        let sizes: Vec<usize> = (1..=4)
            .map(|at| Shard::parse(&format!("{at}/4")).expect("a shard").keep(files.clone()).len())
            .collect();
        assert_eq!(sizes, vec![1027, 1027, 1026, 1026]);
    }

    #[test]
    fn a_shard_takes_a_piece_of_every_directory_rather_than_one_directory() {
        // The corpus is sorted by path, so neighbours are in the same directory. This is the whole
        // argument for round robin and it is worth a test rather than a comment.
        let shard = Shard::parse("1/4").expect("a shard");
        assert!(shard.takes(0));
        assert!(!shard.takes(1));
        assert!(shard.takes(4));
    }

    #[test]
    fn a_shard_that_is_not_two_numbers_and_a_slash_says_so() {
        assert!(Shard::parse("1").is_err());
        assert!(Shard::parse("a/4").is_err());
        assert!(Shard::parse("1/b").is_err());
    }

    #[test]
    fn a_shard_counted_from_zero_is_refused_rather_than_read_as_the_last_one() {
        // `0/4` is what somebody who has read the source writes, and reading it as shard four would
        // silently run the wrong quarter. `5/4` is a typo that would run nothing.
        assert!(Shard::parse("0/4").is_err());
        assert!(Shard::parse("1/0").is_err());
        assert!(Shard::parse("5/4").is_err());
    }

    #[test]
    fn a_shard_prints_the_way_it_is_written() {
        assert_eq!(Shard::parse("3/8").expect("a shard").to_string(), "3/8");
        assert_eq!(Shard::parse("3/8").expect("a shard").numbers(), (3, 8));
    }
}
