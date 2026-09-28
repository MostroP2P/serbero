#!/usr/bin/env python3
"""T0.7 judge spike: run the qs-1 turn questions over hand-written Spanish
conversations and report per-question accuracy.

Throwaway tooling, outside the crate. The real harness is the eval binary
(T3.7). Standard library only.

    set -a; . ./.env; set +a
    python3 eval/spike/run.py            # call the judge, then score
    python3 eval/spike/run.py --score    # re-score the saved answers only

Cases are written compactly in cases.json and expanded into the state of
docs/judgments.md §1. Serbero's own messages are rendered from the real
catalog files, so the judge sees exactly what Serbero would send.
"""

import argparse
import concurrent.futures
import json
import os
import re
import statistics
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
URL = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-1.13.0"
PARTIES = ("buyer", "seller")
ROLES = {
    "buyer": "Pays the fiat money to the seller outside Mostro, then waits for the bitcoin.",
    "seller": "Has bitcoin locked in Mostro escrow and must confirm the fiat arrived before the trade can finish.",
    "serbero": "Automated assistant that asks both parties questions for the human solver. It cannot move funds or decide the dispute.",
}
# Default thresholds from docs/judgments.md §3, used for the at-threshold view.
THRESHOLDS = {
    "guide": 0.90, "fact": 0.80, "human_request": 0.80, "fraud": 0.60, "conflict": 0.75, "outside_scope": 0.80,
}
SHORT = {"b": "buyer", "s": "seller"}


def load_catalogs():
    """Every catalog in messages/, keyed by language code; no code names a language."""
    return {path.stem: tomllib.loads(path.read_text()) for path in sorted((ROOT / "messages").glob("*.toml"))}


def format_amount(catalog, value, currency):
    whole, _, fraction = value.partition(".")
    sep = catalog["format"]["thousands_separator"]
    grouped = "".join(
        (sep if i and (len(whole) - i) % 3 == 0 else "") + d for i, d in enumerate(whole)
    )
    if fraction:
        grouped += catalog["format"]["decimal_separator"] + fraction
    return f"{grouped} {currency}"


def render(catalogs, lang, template_id, order):
    catalog = catalogs[lang]
    text = catalog["templates"][template_id]
    if "{amount}" in text:
        amount = format_amount(catalog, order["fiat_amount"], order["fiat_code"])
        text = text.replace("{amount}", amount)
    return text


def build_state(case, catalogs):
    """Expand a compact case into the judge state of judgments.md §1.

    Message syntax in `msgs`:
      "@b ask_buyer_details"   Serbero sends a template to the buyer
      "@s en:reminder"         ... in another language
      "b: text"                the buyer writes
      "s[2]: text"             the seller writes, with 2 attachments
    Unless `"opening": false`, every case starts with Serbero's opening to
    both parties (intro plus first question, as one message each).
    """
    order = case["order"]
    lang = case.get("opening_lang", "es")
    transcript = []

    def serbero(party, text):
        transcript.append({"id": f"m{len(transcript) + 1}", "from": "serbero", "to": party, "text": text})

    if case.get("opening", True):
        for party, question in (("buyer", "ask_buyer_sent"), ("seller", "ask_seller_received")):
            serbero(party, render(catalogs, lang, "intro", order) + "\n\n" + render(catalogs, lang, question, order))

    for line in case["msgs"]:
        if line.startswith("@"):
            who, template = line[1:].split(" ", 1)
            tlang, _, tid = template.rpartition(":")
            serbero(SHORT[who], render(catalogs, tlang or lang, tid, order))
            continue
        m = re.match(r"^([bs])(?:\[(\d+)\])?: (.*)$", line, re.S)
        if not m:
            sys.exit(f"{case['id']}: bad message line {line!r}")
        msg = {"id": f"m{len(transcript) + 1}", "from": SHORT[m[1]], "text": m[3]}
        if m[2]:
            msg["attachments"] = int(m[2])
        transcript.append(msg)

    latest = {}
    for party in PARTIES:
        last_to = max((i for i, t in enumerate(transcript) if t.get("to") == party), default=-1)
        latest[party] = [t["id"] for t in transcript[last_to + 1 :] if t["from"] == party]
    return {"roles": ROLES, "order": order, "transcript": transcript, "latest": latest}


def build_questions(state, qs):
    questions = dict(qs["case_facts"])
    for party in PARTIES:
        if not state["latest"][party]:
            continue
        for qid, q in qs["per_party"].items():
            text = json.dumps(q).replace("<party>", party)
            questions[qid.replace("<party>", party)] = json.loads(text)
    return questions


