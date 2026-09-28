# Evaluation: es · typesafe/jev-1.13.0

Question set `qs-1-1e7ce156`, 10 cases, 72 labelled answers. Thresholds from defaults (judgments.md §3): guide 0.9, fact 0.8, human_request 0.8, fraud 0.6, conflict 0.75, outside_scope 0.8.

## Targets (evaluation.md §2)

| Question | Metric | Value | Over | Target | Met |
|---|---|---:|---:|---:|---|
| `seller_receipt` | precision of `says_received` at 0.9 | 1.000 | 2 | ≥ 0.98 | yes |
| `buyer_payment` | precision of `says_not_sent` at 0.9 | 1.000 | 1 | ≥ 0.98 | yes |
| `buyer_payment` | accuracy above 0.8 | 1.000 | 8 | ≥ 0.95 | yes |
| `buyer_payment` | coverage at 0.8 | 1.000 | 8 | ≥ 0.7 | yes |
| `seller_receipt` | accuracy above 0.8 | 1.000 | 7 | ≥ 0.95 | yes |
| `seller_receipt` | coverage at 0.8 | 1.000 | 7 | ≥ 0.7 | yes |
| `<party>_wants_human` | recall of `true` at 0.8 | 1.000 | 1 | ≥ 0.9 | yes |
| `fraud_signal` | recall of `true` at 0.6 | 1.000 | 2 | ≥ 0.85 | yes |
| `<party>_language` | accuracy | 1.000 | 13 | ≥ 0.95 | yes |
| `<party>_message_kind` | accuracy | 1.000 | 10 | ≥ 0.85 | yes |

10 of 10 targets met. A target without items is not met.

## Recommended thresholds (evaluation.md §3)

| Threshold | From | Rule | Value |
|---|---|---|---:|
| `guide` | `seller_receipt` | lowest with precision ≥ 0.98 | 0.50 |
| `guide` | `buyer_payment` | lowest with precision ≥ 0.98 | 0.50 |
| `human_request` | `<party>_wants_human` | highest with recall ≥ 0.9 | 0.95 |
| `fraud` | `fraud_signal` | highest with recall ≥ 0.85 | 0.85 |

Values are swept from 0.50 to 0.95 in steps of 0.05.

## Accuracy per question

| Question | Labels | Accuracy |
|---|---:|---:|
| `<party>_language` | 13 | 1.000 |
| `<party>_message_kind` | 10 | 1.000 |
| `<party>_wants_human` | 13 | 1.000 |
| `buyer_details` | 3 | 1.000 |
| `buyer_payment` | 8 | 1.000 |
| `claims_conflict` | 4 | 1.000 |
| `dispute_topic` | 2 | 1.000 |
| `fraud_signal` | 10 | 1.000 |
| `seller_checked` | 2 | 1.000 |
| `seller_receipt` | 7 | 1.000 |

Expected calibration error (10 bins, all answers): 0.029.

Latency: median 433 ms, p95 450 ms. Input tokens: mean 2145.

## Misses (0)
