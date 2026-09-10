-- The statements every part of rudb's parser is checked against, one per line.
--
-- These came from crates/rudb-parse/src/corpus.rs in the engine repository, where they check that
-- the tokenizer, the rule table, the matcher and the transformer all agree with each other. Here
-- they check something the engine cannot check on its own, which is whether DuckDB agrees too.
-- The copy is deliberate and it is the only kind of copy that does not drift, because the run is
-- what compares them and a statement that stopped meaning the same thing shows up as a difference.
--
-- Every one of these has to parse on both engines. None of them can run yet.
SELECT 1;
SELECT 1 + 2 * 3 - 4 / 5 % 6;
SELECT a, b, c FROM t WHERE a = 1 AND b > 2 OR NOT c;
SELECT DISTINCT ON (a) a, b FROM t ORDER BY a, b DESC NULLS LAST;
SELECT count(*), sum(x), avg(y) FROM t GROUP BY a, b HAVING count(*) > 1;
SELECT * FROM t LIMIT 10 OFFSET 5;
SELECT * FROM a LEFT JOIN b USING (id) INNER JOIN c ON c.id = a.id;
SELECT * FROM a NATURAL JOIN b;
SELECT * FROM a ASOF JOIN b ON a.t >= b.t;
WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n < 10) SELECT * FROM c;
SELECT x, row_number() OVER (PARTITION BY a ORDER BY b) FROM t;
SELECT x FROM t WHERE x IN (SELECT y FROM u);
SELECT CASE WHEN a THEN 1 WHEN b THEN 2 ELSE 3 END FROM t;
SELECT CAST(a AS INTEGER), a::VARCHAR, try_cast(a AS DOUBLE) FROM t;
SELECT [1, 2, 3], {'a': 1}, struct_pack(a := 1);
SELECT a[1], a[1:2], a.b.c FROM t;
SELECT * FROM read_parquet('x.parquet');
SELECT * FROM 'file.csv';
SELECT * EXCLUDE (a), * REPLACE (b AS c) FROM t;
SELECT * FROM t WHERE x BETWEEN 1 AND 2;
SELECT x IS NULL, x IS NOT NULL, x IS DISTINCT FROM y FROM t;
SELECT x LIKE 'a%', x ILIKE 'a%', x SIMILAR TO 'a' FROM t;
CREATE TABLE t (a INTEGER PRIMARY KEY, b VARCHAR NOT NULL DEFAULT 'x', CHECK (a > 0));
CREATE OR REPLACE VIEW v AS SELECT 1;
CREATE TABLE t2 AS SELECT * FROM t;
CREATE INDEX i ON t (a, b);
DROP TABLE IF EXISTS t CASCADE;
ALTER TABLE t ADD COLUMN c INTEGER;
ALTER TABLE t RENAME COLUMN a TO b;
INSERT INTO t VALUES (1, 'a'), (2, 'b');
INSERT INTO t (a, b) SELECT * FROM u ON CONFLICT DO NOTHING;
UPDATE t SET a = 1, b = 2 WHERE c = 3;
DELETE FROM t WHERE a = 1;
COPY t TO 'out.parquet' (FORMAT PARQUET);
COPY t FROM 'in.csv' (HEADER, DELIMITER ',');
EXPLAIN ANALYZE SELECT 1;
PRAGMA table_info('t');
SET memory_limit = '1GB';
BEGIN TRANSACTION;
COMMIT;
ATTACH 'x.db' AS x;
DESCRIBE SELECT 1;
SUMMARIZE t;
PIVOT t ON a USING sum(b);
UNPIVOT t ON a, b INTO NAME k VALUE v;
FROM t SELECT a;
FROM t;
SELECT a FROM t QUALIFY row_number() OVER () = 1;
SELECT * FROM t USING SAMPLE 10%;
SELECT * FROM t TABLESAMPLE 10 PERCENT;
SELECT * FROM t USING SAMPLE reservoir(50 ROWS) REPEATABLE (100);
SELECT list_transform([1,2], x -> x + 1);
SELECT 'a' || 'b', 1 <> 2, x @> y FROM t;
SELECT * FROM generate_series(1, 10) t(i);
SELECT a FROM t UNION SELECT b FROM u EXCEPT SELECT c FROM v;
SELECT * FROM (VALUES (1), (2)) v(x);
SELECT $1, ?, $name;
SELECT TIMESTAMP '2020-01-01', INTERVAL 1 DAY, DATE '2020-01-01';
SELECT a FROM t GROUP BY ALL ORDER BY ALL;
SELECT a FROM t GROUP BY GROUPING SETS ((a), (b));
SELECT a FROM t GROUP BY CUBE (a, b);
CREATE MACRO m(a) AS a + 1;
