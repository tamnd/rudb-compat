-- Statements where the dialect is the whole point, and where getting one wrong is invisible until
-- somebody runs it.
--
-- Against the pinned binary all eleven of these parse the same way on both engines, which is what
-- the vendored grammar is for and what `the_dialect_file_agrees_in_full_against_the_pinned_binary`
-- gates on. Running them rather than parsing them is a second number and it is nine of eleven, so
-- two of these say something different once an answer has to come out. Those two carry the issue
-- that tracks them, and they are the reason this file is not expected to come back at a hundred
-- percent through `rudb-compat run`.

-- An operator the dialect does not name. Two or more operator characters that do not spell one of
-- the operators the grammar lists reach OperatorLiteral and become a function call by that name.
SELECT a <=> b;

-- Not an operator. The grammar text says OperatorLiteral takes an Identifier, and it is one of the
-- 24 matcher overrides, so what it actually takes is a run of operator characters.
SELECT a foo b;

-- Both engines have to agree about what is not SQL, not only about what is.
SELECT FROM WHERE;

-- One number token when there is nothing left to read and a 1 aliased e when there is. The pinned
-- binary answers e = 1 for `SELECT 1e;` and cannot convert '1e' to a double for `SELECT 1e`, one
-- semicolon apart, because the tokenizer gives the exponent marker back when it has input left to
-- give it back into. rudb takes the number token both times, which is tamnd/rudb#277.
SELECT 1e;

-- Quoted identifiers keep their case, in DuckDB and here, which is the thing every other database
-- does differently.
SELECT "Quoted Col" FROM t;

-- Fifteen keywords are spelled by a rule and are in none of the five class lists, so they are
-- matchable as a literal and are still perfectly good column names.
SELECT ascending FROM t ORDER BY x ASCENDING;

-- Dollar quoting, which the grammar says nothing about, because the tokenizer owns it. Both engines
-- parse it and both answer the text between the tags, the second half of that having taken
-- tamnd/rudb#276: rudb found the closing tag and then kept every byte it had scanned.
SELECT $$dollar quoted$$;

-- Adjacent string literals are one string.
SELECT 'a' 'b';

-- A dot between two names arrives as a number token, because .5 is a number and the scan cannot
-- know which it has until it has read past the dot.
SELECT a.b.c.d FROM t;

-- A ranged slice. It parses, and what a missing bound means is a question for the run. There is no
-- table t, so the binary looks for it and says catalog error where rudb refuses the slice first and
-- says not implemented, which is tamnd/rudb#278.
SELECT x[1:2] FROM t;

-- Array distance. The newest released DuckDB takes it. The vendored v2.0 tokenizer cannot produce
-- it as one token, because a hyphen is a single byte operator and never joins a run, and <-> is in
-- no rule of the v2.0 grammar either. So on the vendored definition of the dialect this is not SQL.
SELECT [1, 2] <-> [3, 4];
