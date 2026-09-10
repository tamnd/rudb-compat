//! The DuckDB compatibility harness for rudb.
//!
//! The design is `spec/14-rudb-compat.md` in the [rudb repository], and the levels it reports
//! against are `spec/12-duckdb-compat.md` section 12.8. This crate is the part that turns
//! "compatible with DuckDB" from an assertion into a number a machine computed.
//!
//! Nothing here runs a query yet. What exists is the shape of the report, because the shape is
//! what the rest is built to fill in, and because a compatibility percentage with no failure list
//! attached to it is a number nobody should believe.
//!
//! [rudb repository]: https://github.com/tamnd/rudb

#![forbid(unsafe_code)]

/// The four compatibility levels from `spec/12-duckdb-compat.md` section 12.8.
///
/// Four named levels rather than one binary claim, each independently verified, each with its own
/// suite and its own published percentage. A user reads the table and knows what they get, which
/// is a more useful and more honest artifact than the phrase "100 percent compatible".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Reads and writes DuckDB files at full fidelity. The minimum useful claim, and the one that
    /// makes a migration reversible.
    Data,
    /// Data compatible plus the SQL dialect at a published weighted coverage. This is what most
    /// people mean when they say compatible.
    Query,
    /// Query compatible plus the C API, so existing programs and language bindings work
    /// unmodified.
    Api,
    /// API compatible plus extensions loading and the wire protocol, with a per-extension status
    /// table.
    Ecosystem,
}

impl Level {
    /// The number the level is published under, which is how it appears in the report.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Data => 0,
            Self::Query => 1,
            Self::Api => 2,
            Self::Ecosystem => 3,
        }
    }

    /// The short name, as it appears in the published table.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Data => "data compatible",
            Self::Query => "query compatible",
            Self::Api => "API compatible",
            Self::Ecosystem => "ecosystem compatible",
        }
    }

    /// All four, lowest first.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::Data, Self::Query, Self::Api, Self::Ecosystem]
    }
}

/// What a suite says about a level.
///
/// The failure count is not derived from the percentage and the percentage is not derived from the
/// failure count, because the weighting in `spec/10-sql-and-types.md` section 10.7 means a
/// function nobody calls and a function in the top hundred by usage are the same one failure and
/// very different amounts of percentage. Both numbers are carried so that neither can be inferred
/// wrongly from the other.
#[derive(Debug, Clone)]
pub struct Status {
    /// Which level this is about.
    pub level: Level,
    /// The weighted coverage, between zero and one.
    pub coverage: f64,
    /// How many cases failed. Never summarized away, per `spec/14-rudb-compat.md` section 14.1.
    pub failures: usize,
    /// The DuckDB version the comparison ran against. A percentage without this is meaningless,
    /// because the surface moves between releases.
    pub duckdb_version: String,
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn the_levels_are_ordered_and_numbered_the_same_way() {
        let all = Level::all();
        for (i, level) in all.iter().enumerate() {
            assert_eq!(usize::from(level.number()), i);
        }
        assert!(all.windows(2).all(|w| w[0] < w[1]));
    }
}
