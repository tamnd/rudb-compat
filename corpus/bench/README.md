# The benchmarks we wrote ourselves

DuckDB's benchmark suite is vendored into the clone and `rudb-compat cost` reads all of it, which is ClickBench, TPC-H, TPC-DS, the join order benchmark and several hundred micro benchmarks. Every one of those is a query somebody wrote to measure something, which is why the cost run uses them rather than a sqllogictest file, and every one of them was written before rudb existed. That is the hole this directory fills. Nothing in DuckDB's suite is a correlated subquery or a lateral join, because those were never the thing anybody was benchmarking, so the release that made them work moved no number in a cost run at all.

A file here is a `.benchmark` file in DuckDB's own format and goes through the same reader, so a benchmark of ours is a benchmark of theirs everywhere downstream of that. The directory is part of this crate rather than the clone, so it needs no `--refresh` and it is there on a machine that has never fetched anything.

## What a file looks like

Directives at the left margin, each either taking the rest of its line or introducing a block that runs to the next blank line. The ones that matter are `name`, `group`, `load` and `run`. Templates and `require` are read too, because the reader is shared, but nothing here uses them.

The `group` is what the per suite table in `cost` breaks the numbers down by, and a group with one benchmark in it is left out of that table, so write at least two of anything worth a group. Ours are named with a `rudb-` prefix so a row is obviously from this half of the corpus.

## What is in here

Thirteen files, and each of them is one operator sitting inside a correlated subquery or a lateral entry. Six were written when the directory was, for a scalar subquery, an `EXISTS`, a distinct count, a left join lateral, a lateral aggregate and a lateral `VALUES`. Six more are the operators the general unnesting rule learned after that, which are a window, a top N, a limit inside a lateral entry, a set operation, a right join and a full join. The thirteenth is a table function reading the left row, which is the one entry that has no input for the domain to be pushed under and so is the one that gets an operator of its own rather than a rewrite.

The reason each of those is worth a file is the same reason the directory exists. Every one of them was refused by rudb until the release that added its rule, so there is no number for it anywhere in DuckDB's suite and no number for it in an older run of ours either. A benchmark written the day the rule lands is the only way the first measurement of it is not also the measurement somebody is comparing against.

## How to size one

The load and the query are measured separately and the load's share is printed beside every row, so a benchmark that is nine tenths `CREATE TABLE` is visible as such rather than misleading. Even so, aim for a query that costs more than its load, and build the rows out of `range` rather than a file so the benchmark is self contained.

That share reads 100% for everything here and it is not saying what it looks like it is saying. It is taken on the pin, which is the side that answers everything and so the side whose split is trustworthy, but the pin answers all thirteen of these in less time than it takes to start a process. The load and the load with the query on the end of it are then the same number and the share pins at one. Anything DuckDB is fast on reads the same way, so this is not a property of our files, and the column starts meaning something again once there is a way to build a table once and time a query against it separately.

Keep the whole thing near a second on rudb. The cost run measures four things per benchmark and takes the median of several runs of each, so a benchmark that takes a minute costs the run half an hour on its own and a benchmark that hangs is recorded as a refusal. Aggregate the final result down to one row, because the time to print a million rows to a pipe is not the time anybody is trying to measure.

## What is not here yet

Materialised `WITH` shipped in 0.3.39 and has no benchmark, which is deliberate rather than forgotten. The part of it that costs anything is evaluating the definition more than once, and a definition big enough for that difference to rise above process startup on either engine needs a load of a hundred million rows, at which point the load is the measurement. It wants a benchmark that holds the rows across statements rather than one that rebuilds them, which is a change to the harness and not a file in this directory.
