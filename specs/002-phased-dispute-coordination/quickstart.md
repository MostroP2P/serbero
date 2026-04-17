# Quickstart: Serbero

## Prerequisites

- Rust toolchain (stable, edition 2021)
- Access to at least one Nostr relay
- A Nostr key pair for Serbero (hex-encoded private key)
- Nostr public keys of Mostro instance and solvers

## Configuration

Create `config.toml`:

```toml
[serbero]
# Serbero's private key (hex). Can be overridden with
# SERBERO_PRIVATE_KEY environment variable.
private_key = "hex_encoded_nsec"

[mostro]
# Public key of the Mostro instance to monitor
pubkey = "hex_encoded_npub"

[[relays]]
url = "wss://relay.example.com"

[[solvers]]
pubkey = "hex_encoded_solver_npub"
# Permission level: "read" or "write" (used from Phase 2+)
permission = "read"

[[solvers]]
pubkey = "hex_encoded_solver2_npub"
permission = "write"

[timeouts]
# Re-notification timeout in seconds (Phase 2+, default: 300)
renotification_seconds = 300
```

## Build and Run

```bash
# Build
cargo build --release

# Run (config file)
./target/release/serbero --config config.toml

# Run (with env override for private key)
SERBERO_PRIVATE_KEY=hex_key ./target/release/serbero --config config.toml
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

SQLite database is stored at `serbero.db` in the working directory
(configurable via `--db-path` flag or `SERBERO_DB_PATH` env var).
