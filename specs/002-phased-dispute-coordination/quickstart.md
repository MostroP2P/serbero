# Quickstart: Serbero

## Prerequisites

- Rust toolchain (stable, edition 2021)
- Access to at least one Nostr relay
- A Nostr key pair for Serbero, in hex-encoded form
- Hex-encoded Nostr public keys of the Mostro instance and each solver

Serbero's configuration uses hex-encoded keys throughout. If you hold
your keys in Bech32 form (`nsec...`, `npub...`), convert them to hex
before placing them in the config.

## Configuration

Create `config.toml`:

```toml
[serbero]
# Serbero's hex-encoded private key. Can be overridden with the
# SERBERO_PRIVATE_KEY environment variable.
private_key = "<hex-encoded private key>"

[mostro]
# Hex-encoded public key of the Mostro instance to monitor
pubkey = "<hex-encoded public key>"

[[relays]]
url = "wss://relay.example.com"

[[solvers]]
pubkey = "<hex-encoded solver public key>"
# Permission level: "read" or "write".
# Phase 1 notifies ALL configured solvers regardless of this value.
# Permission becomes operationally relevant in later phases (e.g.,
# escalation routing in Phase 4).
permission = "read"

[[solvers]]
pubkey = "<hex-encoded solver public key>"
permission = "write"

[timeouts]
# Re-notification timeout in seconds (Phase 2+, default: 300)
renotification_seconds = 300
```

## Build and Run

Serbero reads its configuration from `config.toml` in the working
directory and applies environment-variable overrides for secrets and
operational parameters. The Phase 1 / Phase 2 plan does not commit to
a CLI flag surface, so the examples below only rely on the working
directory and environment variables.

```bash
# Build
cargo build --release

# Run — expects config.toml in the current directory
./target/release/serbero

# Run with env override for the private key
SERBERO_PRIVATE_KEY=<hex-encoded private key> ./target/release/serbero
```

## Verify Phase 1

1. Start Serbero with a valid config pointing to a test relay.
2. Publish a kind 38386 event with `s=initiated`, `z=dispute`,
   `y=<mostro_instance>`, `d=<dispute_id>`, `initiator=buyer`.
3. Verify each configured solver receives an encrypted gift-wrap
   message containing the dispute ID, initiator role, and timestamp.
4. Publish the same event again — verify no duplicate notification.
5. Restart Serbero — verify no re-notification for the same dispute.

## Verify Phase 2

1. After initial notification, wait for the re-notification timeout.
2. Verify solvers receive a re-notification with status "unattended."
3. Publish an `in-progress` status update for the dispute (simulating
   a solver taking it via Mostro).
4. Verify Serbero transitions the dispute to "taken" and sends an
   assignment notification to all solvers.
5. Verify no further re-notifications are sent for that dispute.

## Database Location

SQLite database is stored at the path given by the `serbero.db_path`
value in `config.toml` (default `serbero.db` in the working
directory). This path can be overridden with the `SERBERO_DB_PATH`
environment variable.
