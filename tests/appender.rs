//! The appender, against what the pin's appender does with the same rows.
//!
//! sqllogictest has no way to spell an appender, so upstream tests it from C++ and the corpus has
//! nothing for it. This is the same set of cases through `rudb::Database::append`, with the answers
//! DuckDB 2.0 gave through `duckdb_appender_create`, `duckdb_append_*` and `duckdb_appender_close`
//! on the same rows. They were taken from the C API of the 2.0.0.dev2609222040 wheel (source
//! 6844d1bd8b), since no build of the pin itself ships as a library yet.
//!
//! What the pin does, and so what this holds rudb to:
//!
//! - A value is converted to its column's type on the way in, an integer into a `DOUBLE` and a
//!   string that is a number into an `INTEGER`, and a string that is not a number is an error.
//! - A batch is all or nothing. A NULL in a `NOT NULL` column or a key that is already there
//!   rejects the batch, and none of its rows land, including the ones before the bad one.
//! - A row with too few values is an error.

use rudb::{Database, Value};

fn table() -> Database {
    let database = Database::new();
    database
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name VARCHAR NOT NULL, score DOUBLE)")
        .expect("the table");
    database
}

fn text(value: &str) -> Value {
    Value::Varchar(value.to_owned())
}

/// Every row, as the text a query prints, ordered by the key.
fn rows(database: &Database) -> Vec<String> {
    let result = database.query("SELECT id, name, score FROM t ORDER BY id").expect("the rows");
    (0..result.len())
        .map(|row| (0..3).map(|column| result.text_at(row, column)).collect::<Vec<_>>().join("\t"))
        .collect()
}

fn appended(database: &Database) {
    database
        .append(
            "t",
            &[
                vec![Value::Integer(1), text("a"), Value::Double(1.5)],
                vec![Value::Integer(2), text("b"), Value::BigInt(2)],
                vec![Value::Integer(3), text("c"), Value::Null],
            ],
        )
        .expect("three good rows");
}

#[test]
fn rows_land_converted_to_their_columns() {
    let database = table();
    appended(&database);
    database
        .append("t", &[
            vec![Value::BigInt(4), text("d"), Value::BigInt(7)],
            vec![text("5"), text("e"), Value::Double(0.25)],
        ])
        .expect("values that convert");
    assert_eq!(rows(&database), [
        "1\ta\t1.5",
        "2\tb\t2.0",
        "3\tc\tNULL",
        "4\td\t7.0",
        "5\te\t0.25"
    ]);
}

#[test]
fn a_string_that_is_not_a_number_is_refused() {
    let database = table();
    let error = database
        .append("t", &[vec![text("nope"), text("z"), Value::Double(1.0)]])
        .expect_err("the pin says Could not convert string 'nope' to INT32");
    assert!(error.to_string().contains("nope"), "{error}");
    assert!(rows(&database).is_empty());
}

#[test]
fn a_null_in_a_not_null_column_rejects_the_batch() {
    let database = table();
    appended(&database);
    let error = database
        .append("t", &[
            vec![Value::Integer(6), text("f"), Value::Double(1.0)],
            vec![Value::Integer(7), Value::Null, Value::Double(1.0)],
        ])
        .expect_err("the pin says NOT NULL constraint failed: t.\"name\"");
    assert!(error.to_string().contains("NOT NULL constraint failed"), "{error}");
    assert_eq!(rows(&database).len(), 3, "row 6 came before the bad row and still must not land");
}

#[test]
fn a_key_already_there_rejects_the_batch() {
    let database = table();
    appended(&database);
    let error = database
        .append("t", &[
            vec![Value::Integer(8), text("g"), Value::Double(0.0)],
            vec![Value::Integer(1), text("again"), Value::Double(0.0)],
        ])
        .expect_err("the pin says Duplicate key \"id: 1\" violates primary key constraint.");
    assert!(error.to_string().contains("Duplicate key \"id: 1\""), "{error}");
    assert_eq!(rows(&database).len(), 3);
}

#[test]
fn a_key_twice_in_one_batch_rejects_the_batch() {
    let database = table();
    let error = database
        .append("t", &[
            vec![Value::Integer(7), text("x"), Value::Double(0.0)],
            vec![Value::Integer(7), text("y"), Value::Double(0.0)],
        ])
        .expect_err("the pin says PRIMARY KEY or UNIQUE constraint violation: duplicate key \"7\"");
    assert!(error.to_string().to_lowercase().contains("duplicate key"), "{error}");
    assert!(rows(&database).is_empty());
}

#[test]
fn a_short_row_is_refused() {
    let database = table();
    database
        .append("t", &[vec![Value::Integer(8), text("short")]])
        .expect_err("the pin says Call to EndRow before all columns have been appended to!");
    assert!(rows(&database).is_empty());
}
