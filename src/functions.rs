//! DuckDB's function table, and the calls a differential run puts to it.
//!
//! `spec/14-rudb-compat.md` section 14.4 and section 10.1 of the DuckDB folder describe the same
//! thing: read `duckdb_functions()` off the pinned binary, and for each overload row generate calls
//! from a per type boundary set, then nulls in every position, then wrong types to capture the
//! error. This module is the reading and the generating. Running the calls against both engines is
//! the other half and it is not here yet.
//!
//! The boundary sets are the only judgement in the whole scheme, which is why they are a table in
//! one place rather than a rule spread over a generator. Minimum, maximum, zero, one and minus one
//! for the integers. Empty, embedded null, the twelve byte storage boundary, long and combining for
//! `VARCHAR`, and the invalid UTF-8 on `BLOB` because DuckDB will not build one in a string. Zero,
//! negative zero, both infinities, NaN and the subnormal boundary for `DOUBLE`. The width and scale
//! extremes for `DECIMAL`. The epoch, the infinities and the leap day for `TIMESTAMP`. Every one of
//! those is a place an engine is wrong while looking right, and none of them is a place a query
//! somebody wrote by hand goes.
//!
//! One thing the half that runs these will have to carry and this half does not. Some of these
//! functions answer differently every time they are called. `random`, `now`, `current_date`,
//! `uuid` and the one argument `age` all read something outside the query, so two engines that both
//! work will still disagree on them and the comparison has to know which ones those are before it
//! reports a difference.
//!
//! A type this table has no boundary set for is not guessed at. The overloads that name one are
//! counted and the type is printed with the count beside it, because a generator that quietly skips
//! a third of the catalog and reports a percentage over the rest is the exact failure section 1.2
//! is about.

use std::collections::BTreeMap;
use std::fmt;

use crate::engine::{Cell, Engine, HarnessError, Outcome, Table};

/// What one row of `duckdb_functions()` is.
///
/// A name can be several of these. `date_part` is thirty of them, and each one is its own pass or
/// fail, because an engine that has two of the thirty has not got `date_part`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overload {
    /// The name it is called by.
    pub name: String,
    /// Scalar, aggregate, table and the rest.
    pub kind: Kind,
    /// What it gives back, as DuckDB spells it.
    pub returns: String,
    /// The parameter types, in order, as DuckDB spells them.
    pub parameters: Vec<String>,
    /// The type of the trailing arguments, for a function that takes any number of them.
    pub varargs: Option<String>,
    /// Whether DuckDB considers it built in rather than something an extension added.
    pub internal: bool,
}

/// Which of DuckDB's function kinds a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// One row in, one row out. The kind the generator can call today.
    Scalar,
    /// Many rows in, one row out.
    Aggregate,
    /// Called over a window frame.
    Window,
    /// Produces rows, so it is called in a FROM rather than in a SELECT list.
    Table,
    /// Text substituted at bind time.
    Macro,
    /// A macro that produces rows.
    TableMacro,
    /// A setting rather than a function.
    Pragma,
}

impl Kind {
    /// Every kind, in the order the inventory prints them.
    pub const ALL: [Self; 7] = [
        Self::Scalar,
        Self::Aggregate,
        Self::Window,
        Self::Table,
        Self::Macro,
        Self::TableMacro,
        Self::Pragma,
    ];

    /// The kind out of the word `duckdb_functions()` uses for it.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        match word {
            "scalar" => Some(Self::Scalar),
            "aggregate" => Some(Self::Aggregate),
            "window" => Some(Self::Window),
            "table" => Some(Self::Table),
            "macro" => Some(Self::Macro),
            "table_macro" => Some(Self::TableMacro),
            "pragma" => Some(Self::Pragma),
            _ => None,
        }
    }

    /// The word `duckdb_functions()` uses for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Aggregate => "aggregate",
            Self::Window => "window",
            Self::Table => "table",
            Self::Macro => "macro",
            Self::TableMacro => "table_macro",
            Self::Pragma => "pragma",
        }
    }
}

/// The query that reads the catalog.
///
/// The lists are joined by DuckDB rather than read as lists here, because a parameter type can have
/// a comma inside it, `MAP(K, V)` and `STRUCT(...)` both do, and a reader here that splits on commas
/// would quietly produce two parameters where the catalog has one. The bar is a character no type
/// name in the catalog contains.
pub const QUERY: &str = "SELECT function_name, function_type, return_type, \
     coalesce(array_to_string(parameter_types, '|'), '') AS parameter_types, \
     coalesce(varargs, '') AS varargs, internal \
     FROM duckdb_functions() ORDER BY function_name, parameter_types";

