# rudb-compat

The DuckDB compatibility harness for [rudb](https://github.com/tamnd/rudb).

This is the apparatus that turns "compatible with DuckDB" from an assertion into a number that a machine computed. Differential execution against a real DuckDB binary, weighted function and statement coverage, storage format round trips in both directions, and C ABI conformance generated from DuckDB's own header.

It is a separate repository for three reasons. It depends on DuckDB and on rudb at the same time, which no crate that ships should. It should be runnable by someone who trusts neither. And a compatibility suite that lives inside the implementation it tests is a suite whose failures are easy to explain away.

The design is [`spec/14-rudb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/14-rudb-compat.md) in the rudb repository, and the levels it reports against are [`spec/12-duckdb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/12-duckdb-compat.md) section 12.8.

## Status

Early, and running. rudb executes queries now, so there is a conformance number, and it is small.

```
$ rudb-compat slt
4096 files, 15838 passed, 56291 failed, which is 22.0 percent of what was attempted

40732 records not attempted, by whose gap it is
      21358  excused   the file turned the record off itself
      19297  engine    something rudb does not have, which is the real gap
         11  harness   something this runner does not do, which is work here
         66  machine   something the machine this ran on does not have
```

That is DuckDB's own `sqllogictest` corpus at `v2.0-cyanoptera`, every `.test` file under `test/sql`, run against rudb on every commit and published on the run summary. The M2 exit criterion is above 60 percent, so the distance between those two numbers is the work list for the milestone.

The skips are printed underneath rather than folded into the percentage, and they are split by whose gap they are rather than totalled, because the four rows belong to four different people. Excused is nobody's problem. Engine is the rudb schedule and it is the row that goes down when rudb gets better. Harness is work in this repository and it is the row to watch, because it is the only one that can be removed without the engine improving at all, which makes it the easiest way to a pass rate that means nothing. Machine is the box the run happened on. A file skipped whole counts its records here too, which it did not until recently: several hundred files were in neither the skip count nor the denominator and no line of the report said so.

A `require` line is read the way DuckDB's own runner reads it, in `test/sqlite/sqllogic_test_runner.cpp`, and that is worth saying out loud because most of what the corpus requires is not a feature. `require skip_reload` tells DuckDB's runner not to reopen the database in the middle of the file, `require noforcestorage` tells it not to run the file in the mode that writes everything to disk first, and `require no_alternative_verify` turns off a debug mode. Upstream answers yes to every one of those on an ordinary build and runs the file. Reading them as a missing feature, which this runner did until recently, hid 579 files. The rate went from 23.0 percent to 21.6 percent when they came back, and that is the direction a number moves when it stops being computed over a corpus somebody quietly narrowed.

What is left behind a real `require` is mostly one of four things, and all four are now on the engine row with a count: an extension rudb does not have, of which `json`, `icu` and `httpfs` are the largest, a `vector_size` above rudb's 1024, a `block_size`, which rudb has none of because it keeps its tables in memory, and the `tpch` and `tpcds` generators.

A file stops where the database changes out from under it. Six hundred files in the corpus write something, say `restart`, and then check that what they wrote is still there, which is the whole point of the file. rudb keeps its tables in memory and cannot reopen a database, so running past the `restart` leaves the data exactly where it was and every check after it passes for precisely the reason the file was written to rule out. This runner now ends the file at the directive and puts the records after it on the engine row, named. That moved 4834 records onto that row, took 602 passes and 3573 failures out of the columns they did not belong in, and took the harness row from 1098 down to 11, because almost everything that row held was rudb having no storage rather than work in this repository. The one thing it does not end on is a `load` of a path under the corpus scratch directory that nothing has written to yet, which is an empty database however you open it, and that is most of the six hundred.

A directive the reader does not know is a file it cannot read at all, and there were 68 of those. Six more directives are read now. `statement maybe` takes a result block the way an error does, `include` reads the named file in where the line stood, `reset label` forgets a result two queries were told to share, `tags` names which files a run wants and carries nothing, `continue` ends the turn of the loop it fires on, and `test-env` is carried like the other directives about the world outside the file. That took the unreadable files from 68 to 18 and brought 4412 records onto the engine row, which is what those files were always going to say once anybody could read them.

A `statement maybe` is excused rather than passed, which is a deliberate difference from upstream and the reason is what the two numbers are for. DuckDB's runner asks whether the suite failed, and a `maybe` cannot fail, so passing it costs nothing there. This runner publishes a percentage, and a record that cannot fail is not evidence about the engine in either direction. There are 223 of those lines in 53 files and the loops around them make about 5970 records, and counting them was putting 4337 free passes into the number. One file, `catalog/dependencies/test_concurrent_alter.test`, is a hundred by ten loop around two of them and was 1783 passes on its own, which was nearly a tenth of everything that passed.

Each file gets a process of its own, with ten seconds on a statement and two gigabytes on the process. That is not for speed, although it does make the run use every core. It is because the corpus deliberately contains queries that are meant to be enormous, `range(10000000000000000)` and hundred million row cross joins that are there to be stopped. Both limits are handed to the engine, so the normal way one of those ends is rudb raising an error the report can count against the record that asked for it. This process keeps a clock of its own at twelve times the statement limit and a cap on how large the child may get, as a backstop for an engine that does not stop when it is asked to. Twelve, because a file with ten slow statements in it is a file that exists and killing it would throw away the outcome the limits were handed down to produce. A file that reaches one of those is killed and named in the report, and its records are counted in neither column, because a file that was cut off part way through has records nobody has an answer for. No file reaches one today, which is why there is no such line above.

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

## Driving the two shells

By default the harness runs DuckDB as a process and rudb as a library it links, which is the right comparison for almost everything and the wrong one for the claim the project actually makes. A drop in replacement is a claim about a binary: somebody has a script that runs `duckdb -c '...'` and they change the word `duckdb`. `--shell` runs both sides as binaries through one driver, so the only difference between the two is the path of the executable.

```
RUDB_COMPAT_RUDB=/path/to/rudb cargo run -- run corpus/clickbench.sql --shell
```

Both shells are driven in `.mode quote`, which is the one output mode that keeps a NULL, an empty string and the four letter string `NULL` apart, and which needs nothing from the engine under test. The types come from a second invocation that wraps the statement in a `DESCRIBE`. A statement `DESCRIBE` cannot wrap, `PRAGMA` for instance, still returns its rows, with the type column saying why there is no type rather than guessing one.

## What a record cost

The goal is not compatibility on its own. It is compatibility at ten times the speed and a tenth of the memory, which is three numbers rather than one, and `spec/sql/duckdb/01-what-compatible-means.md` section 1.7 in the engine repository puts the second and third of them in this harness rather than only in the benchmark suite. A benchmark suite measures the shapes somebody chose. The corpus measures the shapes nobody chose, and a feature that is fast on the benchmark and quadratic on the long tail only ever shows up in the second.

```
RUDB_COMPAT_RUDB=/path/to/rudb cargo run -- run corpus/clickbench.sql --shell --measure
```

That prints the three ratios of rudb over DuckDB, which are wall clock, processor time and peak resident set, each as a median with the quartiles beside it and never as a minimum, and then the five slowest records worst first. The worst list is the part to read, because an engine that is fast on the median and two hundred times slower on one shape has a bug rather than a distribution.

It needs `--shell`, because the shells are the only place both engines are processes reached the same way, and it needs GNU time on the machine, which rules out a macOS laptop and is fine because the runs happen on the Linux boxes anyway. A machine that cannot measure prints no ratios rather than ratios from a different measurement.

Four kinds of record are deliberately not timed and the reasons are in the code beside the rule. A record that failed on either side, because timing an error path measures the error path. A record the two engines disagreed about, because that is a ratio between two different amounts of work. A record under ten milliseconds on both engines, because below that it is mostly the cost of starting a shell. And every record on a shared machine, which is why server3 does not produce these at all.

## License

Apache-2.0. See [LICENSE-APACHE](LICENSE-APACHE).

Not affiliated with, endorsed by or derived from DuckDB Labs.
