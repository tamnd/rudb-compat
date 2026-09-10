# rudb-compat

The DuckDB compatibility harness for [rudb](https://github.com/tamnd/rudb).

This is the apparatus that turns "compatible with DuckDB" from an assertion into a number that a machine computed. Differential execution against a real DuckDB binary, weighted function and statement coverage, storage format round trips in both directions, and C ABI conformance generated from DuckDB's own header.

It is a separate repository for three reasons. It depends on DuckDB and on rudb at the same time, which no crate that ships should. It should be runnable by someone who trusts neither. And a compatibility suite that lives inside the implementation it tests is a suite whose failures are easy to explain away.

The design is [`spec/14-rudb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/14-rudb-compat.md) in the rudb repository, and the levels it reports against are [`spec/12-duckdb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/12-duckdb-compat.md) section 12.8.

## Status

Early, and running. rudb cannot execute a query yet, so no level has a coverage number against it. What does work today is the differential loop itself, pointed at the one question rudb can already answer: whether a piece of text is SQL.

That question is not a consolation prize. rudb vendors DuckDB's PEG grammar and generates its rule table from it, precisely so that the dialect cannot drift, and this is the check that the vendoring worked. Every statement in a corpus goes to both engines and the two answers are compared.

```
$ rudb-compat parse corpus/m0.sql
62 of 62 agreed, which is 100.0 percent
```

Errors are compared too, by error kind, so a statement both engines reject counts as agreement only when they reject it as the same kind of error. `--strict-messages` tightens that to the first line of the message.

The full path is there behind `run` rather than `parse`: two engines, full result sets, column names and types, and a per-statement ordering rule that comes from rudb's own AST. It is honest about where it stands. `rudb-compat query 'SELECT 1'` reports the difference that rudb has no executor, and the day it has one that report is what changes.

The DuckDB side is a real binary of a named version, found on `PATH` or named by `RUDB_COMPAT_DUCKDB`. `rudb-compat duckdb` prints which one it found and whether it matches the version rudb tracks.

```
$ rudb-compat duckdb
binary   /opt/homebrew/bin/duckdb
version  v1.5.5 (Variegata) d8cdaa33fd
pinned   v2.0

This is not the version the grammar is vendored from, so a number that
comes out of it is about this DuckDB and not about the one rudb tracks.
```

There is no published v2.0 binary yet, so that mismatch is the state on every machine today rather than a local problem, and saying so in the tool is better than a percentage that quietly means something else.

`corpus/dialect.sql` is the file where the mismatch shows. It comes back at 9 of 11, and both differences are upstream moving between the newest release and the v2.0 ref rather than rudb being wrong: `ORDER BY x ASCENDING` is `AscendingOrder <- 'ASC' / 'ASCENDING'` in the vendored grammar and a syntax error on 1.5.5, and `[1, 2] <-> [3, 4]` is array distance on 1.5.5 and cannot be one token under the v2.0 tokenizer, where a hyphen is a single byte operator that never joins an operator run. A test asserts those two and only those two, so the day a v2.0 binary exists the run is what tells us they went away.

The suites behind the four levels arrive with M2 in [`spec/17-milestones.md`](https://github.com/tamnd/rudb/blob/main/spec/17-milestones.md), which is the milestone where there is first a database to point them at. `rudb-compat levels` prints them and the fact that none has been measured.

## The principle

**No compatibility claim appears anywhere without a test that produced it.** Not in a README, not in a release note, not in a talk. Each of the four levels has a suite, each suite produces a percentage and a failure list, and both are published on every commit.

**The comparison is always against a real DuckDB binary of a named version**, run on the same data on the same machine. Not against documentation, not against remembered behavior, and not against a previous run. The surface moves between releases, so a percentage without a version attached to it does not mean anything.

**Errors are results.** A query that errors on DuckDB has to error here, with a matching code and, where it is specified, a matching message. Succeeding where DuckDB fails is a failure, the same as failing where DuckDB succeeds.

**Every failure is reduced and bisected automatically.** A forty-line generated query that returns the wrong answer tells you nothing about why. The harness shrinks it and then bisects it against the optimizer passes, so the report names the pass that introduced the difference. That is the single highest-value piece of tooling in here, because most wrong answers come from a rewrite and finding out which one by hand costs an afternoon each.

## The four levels

Rather than one binary claim, four named levels, each independently verified and published with its own status.

| | | |
|---|---|---|
| 0 | data compatible | Reads and writes DuckDB files at full fidelity. The minimum useful claim, and the one that makes migration reversible. |
| 1 | query compatible | Level 0 plus the SQL dialect at a published weighted coverage. What most people mean when they say compatible. |
| 2 | API compatible | Level 1 plus the C API, so existing programs and language bindings work unmodified. |
| 3 | ecosystem compatible | Level 2 plus extensions loading and the wire protocol, with a per-extension status table. |

The weighting matters. A function nobody calls and a function in the top hundred by usage are the same amount of code and very different amounts of compatibility, so coverage is weighted by observed usage from a corpus of real queries rather than counted as a fraction of the function list. A function that differs on any tested input counts as not implemented, with no partial credit.

## Where the queries come from

Five sources, in increasing order of how much they find.

DuckDB's own `sqllogictest` corpus, which is tens of thousands of queries with expected results written by the people who know where the edges are. Running it is the highest-value first step and it is what M2 does.

The benchmark suites, which are modest in count and exercise real plan shapes.

A corpus of real queries scraped from public repositories and notebooks, which is what the usage weighting is computed from and which contains shapes nobody would have thought to generate.

Generated queries, from a grammar-based generator over a random schema, in the spirit of SQLancer and SQLsmith. This is where the volume is and where most of the wrong answers will come from.

Metamorphic queries, meaning transformations that have to preserve the result: a predicate rewritten to an equivalent form, a join reordered, an aggregation expressed two ways. These find bugs a random generator does not, because they produce pairs of queries whose relationship is known even when the correct answer is not.

## What this suite cannot do

It compares against DuckDB's behavior, so where DuckDB has a bug, matching it is what the suite rewards. That is mostly correct, because a user migrating depends on the behavior they have rather than the behavior that is right, but it means this is not a correctness suite. Correctness is covered separately in `spec/16-testing.md` and the two are not substitutes.

It cannot test what it does not generate. A grammar explores the space its author described, and the bugs that matter are often in the corners the author did not think of.

It cannot measure performance compatibility. A query that returns the right answer fifty times slower than DuckDB is a passing test and a failed product. That is [`tamnd/rudb-bench`](https://github.com/tamnd/rudb-bench), and the two should be read together.

## Building

```
git clone https://github.com/tamnd/rudb-compat
cd rudb-compat
cargo build --release
cargo test
```

The tests that need a DuckDB skip with a printed reason when there is not one, rather than failing, because a harness nobody can build without the thing it compares against is a harness nobody works on. CI installs one and gates on it, so the skip is for a laptop and not for the merge queue. If your DuckDB is not on `PATH`, point at it:

```
RUDB_COMPAT_DUCKDB=/path/to/duckdb cargo test
```

## License

Apache-2.0. See [LICENSE-APACHE](LICENSE-APACHE).

Not affiliated with, endorsed by or derived from DuckDB Labs.