/// Read the function table off an engine.
///
/// # Errors
///
/// When the query fails, which on a binary that has `duckdb_functions()` means the binary is not
/// the one this harness thinks it is.
pub fn catalog(engine: &mut dyn Engine) -> Result<Vec<Overload>, HarnessError> {
    match engine.run(QUERY)? {
        Outcome::Rows(table) => read(&table),
        Outcome::Error(e) => Err(HarnessError::new(format!("cannot read duckdb_functions(): {e}"))),
    }
}

/// Turn the result of [`QUERY`] into overloads.
///
/// # Errors
///
/// When the table is not the shape the query asks for, which is a changed catalog rather than a bad
/// row, so it stops rather than skipping.
pub fn read(table: &Table) -> Result<Vec<Overload>, HarnessError> {
    if table.width() != 6 {
        return Err(HarnessError::new(format!(
            "duckdb_functions() came back {} columns wide and this reader wants 6",
            table.width()
        )));
    }
    let mut out = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let text = |at: usize| match row.get(at) {
            Some(Cell::Text(t)) => t.as_str(),
            _ => "",
        };
        let word = text(1);
        let Some(kind) = Kind::of(word) else {
            return Err(HarnessError::new(format!(
                "duckdb_functions() has a {word} in it, which this reader has never seen"
            )));
        };
        let varargs = text(4);
        out.push(Overload {
            name: text(0).to_owned(),
            kind,
            returns: text(2).to_owned(),
            parameters: parameters(text(3)),
            varargs: if varargs.is_empty() { None } else { Some(varargs.to_owned()) },
            internal: text(5) == "true",
        });
    }
    Ok(out)
}

/// The parameter types out of the joined list.
fn parameters(joined: &str) -> Vec<String> {
    if joined.is_empty() {
        return Vec::new();
    }
    joined.split('|').map(str::to_owned).collect()
}

/// What the catalog on this pin holds, and how much of it the generator can reach.
///
/// The second half is the point. A count of what exists is a denominator, and a denominator without
/// the part of it nothing can call yet is a denominator that flatters.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    /// Every row.
    pub overloads: usize,
    /// Every distinct name.
    pub names: usize,
    /// Rows and names for each kind.
    pub kinds: Vec<(Kind, usize, usize)>,
    /// Rows the generator can make calls for today.
    pub callable: usize,
    /// Types nothing here has a boundary set for, with how many rows name one, largest first.
    pub unknown: Vec<(String, usize)>,
}

/// Count a catalog.
#[must_use]
pub fn inventory(catalog: &[Overload]) -> Inventory {
    let mut names: BTreeMap<&str, ()> = BTreeMap::new();
    let mut per_kind: BTreeMap<Kind, (usize, BTreeMap<&str, ()>)> = BTreeMap::new();
    let mut unknown: BTreeMap<&str, usize> = BTreeMap::new();
    let mut callable = 0;
    for overload in catalog {
        names.insert(&overload.name, ());
        let row = per_kind.entry(overload.kind).or_default();
        row.0 += 1;
        row.1.insert(&overload.name, ());
        if calls(overload).is_empty() {
            for ty in &overload.parameters {
                if boundaries(ty).is_none() {
                    *unknown.entry(ty).or_default() += 1;
                }
            }
        } else {
            callable += 1;
        }
    }
    let mut unknown: Vec<(String, usize)> =
        unknown.into_iter().map(|(ty, count)| (ty.to_owned(), count)).collect();
    unknown.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Inventory {
        overloads: catalog.len(),
        names: names.len(),
        kinds: Kind::ALL
            .into_iter()
            .filter_map(|kind| per_kind.get(&kind).map(|(rows, names)| (kind, *rows, names.len())))
            .collect(),
        callable,
        unknown,
    }
}

impl fmt::Display for Inventory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} overloads over {} names", self.overloads, self.names)?;
        for (kind, rows, names) in &self.kinds {
            writeln!(f, "    {rows:>6}  {:<12}{names} names", kind.name())?;
        }
        writeln!(f)?;
        writeln!(
            f,
            "{} of the {} rows can be called by the generator today",
            self.callable, self.overloads
        )?;
        if self.unknown.is_empty() {
            return Ok(());
        }
        writeln!(f)?;
        writeln!(f, "types with no boundary set here, and how many rows name one")?;
        for (ty, count) in &self.unknown {
            writeln!(f, "    {count:>6}  {ty}")?;
        }
        Ok(())
    }
}

