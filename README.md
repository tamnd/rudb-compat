# rudb-compat

The DuckDB compatibility harness for [rudb](https://github.com/tamnd/rudb).

This is the apparatus that turns "compatible with DuckDB" from an assertion into a number that a machine computed. Differential execution against a real DuckDB binary, weighted function and statement coverage, storage format round trips in both directions, and C ABI conformance generated from DuckDB's own header.

It is a separate repository for three reasons. It depends on DuckDB and on rudb at the same time, which no crate that ships should. It should be runnable by someone who trusts neither. And a compatibility suite that lives inside the implementation it tests is a suite whose failures are easy to explain away.

The design is [`spec/14-rudb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/14-rudb-compat.md) in the rudb repository, and the levels it reports against are [`spec/12-duckdb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/12-duckdb-compat.md) section 12.8.

## Status

Early, and running. rudb executes queries now, so there is a conformance number, and it is small.

```
$ rudb-compat slt
4084 files, 12803 passed, 51427 failed, which is 19.9 percent of what was attempted
14306 skipped, of which 13391 the file turned off, 0 in a skipped section and 915 behind a directive the runner does not implement
7 files were cut off and are counted in neither column, which is listed above
```

That is DuckDB's own `sqllogictest` corpus at `v2.0-cyanoptera`, every `.test` file under `test/sql`, run against rudb on every commit and published on the run summary. The M2 exit criterion is above 60 percent, so the distance between those two numbers is the work list for the milestone. The skips are printed on the line underneath rather than folded into the percentage, because a record the file itself turned off with `skipif` and a record behind a directive this runner does not implement mean completely different things and neither of them is a pass.

Each file gets a process of its own, with ten seconds and two gigabytes on it. That is not for speed, although it does make the run use every core. It is because the corpus deliberately contains queries that are meant to be enormous, `range(10000000000000000)` and hundred million row cross joins that DuckDB stops with a memory manager and a timeout that rudb does not have yet. Run in one process, a single one of those takes the whole run with it and CI publishes nothing at all. A file that goes over either limit is killed and named in the report, and its records are counted in neither column, because a file that was cut off part way through has records nobody has an answer for.

This half of the harness needs no DuckDB on the machine. A `.test` file already carries what every statement is supposed to produce, which is what makes it something CI can run on every commit in fourteen seconds rather than a nightly job whose result nobody can attribute to a commit. The corpus is fetched rather than committed, by `rudb-compat vendor`, into `target/corpus` at the pinned ref.

The other half is the differential loop, which does need a binary, and which is pointed at the question the vendored grammar exists to answer: whether a piece of text is SQL. rudb vendors DuckDB's PEG grammar and generates its rule table from it, precisely so that the dialect cannot drift, and this is the check that the vendoring worked. Every statement in a corpus goes to both engines and the two answers are compared.

```
$ rudb-compat parse corpus/m0.sql
62 of 62 agreed, which is 100.0 percent
```

Errors are compared too, by error kind, so a statement both engines reject counts as agreement only when they reject it as the same kind of error. `--strict-messages` tightens that to the first line of the message.

The full path is there behind `run` rather than `parse`: two engines, full result sets, column names and types, and a per-statement ordering rule that comes from rudb's own AST. `rudb-compat query 'SELECT 1'` runs one statement on both and prints what differs.

The DuckDB side is a real binary built at a named commit, found on `PATH` or named by `RUDB_COMPAT_DUCKDB`. `rudb-compat duckdb` prints which one it found and whether it is the commit rudb vendors its grammar from.

```
$ rudb-compat duckdb
binary   /home/tam/.local/bin/duckdb
version  v2.0.0-dev84237 (Development Version) cc7e7bac7f
commit   cc7e7bac7f
pinned   v2.0 at cc7e7bac7f

This is the commit the grammar is vendored from.
```

The commit is the check rather than the version. `v2.0-cyanoptera` is a development branch that moves every day, so two binaries can both say `v2.0.0-dev` and disagree about the language, and a version string comparison passes on both. `duckdb --version` prints the short hash as the last word of the line, so the comparison is against that. Same version and a different hash is its own reported state and not a pass.

There is no published v2.0 binary to download, so the pinned one is built from source. `scripts/oracle` in the rudb repository does that on a named machine: it clones DuckDB, checks out the vendored commit, builds the CLI with the parquet and json extensions in it, and installs it into `~/.local/bin` under a name carrying the version and the hash, leaving whatever DuckDB was already there alone. server1, server2, server3 and gamingpc all have it.

A release binary is still allowed, because most machines have one and a run against it is better than no run. Every report produced that way carries a line saying it is not the pinned commit, and `rudb-compat duckdb --pinned` fails outright rather than printing it, which is how a machine that publishes numbers refuses to publish the wrong ones.

`corpus/dialect.sql` is the file where the difference showed. Against a 1.5.5 it came back at 9 of 11, and both differences were upstream moving rather than rudb being wrong: `ORDER BY x ASCENDING` is `AscendingOrder <- 'ASC' / 'ASCENDING'` in the vendored grammar and a syntax error on 1.5.5, and `[1, 2] <-> [3, 4]` is array distance on 1.5.5 and cannot be one token under the v2.0 tokenizer, where a hyphen is a single byte operator that never joins an operator run. Against the pinned binary it is 11 of 11, and the test asserts the full agreement there and the two known differences under a fallback.

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

DuckDB's own `sqllogictest` corpus, which is tens of thousands of queries with expected results written by the people who know where the edges are. Running it is the highest-value first step and it is the one thing here that is already wired up, under `rudb-compat slt`.

There is a second, much smaller corpus under `corpus/slt`, which is ours rather than upstream's, written in the same format, and which covers what rudb is supposed to be able to do today. `cargo test` runs it and every record in it has to pass. The upstream corpus is a measurement that goes up over the milestones and the committed one is a gate that is never allowed to go down, and confusing the two is how a conformance number ends up meaning nothing.

The benchmark suites, which are modest in count and exercise real plan shapes. ClickBench is here already, in `corpus/clickbench.sql` as the forty three queries are written and in `corpus/clickbench-settled.sql` as the same queries with the tie at the cut broken, and `tests/clickbench.rs` runs both against a real DuckDB over the same Parquet file. Thirty two of those queries end in a `LIMIT` over a `GROUP BY` whose `ORDER BY` does not fix a total order, so two engines can return different rows and both be right, which is why there are two files rather than one: the first asks whether the engines ever differ by more than which tied row came back, and the second adds the grouping keys to the sort and asks whether the answers match exactly. A query in the settled file is not a ClickBench query and no timing taken on one means anything, and the timings live in `tamnd/rudb-bench` in any case.

That one needs the corpus, which is fourteen gigabytes and is not in this repository, so it skips with the reason printed unless somebody points `RUDB_COMPAT_HITS` at the file or `RUDB_BENCH_DATA` at the directory holding it. Asking for it by hand is deliberate. A machine that happens to have the file in the usual place should not have `cargo test` turn into a run of a hundred million rows through forty three queries twice, which takes hours and is a decision somebody makes rather than one they discover. The hundred thousand row partition works too and is what most runs use, and it answers a weaker question, because ties at a `LIMIT` are rarer on a smaller file.

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
