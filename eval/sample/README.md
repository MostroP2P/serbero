# Evaluation sample

Ten Spanish cases taken from the T0.7 spike ([`../spike/`](../spike/)) in the
golden-case format of [`docs/evaluation.md` §1](../../docs/evaluation.md#1-golden-set),
used to show the eval binary working end to end (plan T3.7). They are
**not** the golden set: they have not been reviewed by a native speaker, and
ten cases say nothing about calibration.

- `cases/es/`: the cases.
- `REPORT-es.md`: the report of a live run against `typesafe/jev-1.13.0`.
- `recorded-es.json`: that run's answers. `tests/eval_sample.rs` replays
  them offline in CI, so a question change fails until the sample is run
  again.

```sh
set -a; . ./.env; set +a
cargo run --bin serbero-eval -- --lang es --cases eval/sample/cases \
    --report eval/sample/REPORT-es.md --record-to eval/sample/recorded-es.json

# offline, from the recording:
cargo run --bin serbero-eval -- --lang es --cases eval/sample/cases \
    --provider recorded --recording eval/sample/recorded-es.json --report /tmp/replay.md
```
