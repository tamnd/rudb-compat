# rudb-compat

The DuckDB compatibility harness for [rudb](https://github.com/tamnd/rudb).

This is the apparatus that turns "compatible with DuckDB" from an assertion into a number that a machine computed. Differential execution against a real DuckDB binary, weighted function and statement coverage, storage format round trips in both directions, and C ABI conformance generated from DuckDB's own header.

It is a separate repository for three reasons. It depends on DuckDB and on rudb at the same time, which no crate that ships should. It should be runnable by someone who trusts neither. And a compatibility suite that lives inside the implementation it tests is a suite whose failures are easy to explain away.

The design is [`spec/14-rudb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/14-rudb-compat.md) in the rudb repository, and the levels it reports against are [`spec/12-duckdb-compat.md`](https://github.com/tamnd/rudb/blob/main/spec/12-duckdb-compat.md) section 12.8.

## Status

Early, and running. rudb executes queries now, so there is a conformance number, and it is small.

```
$ rudb-compat slt
4096 files, 15920 passed, 56548 failed, which is 22.0 percent of what was attempted

40827 records not attempted, by whose gap it is
      21358  excused   the file turned the record off itself
      19403  engine    something rudb does not have, which is the real gap
          0  harness   something this runner does not do, which is work here
         66  machine   something the machine this ran on does not have
```

That is DuckDB's own `sqllogictest` corpus at `v2.0-cyanoptera`, every `.test` file under `test/sql`, run against rudb on every commit and published on the run summary. The M2 exit criterion is above 60 percent, so the distance between those two numbers is the work list for the milestone.

The skips are printed underneath rather than folded into the percentage, and they are split by whose gap they are rather than totalled, because the four rows belong to four different people. Excused is nobody's problem. Engine is the rudb schedule and it is the row that goes down when rudb gets better. Harness is work in this repository and it is the row to watch, because it is the only one that can be removed without the engine improving at all, which makes it the easiest way to a pass rate that means nothing. Machine is the box the run happened on. A file skipped whole counts its records here too, which it did not until recently: several hundred files were in neither the skip count nor the denominator and no line of the report said so.

A `require` line is read the way DuckDB's own runner reads it, in `test/sqlite/sqllogic_test_runner.cpp`, and that is worth saying out loud because most of what the corpus requires is not a feature. `require skip_reload` tells DuckDB's runner not to reopen the database in the middle of the file, `require noforcestorage` tells it not to run the file in the mode that writes everything to disk first, and `require no_alternative_verify` turns off a debug mode. Upstream answers yes to every one of those on an ordinary build and runs the file. Reading them as a missing feature, which this runner did until recently, hid 579 files. The rate went from 23.0 percent to 21.6 percent when they came back, and that is the direction a number moves when it stops being computed over a corpus somebody quietly narrowed.

What is left behind a real `require` is mostly one of four things, and all four are now on the engine row with a count: an extension rudb does not have, of which `json`, `icu` and `httpfs` are the largest, a `vector_size` above rudb's 1024, a `block_size`, which rudb has none of because it keeps its tables in memory, and the `tpch` and `tpcds` generators.

A file stops where the database changes out from under it. Six hundred files in the corpus write something, say `restart`, and then check that what they wrote is still there, which is the whole point of the file. rudb keeps its tables in memory and cannot reopen a database, so running past the `restart` leaves the data exactly where it was and every check after it passes for precisely the reason the file was written to rule out. This runner now ends the file at the directive and puts the records after it on the engine row, named. That moved 4834 records onto that row, took 602 passes and 3573 failures out of the columns they did not belong in, and took the harness row from 1098 down to 11, because almost everything that row held was rudb having no storage rather than work in this repository. The one thing it does not end on is a `load` of a path under the corpus scratch directory that nothing has written to yet, which is an empty database however you open it, and that is most of the six hundred.

A directive the reader does not know is a file it cannot read at all, and there were 68 of those. Six more directives are read now. `statement maybe` takes a result block the way an error does, `include` reads the named file in where the line stood, `reset label` forgets a result two queries were told to share, `tags` names which files a run wants and carries nothing, `continue` ends the turn of the loop it fires on, and `test-env` is carried like the other directives about the world outside the file. That took the unreadable files from 68 to 18 and brought 4412 records onto the engine row, which is what those files were always going to say once anybody could read them.

A `statement maybe` is excused rather than passed, which is a deliberate difference from upstream and the reason is what the two numbers are for. DuckDB's runner asks whether the suite failed, and a `maybe` cannot fail, so passing it costs nothing there. This runner publishes a percentage, and a record that cannot fail is not evidence about the engine in either direction. There are 223 of those lines in 53 files and the loops around them make about 5970 records, and counting them was putting 4337 free passes into the number. One file, `catalog/dependencies/test_concurrent_alter.test`, is a hundred by ten loop around two of them and was 1783 passes on its own, which was nearly a tenth of everything that passed.

Every file in the corpus parses now, and the last 18 were two more readings rather than two more directives. A result block is kept as the lines the file wrote and split into values against the result that came back, because a line is a whole row in some files and one value in others and nothing in the file says which. DuckDB decides it in `result_helper.cpp` from the row count the engine returned, falls back to the every line has a tab guess only when that does not fit, and fails the record when what is left does not divide by the column count. Sixteen files were being thrown away whole for a block that does not divide, which is one record that cannot be right and not a file with no outcome. And a line that ends a record is an empty line and not a blank one, which is what `test_bar.test` turns on: it draws bar charts, its first bar is the empty one, and a reader that stops at eighty spaces reads the second bar as a directive. The same reading keeps a statement whose entire body is a Unicode space, which is what `invisible_spaces.test` is about.

A result block can also be a digest rather than a list of values, and section 9.3.1 of the harness spec settles what to do about that. Of the 34329 `query` records in the corpus, 231 store one, in 32 files, and 212 of those are in `.test_slow` files that an ordinary run leaves out, so a run sees 19 hashed records in 15 files. There is no `hash-threshold` line anywhere in the corpus at the pin, so the mechanism the format is named after is not reachable from these files at all. Nineteen records cannot move a percentage printed to one decimal place, so the digest match stands as the outcome for them rather than a run of the record against a live binary, which would turn a corpus run that needs no DuckDB into one that does. What the rule protects against is us: nothing here hashes a result it could have compared, no corpus written here stores a digest, and the harness gets no `hash-threshold` of its own. A digest is read because 32 of DuckDB's files contain one and for no other reason, and the count of values is checked before the digest because that is the one thing the format gives besides the digest. When one of them does fail, `expected 100000 values hashing to X and got Y` is a single bit and there is nothing in it to reduce, so getting the values back means putting the record to the live binary, and that happens where a person is already waiting rather than on every commit.

The harness row is zero. It was 1098 before any of this and the last 11 records on it were the settings a file makes about itself, so those are carried out now rather than stepped over. `set ignore_error_messages` names errors that mean the rest of the file is not worth running, which is how a file that reaches out to a network says so, and when one fires the file stops and the rest of it is excused rather than failed. `set always_fail_error_messages` names errors no expected error may be satisfied by, and it starts with `INTERNAL` in it with no line in any file asking for that, because a `statement error` that is satisfied by the engine breaking an invariant of its own is the one place where a passing record is worse than a failing one. `set variable` and `test-env` name something the SQL below writes as `{name}`. `set seed` becomes `SELECT setseed(n)`, and rudb does not have that function yet, so 106 records in 23 files now say that on the engine row instead of being scored against a sequence of random numbers this run cannot produce. `sleep` sleeps.

Each file gets a process of its own, with ten seconds on a statement and two gigabytes on the process. That is not for speed, although it does make the run use every core. It is because the corpus deliberately contains queries that are meant to be enormous, `range(10000000000000000)` and hundred million row cross joins that are there to be stopped. Both limits are handed to the engine, so the normal way one of those ends is rudb raising an error the report can count against the record that asked for it. This process keeps a clock of its own at twelve times the statement limit and a cap on how large the child may get, as a backstop for an engine that does not stop when it is asked to. Twelve, because a file with ten slow statements in it is a file that exists and killing it would throw away the outcome the limits were handed down to produce. A file that reaches one of those is killed and named in the report, and its records are counted in neither column, because a file that was cut off part way through has records nobody has an answer for. No file reaches one today, which is why there is no such line above.

Those processes can be spread over machines as well as over cores, which is what `--shard k/n` does. A shard takes every kth file of the sorted list, round robin the way `cargo nextest --partition` does it rather than in contiguous blocks, because the list is sorted by path, so a block is a directory, and `copy` and `aggregate` are not the same amount of work. `--out` writes what the shard found in the form `rudb-compat merge` reads back, and `scripts/shard` is the whole loop over server1, server2, server3 and the gaming machine with the merge at the end. A merged run prints the corpus summary and no page, because a page carries the provenance of the one machine it was measured on and this has four. Every shard writes down the corpus commit it ran over and `merge` refuses a set of shards that do not agree on it, which is not a precaution but a bug that already happened: the corpus is fetched per machine, the upstream ref is a branch that moves, and the first real run of this had two machines ten files apart and merged into a pass rate that was neither of theirs. It also buys nothing today, and that is worth knowing before anybody runs it: the corpus is 364 processor seconds of work over 4106 files, one file is 125 of them and seven files are 310 of the 364, so the run takes 121 seconds on 32 cores against 143 on six and no number of machines gets below the slowest single file. It is here because it is fifty lines and because the file at the top of that list is one join operator away from going. Section 9.8 of the harness spec has the measurements and the reason two machines do not currently print the same pass rate.

This half of the harness needs no DuckDB on the machine. A `.test` file already carries what every statement is supposed to produce, which is what makes it something CI can run on every commit in fourteen seconds rather than a nightly job whose result nobody can attribute to a commit. The corpus is fetched rather than committed, by `rudb-compat vendor`, into `target/corpus` at the pinned ref.

The numbers and the things they depend on go on one page, written by `rudb-compat report` and never edited by hand. It is the same corpus run as `slt` with the page written out of it: the pass rate with the records nobody attempted beside it, all nine failure reasons including the ones at zero, and a provenance block carrying the rudb commit, the commit of this harness and whether its tree was clean, the DuckDB pin and the md5 of the binary that was on the machine, the corpus commit, the machine and the seed. Section 11.2 of `spec/sql/duckdb/11-the-number.md` asks for eleven numbers and the five error levels are the only ones nothing anywhere computes yet, so they are listed under a heading of their own with what would produce them, along with anything this particular machine has not measured, rather than being left off the page or printed as a zero. A zero is a measurement and a missing measurement is not. Each run writes a new file under `target/report` and appends one row to `series.tsv` beside it, because section 11.4 says the page is allowed to go down and a page that is overwritten cannot be seen going down.

Statement coverage is the other level two number on that page and this run does compute it, out of the same records the pass rate comes out of. Section 1.2 says a statement kind counts when every record of it either works or fails the way the file said it fails, so that is a question about a corpus run rather than about a list of statements somebody ticked. Every record that ran is handed to the parser and sorted into whichever of the 36 alternatives of the `Statement` rule matched, which is the vendored grammar's own list of names, so the denominator is read off the grammar table at run time and moves when the pin moves. The parser does the sorting and not the first word of the statement, because `WITH x AS (...) INSERT INTO t SELECT * FROM x` starts with WITH and is an insert.

What it says today is zero of 36, with 35 kinds that have at least one record that did something else and one, `UpdateExtensionsStatement`, that no record in the corpus is. That is the strict rule doing what it was written for rather than an engine that answers nothing, since 7293 of the 40634 select records pass and one failure in a kind is enough to take the whole kind out. So the per kind table goes on the page beside the number, with the records, the passes and the failures of each, because early on that table is the thing to read and the single number is the thing to watch. The 447 records the vendored grammar does not accept are counted on their own rather than dropped, because most of those are the corpus writing a syntax error on purpose and the rest are a hole in our grammar or tokenizer, which is a count that goes up without the corpus changing.

The function coverage number is on that page and it is not measured by that run. The sweep behind it takes about forty minutes and the corpus run takes minutes, so a full sweep against the pinned binary appends one row to `coverage.tsv` beside the pages and the page reads the most recent row back. The page says when and where the sweep ran and against which engine build, and says out loud when that is not the build and the machine the corpus numbers on the same page came from, because two numbers from two runs are two facts and not one measurement. A machine that has never run a sweep has nothing to carry, and its page names function coverage under the numbers nothing measures yet, which is what every page said before the sweep existed. A sweep over one name, or against a DuckDB that is not the pin, is printed and not recorded, since a row that looks like the published one and is about something narrower is worse than no row.

The function coverage number needs a denominator and `duckdb_functions()` is it. `rudb-compat functions` reads that table off the pinned binary and prints what is in it, which today is 3245 overloads over 1159 names, and then prints how many of those rows the generator can build calls for and every type it cannot, largest first. The generator takes one overload and produces one call per value in a boundary set for each parameter in turn with the others held at an ordinary value, then a null in every position on its own, then a wrong type in every position to capture the error. `rudb-compat functions upper` prints the calls for one name rather than running anything, which is how a case gets read before a run puts thousands of them to two engines.

The boundary sets are the only judgement in the whole scheme, so they are one table in one file rather than a rule spread over a generator. Minimum, maximum, zero, one and minus one for the integers. Empty, embedded null, the twelve byte point where a string stops being stored beside its pointer, long, combining and an emoji for `VARCHAR`, with the invalid UTF-8 case on `BLOB` because DuckDB refuses to put those bytes in a string at all. Zero, negative zero, both infinities, NaN and the subnormal boundary for `DOUBLE`. The width and scale extremes for `DECIMAL`. The epoch, the infinities and the leap day for the timestamps. A test puts every one of those values to a live DuckDB and fails when one of them stops constructing, because a literal that does not parse is not a boundary, it is a call both engines reject for a reason that has nothing to do with the function under test. A type with no boundary set produces no calls rather than a guess, and the count of rows it leaves out is printed rather than quietly dropped from a percentage.

`rudb-compat coverage` runs those calls on both engines and scores them. The rule is section 1.2 of `spec/sql/duckdb/01-what-compatible-means.md` read strictly: a function that is implemented but differs from DuckDB on any tested input counts as not implemented, so an overload passes only when every call generated for it agreed, and a name passes only when every overload of that name passed. An engine with twenty nine of the thirty `date_part` overloads has not got `date_part`. The number is over the 1159 names in the catalog and not over the ones that were tested, because dividing by what was tested is how a coverage number goes up by testing less. Three outcomes are printed and not two, since passed, failed and never tested are three different facts and a report that adds the last two together hides the one it should be showing.

A call that makes an engine panic is caught and counted rather than allowed to end the run. rudb is linked into this process rather than run beside it, so a panic in rudb is a panic here, and the first full sweep lost forty minutes of work to one bad call. Now the panic is recorded as a difference against the overload that caused it, the engine that came apart is reset before the next call, and the count of crashes is printed apart from the count of failures, because a crash and a wrong answer are both one failing call and they are not the same news. The first one it found was `SELECT regexp_extract('a', 'a', -1)`, which is tamnd/rudb#495. A call that never comes back is stopped the same way and for the same reason. DuckDB is a subprocess here, so it gets ten seconds and then it is killed, which the sweep learned by generating `sleep_ms(9223372036854775807::BIGINT)` and waiting an hour behind it.

A name can be never tested for three reasons and each is counted on its own. A parameter type with no boundary set. A kind the generator does not build calls for, which today is everything that is not scalar. And a function whose answer depends on something outside the call, which is `random`, `now`, `version`, the one argument `age` and about twenty others, where two engines that both work perfectly disagree every time. That list was read off the catalog and it is a best effort rather than a proof, and the run itself is what finds what is missing from it, because a function that fails every generated call and works by hand is what volatility looks like from the outside.

The other half is the differential loop, which does need a binary, and which is pointed at the question the vendored grammar exists to answer: whether a piece of text is SQL. rudb vendors DuckDB's PEG grammar and generates its rule table from it, precisely so that the dialect cannot drift, and this is the check that the vendoring worked. Every statement in a corpus goes to both engines and the two answers are compared.

```
$ rudb-compat parse corpus/m0.sql
62 of 62 agreed, which is 100.0 percent
```

Errors are compared too, by error kind, so a statement both engines reject counts as agreement only when they reject it as the same kind of error. `--strict-messages` tightens that to the first line of the message.

The full path is there behind `run` rather than `parse`: two engines, full result sets, column names and types, and a per-statement ordering rule that comes from rudb's own AST. `rudb-compat query 'SELECT 1'` runs one statement on both and prints what differs.

A statement that differs goes to `rudb-compat reduce`, which shrinks it until nothing else can come out of it and it still fails the same way. Not until it still fails: a cut that turns a wrong answer into a parse error has thrown one bug away and found another, so every step is held to the difference the statement started with rather than to the statement failing at all. The moves are the ones a person makes by hand. Drop a clause, drop everything from a clause to the end, drop an item from a list, drop one side of an `AND`, replace a bracketed group by a constant, empty a string, shrink a number, delete a run of tokens. Each candidate goes through rudb's parser before either engine sees it, which throws most of them away for nothing, and there is a budget on how many reach the engines because every one of those is two runs and one of them is a subprocess.

```
$ rudb-compat reduce "SELECT a, b, date_part('microsecond', TIME '12:34:56.789') FROM (SELECT 1 AS a, 2 AS b) WHERE a = 1 ORDER BY a"
keeping this alive, and a step that loses all of it is not kept
    only the right engine errored, Binder Error

SELECT date_part('microsecond', TIME '12:34:56.789')

110 bytes down to 52, in 5 steps out of 38 candidates
```

Point it at a file with more than one statement in it and it reduces every one of them and groups what comes out. That is the shape a real run has. A corpus sweep produces thousands of failing statements and they are not thousands of bugs, they are a few dozen bugs each found by every query that happens to touch one, and a list with one line per failure is a list nobody reads twice. The key is the reduced statement itself, byte for byte, with no normalising on top, which works because the reduction has already done the normalising that matters: literals are down to `''` and `0`, every clause that did not contribute is gone, and every column the failure did not need has been dropped. Two queries that fail for the same reason arrive at the same text. Folding `SELECT f(a)` and `SELECT f(b)` together would be a judgement about what makes two bugs one bug, and this does not make it.

```
$ rudb-compat reduce --file failures.sql
412 statements, 6 agreed, 406 differed, 3 distinct cases

   301  6f1b1c0d2e9a4f5b8c7d6e5f4a3b2c1d
        SELECT date_part('', TIME '')
        only the right engine errored, Binder Error
        found in: SELECT a, date_part('microsecond', TIME '12:34:56.789') FROM t
```

It cuts on tokens and clause boundaries rather than on the parse tree, and the reason is worth saying rather than hiding. rudb parses into an arena AST, but the nodes carry no spans and there is no printer, so from outside the parser there is no way to say which bytes a node came from or to turn a node back into SQL. Tokens and bracket depth get most of the way there, because the cuts that matter are clause boundaries and items of a list at a depth and both of those are visible without a tree. Cutting on nodes needs one of those two things in rudb first, which is tamnd/rudb#519.

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

**Every failure is reduced and bisected automatically.** A forty-line generated query that returns the wrong answer tells you nothing about why. The harness shrinks it and then bisects it against the optimizer passes, so the report names the pass that introduced the difference. That is the single highest-value piece of tooling in here, because most wrong answers come from a rewrite and finding out which one by hand costs an afternoon each. The shrinking half is `rudb-compat reduce` and it works. The bisector waits on rudb having a setting that turns one optimizer pass off.

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

A corpus of real queries scraped from public repositories and notebooks, which is what the usage weighting is computed from and which contains shapes nobody would have thought to generate. The first eight hundred of those are here already, because upstream's benchmark suite is a pile of queries somebody wrote to get an answer rather than to break an engine, and `rudb-compat queries` reads them and says which of the catalog they call.

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

## Two oracles per record

Every file in the upstream corpus already says what each statement is supposed to produce, and the pinned DuckDB binary is on the machine as well, so there are two oracles for the same record and not one. Running both and lining them up record by record gives four answers rather than two, and the fourth one is the reason this exists.

```
RUDB_COMPAT_RUDB=/path/to/rudb cargo run --release -- oracles target/corpus/test/sql/cast
```

```
32 files, 1037 records both oracles answered

     437  agreed      both engines did what the file says
     357  engine gap  rudb did not and the pinned binary did, which is the real gap
     243  stale file  neither did, so the file is stale or the pin moved past it
       0  harness bug rudb did and the binary did not, so this runner is wrong
```

A stale file is a note and nothing more, because a file the pinned binary itself cannot pass is not a statement about rudb. An engine gap is the real work and it is what the ordinary corpus run already reports. A harness bug is the interesting one: rudb passed a record the binary failed, which means we are running the record differently from the way the file means it, and that pass is one this harness has not earned. A run with one oracle cannot tell that case apart from a genuine pass, and it is the case that quietly inflates a pass rate. So `oracles` prints those records in full and exits nonzero on them, and on nothing else.

It always drives both engines as shells that remember what they were told, rather than taking `--shell` as a choice. A test file makes a table and then asks questions about it, so a driver that forgets between statements fails every record after the first one for a reason that has nothing to do with the record. The library pair forgets on one side only, because the linked rudb keeps a connection open and the DuckDB driver spawns a fresh in memory process per statement, so every record after the first `CREATE TABLE` passes on rudb and fails on the binary and the whole corpus reads as a harness bug. Two bare shells forget on both sides instead, which cancels out into a stale file rather than a harness bug, and a run where four fifths of the records are the pin failing to find a table it was never told about is a run that measures nothing. Sessions replay the statements that left something behind in front of the next one, which is the only arrangement where a whole file means anything.

## Upstream's own generator

DuckDB ships a `sqlsmith` extension and runs it against itself as a public fuzzer, so there is a generated query source here with no generator to write. It produces deep joins, correlated subqueries in select lists, `tablesample` clauses and casts to types nobody would write, which is a fair description of the part of the grammar a corpus of hand written tests does not reach.

```
RUDB_COMPAT_SQLSMITH=/usr/local/bin/duckdb-v1.5.1 rudb-compat sqlsmith --count 200 --seed 1
```

The generator is not the pinned binary, and that is not a workaround. Extension binaries are published for releases and the pin is a development commit, so `INSTALL sqlsmith` against the pin is a 404. It does not matter, because what comes out of a generator is SQL text and text has no version. Every statement it writes is still put to the pinned binary and to rudb, which is where the version matters.

What does depend on the generator is the shape of what it writes, since sqlsmith builds queries out of the catalog and the function list of the database it is running in. So the tables are fixed in the code rather than taken from whatever database somebody points it at.

A query both engines refused is counted apart from the rest. The generator is a different build from the pin and a share of what it produces names something the pin has never heard of. The pin refuses those, rudb refuses them for a different reason, and the two reasons side by side look exactly like an error kind divergence while saying nothing about compatibility. In a corpus somebody wrote on purpose, two different error kinds is a real finding. Here it is the generator talking to itself, and counting it would make the largest group on the page the one group nobody can act on.

The seed is printed whether it was given or not, because a generated run whose findings cannot be replayed is a generated run whose findings do not get fixed.

## Our own generator

The other half of that. sqlsmith writes queries out of a catalog, so what it produces is realistic and narrow, and all of it is `SELECT`. This one walks the same 1088 rule grammar table the matcher walks, in the other direction, so what it produces is unrealistic and wide: every statement kind the grammar has, in proportion to how cheaply the grammar can write each one, reaching clauses nobody has ever typed.

```
rudb-compat grammar --count 2000 --seed 1
rudb-compat grammar --count 2000 --seed 1 --rule SelectStatement
```

It only asks whether each statement parses. A walk of the whole grammar writes `DROP`, `ATTACH`, `EXPORT DATABASE`, `COPY t TO 'out.csv'` and `INSTALL`, so a mode that ran what it wrote would be a mode that writes files into whatever directory somebody started it in. Both engines are asked the parse question and nothing else, which also means no session and no catalog, and that is what makes the answer a fact about the two grammars rather than about two schemas.

There are two directions and they are reported apart. A statement DuckDB parses and rudb does not is the gap, and it is a query somebody cannot run at all. A statement rudb parses and DuckDB does not is a dialect we invented, which is a smaller problem and still a real one, because a parser that accepts more than the thing it is compatible with will bind something it should have refused.

The first runs, 2000 statements each against the pinned binary, found nothing in the first direction and a lot in the second. From `Statement`: 83 neither engine parses, 1617 both parse, 0 in the gap, 300 in 122 groups where rudb parses and DuckDB does not. From `SelectStatement`: 262 neither parses, 474 both parse, 0 in the gap, 1264 in 159 groups the other way.

The empty gap is not a compatibility result and it should not be read as one. This generator writes from the same table the matcher reads, so almost everything it writes is text our own parser accepts by construction, and a source that cannot write a statement rudb refuses cannot measure what rudb refuses. What the gap column is worth here is as a check on that construction: a nonzero number would mean the generator and the matcher disagree about the grammar they share, which would be a bug in one of them. The level two number comes from the corpus and from sqlsmith, which write statements this one cannot.

The second direction is what the mode is actually for, and the groups say the same thing over and over: DuckDB checks things while it parses that its own grammar allows. `SELECT` with no select list, a CTE body that is a `CHECKPOINT`, an empty subscript, two aliases on one table reference, a window name used twice. Those are in the grammar, DuckDB's transformer refuses them, and rudb has no transformer yet so it accepts all of them. Most of that surface closes with the binder rather than with the parser, which is why these are counted and printed rather than filed one at a time.

## An oracle with one engine in it

Every other check here needs a DuckDB on the machine to say what the answer should have been. This one does not, because what it tests is a property of SQL rather than an agreement between two engines. Take a predicate `p`. Every row a query can see is in exactly one of three buckets: the rows where `p` is true, the rows where it is false, and the rows where it is neither of those because something in it was NULL. So the rows of `WHERE p`, `WHERE NOT (p)` and `WHERE (p) IS NULL` put together are the rows of the query with no predicate on it at all, and that has to hold for every predicate an engine will accept. The paper is Rigger and Su, "Finding Bugs in Database Systems via Query Partitioning", OOPSLA 2020, where it is called ternary logic partitioning.

```
cargo run --release -- tlp --count 20000 --seed 1
```

```
ternary logic partitioning over the rows against rudb from git
seed 1, which is what replays this run exactly

20000 predicates, 0 the engine would not run, 20000 left
20000 of those split into three parts that add back up to the whole, 16378 of which divided the rows rather than putting everything in one part
```

Three things make it worth having beside the differential loop. It runs on a machine with no DuckDB on it, which makes it a check every commit can afford. It localises, because the three parts are the same query four times over and the only thing that moved is the predicate, so a failure names the predicate rather than the query. And it tests three valued logic directly, which is the part of SQL a young engine gets wrong quietly, since a predicate that returns false where it should return unknown gives the right answer for almost every query anybody writes by hand and the wrong answer for the rest.

The three parts are run as three queries and put back together in the harness rather than written as one `UNION ALL` the way the paper does it. It is the same property and it fails in fewer places, since a bug in `UNION ALL` would break every case in a run and say nothing about any predicate. The one statement form is printed beside every failure anyway, because that is what a person pastes into a shell.

The table is a constant in the source rather than something generated from the seed, so a run is replayed by the seed alone and the rows can be chosen for the job instead of sampled. What the job needs is rows that straddle every boundary a generated predicate might draw, and one row per column whose value in that column is NULL while the rest of the row is ordinary, because that is the row a predicate on one column is unknown about and everything else in the same expression is not.

The count of predicates that divided the table is the number to read second. A predicate that puts every row in one part passed without testing anything, so a generator that drifts into writing those would produce a run that is green and empty, and the two numbers side by side is what makes that visible.

rudb passes every predicate this has generated so far and refuses none of them, and the pinned binary passes the same predicates, which says the generator is writing SQL that means what it looks like. That leaves the failure path untested by any real run, so it is tested against an engine written for the purpose that answers every partition with the rows where the predicate is true, which is exactly what an engine that treats unknown as false does.

## The same split under an aggregate and over the groups

The paper has three forms and the one above is the first of them. `--form` picks which one runs, and the other two are not decoration: the `WHERE` form only ever compares row counts, so a bug that keeps the right number of rows and gets the values wrong walks straight through it.

The aggregate form puts the same three predicates under the same aggregates. `SELECT count(*), sum(i), min(i), max(i), sum(j), min(j), max(j) FROM t WHERE (p)` and the same for `NOT (p)` and `(p) IS NULL`, and the three answers have to fold back to the aggregates over the whole table, with the counts and the sums added, the minimums reduced to the smallest and the maximums raised to the largest. The folding happens here rather than in a `UNION ALL` inside a subquery the way the paper writes it, because a subquery is a feature and an oracle that only works where that feature works stops working exactly when it would be useful. That choice is why the select list is seven integer aggregates and not more: summing the `DOUBLE` column here would have the harness add floats in an order the engine did not, and a `min` over the text column would have the harness decide the collation, and in both cases a disagreement would say more about the harness than about the engine.

The `HAVING` form groups by an expression and partitions on the `HAVING` clause instead of the `WHERE`, so what is divided is the groups rather than the rows. The predicates are made of aggregates over the group, `count(*)`, `count` of a column, `sum` of a number, `min` and `max` of anything and `IS NULL` over those, plus the grouping expression itself, which is the only column a `HAVING` clause is allowed to name on its own. The unknown comes from somewhere different than it does in a `WHERE` clause: `max(ts) > DATE '2020-01-01'` is unknown for a group whose column was NULL in every row, and nothing in the first form reaches that.

```
cargo run --release -- tlp --form aggregate --count 20000 --seed 1
cargo run --release -- tlp --form having --count 20000 --seed 1
```

```
ternary logic partitioning under an aggregate against rudb from git
seed 1, which is what replays this run exactly

20000 predicates, 0 the engine would not run, 20000 left
20000 of those split into three parts that add back up to the whole, 16378 of which divided the rows rather than putting everything in one part
```

```
ternary logic partitioning over the groups against rudb from git
seed 1, which is what replays this run exactly

20000 predicates, 0 the engine would not run, 20000 left
20000 of those split into three parts that add back up to the whole, 8733 of which divided the groups rather than putting everything in one part
```

Both are eight seconds. Three hundred of the same predicates go to the pinned binary in a minute each and hold there too, which is the check that says the generator writes SQL an engine means the same thing by.

The `HAVING` number is the one worth looking at twice. Eight thousand of twenty thousand divided the groups, against sixteen thousand for the other two forms, and that is honest rather than a problem: a predicate over `count(*)` on a table of thirteen rows grouped a dozen ways puts every group on one side of it more often than a predicate over a column does. It is the number that would say something had gone wrong with the generator if it fell much further.

The first real thing this form found was a wrong answer in rudb, filed as tamnd/rudb#540 and fixed in 0.3.10. `SELECT x, count(*) FROM t WHERE x IS NULL GROUP BY x` over two nulls and a value answered with two groups of one instead of one group of two, because a filter that drops a row hands its columns on as dictionary vectors, a dictionary keeps its nulls in the values its codes point at, and the grouping table asked the wrong level whether a row was null. Five cases in two hundred failed on it at the first seed this form ever ran.

## The optimizer against itself

The same predicates, asked a different way. `SELECT * FROM t WHERE (p)` is the query an optimizer works on: it pushes the predicate down, it rewrites the expression, it decides a block of rows cannot match and skips it. Move the predicate out of the `WHERE` and into the select list, `SELECT (p) IS TRUE FROM t`, and none of that is available, because there is no filter to push and nothing to skip and the engine has to evaluate the expression once per row. Count the true ones and it is the same number by a route the optimizer cannot take. The paper is Rigger, Rui and Su, "Detecting Optimization Bugs in Database Engines via Non-Optimizing Reference Engine Construction", ESEC/FSE 2020, and it calls the second query a non optimizing reference engine, which is a reference implementation the engine is already obliged to have rather than one somebody had to write.

```
cargo run --release -- norec --count 20000 --seed 1
```

```
the optimizer against itself on rudb from git
seed 1, which is what replays this run exactly

20000 predicates, 0 the engine would not run, 20000 left
20000 of those count the same filtered as they do evaluated a row at a time, 14072 of which matched some rows and not all of them
```

It is worth having beside the partitioning oracle above because the two fail on different bugs. A filter that drops the rows where the predicate is unknown is a partitioning failure and passes here, since both of these queries are filters and both drop the same rows. A pushdown that loses rows is a failure here and passes there, since the three parts are all wrong in the same direction and still add up.

On a disagreement the same predicate goes to a second rudb with every optimizer pass turned off. If the two counts agree there, a rewrite did it and `bisect` names which one. If they disagree there too, the optimizer is not where to look and the answer is already wrong in the binder or the executor, which is the one sentence that saves a day of reading plans. That pairing is the reason this oracle and the pass bisector below it are the same piece of work.

`IS TRUE` rather than the bare predicate, because what is wanted is one value per row that is never NULL. A column that is true, false or NULL would make the harness decide what an unknown is worth, and `IS TRUE` is the engine's own answer to that question and the one `WHERE` already uses.

rudb passes every predicate this has generated and refuses none of them, and `--pinned` puts the same predicates to the pinned binary, which passes 300 of them and refuses none either. That is the check an oracle with one engine in it cannot do without, since a generator writing SQL no optimizer would touch produces exactly the same green run.

Passing everything leaves the failure path with no run to exercise it, so it is tested against an engine whose filter loses a row its select list finds, which is what a pushdown bug looks like from outside. The same fake engine with the bug present in both halves is used to check that the verdict comes back saying the optimizer is not to blame.

## Which pass changed the answer

A wrong answer is a sentence and a plan, and the plan is the part nobody wants to read. rudb takes DuckDB's `SET disabled_optimizers` spelling, so the question "which rewrite did this" can be asked by running the statement again rather than by reading anything.

```
RUDB_COMPAT_RUDB=/path/to/rudb cargo run --release -- bisect "SELECT unnest([1,2,3]) AS x"
```

```
every pass off answers the same way, so this is the binder or the executor and not the optimizer
```

It runs the statement once per pass with that pass turned off, and then once more with every pass off. The search is linear rather than a binary search over subsets, because the list is a handful of names long, that many extra runs of a statement that already ran is nothing, and a binary search finds one pass and quietly picks a side when two of them are involved.

The last run is the one to read. The unoptimized plan is the right answer by construction, so a statement that is still wrong with every rewrite off is wrong in the binder or the executor, and that is worth one run because it sends somebody to the right file instead of a week of reading plans. Every difference found by hand so far has come back that way, which is a fair summary of where rudb is: the optimizer is not yet where the answers go wrong.

Turning a pass off is a `SET`, so this only works through a driver that remembers what it was told, and the setting is put back after each run whatever happened, because the engine here is the one the next record uses.

## The per pass sweep

`bisect` is asked about a statement somebody already found. The sweep asks about all of them at once, before anybody has found anything, by running the whole corpus once per pass with that pass on and every other one off.

```
cargo run --release -- sweep
```

```
9 passes swept over 35 files
          0  every pass off              591 of 591 records passed
          0  expression_rewriter         591 of 591 records passed
          0  distinct_aggregate_rewrite  591 of 591 records passed
          0  dependent_group_keys        591 of 591 records passed
          0  filter_pushdown             591 of 591 records passed
          0  empty_result_pullup         591 of 591 records passed
          0  unused_columns              591 of 591 records passed
          0  limit_pushdown              591 of 591 records passed
          0  top_n                       591 of 591 records passed
          0  late_materialization        591 of 591 records passed

no pass changed an answer
```

The attribution is the arrangement rather than a search afterwards. Every run has exactly one rewrite in it, so a record that fails in one of them was changed by the pass that names the row, and nothing has to be bisected to find that out. `cargo test` runs the same sweep, because it is three and a half seconds and a property that can be gated per commit should be.

The first row earns its run. Without it, a record the binder or the executor gets wrong fails in all nine of the others and is reported nine times against nine innocent passes. With it, those records come off every pass's list and are printed once at the bottom under the sentence that says the optimizer is not where to look, which is the same answer `bisect` gives for one statement.

What a sweep cannot see is a pair of passes that is only wrong when both are on, because no run here has two passes in it. That is the other half of the property and it is `cargo test`'s: the committed corpus with every pass on and with every pass off has to answer identically. A clean sweep beside a red on-against-off gate is the pair, and that is a reading neither check gives on its own.

The nightly runs it twice, once against the pinned rudb and once against the tip of the branch. The pin is what every pull request here measures, so a pass that changed an answer this morning is invisible until somebody bumps the pin, and the second run is what says so overnight instead of a week later inside an unrelated change.

Upstream's corpus is not swept yet. This runs the corpus in this process, which is fine for a corpus every record of which is supposed to pass, and upstream has queries that do not stop. Those need the isolating runner, which re-runs this binary once per file and has no way yet to tell the child which pass to leave on.

## What the generators never reach

Every generator here can tell you what it produced and none of them can tell you what it never produced. `scripts/reach` builds the engine with `-C instrument-coverage` on, runs the grammar generator, both partitioning oracles and upstream's `sqlsmith` over it, and `rudb-compat reach` reads the lcov back and says which crates and which files of rudb the run never entered. It is a rebuild and twenty minutes, so it is a remote machine and not a pre commit check.

The first run says the generators reach 32.4 percent of the 20171 lines of the engine's query path, and the interesting part is not that number. It is that 22 files were never entered at all, and the largest of them are `rudb-exec/src/group.rs`, `rudb-kernels/src/aggregate.rs`, `rudb-exec/src/join.rs`, `rudb-exec/src/sort.rs`, `rudb-exec/src/topn.rs` and `rudb-exec/src/setop.rs`. Read as a sentence about the generators rather than about the engine, that says the generated queries never aggregate, never join, never sort and never take a union, which is a work list for the generators that no amount of looking at their output would have produced.

This is feedback and not a published number, and the difference matters. Line coverage of an engine says very little about whether it matches another engine, so it does not go on the page and nothing gates on it. Section 10.5 of the generation spec is the decision and section 10.5.1 is what this first run found.

The engine is a dependency of this crate rather than a member of its workspace, and `cargo llvm-cov` instruments the workspace and nothing else unless the dependency is named, which is what the crate list in the script is for. A run that forgets it produces a full report of the harness reaching itself, which reads exactly like a measurement, so `reach` refuses to print a report with no engine in it and says which half of the setup is wrong.

## Which of the catalog is worth anything

Every percentage this project publishes is unweighted, and says so, because nobody has the weights. The weights have to come from queries somebody wrote because they wanted an answer, and the nearest thing to a pile of those already in the vendored clone is upstream's own benchmark suite. It is ClickBench, the join order benchmark over IMDB, TPC-H, TPC-DS, h2oai, LDBC, the JSON benchmarks, the taxi data and several hundred micro benchmarks.

```
rudb-compat queries --pinned --limit 40
```

That reads eight hundred and forty five distinct queries and counts what they call. A hundred and thirty four names out of the eleven hundred and fifty nine in the catalog appear in one of them and a thousand and twenty five appear in none, and the top of the list is `sum`, `min`, `count`, `position`, `substr` and `avg` by a distance. That is the whole point of the exercise. A suite that treats all eleven hundred names as equally important spends most of its effort on functions nobody has ever called, and there was no way to say which those were until this could be run.

Nothing here is run against an engine. The loads in that suite build tables of a hundred million rows, and a histogram does not need them, so this reads the files and counts and stops. A pass rate over these queries wants the loads cut down to a size a test can carry, which is a separate job.

Two counts are deliberately short. A query that appears in five benchmark files at five scale factors is one query, because five measurements of one query are not five queries and counting them that way would weight the histogram towards the two suites that are parameterised that way. And a name counts only when the bracket comes straight after it, so an operator scores nothing and `EXTRACT` and `CAST` score nothing, which means both read here as unused. Both of those want a parser rather than a scan over text, and a number nobody can check by reading a file is worse than a number that is honest about what it left out.

## What the corpus costs on both engines

The same eight hundred and forty five queries run on both engines with the clock and the meter around them, which is the three ratios the goal is actually about. `queries` counts what the corpus calls and runs nothing. `cost` runs it.

```
RUDB_COMPAT_RUDB=/path/to/rudb rudb-compat cost --runs 5 --seconds 30
```

```
benchmarks 845
measured   34
refused    337, which is one engine declining it or taking too long
      107  rudb-shell said Not implemented Error
      100  the load: rudb-shell said Not implemented Error
       65  the load: rudb-shell said Catalog Error
       41  rudb-shell said Catalog Error
        8  the load: rudb-shell said Parser Error
        6  rudb-shell said Timeout
        4  rudb-shell said Binder Error
        3  the load: rudb-shell said Binder Error
        1  rudb-shell said Error
        1  rudb-shell said Parser Error
        1  the load: duckdb-shell said Timeout
skipped    474

whole corpus
  time        0.48   quartiles 0.28 to 0.76 over 34 benchmarks
  cpu         1.54   quartiles 0.73 to 2.64 over 34 benchmarks
  memory      0.30   quartiles 0.24 to 0.40 over 34 benchmarks
```

Wall clock and memory are going the right way and processor time is not, which is exactly the case three ratios exist to catch. The median benchmark finishes in a bit under half the pinned binary's elapsed time on under a third of its peak memory, and spends half again as much processor time doing it. An engine ahead on the clock and behind on the meter is getting its speed from cores rather than from work, and a report that published elapsed time alone would have said rudb was twice as fast and stopped there.

Per suite it is the join family at twenty four times and everything else between a third and two thirds. The worst single benchmark is a range join at two hundred and seven times the elapsed time and eight hundred times the processor time, which is a nested loop against an engine that has a range join operator, and the next two are hash joins at twenty four times on ten times the peak memory. None of that makes the median, though. The median is 0.48 and the goal is 0.1, so the work ahead is a factor of five across the whole corpus as well as two orders of magnitude on the joins.

A benchmark here is the load and the query in one process rather than the query on its own, and that is not a shortcut. rudb has no storage format yet, tamnd/rudb#103, so there is no way to build a table once and time a query against it afterwards. Both engines are handed the load as setup statements and the query after it, the process is what gets measured, and the load is measured again on its own so the share of each number that is ingestion can be printed beside it. The median benchmark is eighty eight percent load, which is worth knowing before anybody reads one of these ratios as a statement about a query.

The row counts are cut down. The suite builds tables of a hundred million rows because it is a benchmark suite for a finished database, and every `range` and `generate_series` argument above a million is brought down to a million before either engine sees it. Nothing else is rewritten, so a modulus or a seed or a hash constant is still whatever its author wrote. That makes these ratios at a million rows and nothing else, which is a smaller claim than the one `tamnd/rudb-bench` exists to make.

Two thirds of the corpus produces no ratio and the reasons are counted rather than dropped. Four hundred and seventy four are skipped before either engine sees them, almost all of them behind a `require` for httpfs or parquet or json or one of the two data generators, and thirty seven because they read or write a file that is not in the sparse checkout. Three hundred and thirty seven are refused, which is one engine declining the load or the query, and all but one of those is rudb: a hundred and seven not implemented, a hundred more not implemented in the load, and a hundred and twenty two between a catalog, binder and parser error. Six are rudb taking longer than thirty seconds on something the pinned binary answers in under one, and those are the interesting ones, because a benchmark stopped on our side leaves the ratios above rather than making them worse. The timeout count is part of the result and not a footnote.

The one that is not ours is a load the pinned binary did not finish in thirty seconds either, which is the first time the other side has been the one to run out of time.

## Those ratios on the published page

They do not stay in the terminal. A whole run against the pinned binary appends its rows to `target/report/cost.tsv` beside the pages, and `rudb-compat report` reads the most recent of them back and prints a Resources section on the page, at the three granularities section 11.2 of `spec/sql/duckdb/11-the-number.md` asks for: the whole corpus, then per suite, then the worst twenty.

Carried onto the page rather than computed there, the same way the function coverage number is, and for the same reason. A cost run is four measurements per benchmark and five processes per engine per measurement, over eight hundred and forty five benchmarks, and it took thirty five minutes on server2 with most of the corpus refused. A corpus run is minutes and goes on every commit. So the page says when and where the ratios were measured and whether that was the same engine build on the same machine as the corpus numbers above them, and a reader who finds it was not does not have to compare two commits themselves to notice.

```
### The whole corpus

    time        0.48   quartiles 0.28 to 0.76 over 34 benchmarks
    cpu         1.54   quartiles 0.73 to 2.64 over 34 benchmarks
    memory      0.30   quartiles 0.24 to 0.40 over 34 benchmarks

### Per suite

    suite                        time      cpu   memory      n
    join                        24.27    15.94     0.16      5
    timestamp                    0.60     2.62     0.31      2
    cast                         0.55     1.28     0.31     10
    aggregate                    0.43     0.81     0.36      3
    micro                        0.34     0.71     0.34      4
    case                         0.33     1.20     0.30      3
    date                         0.33     1.36     0.23      3

### The worst 20

    benchmark                                    time      cpu   memory    load
    Range Join                                 207.27   813.69     0.16     82%
    Hash Join Dictionary Emit + Probe           24.53    22.18    10.31     79%
    Hash Join Constant Probe                    24.27    15.94     8.23    100%
    Order By (Single Integer)                    8.46     7.17     2.85     27%
    Big case                                     4.77     8.95     0.17     11%

    measured    2026-09-15 01:52:40 UTC on vmi3112167
    engine      rudb from git at 4561d0da83
    rudb-compat a3b256db82, dirty
    duckdb      v2.0.0-dev84237 (Development Version) cc7e7bac7f
```

The worst list is cut to five above and the page prints twenty. The processor time column is the one the terminal output does not have room for and the page does, and it earns its place on the first row: a range join at two hundred times the elapsed time and eight hundred times the processor time is a nested loop burning every core it can get, and the elapsed number on its own would have made it look four times better than it is.

One run writes several rows to that file and the rows of a run share their stamp and their machine, which is how they are read back as one measurement. The scope column says whether a row is the corpus, a suite or a benchmark. It is a denormalised table on purpose: the six provenance fields and the three run totals repeat on every row, because the alternative is a row whose meaning depends on another row somewhere above it, and one row of a file like this should say what it is a measurement of on its own.

A run narrowed with `--count` or `--group` is not recorded. A ratio over eleven benchmarks somebody picked is a useful thing to look at and it is not the ratio the page is about. Neither is a run against a DuckDB that is not the pin, because then the divisor came from another database. Both cases print what they found and say they were not written down, which is the same rule `coverage` follows.

Until a machine has run one, the three ratios stay in the list of numbers the page does not say yet, with the command that would produce them beside them. That list is the point of the page as much as the numbers are.

## Getting a generated run back

Four modes here write their own input and every one of them can find something nobody has time to look at the day it turns up. So each of them ends by printing what it was measured against and appending one row to `target/report/generated.tsv`.

```
grammar on server2, 2026-09-15 00:45:38 UTC
put to the pinned duckdb and rudb

  rudb           rudb from git at 1dbe862509
  harness        4a0bbfa4d6
  duckdb         v2.0.0-dev84237 cc7e7bac7f md5 3051ffc8ff809c3856697f6bbce994a8
  pinned         yes, this is the commit the grammar is vendored from
  corpus         none, this run wrote its own cases
  machine        linux x86_64, 32 cores
  seed           42
  replay with    rudb-compat grammar --rule Statement --count 2000 --seed 42
  recorded in    target/report/generated.tsv
```

A seed on its own is not enough and that is the whole reason for the block. A seed reproduces a run against the rudb, the DuckDB and the generator that happened to be on the machine, and all three of those move, so a finding from three weeks ago is a finding about three things nobody wrote down. The six fields are the ones section 11.2 of `spec/sql/duckdb/11-the-number.md` asks for, and the md5 of the binary is there because two builds can call themselves the same version.

The replay line is built out of the values the run used rather than written down beside them, so it cannot come to disagree with them, and it always gives `--count` and `--seed` even when both were defaults, because a default is a thing that changes and a recorded command should keep working after it does.

The series has one row per run with a mode column, rather than one file per mode. What all four have in common is how many cases were generated, how many said anything at all, and how many of those came out wrong, and the question the series exists to answer is whether generated testing found more this month than last. Four files would make somebody open four of them to answer it.

Two of the columns need reading carefully. `usable` is smaller than `cases` in every mode and by a different rule in each: a query the pinned binary cannot run says nothing about rudb, a statement neither parser accepts says nothing about either, and a predicate the engine refused says nothing either way. `groups` is smaller than `findings` in the two differential modes because they group by what the engine that refused said, and equal to it in the two oracles, because what those report is a predicate whose parts did not add up and there is nothing to group that by until the reducer has been over it.

## License

Apache-2.0. See [LICENSE-APACHE](LICENSE-APACHE).

Not affiliated with, endorsed by or derived from DuckDB Labs.
