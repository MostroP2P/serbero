Model `jev-1.13.0`, question set `qs-1`, 55 cases.

| Question | Labels | Accuracy | Mean P(label) | Misses |
|---|---:|---:|---:|---|
| `buyer_payment` | 39 | 39/39 (100%) | 1.00 | — |
| `buyer_details` | 17 | 17/17 (100%) | 0.95 | — |
| `seller_receipt` | 29 | 29/29 (100%) | 0.99 | — |
| `seller_checked` | 9 | 8/9 (89%) | 0.88 | `es-ar-me-entro-la-guita` |
| `claims_conflict` | 8 | 8/8 (100%) | 0.89 | — |
| `fraud_signal` | 55 | 55/55 (100%) | 0.91 | — |
| `dispute_topic` | 8 | 7/8 (88%) | 0.77 | `es-buyer-greeting-only` |
| `<party>_message_kind` | 34 | 34/34 (100%) | 0.98 | — |
| `<party>_language` | 59 | 59/59 (100%) | 0.99 | — |
| `<party>_wants_human` | 62 | 61/62 (98%) | 0.95 | `es-fraud-telegram-fake-support` |

### At the default thresholds

| Fact | Threshold | Labelled positive | Fired | True positives | False positives | Precision | Recall |
|---|---:|---:|---:|---:|---:|---:|---:|
| `buyer_payment = says_sent` | `fact` 0.8 | 18 | 18 | 18 | 0 | 1.00 | 1.00 |
| `buyer_payment = says_not_sent` | `fact` 0.8 | 8 | 8 | 8 | 0 | 1.00 | 1.00 |
| `buyer_payment = says_not_sent` | `guide` 0.9 | 8 | 8 | 8 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_received` | `fact` 0.8 | 7 | 7 | 7 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_received` | `guide` 0.9 | 7 | 7 | 7 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_not_received` | `fact` 0.8 | 10 | 10 | 10 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_received_with_problem` | `outside_scope` 0.8 | 3 | 3 | 3 | 0 | 1.00 | 1.00 |
| `<party>_wants_human = true` | `human_request` 0.8 | 6 | 6 | 6 | 0 | 1.00 | 1.00 |
| `fraud_signal = true` | `fraud` 0.6 | 9 | 9 | 9 | 0 | 1.00 | 1.00 |
| `claims_conflict = true` | `conflict` 0.75 | 3 | 2 | 2 | 0 | 1.00 | 0.67 |

Latency: median 324 ms, max 531 ms. Input tokens: mean 2090, max 2489.

### Every miss

| Case | Question | Label | Answer | P(label) |
|---|---|---|---|---:|
| `es-ar-me-entro-la-guita` | `seller_checked` | `true` | `0.43` | 0.43 |
| `es-buyer-greeting-only` | `dispute_topic` | `not_yet_clear` | `payment_not_confirmed` | 0.05 |
| `es-fraud-telegram-fake-support` | `buyer_wants_human` | `false` | `0.59` | 0.41 |
