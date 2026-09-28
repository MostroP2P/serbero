Model `jev-1.13.0`, brief questions of `qs-1`, 36 cases.

| Question | Labels | Winner correct | Kept at `fact` | Kept and correct | Wrong party |
|---|---:|---:|---:|---:|---:|
| `quote_buyer_payment` | 22 | 22/22 | 21 | 21/21 | 0 |
| `quote_buyer_details` | 10 | 9/10 | 5 | 5/5 | 0 |
| `quote_seller_receipt` | 18 | 18/18 | 17 | 17/17 | 0 |
| `quote_concern` | 12 | 9/12 | 8 | 8/8 | 0 |

`evidence_balance`: most likely level equals the label in 19/19 cases, within one level in 19/19.

Latency: median 315 ms, max 769 ms. Input tokens: mean 1100, max 1315.

### Every miss

| Case | Question | Label | Answer (P) |
|---|---|---|---|
| `es-ve-sent-no-details` | `quote_buyer_details` | `none` | `m4` (0.56) |
| `es-buyer-greeting-only` | `quote_concern` | `none` | `m4` (0.63) |
| `es-cl-received-slang` | `quote_concern` | `none` | `m4` (0.53) |
| `es-es-formal-received` | `quote_concern` | `none` | `m3` (0.79) |
