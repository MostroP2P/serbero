# Serbero — Specification

Serbero helps the parties of a [Mostro](https://mostro.network) dispute resolve
it themselves, and brings in a human solver when that is needed. It talks to
both parties in their own language, establishes the payment facts with typed
judgments from Jev (TypeSafe's System One model), and explains the options Mostro
already gives them: the seller can release, and both can cancel cooperatively.
When the facts are contested, suspicious, or stalled, it hands the case to a
solver with a clear, cited brief. It also notifies solvers of every dispute.

Serbero never moves funds and never decides a dispute.

## Documents

| Document | What it covers |
|---|---|
| [spec.md](spec.md) | Goals, principles, scope, architecture, lifecycle, self-resolution paths, data model, configuration, degraded mode |
| [judgments.md](judgments.md) | The Jev question set: state shape, every question, thresholds, and the decision table code applies to the answers |
| [messages.md](messages.md) | Every party-facing template (en / es / pt) and every solver-facing DM |
| [evaluation.md](evaluation.md) | How the question set is validated and how thresholds are calibrated before and after release |
| [plan.md](plan.md) | Implementation plan: phases of atomic tasks, each one a pull request |

## One-paragraph design

Code owns the whole workflow: Nostr transport, dispute state, timers, and every
message that reaches a person. Each time a party replies, Serbero sends the
conversation to Jev once, with a fixed set of narrow questions ("Does the seller
say the fiat arrived?", "Is the buyer asking for a human?", "Which language is the
seller writing in?"). Jev returns calibrated probabilities. A small, deterministic
decision table turns those probabilities into the next step: ask a specific
question from a human-written template, explain a self-resolution path, or hand
the case to a solver with a brief whose quotes are copied verbatim from the chat.