def call_judge(key, state, questions):
    body = json.dumps({"model": MODEL, "state": state, "questions": questions}).encode()
    for attempt in range(6):
        req = urllib.request.Request(
            URL, body, {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
        )
        started = time.monotonic()
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                data = json.load(resp)
            data["latency_ms"] = round((time.monotonic() - started) * 1000)
            return data
        except urllib.error.HTTPError as e:
            # Same retryable set as the Rust adapter: timeout, rate limit, any 5xx.
            if not (e.code in (408, 429) or 500 <= e.code <= 599) or attempt == 5:
                raise RuntimeError(f"HTTP {e.code}: {e.read()[:300]!r}") from None
        except (urllib.error.URLError, TimeoutError):
            if attempt == 5:
                raise
        time.sleep(2**attempt)
    raise RuntimeError("unreachable")


def top(answer):
    """The winning option: a choice label, or a bool for a noul at 0.5."""
    if answer["type"] == "noul":
        return answer["noul"] >= 0.5
    return answer["choice"]


def prob_of(answer, label):
    """Probability the judge gives to the labelled option."""
    if answer["type"] == "noul":
        return answer["noul"] if label is True else 1 - answer["noul"]
    return answer["probabilities"].get(label, 0.0)


def base_question(qid):
    for party in PARTIES:
        if qid.startswith(party + "_") and qid[len(party) + 1 :] in ("message_kind", "language", "wants_human"):
            return "<party>_" + qid[len(party) + 1 :]
    return qid


def run(cases, catalogs, qs, results_path):
    key = os.environ.get("TYPESAFE_API_KEY")
    if not key:
        sys.exit("TYPESAFE_API_KEY is not set (source .env first)")

    def one(case):
        state = build_state(case, catalogs)
        response = call_judge(key, state, build_questions(state, qs))
        return case["id"], {"state": state, "response": response}

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        results = dict(pool.map(one, cases))
    results_path.write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n")
    return results


def score(cases, results, version):
    rows = {}  # base question -> list of (case id, qid, label, answer)
    for case in cases:
        answers = results[case["id"]]["response"]["answers"]
        # A question set that adds options may label some cases differently.
        expect = case["expect"] | case.get("expect_by_version", {}).get(version, {})
        for qid, label in expect.items():
            if label is None:  # unlabelled for this version
                continue
            if qid not in answers:
                sys.exit(f"{case['id']}: {qid} is labelled but was not asked")
            rows.setdefault(base_question(qid), []).append((case["id"], qid, label, answers[qid]))
    return rows


def report(cases, results, rows, version):
    out = [f"Model `{MODEL}`, question set `{version}`, {len(cases)} cases.", ""]
    out += ["| Question | Labels | Accuracy | Mean P(label) | Misses |", "|---|---:|---:|---:|---|"]
    for qid, items in rows.items():
        hits = [top(a) == label for _, _, label, a in items]
        misses = [f"`{cid}`" for (cid, _, _, _), ok in zip(items, hits) if not ok]
        mean_p = statistics.mean(prob_of(a, label) for _, _, label, a in items)
        out.append(
            f"| `{qid}` | {len(items)} | {sum(hits)}/{len(items)} ({sum(hits) / len(items):.0%}) "
            f"| {mean_p:.2f} | {', '.join(misses) or '—'} |"
        )

    out += ["", "### At the default thresholds", ""]
    out += ["| Fact | Threshold | Labelled positive | Fired | True positives | False positives | Precision | Recall |"]
    out += ["|---|---:|---:|---:|---:|---:|---:|---:|"]
    checks = [
        ("buyer_payment", "says_sent", "fact"),
        ("buyer_payment", "says_not_sent", "fact"),
        ("buyer_payment", "says_not_sent", "guide"),
        ("seller_receipt", "says_received", "fact"),
        ("seller_receipt", "says_received", "guide"),
        ("seller_receipt", "says_not_received", "fact"),
        ("seller_receipt", "says_received_with_problem", "outside_scope"),
        ("<party>_wants_human", True, "human_request"),
        ("fraud_signal", True, "fraud"),
        ("claims_conflict", True, "conflict"),
    ]
    for qid, positive, threshold in checks:
        t = THRESHOLDS[threshold]
        items = rows.get(qid, [])
        fired = [(cid, label) for cid, _, label, a in items if prob_of(a, positive) >= t]
        tp = sum(label == positive for _, label in fired)
        pos = sum(label == positive for _, _, label, _ in items)
        fp = [cid for cid, label in fired if label != positive]
        precision = f"{tp / len(fired):.2f}" if fired else "—"
        recall = f"{tp / pos:.2f}" if pos else "—"
        out.append(
            f"| `{qid} = {str(positive).lower()}` | `{threshold}` {t} | {pos} | {len(fired)} | {tp} "
            f"| {len(fp)}{' (' + ', '.join(f'`{c}`' for c in fp) + ')' if fp else ''} | {precision} | {recall} |"
        )

    latencies = [r["response"]["latency_ms"] for r in results.values()]
    tokens = [r["response"]["usage"]["input_tokens"] for r in results.values()]
    out += [
        "",
        f"Latency: median {statistics.median(latencies):.0f} ms, max {max(latencies)} ms. "
        f"Input tokens: mean {statistics.mean(tokens):.0f}, max {max(tokens)}.",
    ]

    out += ["", "### Every miss", "", "| Case | Question | Label | Answer | P(label) |", "|---|---|---|---|---:|"]
    for items in rows.values():
        for cid, qid, label, a in items:
            if top(a) != label:
                got = f"{a['noul']:.2f}" if a["type"] == "noul" else f"{a['choice']}"
                out.append(f"| `{cid}` | `{qid}` | `{str(label).lower()}` | `{got}` | {prob_of(a, label):.2f} |")
    return "\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--score", action="store_true", help="re-score saved answers without calling the judge")
    parser.add_argument("--questions", default="questions.json", help="question set file in this directory")
    args = parser.parse_args()

    qs = json.loads((HERE / args.questions).read_text())
    cases = json.loads((HERE / "cases.json").read_text())
    ids = [c["id"] for c in cases]
    if len(ids) != len(set(ids)):
        sys.exit("duplicate case ids")
    results_path = HERE / f"results-{MODEL}-{qs['version']}.json"
    catalogs = load_catalogs()

    if args.score:
        results = json.loads(results_path.read_text())
    else:
        results = run(cases, catalogs, qs, results_path)
    rows = score(cases, results, qs["version"])
    text = report(cases, results, rows, qs["version"])
    (HERE / f"metrics-{qs['version']}.md").write_text(text)
    print(text)


if __name__ == "__main__":
    main()
