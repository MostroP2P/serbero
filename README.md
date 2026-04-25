<p align="center">
  <img src="serbero.jpg" alt="Serbero" width="400">
</p>

# Serbero

Dispute coordination, notification, and assistance system for the [Mostro](https://mostro.network/) ecosystem.

Serbero helps operators and users handle disputes more quickly, more consistently, and with better visibility — without expanding the system's fund-risk surface.

---

## Table of Contents

- [What It Does](#what-it-does)
- [What It Does Not Do](#what-it-does-not-do)
- [Architecture](#architecture)
- [Install from Release](#install-from-release)
- [Build from Source](#build-from-source)
- [Configuration Reference](#configuration-reference)
- [How Serbero Behaves at Runtime](#how-serbero-behaves-at-runtime)
- [Notification Format](#notification-format)
- [Observability and Audit Trail](#observability-and-audit-trail)
- [Degraded-Mode Behavior](#degraded-mode-behavior)
- [Troubleshooting](#troubleshooting)
- [Project Layout](#project-layout)
- [Running the Test Suite](#running-the-test-suite)
- [Technical Constraints](#technical-constraints)
- [Project Principles](#project-principles)
- [Project History](#project-history)
- [Release a New Version](#release-a-new-version)
- [License](#license)

---

## What It Does

Serbero sits alongside Mostro as a coordination layer that:

- **Detects disputes** by subscribing to Mostro's `kind 38386` dispute events on Nostr relays.
- **Notifies solvers** promptly via encrypted NIP-17 / NIP-59 gift-wrapped direct messages.
- **Deduplicates** across relay replays, reconnections, and process restarts using SQLite-backed persistence.
- **Tracks lifecycle state** (`new → notified → taken → waiting → escalated → resolved`) and records every transition.
- **Re-notifies unattended disputes** on a configurable timer and **suppresses further notifications** once a solver takes a dispute.
- **Records an audit trail** of every detection, notification attempt, state transition, and assignment event.

## What It Does Not Do

Serbero never moves funds. It cannot sign `admin-settle` or `admin-cancel`, and it is never granted credentials that would allow it to do so. Dispute-closing authority belongs to Mostro and its human operators.

Mostro operates normally with or without Serbero. If Serbero is offline, operators continue resolving disputes manually as they always have.

---

## Architecture

```text
┌──────────────┐      kind 38386 events     ┌────────────────────────────────┐
│    Mostro    │ ──────────────────────────▶│            Serbero             │
│              │                             │                                │
│  - Escrow    │                             │  Core (always on):             │
│  - Settle    │      NIP-59 gift wraps      │   - Detection + dedup          │
│  - Cancel    │ ◀─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ │   - Solver notification        │
│  - Perms     │       (to solvers)          │   - Lifecycle + assignment     │
│  - Chat      │                             │   - Re-notification timer      │
│              │   NIP-59 to shared keys     │                                │
│              │ ◀─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ │  AI mediation (opt-in):        │
│              │      (to dispute parties)   │   - Take-flow + clarifying msg │
└──────┬───────┘                             │   - Inbound ingest + dedup     │
       │                                     │   - Classification + policy    │
       │                                     │   - Summary or escalation      │
       │                                     │   - Escalation dispatch        │
       │                                     └─────────┬──────────────┬───────┘
       │ NIP-44 chat                                   │              │
       │ (parties ↔ Serbero)                  HTTP /chat/completions  │
       │                                       (OpenAI-compatible)    │
       │                                               │              │
       ▼                                               ▼              ▼
┌──────────────┐                                ┌─────────────┐  ┌──────────┐
│ Buyer/Seller │                                │  Reasoning  │  │  SQLite  │
│ (per-trade   │                                │  endpoint   │  │   v3     │
│ shared keys) │                                └─────────────┘  └──────────┘
└──────────────┘
```

- **Mostro** owns escrow state, permissions, and dispute-closing authority.
- **Serbero** owns notification, coordination, assignment visibility, audit logging, and — when AI mediation is enabled — guided mediation between dispute parties.
- **Reasoning backend** (only used when AI mediation is enabled): an OpenAI-compatible or Anthropic HTTP endpoint that Serbero calls for classification + summary drafting. Pluggable via config (`api_base` + `api_key_env`); covers hosted OpenAI, hosted Anthropic, PPQ.ai, self-hosted vLLM / llama.cpp / Ollama, LiteLLM, and any router proxy exposing `/chat/completions`. Outputs flow through a strict policy layer that suppresses fund-moving / dispute-closing instructions before any solver ever sees them.

---

## Install from Release

The fastest way to get Serbero is to download a pre-built binary.

```bash
curl -fsSL https://raw.githubusercontent.com/MostroP2P/serbero/main/install.sh | sh
```

The install script detects your OS and architecture, downloads the latest release, verifies the SHA-256 checksum of your specific asset (not with `--ignore-missing`, so a malformed release fails loudly), and installs the binary.

**Install location:** `/usr/local/bin` when that directory is writable by the running user, or when the script is running as root; otherwise `~/.local/bin` (created if missing). If the chosen directory is not already on your `PATH`, the installer prints the exact `export PATH="..."` line to add to your shell profile. Set `SERBERO_INSTALL_DIR` before running to pick a different location.

**Installer requirements:** a POSIX shell (`sh`, `dash`, `bash` all work) plus `curl` or `wget`. No `jq`, no Python, no other runtimes. Checksum verification additionally needs `sha256sum` (Linux, GNU coreutils) or `shasum -a 256` (macOS). If neither is present the installer prints a warning and continues without verifying.

**Runtime requirements for the binary:** none — not even Rust. The release binary is statically linked against musl (Linux) or the platform's system libraries (macOS / Windows) and is fully self-contained.

**Before you run it**, you will still need to have ready:

- A **hex-encoded** Nostr key pair for Serbero. You can generate one with [rana](https://github.com/grunch/rana) or any Nostr key tool; Bech32 keys (`nsec...`, `npub...`) must be converted to hex. The public key of this pair is the identity Serbero uses on Nostr — register it as a solver on the target Mostro instance before enabling AI mediation (see [Enable AI-guided mediation](#enable-ai-guided-mediation)).
- The **hex-encoded** public key of the Mostro instance you want to monitor, plus hex public keys for every solver you want to notify.
- At least one Nostr relay URL that carries Mostro dispute events.

After installing, fetch the sample config and edit it. The binary-only install does not pull repository files, so download the sample directly from the repo:

```bash
curl -fsSL https://raw.githubusercontent.com/MostroP2P/serbero/main/config.sample.toml -o config.toml
# Edit config.toml with your keys, relays, and solvers
serbero
```

If you cloned the repository (e.g. for Build from Source below), `cp config.sample.toml config.toml` works too — the file is checked in at the repo root.

### Manual download

If you prefer not to pipe to `sh`, download the binary for your platform from the [Releases](https://github.com/MostroP2P/serbero/releases) page, verify the checksum against `checksums.sha256`, make it executable, and place it somewhere on your `PATH`.

### Available platforms

| Platform            | Binary name                  |
|---------------------|------------------------------|
| Linux x86_64        | `serbero-linux-x86_64`       |
| Linux ARM64         | `serbero-linux-arm64`        |
| macOS Intel         | `serbero-macos-x86_64`       |
| macOS Apple Silicon | `serbero-macos-arm64`        |
| Windows x64         | `serbero-windows-x86_64.exe` |

---

## Build from Source

If you prefer to compile Serbero yourself (e.g. for development, debugging, or running an unreleased commit):

### Prerequisites (for building from source)

- **Rust toolchain**, stable, edition 2021. Install via [rustup](https://rustup.rs/). Only needed if you compile from source. Users who install the pre-built binary do not need Rust.

### Build

```bash
cargo build --release
```

The binary is produced at `./target/release/serbero`.

### Configure

Create `config.toml` in the working directory. A reference template is provided at [`config.sample.toml`](config.sample.toml) — copy it and fill in your values (see [Configuration Reference](#configuration-reference) for the full surface):

```toml
[serbero]
# Serbero's hex-encoded private key. Override via SERBERO_PRIVATE_KEY env var
# when running in production — do NOT commit this file with a real key.
private_key = "<hex-encoded private key>"
db_path     = "serbero.db"
log_level   = "info"

[mostro]
# Hex-encoded public key of the Mostro instance to monitor.
pubkey = "<hex-encoded public key>"

[[relays]]
url = "wss://relay.example.com"

[[solvers]]
pubkey     = "<hex-encoded solver public key>"
permission = "read"   # "read" or "write" — see notes below

[[solvers]]
pubkey     = "<hex-encoded solver public key>"
permission = "write"

[timeouts]
renotification_seconds                = 300   # re-notify disputes unattended this long
renotification_check_interval_seconds = 60    # how often to scan for unattended disputes
```

**About the `permission` field:** core dispute notifications (initial / re-notification / assignment) go to **every** configured solver regardless of this value. The escalation dispatcher routes on it: when a mediation session produces a handoff package, the structured `escalation_handoff/v1` DM goes to `write` solvers. `read` solvers are only targeted as a fallback, and only when `[escalation].fallback_to_all_solvers = true`. Setting the correct permission on every `[[solvers]]` entry is load-bearing.

### Run

Serbero reads `config.toml` from the current working directory. Secrets and a few operational parameters can be overridden via environment variables:

```bash
# Minimal invocation — expects ./config.toml
./target/release/serbero

# Override the private key via env (recommended for production)
SERBERO_PRIVATE_KEY="<hex-encoded private key>" ./target/release/serbero

# Point at a different config file (any path)
SERBERO_CONFIG=/etc/serbero/config.toml ./target/release/serbero

# Verbose tracing (module-level filters also supported)
SERBERO_LOG=debug ./target/release/serbero
SERBERO_LOG="serbero=debug,nostr_sdk=info" ./target/release/serbero
```

Shut down with `Ctrl-C` (SIGINT). On Unix hosts Serbero also catches SIGTERM (so `systemctl stop`, `kill`, and container shutdowns work). Both paths abort the re-notification timer and exit cleanly.

### Verify it's working

**Detection + initial notification.**

1. Start Serbero with a valid config pointing at a test relay.
2. Publish a `kind 38386` event with tags `s=initiated`, `z=dispute`, `y=<mostro_pubkey>`, `d=<dispute_id>`, and `initiator=buyer` (or `seller`).
3. Every configured solver should receive an encrypted gift-wrap DM within seconds containing the dispute ID, initiator role, and event timestamp.
4. Publish the same event again — **no duplicate** notification should be sent.
5. Restart Serbero pointed at the same `db_path` — previously-seen disputes should **not** be re-notified.

**Re-notification + assignment.**

1. After the initial notification, wait for `renotification_seconds` to elapse. Solvers should receive a single re-notification with `notif_type='re-notification'` and a status-aware payload.
2. Publish an `s=in-progress` event for the same dispute (this simulates a solver taking it via Mostro).
3. Serbero transitions the dispute to `taken`, records the `assigned_solver` from the event's `p` tag if present, and sends an **assignment** notification to all solvers.
4. No further re-notifications are sent for that dispute.

### Enable AI-guided mediation

AI-guided mediation is opt-in and layers on top of the core notification flow. To enable it:

1. **Register Serbero as a solver** on the target Mostro instance with at least `read` permission. Serbero's public key is derived from the `private_key` field in `[serbero]` — you can obtain it with any Nostr key tool (e.g. `nak key public <hex-secret-key>`). In **Mostrix**, go to **Settings → Solvers**, paste the hex pubkey, and select `read` permission. Serbero never holds fund-moving credentials.
2. **Provision a reasoning endpoint.** Any of the following works — pick whichever is easiest for you:
   - **PPQ.ai** (recommended, hosted, single key for many models) — sign up at <https://ppq.ai>; see the [PPQ.ai quick start](#quick-start-ppqai-easiest-hosted-option) below.
   - **Hosted OpenAI** — an account at <https://platform.openai.com> with billing enabled.
   - **Anthropic (Claude) directly** — an account at <https://console.anthropic.com>.
   - **Self-hosted** — vLLM, llama.cpp, Ollama, LiteLLM, or any router proxy exposing `/chat/completions`.
3. **Export the API key** under the env-var name configured in `[reasoning].api_key_env` (default: `SERBERO_REASONING_API_KEY`):

   ```bash
   export SERBERO_REASONING_API_KEY="<your key>"
   ```

4. **Add the mediation sections** to `config.toml` (see [Mediation configuration](#mediation-configuration)) and ensure the `prompts/phase3-*.md` files exist and contain real mediation content (the repo ships a working bundle — see [Prompt bundle](#prompt-bundle)).
5. **Restart**:

   ```bash
   ./target/release/serbero
   ```

   At startup you should see (alongside the core notification log lines):

   ```text
   loaded config                    mostro_pubkey=<hex> db_path=serbero.db relay_count=N solver_count=M ...
   prompt bundle loaded             prompt_bundle_id=phase3-default policy_hash=<hex>
   reasoning provider health check ok
   mediation engine task spawned
   ```

If the reasoning health check fails, mediation stays disabled for the run and core notifications continue unaffected:

```text
reasoning health check failed; mediation disabled for this run
(detection and notification continue unaffected)
```

If the initial solver-auth check fails, mediation refuses to open new sessions and a bounded retry loop runs in the background; warnings log per attempt.

### Verify mediation

1. **Cooperative path** — publish a buyer-initiated dispute that the policy layer can classify as `coordination_failure_resolvable` (e.g., a payment-timing case). Expected:
   - A `mediation_sessions` row with `state='awaiting_response'` and the policy hash pinned.
   - The buyer's and seller's **shared (per-trade) pubkeys** receive the first clarifying gift wrap (never their primary pubkeys).
   - After both parties reply, a `mediation_summaries` row is written and the assigned solver receives a `mediation_summary` notification. The session transitions `summary_pending → summary_delivered → closed`.
2. **Escalation path** — drive any of the 12 triggers (let `party_response_timeout_seconds` elapse without replies, exceed `max_rounds`, or take the reasoning provider offline). Expected:
   - Session transitions to `escalation_recommended`.
   - A `mediation_events` row records the trigger and a `handoff_prepared` row carries the escalation package for the dispatcher.
   - The configured solvers receive a `mediation_escalation_recommended` notification ("Needs human judgment").
3. **Provider swap** — stop the daemon, change `[reasoning].provider` / `model` / `api_base` / `api_key_env` to point at a different OpenAI-compatible endpoint, export the new key, and restart. New sessions call the new endpoint; no rebuild needed.
4. **Restart resume** — kill the daemon mid-session and restart. The startup-resume pass rebuilds the per-session key cache from the database, so inbound replies are deduped correctly and outbound responses go to the right shared keys.

For the full operator walkthrough see [`specs/003-guided-mediation/quickstart.md`](specs/003-guided-mediation/quickstart.md).

### Quick start: PPQ.ai (easiest hosted option)

If you just want mediation working with the minimum amount of setup, **PPQ.ai** is the path of least resistance: a single account, a single API key, and access to dozens of upstream models (OpenAI, Anthropic, Google, Mistral, DeepSeek, …) behind one OpenAI-compatible endpoint. You pay PPQ.ai; they handle the relationships with the upstream providers.

**Step 1 — get a key.** Sign up at <https://ppq.ai>, top up credits, and create an API key in the dashboard.

**Step 2 — export the key.**

```bash
export SERBERO_REASONING_API_KEY="your-ppq-key"
```

**Step 3 — paste this into `config.toml`** (replacing the existing `[mediation]` and `[reasoning]` blocks if present):

```toml
[mediation]
enabled = true
max_rounds = 2
party_response_timeout_seconds = 1800

[reasoning]
enabled                 = true
provider                = "openai-compatible"
api_base                = "https://api.ppq.ai"
api_key_env             = "SERBERO_REASONING_API_KEY"
model                   = "autoclaw"
request_timeout_seconds = 30
followup_retry_count    = 1
```

**Step 4 — restart Serbero.** You should see at startup:

```text
prompt bundle loaded ...
reasoning provider health check ok
mediation engine task spawned
```

**Common pitfalls (read these first if it doesn't work):**

- ❌ `provider = "PPQ.AI"` — wrong. The string is case-sensitive and there is no provider named that. Use `"openai-compatible"`.
- ❌ `provider = "ppqai"` — wrong. That name exists in code but routes to a "not yet implemented" stub on purpose. Use `"openai-compatible"`.
- ❌ `api_base = "https://api.ppq.ai/v1"` — wrong. PPQ.ai does not version its endpoint; the `/v1` suffix produces a 404. Use the bare host `https://api.ppq.ai`.
- ❌ `model = "ppq/autoclaw"` — wrong. PPQ.ai's auto-router is the bare string `"autoclaw"` (no `ppq/` prefix). Most other models, however, *do* use a vendor prefix like `openai/...`, `anthropic/...`, `google/...`.

**Picking a model.** `autoclaw` lets PPQ.ai pick the upstream model for you (a sensible default). If you want a specific model, pull the authoritative current list with:

```bash
curl -s https://api.ppq.ai/v1/models \
  -H "Authorization: Bearer $SERBERO_REASONING_API_KEY" \
  | jq '.data[].id' | sort
```

Use any ID from that list verbatim as `model = "..."`. Examples that PPQ.ai routinely exposes (subject to their catalog at any given time):

| Model ID                          | What it is                                  |
|-----------------------------------|---------------------------------------------|
| `autoclaw`                        | PPQ.ai's auto-router (default-friendly)     |
| `auto`                            | Alternate auto-router                       |
| `claude-sonnet-4.6`               | Anthropic Claude Sonnet (bare alias)        |
| `claude-opus-4.7`                 | Anthropic Claude Opus (bare alias)          |
| `claude-haiku-4.5`                | Anthropic Claude Haiku (bare alias)         |
| `openai/gpt-5`                    | OpenAI GPT-5                                |
| `openai/gpt-4o-mini`              | OpenAI GPT-4o mini (cheap, fast)            |
| `anthropic/claude-sonnet-4.5`     | Anthropic Claude Sonnet (prefixed alias)    |
| `google/gemini-2.5-flash`         | Google Gemini 2.5 Flash                     |
| `deepseek/deepseek-v3.2`          | DeepSeek V3.2                               |
| `x-ai/grok-4`                     | xAI Grok 4                                  |

If a model name returns `400 "<name> is not a valid model ID"`, it has been renamed or deprecated upstream — re-run the `/v1/models` query and pick a current ID.

---

## Configuration Reference

### `config.toml` structure

| Section          | Key                                        | Type     | Required | Notes                                                                                       |
|------------------|--------------------------------------------|----------|----------|---------------------------------------------------------------------------------------------|
| `[serbero]`      | `private_key`                              | string   | ✓        | Hex-encoded secret key. Override: `SERBERO_PRIVATE_KEY`.                                    |
| `[serbero]`      | `db_path`                                  | string   |          | Defaults to `serbero.db`. Override: `SERBERO_DB_PATH`.                                      |
| `[serbero]`      | `log_level`                                | string   |          | `trace` / `debug` / `info` / `warn` / `error`. Defaults to `info`. Override: `SERBERO_LOG`. |
| `[mostro]`       | `pubkey`                                   | string   | ✓        | Hex-encoded public key of the Mostro instance to monitor.                                   |
| `[[relays]]`     | `url`                                      | string   | ≥ 1      | One or more `wss://…` relay URLs. Serbero connects to all of them.                          |
| `[[solvers]]`    | `pubkey`                                   | string   |          | Hex-encoded solver public key.                                                              |
| `[[solvers]]`    | `permission`                               | string   |          | `"read"` or `"write"`. Core notifications go to every solver regardless. The escalation dispatcher routes structured handoff DMs to `write` solvers; `read` solvers only act as a fallback when `[escalation].fallback_to_all_solvers = true`. |
| `[timeouts]`     | `renotification_seconds`                   | integer  |          | Defaults to `300`. Disputes in `notified` state older than this are re-notified.            |
| `[timeouts]`     | `renotification_check_interval_seconds`    | integer  |          | Defaults to `60`. How often the re-notification timer scans the DB.                         |

### Environment variable overrides

| Variable                | Overrides                | Behavior                                                                 |
|-------------------------|--------------------------|--------------------------------------------------------------------------|
| `SERBERO_CONFIG`        | path of config file      | Defaults to `./config.toml`.                                             |
| `SERBERO_PRIVATE_KEY`   | `[serbero].private_key`  | Preferred way to inject the key in production / systemd / containers.   |
| `SERBERO_DB_PATH`       | `[serbero].db_path`      | Absolute or relative path.                                               |
| `SERBERO_LOG`           | `[serbero].log_level`    | Accepts either a level (`info`) or a `tracing-subscriber` filter string. |

Empty or whitespace-only env values are **ignored** — an accidentally-unset shell variable will not wipe a valid config entry.

### No CLI flag surface

Phases 1 and 2 intentionally do not commit to a CLI flag surface. The entire configuration lives in `config.toml` plus the environment variables above. If you need to point at a different config file, use `SERBERO_CONFIG`, not a flag.

### Mediation configuration

AI-guided mediation adds four optional config sections. They are all `#[serde(default)]` — if you omit them, the daemon runs in core-only mode (detection + notification + lifecycle). With both `[mediation].enabled = true` and `[reasoning].enabled = true`, the daemon runs the mediation bring-up (prompt-bundle load, reasoning health check) and spawns the mediation engine task.

```toml
[mediation]
enabled = true                   # AI mediation feature flag (see caveat above)
max_rounds = 2
party_response_timeout_seconds = 1800

# Solver-auth bounded revalidation loop (scope-controlled)
solver_auth_retry_initial_seconds      = 60
solver_auth_retry_max_interval_seconds = 3600
solver_auth_retry_max_total_seconds    = 86400
solver_auth_retry_max_attempts         = 24

[reasoning]
enabled                 = true
provider                = "openai"                    # "openai" / "openai-compatible" / "anthropic"
model                   = "gpt-5"                     # anything the endpoint supports
api_base                = "https://api.openai.com/v1" # swap for any OpenAI-compatible endpoint
api_key_env             = "SERBERO_REASONING_API_KEY" # vendor-neutral on purpose
request_timeout_seconds = 30
followup_retry_count    = 1                           # adapter owns the HTTP retry budget

[prompts]
system_instructions_path   = "./prompts/phase3-system.md"
classification_policy_path = "./prompts/phase3-classification.md"
escalation_policy_path     = "./prompts/phase3-escalation-policy.md"
mediation_style_path       = "./prompts/phase3-mediation-style.md"
message_templates_path     = "./prompts/phase3-message-templates.md"

[chat]
inbound_fetch_interval_seconds = 10
```

Per-field notes:

| Section        | Key                                       | Notes                                                                                                    |
|----------------|-------------------------------------------|----------------------------------------------------------------------------------------------------------|
| `[mediation]`  | `enabled`                                 | Master switch for AI mediation. `false` → daemon runs in core-only mode (detection + notification + lifecycle). |
| `[mediation]`  | `max_rounds`                              | Number of outbound+inbound pairs per session before `round_limit` escalation. Defaults to `2`.           |
| `[mediation]`  | `party_response_timeout_seconds`          | Triggers `party_unresponsive` escalation. Defaults to `1800` (30 min). Set to `0` to disable the timer.  |
| `[mediation]`  | `solver_auth_retry_*`                     | Bounded revalidation loop for Serbero's solver registration in Mostro. Defaults: 60s→3600s, 24h/24 caps. |
| `[reasoning]`  | `enabled`                                 | Must be `true` (alongside `[mediation].enabled`) for the engine task to spawn.                           |
| `[reasoning]`  | `provider`                                | `openai` / `openai-compatible` (hosted OpenAI, vLLM, llama.cpp, Ollama, LiteLLM, router proxies, PPQ.ai) or `anthropic` (native Messages API). See [Reasoning provider compatibility](#reasoning-provider-compatibility). `ppqai` / `openclaw` are NYI placeholders; use `openai-compatible` with the appropriate `api_base` instead. |
| `[reasoning]`  | `model`                                   | Whatever model the configured endpoint accepts (e.g., `gpt-5`, `gpt-4o-mini`, a self-hosted model name). |
| `[reasoning]`  | `api_base`                                | Where the HTTP client points. Change this to swap to any OpenAI-compatible endpoint without a rebuild.   |
| `[reasoning]`  | `api_key_env`                             | **Environment variable name** whose value holds the credential. Defaults to `SERBERO_REASONING_API_KEY`. The variable name is just configuration — point it at any var your secrets pipeline already sets. |
| `[reasoning]`  | `request_timeout_seconds`                 | Per-HTTP-call timeout. Floored to ≥ 1 s.                                                                 |
| `[reasoning]`  | `followup_retry_count`                    | Adapter-owned bounded retry budget. Additional attempts after the initial request on retryable errors. `0` = no retry. |
| `[prompts]`    | `*_path`                                  | Paths to the versioned prompt bundle files. The default paths match the `prompts/` tree in this repo.    |
| `[chat]`       | `inbound_fetch_interval_seconds`          | Mostro-chat inbound polling cadence used by the engine ingest loop.                                      |

### Reasoning provider compatibility

Two adapters ship today. The `openai` / `openai-compatible` adapter covers anything exposing `POST {api_base}/chat/completions` with OpenAI's request and response shape; the `anthropic` adapter speaks the native Messages API. Swap providers by changing `[reasoning].provider` and `[reasoning].api_base` — no rebuild needed.

| Provider                              | Config `provider`   | `api_base`                        | Notes                                                                                                               |
|---------------------------------------|---------------------|-----------------------------------|---------------------------------------------------------------------------------------------------------------------|
| OpenAI                                | `openai`            | `https://api.openai.com/v1`       | Default. Azure OpenAI works by pointing `api_base` at the Azure deployment URL.                                     |
| Self-hosted (vLLM, Ollama, llama.cpp) | `openai-compatible` | your local endpoint, e.g. `http://localhost:8080/v1` | Any server exposing `/chat/completions` with OpenAI's request/response shape.                                       |
| LiteLLM proxy                         | `openai-compatible` | your proxy URL                    | Same OpenAI wire format; LiteLLM fronts mixed upstream providers.                                                   |
| **PPQ.ai** (issue #39)                | `openai-compatible` | `https://api.ppq.ai`              | Aggregator that routes many models (`autoclaw`, `claude-sonnet-4.6`, `openai/gpt-5`, `google/gemini-2.5-flash`, …) behind a single OpenAI-compatible `/chat/completions` path. The adapter appends `/chat/completions` to `api_base` verbatim, so set `api_base` to the bare host (`https://api.ppq.ai`) — appending `/v1` would produce `https://api.ppq.ai/v1/chat/completions` and a 404, since PPQ.ai does not version its endpoint. Get the live model list with `curl -s https://api.ppq.ai/v1/models -H "Authorization: Bearer $KEY"`. See [PPQ.ai quick start](#quick-start-ppqai-easiest-hosted-option) for a beginner-friendly walkthrough. |
| Anthropic (issue #38)                 | `anthropic`         | `https://api.anthropic.com`       | Native Messages API. Uses `x-api-key` + `anthropic-version` headers and top-level `system` string. Any model name the account can invoke (e.g. `claude-3-5-sonnet-20241022`). |

`ppqai` and `openclaw` remain reserved provider names that currently route to a loud "not yet implemented" stub; configure PPQ.ai (including OpenClaw-style deployments) via `openai-compatible` with `api_base = "https://api.ppq.ai"` as per the table above.

### Secrets and environment variable resolution

- `config.toml` **never** carries secrets. The `[reasoning].api_key` field is `skip_deserializing`; TOML cannot set it.
- At startup the daemon reads the env variable named by `[reasoning].api_key_env` (default: `SERBERO_REASONING_API_KEY`) and stores the trimmed value. Surrounding whitespace or trailing newlines are stripped so nothing breaks bearer-token auth.
- If `[reasoning].enabled = true` and the named variable is unset or empty, the daemon returns a loud `Error::Config` and mediation stays off. Core notifications are unaffected.
- Choose a variable name that fits your secrets pipeline. The default is vendor-neutral so a freshly-cloned daemon does not imply "OpenAI-only"; point it at whatever variable your deployment environment is already exporting.

### Prompt bundle

The default layout — matched by `[prompts].*` defaults — is:

```text
prompts/
├── phase3-system.md             # mediation identity + authority limits + honesty discipline
├── phase3-classification.md     # 5 labels (snake_case canonical), 5 flags, confidence semantics
├── phase3-escalation-policy.md  # 12 triggers + handoff package shape
├── phase3-mediation-style.md    # tone, prohibited / preferred phrasings
└── phase3-message-templates.md  # first / follow-up / summary / escalation / timeout templates
```

The shipped bundle is real, working content matching `spec.md` §AI Agent Behavior Boundaries — assistance-only identity, no fund-moving authority, explicit honesty / uncertainty rules, allowed vs. disallowed outputs. Operators can amend it (e.g., to localize message templates) without code changes; the `policy_hash` regenerates deterministically at startup.

Every mediation session pins the bundle's `policy_hash` and `prompt_bundle_id`. Every audit row in `mediation_sessions`, `mediation_messages`, `mediation_summaries`, `mediation_events`, and `reasoning_rationales` carries the same pair, so behavior is reproducible from git history and the audit trail can be replayed against the exact bundle bytes that produced it.

Missing files → `Error::PromptBundleLoad`; mediation stays off, core notifications keep running.

---

## How Serbero Behaves at Runtime

### Startup

1. Load config from `$SERBERO_CONFIG` (or `./config.toml`) and apply env overrides.
2. Initialize `tracing-subscriber` using `SERBERO_LOG` or `log_level` from the config.
3. Open the SQLite database at `db_path`; run migrations (`schema_version` is tracked so this is idempotent and survives restarts).
4. Build the Nostr client from the private key and connect to every configured relay. nostr-sdk handles automatic reconnection with backoff.
5. Subscribe to `kind 38386` events for the configured Mostro pubkey with `s ∈ {initiated, in-progress}`, `z=dispute`, `y=<mostro_pubkey>`.
6. Spawn the re-notification timer task.
7. Enter the main notification-handling loop, dispatching each incoming event by its `s` tag.

### New dispute (`s=initiated`)

1. Extract `dispute_id` (from `d` tag), `initiator` (buyer or seller), `mostro_pubkey` (from `y`), and the event's `id` / `created_at`.
2. Attempt to `INSERT` into `disputes` (keyed by `dispute_id` with `ON CONFLICT DO NOTHING`).
   - **Duplicate** → log at debug, skip notification (idempotent replay / restart).
   - **Insert fails** → log an error and **do not notify**. This is a deliberate policy: the dispute may not be notified unless the same event is observed again after persistence recovers. See `plan.md` §Deduplication Strategy and `spec.md` clarification 3.
   - **Inserted** → proceed.
3. For each configured solver: parse pubkey → send NIP-17/NIP-59 gift-wrapped DM via `send_private_msg` → record the attempt (`sent` or `failed`, with the error message) in the `notifications` table.
4. If at least one notification was sent, transition the dispute `new → notified`, record the transition in `dispute_state_transitions`, and update `last_notified_at`.

### Dispute taken (`s=in-progress`)

1. Look up the dispute by `dispute_id`.
2. If the dispute is already in `taken` / `waiting` / `escalated` / `resolved`, treat as idempotent no-op.
3. Otherwise transition `→ taken`, record the solver pubkey from the event's `p` tag (if present) in `assigned_solver`, and record the state transition.
4. Send an **assignment notification** (`notif_type='assignment'`) to every configured solver.

### Re-notification timer

Every `renotification_check_interval_seconds`, the background task:

1. Computes `cutoff = now - renotification_seconds`.
2. Queries disputes with `lifecycle_state = 'notified' AND last_notified_at < cutoff`.
3. For each match: sends a re-notification (`notif_type='re-notification'`) including the current `lifecycle_state` and elapsed time, then bumps `last_notified_at` to prevent the same tick from double-firing.

Disputes that are already `taken`, `waiting`, `escalated`, or `resolved` never trigger re-notifications — the SQL filter enforces this.

### AI mediation engine

When `[mediation].enabled = true` and the bring-up succeeds, an engine task runs alongside the core notification loop. Each tick:

1. **Open** — for any new dispute that passes the eligibility gate, run the dispute-chat take-flow against Mostro, derive per-trade ECDH shared keys for both parties, persist a `mediation_sessions` row with the bundle pinned, and dispatch the first clarifying message to each party's **shared pubkey**. Outbound rows land in `mediation_messages` with provenance.
2. **Ingest** — `fetch_inbound` polls Mostro's chat surface every `inbound_fetch_interval_seconds`. `ingest_inbound` authenticates the inner event's author against the expected trade pubkey, pins the inner kind to `TextNote`, dedups by `(session_id, inner_event_id)` (so a relay replay or daemon restart never double-counts), recomputes `round_count` from the transcript, and updates per-party last-seen timestamps.
3. **Classify** — once both parties have replied for the round, the engine calls the reasoning provider's `classify` method with the full prompt bundle (so the `policy_hash` pin is honest) and the transcript. The response is parsed into a snake_case-keyed JSON shape (`coordination_failure_resolvable`, `conflicting_claims`, `suspected_fraud`, `unclear`, `not_suitable_for_mediation`) plus a confidence score and flags.
4. **Apply policy** — fraud / conflicting-claims flags escalate immediately; `confidence < 0.5` escalates with `low_confidence`; `Summarize` paired with a non-cooperative label escalates with `invalid_model_output`; any output containing fund-moving / dispute-closing phrases is suppressed and escalated with `authority_boundary_attempt`. Otherwise the policy decides between `AskClarification`, `Summarize`, or `Escalate(reason)`.
5. **Summarize or escalate** — the cooperative path calls `summarize`, persists `mediation_summaries`, and routes a `mediation_summary` notification to the assigned solver (or broadcasts to all configured solvers if none is assigned). The session transitions `summary_pending → summary_delivered → closed`. The escalation path writes a `handoff_prepared` row with the escalation handoff package and sends a `mediation_escalation_recommended` notification.
6. **Resume after restart** — `startup_resume_pass` rebuilds the per-session ECDH key cache from `mediation_sessions` so a daemon restart never breaks dedup or outbound key derivation. Sessions whose `policy_hash` no longer matches the loaded bundle are escalated with `policy_bundle_missing`.
7. **Auth retry** — if the initial solver-auth check fails, a bounded background loop revalidates with exponential backoff (knobs under `[mediation].solver_auth_retry_*`). Until it recovers, new session opens are deterministically refused; core notifications continue unaffected.

All rationale text is written only to the audit store (`reasoning_rationales`) and referenced by `rationale_id` (SHA-256 content hash) in general logs and `mediation_events.payload_json`. The `RationaleText::Debug` impl redacts the body to `<N bytes redacted>`.

---

## Notification Format

All notifications are NIP-17/NIP-59 gift-wrapped direct messages. The rumor content is plain UTF-8 text. The core notification flow uses three types:

### Initial notification

```text
New Mostro dispute requires attention.
dispute_id: <dispute-id>
initiator: <buyer|seller>
event_timestamp: <unix-seconds>
status: initiated
```

### Re-notification

```text
Mostro dispute is still unattended.
dispute_id: <dispute-id>
lifecycle_state: notified
time_elapsed_seconds: <n>
```

### Assignment notification

```text
Mostro dispute has been taken.
dispute_id: <dispute-id>
assigned_solver: <pubkey|unknown>
lifecycle_state: taken
```

### Mediation summary

`notif_type='mediation_summary'`, gift-wrapped to the assigned solver (targeted) or every configured solver (broadcast):

```text
<summary text from the reasoning provider>

Suggested next step: <single-line recommendation>
```

The summary text describes what each party reported and proposes a cooperative resolution. The "Suggested next step" line is advisory only — no fund-moving instructions are ever drafted (the policy layer suppresses them). The full rationale is preserved in `reasoning_rationales` and referenced by `rationale_id` in `mediation_events`.

### Mediation escalation

`notif_type='mediation_escalation_recommended'`, gift-wrapped to the assigned solver or all configured solvers:

```text
Mediation session <session_id> (dispute <dispute_id>) escalated —
trigger: <snake_case_trigger>. Needs human judgment.
```

The compact body keeps DMs readable across Nostr clients; the full handoff package (evidence refs, rationale refs, prompt bundle id, policy hash, assembled-at timestamp) lives alongside in `mediation_events` as a `handoff_prepared` row, which the escalation dispatcher consumes to deliver a structured `escalation_handoff/v1` DM to the write-permission solver.

### Pre-take escalation

`notif_type='mediation_escalation_recommended'`, broadcast to **all** configured solvers (no session was opened, so there is no assigned solver):

```text
Dispute <dispute_id> escalated before mediation take —
trigger: <snake_case_trigger>. Serbero ran the reasoning verdict and
the policy layer said this dispute is not a mediation candidate.
No session was opened. Needs human judgment.
```

This fires when the reasoning verdict at session-open time is negative (e.g. `suspected_fraud` or `not_suitable_for_mediation`). No `TakeDispute` is issued and no `mediation_sessions` row is committed.

### Final resolution report

`notif_type='mediation_resolution_report'`, broadcast to all configured solvers:

```text
Final resolution report for dispute <dispute_id>.
resolution: <settled|cancelled|...>
escalation_count: <N>
rounds: <N>
duration_seconds: <N>
handoff: <true|false>
```

Emitted once when a dispute that had any mediation context (session rows, pre-take escalation events, or mediation messages) transitions to a resolved terminal state. Idempotent: duplicate `dispute_resolved` events do not trigger additional reports. Contains no rationale text.

Notifications **never include** the initiator's primary pubkey — only their trade role (buyer / seller). Outbound mediation gift wraps address parties' **shared (per-trade) pubkeys**, never their primary pubkeys.

---

## Observability and Audit Trail

Serbero emits structured `tracing` spans and events at every decision point:

- `detected` / `duplicate_skip` / `persistence_failed`
- `notification_sent` / `notification_failed` (with `solver` and error)
- `state_transition` (with `from`, `to`, `trigger`)
- `assignment_detected` (with `assigned_solver`)
- `assignment_notification_sent` / `assignment_notification_failed`
- `renotification_tick` (with `count`)
- `start_attempt_started` / `start_attempt_stopped` (with `trigger`, `stop_reason`)
- `reasoning_verdict` / `reasoning_verdict_negative`
- `take_dispute_issued` (with `outcome: success|failure`)
- `solver_dispute_escalation_notified` (pre-take handoff)
- `solver_final_resolution_report_sent`

Use `SERBERO_LOG` to tune the filter:

```bash
SERBERO_LOG="serbero=debug,nostr_sdk=warn" ./target/release/serbero
```

### SQLite tables

Every audit-relevant fact is also in the database, so you can reconstruct the history of a dispute without grepping logs.

**Core tables:**

- `disputes` — one row per detected dispute, including `lifecycle_state`, `assigned_solver`, `last_notified_at`, `last_state_change`.
- `notifications` — one row per notification attempt (`initial`, `re-notification`, `assignment`, `mediation_summary`, `mediation_escalation_recommended`), with `status` (`sent` / `failed`) and `error_message`.
- `dispute_state_transitions` — every state change with `from_state`, `to_state`, `transitioned_at`, `trigger`.
- `schema_version` — tracks applied migrations; migrations are idempotent and wrapped in per-version transactions.

**Mediation tables (migration v3):**

- `mediation_sessions` — one row per opened session: `state`, `round_count`, the pinned `prompt_bundle_id` + `policy_hash`, `buyer_shared_pubkey` / `seller_shared_pubkey`, per-party last-seen timestamps.
- `mediation_messages` — every outbound and inbound message, dedup-keyed by `(session_id, inner_event_id)`, with the bundle pinned on outbound rows.
- `reasoning_rationales` — content-addressed (SHA-256) rationale text from every classify / summarize call, with provider, model, and bundle pinned. Operator-only audit store; the body never leaks into general logs or `mediation_events.payload_json`.
- `mediation_summaries` — one row per cooperative summary delivered, with classification, confidence, summary text, suggested next step, and the rationale reference id.
- `mediation_events` — every lifecycle / audit event (session-open, classification, summary-generated, escalation-triggered, handoff-prepared, auth-retry-{attempt,recovered,terminated}, etc.) with the bundle and (where applicable) rationale referenced by id.

Inspect with the usual `sqlite3` CLI:

```bash
# Core — recent disputes + notifications
sqlite3 serbero.db "SELECT dispute_id, lifecycle_state, assigned_solver, last_state_change \
                    FROM disputes ORDER BY detected_at DESC LIMIT 20;"

sqlite3 serbero.db "SELECT dispute_id, notif_type, status, sent_at, error_message \
                    FROM notifications ORDER BY sent_at DESC LIMIT 50;"

sqlite3 serbero.db "SELECT dispute_id, from_state, to_state, trigger, transitioned_at \
                    FROM dispute_state_transitions ORDER BY id DESC LIMIT 50;"

# Mediation — sessions and their state
sqlite3 serbero.db "SELECT session_id, dispute_id, state, round_count, policy_hash \
                    FROM mediation_sessions ORDER BY started_at DESC LIMIT 20;"

# Mediation — full transcript for a session
sqlite3 serbero.db "SELECT direction, party, inner_event_created_at, substr(content,1,80) \
                    FROM mediation_messages WHERE session_id='<sid>' \
                    ORDER BY inner_event_created_at ASC;"

# Mediation — lifecycle / escalation events
sqlite3 serbero.db "SELECT kind, substr(payload_json,1,120), occurred_at \
                    FROM mediation_events WHERE session_id='<sid>' \
                    ORDER BY id ASC;"

# Mediation — rationale audit store (operator-only; gate behind filesystem permissions)
sqlite3 serbero.db "SELECT rationale_id, provider, model, policy_hash, generated_at \
                    FROM reasoning_rationales ORDER BY generated_at DESC LIMIT 20;"
```

### Authority-boundary audit

Re-confirm at any time that no Mostro admin action ever flowed through Serbero:

```bash
sqlite3 serbero.db "SELECT COUNT(*) FROM notifications \
                    WHERE notif_type IN ('admin_settle','admin_cancel');"
# Expected: 0
```

Combined with the constitutional invariant that Serbero holds no credentials for those actions, this satisfies *I. Fund Isolation First*.

---

## Degraded-Mode Behavior

| Failure                                        | Behavior                                                                                                                                                                                                        |
|------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Single relay drops                             | nostr-sdk auto-reconnects with backoff. Other relays continue serving events.                                                                                                                                   |
| All relays drop                                | Reconnection keeps retrying. Notifications halt until a relay comes back. The daemon keeps running.                                                                                                            |
| SQLite read failure                            | Notifications halt. The daemon logs the error, keeps retrying DB access, and resumes notifications when persistence recovers. Deduplication integrity is prioritized over delivery.                            |
| SQLite write failure on INSERT                 | No outbound queue exists. The dispute may not be notified at all unless the same event is observed again after persistence recovers (e.g., a relay retransmission or operator replay).                         |
| Notification send failure                      | Logged as a `failed` row in `notifications` with the error message. Individual sends are not retried. The re-notification timer covers disputes that stay unattended.                                          |
| Invalid solver pubkey in config                | Logged as a `failed` notification row; other solvers still receive the notification. The daemon keeps running.                                                                                                 |
| No solvers configured                          | Logged as a WARN at startup. Serbero still detects and persists disputes, but the notification loop is skipped — the audit trail is preserved.                                                                 |
| Serbero fully offline                          | Mostro operates normally. Solvers resolve disputes manually. When Serbero comes back and reconnects, it resumes detecting **new** events. Historic events delivered while offline are the relay's to replay.   |
| Mediation — prompt bundle missing / unloadable | `Error::PromptBundleLoad` at startup; mediation stays disabled for the run. Core notifications continue unaffected. Resumed sessions whose pinned `policy_hash` no longer matches the loaded bundle are escalated with `policy_bundle_missing`. |
| Mediation — reasoning provider health-check fails | Mediation stays disabled for the run; engine task is not spawned; core notifications continue unaffected. Operator-actionable error logs `provider`, `model`, `api_base`, and the underlying error. |
| Mediation — reasoning provider unreachable mid-session | The classify / summarize call surfaces `ReasoningError`; the session escalates with `reasoning_unavailable`. Adapter-owned bounded retry budget (`followup_retry_count`) covers transient errors first. |
| Mediation — reasoning provider returns garbage | `MalformedResponse` → escalates with `reasoning_unavailable`. Structurally inconsistent shape (e.g. `Summarize` + non-cooperative label) escalates with `invalid_model_output`. |
| Mediation — reasoning output crosses authority boundary | Suppressed by `AUTHORITY_BOUNDARY_PHRASES` detection; session escalates with `authority_boundary_attempt`. The full output is preserved in the rationale store; general logs reference it by id only. |
| Mediation — solver auth lost at startup        | Initial check fails → bounded auth-retry loop runs in the background with exponential backoff; session opens are deterministically refused until recovery. Core notifications unaffected. |
| Mediation — solver auth revoked mid-session    | Outbound auth failure surfaces as `AuthorizationLost`; affected session escalates with `authorization_lost`. Auth-retry loop resumes. |
| Mediation — party stops responding             | After `party_response_timeout_seconds`, session escalates with `party_unresponsive`. Set the timeout to `0` to disable the check (test / staging only). |
| Mediation — round limit reached                | After `max_rounds` outbound+inbound pairs without convergence, session escalates with `round_limit`. |
| Mediation — daemon restart mid-session         | `startup_resume_pass` rebuilds the per-session ECDH key cache from `mediation_sessions`; inbound dedup and outbound key derivation survive intact. The restart-dedup integration test pins this. |
| Mediation — summary undeliverable              | If the summary persists but every solver send fails (or no recipients are configured), session escalates with `notification_failed` so the audit trail surfaces it instead of stranding it at `summary_pending`. |

---

## Troubleshooting

### Mediation won't start: `unknown reasoning provider 'X'`

The `provider` field is case-sensitive and the only accepted strings are `openai`, `openai-compatible`, and `anthropic`. Anything else fails fast at startup. The strings `ppqai` and `openclaw` are reserved in code but route to a "not yet implemented" stub that fails on purpose — to use PPQ.ai, set `provider = "openai-compatible"` instead. See [Quick start: PPQ.ai](#quick-start-ppqai-easiest-hosted-option).

### Mediation won't start: `<model> is not a valid model ID`

Your `[reasoning].model` value isn't in the upstream catalog. Pull the live list from your provider:

```bash
# PPQ.ai
curl -s https://api.ppq.ai/v1/models \
  -H "Authorization: Bearer $SERBERO_REASONING_API_KEY" \
  | jq '.data[].id' | sort

# Hosted OpenAI
curl -s https://api.openai.com/v1/models \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  | jq '.data[].id' | sort
```

Pick any ID from the list verbatim. Common foot-guns: a `ppq/` prefix on PPQ.ai's bare-name aliases (use `autoclaw`, not `ppq/autoclaw`); or a stale model name that the provider has since renamed.

### Mediation won't start: `api_key_env "X" is unset or empty`

`[reasoning].api_key_env` names an environment variable; the actual secret must be exported before launching the daemon. Check with `echo $YOUR_VAR_NAME`. Empty or whitespace-only values are treated as unset. Whatever name you put in `api_key_env` must match the name you `export`.

### Reasoning calls fail with HTTP 404

`[reasoning].api_base` is wrong for your provider. Two common cases:

- **PPQ.ai**: must be `https://api.ppq.ai` (bare host). The adapter appends `/chat/completions`. Adding `/v1` produces a 404 because PPQ.ai does not version its endpoint.
- **Self-hosted OpenAI-compatible**: most servers expect a `/v1` prefix (e.g. `http://localhost:11434/v1` for Ollama). The adapter still appends `/chat/completions`.

### Mediation engine isn't spawned and no error is logged

Check that BOTH flags are `true`: `[mediation].enabled = true` AND `[reasoning].enabled = true`. Either one set to `false` (or omitted) keeps the daemon in core-only mode.

### Solvers receive notifications but mediation never engages parties

The Mostro instance must register Serbero's pubkey as a solver with at least `read` permission before the daemon will issue `TakeDispute` calls. Inspect the bring-up logs for `solver-auth` warnings. The auth-retry loop revalidates with exponential backoff; new sessions are deterministically refused until it recovers. Core notifications are unaffected.

### Prompt bundle missing at startup

`Error::PromptBundleLoad` means one of the `prompts/phase3-*.md` files referenced in `[prompts]` is not found at the configured path. The repo ships a working bundle — make sure your working directory is the repo root, or set absolute paths in `[prompts]`. Mediation stays disabled for the run; core notifications keep running.

---

## Project Layout

```text
.
├── Cargo.toml, Cargo.lock
├── clippy.toml, rustfmt.toml
├── config.toml                          (you create this; gitignored)
├── prompts/                             # versioned mediation prompt bundle
│   ├── phase3-system.md
│   ├── phase3-classification.md
│   ├── phase3-escalation-policy.md
│   ├── phase3-mediation-style.md
│   └── phase3-message-templates.md
├── src/
│   ├── main.rs                          # binary entry point
│   ├── lib.rs                           # re-exports modules for tests
│   ├── error.rs                         # Error + Result types
│   ├── config.rs                        # TOML + env loader
│   ├── daemon.rs                        # main loop + re-notification + mediation bring-up
│   ├── dispatcher.rs                    # event routing by `s` tag
│   ├── nostr/                           # Client, subscriptions, gift-wrap notifier
│   ├── handlers/                        # s=initiated / s=in-progress handlers
│   ├── chat/                            # dispute-chat surface (mediation)
│   │   ├── dispute_chat_flow.rs         # take-flow against Mostro
│   │   ├── inbound.rs                   # fetch + ingest with author auth + dedup
│   │   ├── outbound.rs                  # build wraps for shared pubkeys
│   │   └── shared_key.rs                # ECDH per-trade key derivation
│   ├── reasoning/                       # reasoning-provider abstraction
│   │   ├── mod.rs                       # ReasoningProvider trait + factory
│   │   ├── openai.rs                    # OpenAI-compatible adapter
│   │   ├── not_yet_implemented.rs       # NYI guard for unshipped vendor names
│   │   └── health.rs                    # startup health check
│   ├── prompts/                         # prompt-bundle loader + hash
│   │   ├── mod.rs                       # PromptBundle + load_bundle
│   │   └── hash.rs                      # deterministic SHA-256 of bundle bytes
│   ├── mediation/                       # mediation engine
│   │   ├── mod.rs                       # run_engine, draft_and_send_initial_message,
│   │   │                                # deliver_summary, notify_solvers_escalation,
│   │   │                                # startup_resume_pass
│   │   ├── session.rs                   # open_session + auth gate
│   │   ├── start.rs                     # try_start_for: unified entry for event-driven + tick
│   │   ├── auth_retry.rs                # bounded solver-auth revalidation
│   │   ├── policy.rs                    # classification → action decision
│   │   ├── eligibility.rs               # composed eligibility predicate
│   │   ├── follow_up.rs                 # mid-session ingest + classify loop
│   │   ├── transcript.rs                # transcript builder for reasoning calls
│   │   ├── report.rs                    # final solver-facing resolution report
│   │   ├── summarizer.rs                # summarize + AUTHORITY_BOUNDARY_PHRASES
│   │   ├── router.rs                    # targeted vs broadcast solver routing
│   │   └── escalation.rs                # 12 triggers + handoff package
│   ├── db/
│   │   ├── mod.rs                       # connection + pragmas
│   │   ├── migrations.rs                # schema_version (v1, v2, v3) + per-version txns
│   │   ├── disputes.rs                  # insert, get, lifecycle state helpers
│   │   ├── notifications.rs             # record_notification{,_logged}
│   │   ├── state_transitions.rs         # unattended dispute query
│   │   ├── mediation.rs                 # mediation_sessions + mediation_messages
│   │   ├── mediation_events.rs          # lifecycle / audit events
│   │   └── rationales.rs                # content-addressed rationale audit store
│   └── models/
│       ├── config.rs                    # typed config structs (incl. mediation sections)
│       ├── dispute.rs                   # Dispute + LifecycleState state machine
│       ├── notification.rs              # NotificationStatus / NotificationType
│       ├── mediation.rs                 # ClassificationLabel, Flag, EscalationTrigger, …
│       └── reasoning.rs                 # ReasoningRequest / Response + RationaleText
├── tests/
│   ├── common/mod.rs                    # MockRelay harness + SolverListener
│   ├── phase1_detection.rs              ├── phase2_lifecycle.rs
│   ├── phase1_dedup.rs                  ├── phase2_assignment.rs
│   ├── phase1_failure.rs                ├── phase2_renotification.rs
│   ├── phase3_session_open.rs           ├── phase3_summary_escalation.rs
│   ├── phase3_session_open_gating.rs    ├── phase3_authority_boundary.rs
│   ├── phase3_response_ingest.rs        ├── phase3_escalation_triggers.rs
│   ├── phase3_response_dedup_restart.rs ├── phase3_provider_swap.rs
│   ├── phase3_stale_message.rs          ├── phase3_provider_not_yet_implemented.rs
│   ├── phase3_routing_model.rs          ├── phase3_cooperative_summary.rs
│   ├── phase3_event_driven_start.rs     ├── phase3_superseded_by_human.rs
│   ├── phase3_take_reasoning_coupling.rs├── phase3_external_resolution_report.rs
│   ├── phase3_followup_round.rs         ├── phase3_followup_summary.rs
│   ├── phase3_followup_reasoning_failure.rs
│   └── fixtures/prompts/                # stable bundle for tests (untouched)
└── specs/
    ├── 002-phased-dispute-coordination/ # core notification spec + plan + tasks
    └── 003-guided-mediation/            # mediation spec + plan + tasks + contracts + quickstart
```

---

## Running the Test Suite

> **Note:** Tests require the Rust toolchain. If you installed Serbero via the release binary and want to run tests, clone the repo and build from source.

The crate ships **228 tests**: 179 inline `#[cfg(test)]` lib unit tests (covering parsers, policy decisions, audit-store invariants, migrations, prompt loading, …) plus 49 integration tests that spin up an in-process `nostr-relay-builder::MockRelay` (and, where relevant, an `httpmock` reasoning endpoint) and exercise the daemon end-to-end.

```bash
# Unit tests only (fast)
cargo test --lib

# Full suite (unit + integration)
cargo test --all-targets

# Lint + format checks
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# Release build
cargo build --release
```

**Core integration tests** cover the scenarios from `specs/002-phased-dispute-coordination/quickstart.md`:

| Test file                        | What it verifies                                                                 |
|----------------------------------|----------------------------------------------------------------------------------|
| `phase1_detection.rs`            | New dispute detected → every solver receives correct gift-wrapped DM             |
| `phase1_dedup.rs`                | Duplicate events and daemon restarts produce exactly one notification            |
| `phase1_failure.rs`              | Invalid solver pubkey → `failed` row recorded, other solvers still notified; no-solvers path persists without notifying |
| `phase2_lifecycle.rs`            | `new → notified → taken` transition chain recorded in correct order              |
| `phase2_assignment.rs`           | `s=in-progress` → lifecycle_state `taken`, assigned_solver set, assignment notification delivered, no further re-notifications |
| `phase2_renotification.rs`       | Unattended disputes re-notified past timeout; taken disputes are not             |

**Mediation integration tests** cover the scenarios from `specs/003-guided-mediation/quickstart.md`:

| Test file                                       | What it verifies                                                                                                   |
|-------------------------------------------------|--------------------------------------------------------------------------------------------------------------------|
| `phase3_session_open.rs`                        | Open session → take-flow → first clarifying message dispatched to both parties' shared pubkeys                      |
| `phase3_session_open_gating.rs`                 | Reasoning health-check failure → session open refused; core notifications still fire                                |
| `phase3_response_ingest.rs`                     | Inbound replies authenticated, dedup-keyed by inner event id, transcript + last-seen updated                        |
| `phase3_response_dedup_restart.rs`              | Inbound dedup survives daemon restart (`startup_resume_pass` rebuilds the session-key cache)                       |
| `phase3_stale_message.rs`                       | Stale inbound is persisted but does not advance the session                                                        |
| `phase3_routing_model.rs`                       | Targeted (assigned solver) vs broadcast routing; fallback when assigned solver is unknown                          |
| `phase3_cooperative_summary.rs`                 | Cooperative happy path: summary persisted, session closes, assigned solver notified, rationale-redaction + audit consistency pinned |
| `phase3_summary_escalation.rs`                  | Empty recipient list → `notification_failed` escalation (no stranded summaries)                                    |
| `phase3_authority_boundary.rs`                  | Fund-moving / dispute-closing output suppressed; session escalates with `authority_boundary_attempt`               |
| `phase3_escalation_triggers.rs`                 | All applicable triggers fire correctly: `conflicting_claims`, `fraud_indicator`, `low_confidence`, `party_unresponsive`, `round_limit`, `reasoning_unavailable`, `authorization_lost` |
| `phase3_provider_swap.rs`                       | Two `OpenAiProvider`s pointing at distinct httpmock endpoints both work; `openai-compatible` routes to the same adapter |
| `phase3_event_driven_start.rs`                  | Event-driven path opens session without the background tick running                                                |
| `phase3_superseded_by_human.rs`                 | External resolution (human solver) closes session + fires final resolution report to all solvers                   |
| `phase3_take_reasoning_coupling.rs`             | Negative reasoning verdict (fraud flag or model escalate) skips TakeDispute entirely                               |
| `phase3_external_resolution_report.rs`          | Final solver-facing report emitted for every dispute with mediation context; idempotent on re-fire                 |
| `phase3_followup_round.rs`                      | Mid-session happy path — party replies trigger second outbound within one ingest tick                              |
| `phase3_followup_summary.rs`                    | Mid-session summarize branch fires exactly once and closes session                                                 |
| `phase3_followup_reasoning_failure.rs`          | Three consecutive reasoning failures escalate with `reasoning_unavailable`                                         |
| `phase3_provider_not_yet_implemented.rs`        | Unshipped vendor names (`ppqai`, `openclaw`) fail loudly at startup with an actionable error                       |

---

## Technical Constraints

- **Rust**, stable, edition 2021.
- **[nostr-sdk](https://docs.rs/nostr-sdk/0.44.1) v0.44.1** for all Nostr communication (subscriptions, event handling, NIP-17 / NIP-59 gift-wrap messaging). The `nip59`, `nip44`, and `nip04` features are enabled.
- **[mostro-core](https://docs.rs/mostro-core/0.8.4) v0.8.4** for protocol types (`NOSTR_DISPUTE_EVENT_KIND`, dispute `Status` enum, `Action` variants).
- **[rusqlite](https://docs.rs/rusqlite) 0.31** with the `bundled` feature — no external SQLite install required. No ORM, no storage abstraction layer.
- **[tokio](https://docs.rs/tokio) 1** runtime (required by nostr-sdk), `tracing` for structured logs, `toml` + `serde` for configuration.
- Prefers **Nostr-native** communication (encrypted gift wraps) over external bridges or dashboards.

---

## Project Principles

Serbero is governed by a [constitution](.specify/memory/constitution.md) that defines non-negotiable rules. The key principles:

1. **Fund Isolation First** — never touch funds or sign dispute-closing actions.
2. **Protocol-Enforced Security** — safety boundaries enforced by Mostro, not by prompts or model behavior.
3. **Human Final Authority** — complex, adversarial, or ambiguous disputes always go to a human operator.
4. **Operator Notification as Core** — detecting and notifying operators is a primary responsibility.
5. **Assistance Without Authority** — assist and guide, never impose outcomes.
6. **Auditability by Design** — every action, classification, and state transition is logged.
7. **Graceful Degradation** — Mostro works fine without Serbero.
8. **Privacy by Default** — minimum necessary information to each participant.
9. **Nostr-Native Coordination** — encrypted messaging first, external integrations second.
10. **Portable Reasoning Backends** — no lock-in to any single AI provider or runtime.
11. **Incremental Scope** — evolve in stages through explicit specifications.
12. **Honest System Behavior** — surface uncertainty, never fabricate evidence.
13. **Mostro Compatibility** — complement Mostro, never duplicate or weaken its authority.

---

## Project History

Serbero was built in four numbered phases. Everything below has shipped on `main`; this section exists so contributors can map issues, PRs, and spec folders back to the original scope. Newcomers can safely skip it.

| Phase | Scope                                                              | Specs                                                                  |
|-------|--------------------------------------------------------------------|------------------------------------------------------------------------|
| 1     | Always-on dispute listener and solver notification                 | [`specs/002-phased-dispute-coordination/`](specs/002-phased-dispute-coordination/) |
| 2     | Intake tracking, assignment visibility, re-notification timer      | (same as above)                                                        |
| 3     | AI-guided mediation engine — take-flow, classification, summary, escalation routing, audit-trail rationale store | [`specs/003-guided-mediation/`](specs/003-guided-mediation/) |
| 4     | Escalation dispatcher — consumes handoff packages, routes structured `escalation_handoff/v1` DMs to write-permission solvers, supersession + parse-failed handling | [`specs/004-escalation-execution/`](specs/004-escalation-execution/) |

The escalation dispatcher deliberately does NOT track solver acks, retry, or re-escalate — the existing re-notification loop covers follow-up. There is no dedicated operator UI: inspection lives in the `sqlite3` query recipes documented in `specs/004-escalation-execution/quickstart.md`.

Vendor-specific reasoning adapters (Anthropic native, PPQ.ai validation) are tracked separately as issues [#38](https://github.com/MostroP2P/serbero/issues/38) and [#39](https://github.com/MostroP2P/serbero/issues/39). The OpenAI-compatible adapter already covers hosted OpenAI, PPQ.ai, vLLM, llama.cpp, Ollama, LiteLLM, and any router proxy exposing `/chat/completions`; the native Anthropic adapter covers Claude direct.

---

## Release a New Version

Serbero uses [cargo-release](https://github.com/crate-ci/cargo-release) to automate versioning and tagging.

```bash
# Install cargo-release (once)
cargo install cargo-release

# Bump patch version (0.1.0 → 0.1.1), commit, tag, and push
cargo release patch --execute

# Or bump minor (0.1.0 → 0.2.0)
cargo release minor --execute

# Or bump major (0.1.0 → 1.0.0)
cargo release major --execute
```

Pushing a tag `v*.*.*` triggers a [GitHub Actions workflow](.github/workflows/release.yml) that builds binaries for all supported platforms and publishes them to the [Releases](https://github.com/MostroP2P/serbero/releases) page. Tags containing `-rc` or `-beta` are marked as pre-releases automatically.

You can also create a release manually with `git tag`:

```bash
git tag v0.1.0
git push origin v0.1.0
```

---

## License

Serbero is licensed under the [MIT License](LICENSE).
