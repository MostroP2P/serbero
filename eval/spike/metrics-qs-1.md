Model `jev-1.13.0`, question set `qs-1`, 51 cases.

| Question | Labels | Accuracy | Mean P(label) | Misses |
|---|---:|---:|---:|---|
| `buyer_payment` | 37 | 37/37 (100%) | 1.00 | — |
| `buyer_details` | 16 | 16/16 (100%) | 0.95 | — |
| `seller_receipt` | 28 | 28/28 (100%) | 0.99 | — |
| `seller_checked` | 9 | 8/9 (89%) | 0.88 | `es-ar-me-entro-la-guita` |
| `claims_conflict` | 8 | 8/8 (100%) | 0.89 | — |
| `fraud_signal` | 51 | 51/51 (100%) | 0.91 | — |
| `dispute_topic` | 8 | 7/8 (88%) | 0.79 | `es-buyer-greeting-only` |
| `<party>_message_kind` | 34 | 34/34 (100%) | 0.98 | — |
| `<party>_language` | 55 | 55/55 (100%) | 0.99 | — |
| `<party>_wants_human` | 58 | 58/58 (100%) | 0.97 | — |

### At the default thresholds

| Fact | Threshold | Labelled positive | Fired | True positives | False positives | Precision | Recall |
|---|---:|---:|---:|---:|---:|---:|---:|
| `buyer_payment = says_sent` | `fact` 0.8 | 16 | 16 | 16 | 0 | 1.00 | 1.00 |
| `buyer_payment = says_not_sent` | `fact` 0.8 | 8 | 8 | 8 | 0 | 1.00 | 1.00 |
| `buyer_payment = says_not_sent` | `guide` 0.9 | 8 | 8 | 8 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_received` | `fact` 0.8 | 7 | 7 | 7 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_received` | `guide` 0.9 | 7 | 7 | 7 | 0 | 1.00 | 1.00 |
| `seller_receipt = says_not_received` | `fact` 0.8 | 10 | 10 | 10 | 0 | 1.00 | 1.00 |
| `<party>_wants_human = true` | `human_request` 0.8 | 6 | 6 | 6 | 0 | 1.00 | 1.00 |
| `fraud_signal = true` | `fraud` 0.6 | 6 | 6 | 6 | 0 | 1.00 | 1.00 |
| `claims_conflict = true` | `conflict` 0.75 | 3 | 3 | 3 | 0 | 1.00 | 1.00 |

Latency: median 334 ms, max 459 ms. Input tokens: mean 2062, max 2456.

### Every miss

| Case | Question | Label | Answer | P(label) |
|---|---|---|---|---:|
| `es-ar-me-entro-la-guita` | `seller_checked` | `true` | `0.43` | 0.43 |
| `es-buyer-greeting-only` | `dispute_topic` | `not_yet_clear` | `payment_not_confirmed` | 0.05 |
