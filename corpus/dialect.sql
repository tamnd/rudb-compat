-- Statements where the dialect is the whole point, and where getting one wrong is invisible until
-- somebody runs it.
--
-- This file is not expected to come back at a hundred percent, and that is what it is for. Two of
-- these disagree between the newest released DuckDB and the grammar rudb has vendored from the
-- v2.0 development ref, and both disagreements are upstream moving rather than rudb being wrong.
-- They are written down here so that the day a v2.0 binary exists, the run says so.

-- An operator the dialect does not name. Two or more operator characters that do not spell one of
-- the operators the grammar lists reach OperatorLiteral and become a function call by that name.
SELECT a <=> b;

-- Not an operator. The grammar text says OperatorLiteral takes an Identifier, and it is one of the
-- 24 matcher overrides, so what it actually takes is a run of operator characters.
SELECT a foo b;

-- Both engines have to agree about what is not SQL, not only about what is.
SELECT FROM WHERE;

-- One number token and not a 1 aliased e. Reading the tokenizer source rather than the docs is
-- what settled this one.
SELECT 1e;

-- Quoted identifiers keep their case, in DuckDB and here, which is the thing every other database
-- does differently.
SELECT "Quoted Col" FROM t;

-- Fifteen keywords are spelled by a rule and are in none of the five class lists, so they are
-- matchable as a literal and are still perfectly good column names.
SELECT ascending FROM t ORDER BY x ASCENDING;

-- Dollar quoting, which the grammar says nothing about, because the tokenizer owns it.
SELECT $$dollar quoted$$;

-- Adjacent string literals are one string.
SELECT 'a' 'b';

-- A dot between two names arrives as a number token, because .5 is a number and the scan cannot
-- know which it has until it has read past the dot.
SELECT a.b.c.d FROM t;

-- A ranged slice. It parses, and what a missing bound means is a question for the run.
SELECT x[1:2] FROM t;

-- Array distance. The newest released DuckDB takes it. The vendored v2.0 tokenizer cannot produce
-- it as one token, because a hyphen is a single byte operator and never joins a run, and <-> is in
-- no rule of the v2.0 grammar either. So on the vendored definition of the dialect this is not SQL.
SELECT [1, 2] <-> [3, 4];
