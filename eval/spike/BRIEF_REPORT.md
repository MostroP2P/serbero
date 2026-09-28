# Brief questions evaluation, T3.11

**Question:** do the brief questions of
[`docs/judgments.md` §5](../../docs/judgments.md#5-brief-request) pick the
right messages to quote for the solver, and read the evidence sensibly?

**Answer: yes, with one weak spot.** On 36 of the spike's Spanish
conversations, every quote that passes the `fact` threshold (0.80) is the
labelled message (51 of 51), and no answer ever named a message from the
wrong party. `evidence_balance` put its most likely level on the labelled one
in all 19 labelled cases. The weak spot: when nothing is suspicious,
`quote_concern` tends to pick some message instead of `none`, with P from
0.53 to 0.79; all such picks stayed below `fact`, so none reached a brief.

## Method

- **Cases.** The T0.7 spike conversations ([`cases.json`](cases.json)); 36 of
  them carry a `brief_expect` label: the message a person would quote for
  each question where one message is clearly the answer (or `none` where no
  message fits), and the `evidence_balance` level where one level is clearly
  right. Cases where two messages would be equally good quotes are left
  unlabelled for that question.
- **Questions.** Read from the §5 block of `judgments.md` and filled per
  case exactly as `src/judge/brief.rs` does: each quote question offers the
  right party's message ids plus `none` (`quote_concern` offers both
  parties' messages, never Serbero's), and a quote whose party wrote nothing
  is left out.
- **Run.** One live run against `jev-1.13.0`; raw answers in
  [`results-jev-1.13.0-brief-qs-1.json`](results-jev-1.13.0-brief-qs-1.json).

```sh
set -a; . ./.env; set +a
python3 eval/spike/run.py --brief           # live
python3 eval/spike/run.py --brief --score   # re-score the saved answers
```

## Results

Full table: [`metrics-brief-qs-1.md`](metrics-brief-qs-1.md).

| Question | Labels | Winner correct | Kept at `fact` | Kept and correct | Wrong party |
|---|---:|---:|---:|---:|---:|
| `quote_buyer_payment` | 22 | 22/22 | 21 | 21/21 | 0 |
| `quote_buyer_details` | 10 | 9/10 | 5 | 5/5 | 0 |
| `quote_seller_receipt` | 18 | 18/18 | 17 | 17/17 | 0 |
| `quote_concern` | 12 | 9/12 | 8 | 8/8 | 0 |

"Kept at `fact`" counts answers that would appear in a brief: not `none`,
with a share of at least 0.80 (§5). Labels of `none` count toward "winner
correct" only, since `none` is never quoted.

**`evidence_balance`:** the most likely level equals the label in 19/19
cases (buyer says not paid → level 0, vague claim → 1, nothing said → 2,
details without confirmation → 3, seller confirms → 4).

Latency: median 315 ms. Input tokens: mean 1,100, about half a turn
request, since the brief asks five questions.

### Misses

| Case | Question | Label | Answer (P) |
|---|---|---|---|
| `es-ve-sent-no-details` | `quote_buyer_details` | `none` | `m4` "desde hace rato" (0.56) |
| `es-buyer-greeting-only` | `quote_concern` | `none` | `m4` "??" (0.63) |
| `es-cl-received-slang` | `quote_concern` | `none` | `m4` "sip, llegó la plata, todo ok" (0.53) |
| `es-es-formal-received` | `quote_concern` | `none` | `m3` "Confirmo que he recibido…" (0.79) |

All four are below `fact` and would be dropped. For `quote_buyer_details`,
the 5 labels naming a message were all picked at P = 1.00 and kept; the other
5 labels are `none`, of which 4 were answered `none` and one ("desde hace
rato", a time with no detail) was not.

## Findings

- **Quotes are safe to show.** Every kept quote was right, and the code
  additionally drops any answer that is not a message of the right party
  (`from_answers` checks it against the state), so a quote is always
  something that party actually wrote.
- **`quote_concern` over-picks when nothing stands out** (3 of 3 `none`
  labels missed, P 0.53–0.79). A spurious concern quote would only point a
  human at an ordinary message, but it is noise in the brief. No change now:
  the question is kept as specified, and this case should get more golden
  cases before T3.10 decides whether the concern quote needs a stricter
  threshold or a reworded `none` option.

## Limits

Same as the spike ([`REPORT.md`](REPORT.md#limits)): synthetic Spanish
conversations pending native review, few labels per question, one model, and
a single run.