/// One call the differential run puts to both engines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The SQL, ready to run.
    pub sql: String,
    /// What this call is for, in a few words, so a failure says which case it was.
    pub note: String,
}

/// Every call for one overload.
///
/// Three groups, in the order section 14.4 puts them. One argument at a time over its boundary set
/// with the others at an ordinary value, because varying two at once produces a case nobody can
/// read and finds nothing the two single cases do not. Then a null in every position, separately,
/// because null handling is per argument and an all null call tests one thing rather than several.
/// Then a wrong type in every position, which is there to capture the error rather than to pass:
/// two engines that both reject a call have to reject it the same way.
///
/// An empty result means this row cannot be called yet, which is a type with no boundary set, or a
/// kind the generator does not build calls for. Nothing is guessed at.
#[must_use]
pub fn calls(overload: &Overload) -> Vec<Call> {
    if overload.kind != Kind::Scalar {
        return Vec::new();
    }
    let mut sets = Vec::with_capacity(overload.parameters.len());
    for ty in &overload.parameters {
        let Some(set) = boundaries(ty) else { return Vec::new() };
        sets.push(set);
    }
    let ordinary: Vec<&str> = sets.iter().map(|set| set.ordinary).collect();
    let name = called(&overload.name);
    let mut out = Vec::new();
    for (at, set) in sets.iter().enumerate() {
        for value in set.values {
            let mut args = ordinary.clone();
            args[at] = value;
            out.push(Call {
                sql: format!("SELECT {name}({})", args.join(", ")),
                note: format!("argument {} is {value}", at + 1),
            });
        }
    }
    for at in 0..overload.parameters.len() {
        let mut args = ordinary.clone();
        args[at] = "NULL";
        out.push(Call {
            sql: format!("SELECT {name}({})", args.join(", ")),
            note: format!("argument {} is null", at + 1),
        });
    }
    for (at, ty) in overload.parameters.iter().enumerate() {
        let mut args = ordinary.clone();
        args[at] = wrong(ty);
        out.push(Call {
            sql: format!("SELECT {name}({})", args.join(", ")),
            note: format!("argument {} is the wrong type", at + 1),
        });
    }
    // A function of no arguments still gets called once. `random()` and `version()` are rows in
    // this catalog and the loops above produce nothing for them.
    if overload.parameters.is_empty() {
        out.push(Call { sql: format!("SELECT {name}()"), note: "no arguments".to_owned() });
    }
    // Somewhere to put the trailing arguments, for the functions that take any number of them. One
    // call with three of them, because the interesting part is that the engine takes more than the
    // fixed parameters at all and not how many more.
    if let Some(set) = overload.varargs.as_deref().and_then(boundaries) {
        let mut args = ordinary.clone();
        for _ in 0..3 {
            args.push(set.ordinary);
        }
        out.push(Call {
            sql: format!("SELECT {name}({})", args.join(", ")),
            note: "three trailing arguments".to_owned(),
        });
    }
    out
}

/// The values worth putting in one argument of a given type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boundaries {
    /// The value the other arguments sit at while one of them varies. Ordinary on purpose: a call
    /// where every argument is an extreme is a call whose failure says nothing about which extreme
    /// caused it.
    pub ordinary: &'static str,
    /// The edges, as SQL text.
    pub values: &'static [&'static str],
}

/// The functions whose answer depends on something that is not in the call.
///
/// `random` gives a different number every time. `now` reads the clock. `version` is the engine
/// saying its own name. Two engines that both work perfectly disagree on every one of these, so a
/// differential run has to know which they are before it reports a difference, and a run that did
/// not would open twenty six wrong answers on its first pass.
///
/// This list was read off the pinned catalog and it is a best effort rather than a proof. The run
/// itself is what finds the ones that are missing from it, because a function that fails on every
/// single generated call and passes by hand is what volatility looks like from the outside.
pub const VOLATILE: [&str; 27] = [
    "current_connection_id",
    "current_database",
    "current_date",
    "current_localtime",
    "current_localtimestamp",
    "current_query",
    "current_query_id",
    "current_schema",
    "current_schemas",
    "current_setting",
    "current_transaction_id",
    "currval",
    "gen_random_uuid",
    "get_current_time",
    "get_current_timestamp",
    "getenv",
    "nextval",
    "now",
    "random",
    "setseed",
    "today",
    "transaction_timestamp",
    "txid_current",
    "uuid",
    "uuidv4",
    "uuidv7",
    "version",
];

