# Contributing

This is a harness, not a product, so the bar here is different from the one in the engine repository. What matters is that a result it produces can be trusted and reproduced by somebody who does not trust us.

Read [CONTRIBUTING.md in tamnd/rudb](https://github.com/tamnd/rudb/blob/main/CONTRIBUTING.md) for the house rules on Rust style, prose and commits. They apply here unchanged. The prose rules are checked by `cargo test`, which is why they are a test and not a convention.

## What a change has to come with

**A change to how a comparison is made comes with a case that shows it.** A harness bug that makes a suite pass is worse than an engine bug, because it hides one. If a comparison is being loosened, say what it was hiding and why the loosening is right.

**A tolerance on a floating point comparison comes with a written reason on the test.** Exact is the default. A silently tolerant comparison hides real bugs in aggregation order and in the agreement between the compiled and interpreted execution paths, which is exactly the class of bug this harness exists to find.

**A new query source comes with a note on what it is expected to find that the existing ones do not.** Volume is not the goal. The generated-query source already provides more volume than anybody can triage, and a source that finds the same bugs more slowly is a cost.

**A skip or an exclusion comes with an issue number.** A suite with silent exclusions reports a number that is not the number it claims to report. Excluded cases are counted and the count is published.

## Running it

```
cargo build --release
cargo test
cargo run -- levels
```

The rest of the commands need a real DuckDB binary and a real rudb build and neither is wired up yet. That arrives with M2.

## License

Apache-2.0, and a contribution is offered under the same terms. There is no separate agreement to sign.
