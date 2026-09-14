# Contributing

This is a harness, not a product, so the bar here is different from the one in the engine repository. What matters is that a result it produces can be trusted and reproduced by somebody who does not trust us.

Read [CONTRIBUTING.md in tamnd/rudb](https://github.com/tamnd/rudb/blob/main/CONTRIBUTING.md) for the house rules on Rust style, prose and commits. They apply here unchanged. The prose rules are checked by `cargo test`, which is why they are a test and not a convention.

## What a change has to come with

**A change to how a comparison is made comes with a case that shows it.** A harness bug that makes a suite pass is worse than an engine bug, because it hides one. If a comparison is being loosened, say what it was hiding and why the loosening is right.

**A tolerance on a floating point comparison comes with a written reason on the test.** Exact is the default. A silently tolerant comparison hides real bugs in aggregation order and in the agreement between the compiled and interpreted execution paths, which is exactly the class of bug this harness exists to find.

**A new query source comes with a note on what it is expected to find that the existing ones do not.** Volume is not the goal. The generated-query source already provides more volume than anybody can triage, and a source that finds the same bugs more slowly is a cost.

**A skip or an exclusion comes with an issue number.** A suite with silent exclusions reports a number that is not the number it claims to report. Excluded cases are counted and the count is published.

## The compatibility rules this repository owns

These come from `spec/sql/duckdb/` in tamnd/rudb, which is the plan this harness is the instrument for. Four of its rules live here rather than there, because this is the code that has to enforce them.

**No failure reaches a human unreduced.** A difference found by any source goes through `reduce` before anybody reads it, and what gets filed is the minimal case and its hash. The cost of a compatibility project is not finding differences, it is the minutes spent per difference, and fifty thousand raw failures are a year of reading while the same failures reduced are a week of it.

**No dialect enters the registry without a corpus, a fuzz target and a published pass rate.** A second query language that is measured by demos is a language whose gaps are found by users. The entry rule is the same one SQL is held to, on the same page, with the same denominator discipline.

**No number on the report page that the harness did not compute, and no single headline percentage.** Every number carries its denominator and its provenance, which is the rudb commit, this commit, the DuckDB commit and binary hash, the corpus commit, the machine and the seed. A percentage without those six is a rumour. A number is allowed to go down, and a change that lowers one says why rather than hiding it.

**Every feature that lands carries its resource ratios.** The harness records wall clock, CPU seconds and peak resident set for both engines on every record, out of the child process it already forks, and publishes the three ratios of rudb over DuckDB as medians with the interquartile range and never as a minimum. The goal is a tenth on all three. The timing exclusions are written down in the code rather than applied by feel, and a ratio taken on a shared machine is not a ratio.

**A DuckDB bug found by this harness gets reproduced and filed in tamnd/duckdb.** Reduce it, confirm it against the pinned binary, and open the issue in our fork. Never upstream.

## Running it

```
cargo build --release
cargo test
cargo run -- levels
```

The rest of the commands need a real DuckDB binary and a real rudb build and neither is wired up yet. That arrives with M2.

## License

Apache-2.0, and a contribution is offered under the same terms. There is no separate agreement to sign.