/// Whether this overload's answer depends on something that is not in the call.
///
/// Asked per overload rather than per name because of `age`, which is two functions wearing one
/// name. `age(TIMESTAMP, TIMESTAMP)` is the difference between two things the caller named and is
/// as pure as subtraction. `age(TIMESTAMP)` is the difference between one thing the caller named
/// and today, so it answers differently tomorrow.
#[must_use]
pub fn volatile(overload: &Overload) -> bool {
    if overload.name == "age" {
        return overload.parameters.len() == 1;
    }
    VOLATILE.contains(&overload.name.as_str())
}

/// How a function name is written in a call.
///
/// A good part of this catalog is operators. `+`, `@`, `^@`, `!~~` and about forty others are rows
/// in `duckdb_functions()` exactly like `upper` is, and writing one of those bare produces a syntax
/// error rather than a call. In double quotes they parse, which was checked against the pinned
/// binary. Ordinary names are left bare, because the generated SQL is what somebody reads when a
/// case fails and `upper('a')` reads better than `"upper"('a')`.
fn called(name: &str) -> String {
    let mut chars = name.chars();
    let plain = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain { name.to_owned() } else { format!("\"{}\"", name.replace('"', "\"\"")) }
}

/// Every type this file has a boundary set for.
///
/// Here so that a test can put all of them to a live DuckDB and fail when one of these values has
/// stopped constructing. A literal that does not even parse is not a boundary, it is a call both
/// engines reject for a reason that has nothing to do with the function under test, and it would
/// quietly count as agreement forever.
pub const TYPES: [&str; 33] = [
    "TINYINT",
    "SMALLINT",
    "INTEGER",
    "BIGINT",
    "HUGEINT",
    "UTINYINT",
    "USMALLINT",
    "UINTEGER",
    "UBIGINT",
    "UHUGEINT",
    "FLOAT",
    "DOUBLE",
    "DECIMAL",
    "VARCHAR",
    "BLOB",
    "BOOLEAN",
    "DATE",
    "TIME",
    "TIME WITH TIME ZONE",
    "TIMESTAMP",
    "TIMESTAMP WITH TIME ZONE",
    "TIMESTAMP_S",
    "TIMESTAMP_MS",
    "TIMESTAMP_NS",
    "INTERVAL",
    "UUID",
    "BIT",
    "JSON",
    "VARCHAR[]",
    "BIGINT[]",
    "DOUBLE[]",
    "FLOAT[]",
    "BOOLEAN[]",
];

/// A string long enough that nothing about it is stored inline anywhere.
///
/// Two hundred and fifty six characters, written out rather than built by a function call, and not
/// the hundred thousand it started as. The length that matters is the one where the representation
/// changes and that is twelve bytes. Past that it is the same code path with a larger allocation,
/// and a hundred thousand character literal in every failure report is a failure report nobody
/// pastes anywhere.
const LONG: &str = "'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'";

/// The boundary set for a type, or nothing when this table has none for it.
///
/// The match is on the name DuckDB prints in `duckdb_functions()`, including the width, because
/// `DECIMAL` and `DECIMAL(18,3)` are different rows there and both appear.
#[must_use]
pub fn boundaries(ty: &str) -> Option<Boundaries> {
    let set = |ordinary, values| Some(Boundaries { ordinary, values });
    match ty {
        "TINYINT" => set(
            "1::TINYINT",
            &["(-128)::TINYINT", "127::TINYINT", "0::TINYINT", "1::TINYINT", "(-1)::TINYINT"],
        ),
        "SMALLINT" => set(
            "1::SMALLINT",
            &[
                "(-32768)::SMALLINT",
                "32767::SMALLINT",
                "0::SMALLINT",
                "1::SMALLINT",
                "(-1)::SMALLINT",
            ],
        ),
        "INTEGER" => set(
            "1::INTEGER",
            &[
                "(-2147483648)::INTEGER",
                "2147483647::INTEGER",
                "0::INTEGER",
                "1::INTEGER",
                "(-1)::INTEGER",
            ],
        ),
        "BIGINT" => set(
            "1::BIGINT",
            &[
                "(-9223372036854775808)::BIGINT",
                "9223372036854775807::BIGINT",
                "0::BIGINT",
                "1::BIGINT",
                "(-1)::BIGINT",
            ],
        ),
        "HUGEINT" => set(
            "1::HUGEINT",
            &[
                "(-170141183460469231731687303715884105728)::HUGEINT",
                "170141183460469231731687303715884105727::HUGEINT",
                "0::HUGEINT",
                "1::HUGEINT",
                "(-1)::HUGEINT",
            ],
        ),
        "UTINYINT" => set("1::UTINYINT", &["0::UTINYINT", "255::UTINYINT", "1::UTINYINT"]),
        "USMALLINT" => set("1::USMALLINT", &["0::USMALLINT", "65535::USMALLINT", "1::USMALLINT"]),
        "UINTEGER" => set("1::UINTEGER", &["0::UINTEGER", "4294967295::UINTEGER", "1::UINTEGER"]),
        "UBIGINT" => {
            set("1::UBIGINT", &["0::UBIGINT", "18446744073709551615::UBIGINT", "1::UBIGINT"])
        }
        "UHUGEINT" => set(
            "1::UHUGEINT",
            &["0::UHUGEINT", "340282366920938463463374607431768211455::UHUGEINT", "1::UHUGEINT"],
        ),
        "FLOAT" => set(
            "1.5::FLOAT",
            &[
                "0.0::FLOAT",
                "(-0.0)::FLOAT",
                "'infinity'::FLOAT",
                "'-infinity'::FLOAT",
                "'nan'::FLOAT",
                "1.1754944e-38::FLOAT",
                "3.4028235e38::FLOAT",
            ],
        ),
        "DOUBLE" => set(
            "1.5::DOUBLE",
            &[
                "0.0::DOUBLE",
                "(-0.0)::DOUBLE",
                "'infinity'::DOUBLE",
                "'-infinity'::DOUBLE",
                "'nan'::DOUBLE",
                "5e-324::DOUBLE",
                "2.2250738585072014e-308::DOUBLE",
                "1.7976931348623157e308::DOUBLE",
            ],
        ),
        "DECIMAL" => set(
            "1.5::DECIMAL(18,3)",
            &[
                "0::DECIMAL(18,3)",
                "999999999999999.999::DECIMAL(18,3)",
                "(-999999999999999.999)::DECIMAL(18,3)",
                "0.001::DECIMAL(18,3)",
                "'99999999999999999999999999999999999999'::DECIMAL(38,0)",
                // As text rather than as a number, because a literal of that many digits is read as a
                // DOUBLE first and arrives at the cast already rounded to 1.0, which DuckDB then
                // refuses. Checked against the pinned binary.
                "'0.99999999999999999999999999999999999999'::DECIMAL(38,38)",
            ],
        ),
        "VARCHAR" => set(
            "'a'",
            &[
                "''",
                // The only way to get a zero byte into a string. There is no escape for it in a
                // literal, so this one case costs a dependency on `chr` and a failure here is a
                // failure of either function.
                "chr(0)",
                "'a' || chr(0) || 'b'",
                // Twelve bytes and thirteen, which is where both engines stop storing a string
                // beside the pointer and start storing it somewhere else. Written out rather than
                // built with `repeat`, so that a function missing from one engine cannot fail every
                // other function's long string case with it.
                "'aaaaaaaaaaaa'",
                "'aaaaaaaaaaaaa'",
                LONG,
                // The letter and the accent as two code points, which is a different string from
                // the one code point that prints the same way.
                "'e\u{301}'",
                "'\u{1f600}'",
                "'  a  '",
                "'A'",
            ],
        ),
        // The invalid UTF-8 case the specification asks for is here and not on `VARCHAR`, because
        // DuckDB will not build one. A string literal with backslash x in it is those four
        // characters, casting a blob to a string escapes the bytes rather than carrying them, and
        // `decode` raises a conversion error on purpose. A blob is where those bytes can exist.
        "BLOB" => set(
            "'ab'::BLOB",
            &[
                "''::BLOB",
                "'\\x00'::BLOB",
                "'\\xff\\xfe'::BLOB",
                "'\\xc3\\x28'::BLOB",
                "'aaaaaaaaaaaaa'::BLOB",
            ],
        ),
        "BOOLEAN" => set("true", &["true", "false"]),
        "DATE" => set(
            "DATE '2024-02-29'",
            &[
                "DATE '1970-01-01'",
                "DATE '2024-02-29'",
                "DATE '0001-01-01'",
                "DATE '9999-12-31'",
                "'infinity'::DATE",
                "'-infinity'::DATE",
            ],
        ),
        "TIME" => set(
            "TIME '12:00:00'",
            &["TIME '00:00:00'", "TIME '23:59:59.999999'", "TIME '12:00:00'"],
        ),
        "TIME WITH TIME ZONE" => set(
            "TIMETZ '12:00:00+00'",
            &[
                "TIMETZ '00:00:00+00'",
                "TIMETZ '23:59:59.999999+00'",
                "TIMETZ '12:00:00+15:59:59'",
                "TIMETZ '12:00:00-15:59:59'",
            ],
        ),
        "TIMESTAMP" => set(
            "TIMESTAMP '2024-02-29 12:00:00'",
            &[
                "TIMESTAMP '1970-01-01 00:00:00'",
                "TIMESTAMP '2024-02-29 23:59:59.999999'",
                "TIMESTAMP '0001-01-01 00:00:00'",
                "TIMESTAMP '9999-12-31 23:59:59.999999'",
                "'infinity'::TIMESTAMP",
                "'-infinity'::TIMESTAMP",
            ],
        ),
        "TIMESTAMP WITH TIME ZONE" => set(
            "TIMESTAMPTZ '2024-02-29 12:00:00+00'",
            &[
                "TIMESTAMPTZ '1970-01-01 00:00:00+00'",
                "TIMESTAMPTZ '2024-02-29 23:59:59.999999+00'",
                "'infinity'::TIMESTAMPTZ",
                "'-infinity'::TIMESTAMPTZ",
            ],
        ),
        "TIMESTAMP_S" => set(
            "'2024-02-29 12:00:00'::TIMESTAMP_S",
            &["'1970-01-01 00:00:00'::TIMESTAMP_S", "'2024-02-29 23:59:59'::TIMESTAMP_S"],
        ),
        "TIMESTAMP_MS" => set(
            "'2024-02-29 12:00:00'::TIMESTAMP_MS",
            &["'1970-01-01 00:00:00'::TIMESTAMP_MS", "'2024-02-29 23:59:59.999'::TIMESTAMP_MS"],
        ),
        "TIMESTAMP_NS" => set(
            "'2024-02-29 12:00:00'::TIMESTAMP_NS",
            &[
                "'1970-01-01 00:00:00'::TIMESTAMP_NS",
                "'2024-02-29 23:59:59.999999999'::TIMESTAMP_NS",
            ],
        ),
        "INTERVAL" => set(
            "INTERVAL 1 DAY",
            &[
                "INTERVAL 0 DAY",
                "INTERVAL 1 DAY",
                "INTERVAL (-1) DAY",
                "INTERVAL 1 MONTH",
                "INTERVAL 999999999 MICROSECOND",
            ],
        ),
        "UUID" => set(
            "'00000000-0000-0000-0000-000000000000'::UUID",
            &[
                "'00000000-0000-0000-0000-000000000000'::UUID",
                "'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID",
            ],
        ),
        "BIT" => set("'0101'::BIT", &["''::BIT", "'0'::BIT", "'1'::BIT", "'0101'::BIT"]),
        "JSON" => set(
            "'{\"a\": 1}'::JSON",
            &["'null'::JSON", "'{}'::JSON", "'[]'::JSON", "'{\"a\": 1}'::JSON"],
        ),
        "VARCHAR[]" => set("['a']", &["[]::VARCHAR[]", "['a']", "[NULL]::VARCHAR[]", "['a', 'b']"]),
        "BIGINT[]" => set(
            "[1::BIGINT]",
            &["[]::BIGINT[]", "[1::BIGINT]", "[NULL]::BIGINT[]", "[1::BIGINT, 2::BIGINT]"],
        ),
        "DOUBLE[]" => set(
            "[1.5::DOUBLE]",
            &["[]::DOUBLE[]", "[1.5::DOUBLE]", "[NULL]::DOUBLE[]", "[1.5::DOUBLE, 2.5::DOUBLE]"],
        ),
        "FLOAT[]" => set("[1.5::FLOAT]", &["[]::FLOAT[]", "[1.5::FLOAT]", "[NULL]::FLOAT[]"]),
        "BOOLEAN[]" => {
            set("[true]", &["[]::BOOLEAN[]", "[true]", "[NULL]::BOOLEAN[]", "[true, false]"])
        }
        _ => None,
    }
}

/// A value of a type the parameter is not, to capture the error both engines should give.
///
/// Text for everything that is not text, and a number for text, which is the pair that is never
/// silently acceptable in either direction. It is deliberately not something exotic: the point is to
/// compare two rejections, and a rejection is easiest to compare when the reason is obvious.
fn wrong(ty: &str) -> &'static str {
    if ty == "VARCHAR" { "42::INTEGER" } else { "'not a value of this type'" }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Overload, TYPES, boundaries, called, calls, inventory, read};
    use crate::engine::{Cell, Column, Table};

    fn table(rows: Vec<Vec<&str>>) -> Table {
        Table {
            columns: [
                "function_name",
                "function_type",
                "return_type",
                "parameter_types",
                "varargs",
                "internal",
            ]
            .into_iter()
            .map(|name| Column { name: name.to_owned(), ty: "VARCHAR".to_owned() })
            .collect(),
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(|c| Cell::Text(c.to_owned())).collect())
                .collect(),
        }
    }

    fn scalar(name: &str, parameters: &[&str]) -> Overload {
        Overload {
            name: name.to_owned(),
            kind: Kind::Scalar,
            returns: "VARCHAR".to_owned(),
            parameters: parameters.iter().map(|p| (*p).to_owned()).collect(),
            varargs: None,
            internal: true,
        }
    }

    #[test]
    fn a_row_of_the_catalog_becomes_an_overload_with_its_parameters_in_order() {
        let read = read(&table(vec![vec![
            "substring",
            "scalar",
            "VARCHAR",
            "VARCHAR|BIGINT|BIGINT",
            "",
            "true",
        ]]))
        .expect("a catalog this reader can read");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "substring");
        assert_eq!(read[0].kind, Kind::Scalar);
        assert_eq!(read[0].parameters, ["VARCHAR", "BIGINT", "BIGINT"]);
        assert_eq!(read[0].varargs, None);
        assert!(read[0].internal);
    }

    #[test]
    fn a_type_with_a_comma_inside_it_is_one_parameter_and_not_two() {
        let read =
            read(&table(vec![vec!["map_extract", "scalar", "ANY[]", "MAP(K, V)|K", "", "true"]]))
                .expect("a catalog this reader can read");
        assert_eq!(read[0].parameters, ["MAP(K, V)", "K"]);
    }

    #[test]
    fn a_function_of_no_arguments_has_no_parameters_rather_than_one_empty_one() {
        let read = read(&table(vec![vec!["random", "scalar", "DOUBLE", "", "", "true"]]))
            .expect("a catalog this reader can read");
        assert!(read[0].parameters.is_empty());
    }

    #[test]
    fn a_kind_this_reader_has_never_seen_stops_it_rather_than_being_dropped() {
        let read = read(&table(vec![vec!["something", "quantum", "VARCHAR", "", "", "true"]]));
        let message = read.expect_err("an unknown kind is not a row to skip").to_string();
        assert!(message.contains("quantum"), "{message}");
    }

    #[test]
    fn one_argument_varies_at_a_time_and_the_others_sit_at_an_ordinary_value() {
        let calls = calls(&scalar("substring", &["VARCHAR", "BIGINT"]));
        let varying =
            calls.iter().filter(|c| c.note == "argument 2 is 0::BIGINT").collect::<Vec<_>>();
        assert_eq!(varying.len(), 1, "{calls:?}");
        assert_eq!(varying[0].sql, "SELECT substring('a', 0::BIGINT)");
    }

    #[test]
    fn a_null_goes_in_every_position_on_its_own_and_never_in_two_at_once() {
        let calls = calls(&scalar("substring", &["VARCHAR", "BIGINT"]));
        let nulls: Vec<&str> =
            calls.iter().filter(|c| c.note.ends_with("is null")).map(|c| c.sql.as_str()).collect();
        assert_eq!(nulls, ["SELECT substring(NULL, 1::BIGINT)", "SELECT substring('a', NULL)"]);
    }

    #[test]
    fn a_wrong_type_goes_in_every_position_because_two_engines_have_to_reject_it_the_same_way() {
        let calls = calls(&scalar("substring", &["VARCHAR", "BIGINT"]));
        let wrong: Vec<&str> = calls
            .iter()
            .filter(|c| c.note.ends_with("is the wrong type"))
            .map(|c| c.sql.as_str())
            .collect();
        assert_eq!(
            wrong,
            [
                "SELECT substring(42::INTEGER, 1::BIGINT)",
                "SELECT substring('a', 'not a value of this type')"
            ]
        );
    }

    #[test]
    fn a_function_of_no_arguments_is_still_called_once() {
        let calls = calls(&scalar("version", &[]));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].sql, "SELECT version()");
    }

    #[test]
    fn a_function_that_takes_any_number_of_arguments_is_called_with_several() {
        let mut concat = scalar("concat", &["VARCHAR"]);
        concat.varargs = Some("VARCHAR".to_owned());
        let calls = calls(&concat);
        let several: Vec<&str> = calls
            .iter()
            .filter(|c| c.note == "three trailing arguments")
            .map(|c| c.sql.as_str())
            .collect();
        assert_eq!(several, ["SELECT concat('a', 'a', 'a', 'a')"]);
    }

    #[test]
    fn a_type_with_no_boundary_set_produces_no_calls_rather_than_a_guess() {
        assert!(calls(&scalar("map_extract", &["MAP(K, V)", "K"])).is_empty());
        assert!(boundaries("ANY").is_none());
        assert!(boundaries("LAMBDA").is_none());
    }

    #[test]
    fn a_kind_the_generator_does_not_call_yet_produces_no_calls_either() {
        let mut aggregate = scalar("sum", &["BIGINT"]);
        aggregate.kind = Kind::Aggregate;
        assert!(calls(&aggregate).is_empty());
    }

    #[test]
    fn the_integer_sets_carry_the_edges_of_the_width_they_are_for() {
        let edges = |ty: &str| boundaries(ty).expect("a set for an integer").values.join(" ");
        assert!(edges("TINYINT").contains("(-128)::TINYINT"));
        assert!(edges("TINYINT").contains("127::TINYINT"));
        assert!(edges("INTEGER").contains("2147483647::INTEGER"));
        assert!(edges("BIGINT").contains("(-9223372036854775808)::BIGINT"));
        assert!(edges("UBIGINT").contains("18446744073709551615::UBIGINT"));
        assert!(!edges("UBIGINT").contains("(-1)"), "an unsigned type has no negative edge");
    }

    #[test]
    fn the_sets_the_specification_names_are_all_here() {
        let has = |ty: &str, want: &str| {
            let set = boundaries(ty).unwrap_or_else(|| panic!("a set for {ty}"));
            assert!(set.values.iter().any(|v| v.contains(want)), "{ty} has no {want} in it");
        };
        has("DOUBLE", "nan");
        has("DOUBLE", "infinity");
        has("DOUBLE", "-0.0");
        has("DOUBLE", "5e-324");
        has("VARCHAR", "''");
        has("VARCHAR", "chr(0)");
        has("VARCHAR", "'aaaaaaaaaaaa'");
        has("VARCHAR", "'aaaaaaaaaaaaa'");
        has("VARCHAR", "e\u{301}");
        has("BLOB", "\\xff\\xfe");
        has("BLOB", "\\xc3\\x28");
        has("DECIMAL", "DECIMAL(38,0)");
        has("DECIMAL", "DECIMAL(38,38)");
        has("TIMESTAMP", "1970-01-01");
        has("TIMESTAMP", "infinity");
        has("TIMESTAMP", "2024-02-29");
    }

    #[test]
    fn an_operator_is_called_in_quotes_and_an_ordinary_name_is_left_alone() {
        assert_eq!(called("upper"), "upper");
        assert_eq!(called("date_part"), "date_part");
        assert_eq!(
            called("__internal_compress_integral_usmallint"),
            "__internal_compress_integral_usmallint"
        );
        assert_eq!(called("md5_number_upper"), "md5_number_upper");
        assert_eq!(called("+"), "\"+\"");
        assert_eq!(called("^@"), "\"^@\"");
        assert_eq!(called("!~~"), "\"!~~\"");
        assert_eq!(called("ST_Point"), "\"ST_Point\"");
    }

    #[test]
    fn an_operator_reaches_the_generated_call_in_quotes() {
        let calls = calls(&scalar("^@", &["VARCHAR", "VARCHAR"]));
        assert!(calls.iter().all(|c| c.sql.starts_with("SELECT \"^@\"(")), "{calls:?}");
    }

    #[test]
    fn the_list_of_types_and_the_table_of_sets_say_the_same_thing() {
        for ty in TYPES {
            let set = boundaries(ty).unwrap_or_else(|| panic!("{ty} is on the list with no set"));
            assert!(!set.values.is_empty(), "{ty} has an empty set");
            assert!(!set.ordinary.is_empty(), "{ty} has no ordinary value");
        }
    }

    #[test]
    fn an_inventory_counts_the_rows_and_the_names_and_what_cannot_be_called_yet() {
        let catalog = vec![
            scalar("upper", &["VARCHAR"]),
            scalar("substring", &["VARCHAR", "BIGINT"]),
            scalar("map_extract", &["MAP(K, V)", "K"]),
            Overload { kind: Kind::Aggregate, ..scalar("upper", &["VARCHAR"]) },
        ];
        let inventory = inventory(&catalog);
        assert_eq!(inventory.overloads, 4);
        assert_eq!(inventory.names, 3);
        assert_eq!(inventory.callable, 2);
        assert_eq!(inventory.kinds, [(Kind::Scalar, 3, 3), (Kind::Aggregate, 1, 1)]);
        assert_eq!(inventory.unknown, [("K".to_owned(), 1), ("MAP(K, V)".to_owned(), 1)]);
    }
}
