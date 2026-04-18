//! Controlled audit store for full reasoning rationales (FR-120).
//!
//! Phase 3 treats a model's rationale text as sensitive: it may
//! recite dispute facts verbatim, include party identifiers, or
//! capture model reasoning that should never appear in general
//! application logs. Mirroring `data-model.md`
//! §reasoning_rationales, this module is the single
//! write + read surface for that table. Everything else in the
//! daemon references the content through the `rationale_id` only
//! (SHA-256 hex over the rationale text), so general logs and
//! audit events stay free of the raw bytes.
//!
//! Dedup / idempotency discipline: the primary key is the content
//! hash, and writes go through `INSERT OR IGNORE`. Re-inserting the
//! same rationale yields the same id with no error and no duplicate
//! row — which keeps retries of a reasoning call (bounded in the
//! OpenAI adapter via `followup_retry_count`) from fanning out into
//! multiple audit rows.

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Row view on `reasoning_rationales`.
///
/// Carries only the columns current callers actually read. Extend
/// on demand rather than speculatively — the table row has fewer
/// than ten columns, but the raw `rationale_text` is the one field
/// we want other code to NOT grab casually.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RationaleRow {
    pub rationale_id: String,
    pub rationale_text: String,
    pub provider: String,
    pub model: String,
}

/// Content-address a rationale text. Public so callers that want to
/// log only the id (without touching the DB) can compute it the
/// same way the insert does.
pub fn rationale_id_for(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Persist a reasoning rationale and return its content-addressed id.
///
/// Semantics:
/// - The id is `SHA-256(rationale_text)` as lowercase hex.
/// - Writes use `INSERT OR IGNORE`, so inserting the same text
///   twice (e.g. on a reasoning-call retry that ultimately
///   succeeded but produced the same rationale) is a no-op and
///   still returns the stable id.
/// - The `session_id` is optional: daemon-scoped rationales (e.g.
///   a classification during handoff prep) may not be tied to a
///   single session yet.
///
/// Provenance-drift guard: because the primary key is only the
/// content hash, two legitimately-different reasoning calls could
/// produce the same rationale text under different
/// provider / model / bundle / policy-hash / session-id values.
/// Silently aliasing them would lose per-occurrence audit metadata.
/// On a hash collision we therefore re-read the persisted row and
/// compare all five fields; if any differ, return
/// [`Error::RationaleProvenanceConflict`] rather than returning the
/// id as if nothing happened. A future slice may move per-occurrence
/// metadata into a dedicated `rationale_occurrences` table; until
/// then this loud error keeps the audit store honest.
#[allow(clippy::too_many_arguments)]
pub fn insert_rationale(
    conn: &Connection,
    session_id: Option<&str>,
    provider: &str,
    model: &str,
    prompt_bundle_id: &str,
    policy_hash: &str,
    rationale_text: &str,
    generated_at: i64,
) -> Result<String> {
    let rationale_id = rationale_id_for(rationale_text);
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO reasoning_rationales (
            rationale_id, session_id, provider, model,
            prompt_bundle_id, policy_hash, rationale_text, generated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            rationale_id,
            session_id,
            provider,
            model,
            prompt_bundle_id,
            policy_hash,
            rationale_text,
            generated_at,
        ],
    )?;

    if inserted == 0 {
        // The id already existed. Validate that the persisted
        // provenance matches what this caller asserted; otherwise
        // the audit store would silently shadow divergent calls.
        let (
            existing_session_id,
            existing_provider,
            existing_model,
            existing_bundle,
            existing_hash,
        ): (Option<String>, String, String, String, String) = conn.query_row(
            "SELECT session_id, provider, model, prompt_bundle_id, policy_hash
             FROM reasoning_rationales WHERE rationale_id = ?1",
            params![rationale_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;

        let mut mismatches: Vec<&'static str> = Vec::new();
        if existing_session_id.as_deref() != session_id {
            mismatches.push("session_id");
        }
        if existing_provider != provider {
            mismatches.push("provider");
        }
        if existing_model != model {
            mismatches.push("model");
        }
        if existing_bundle != prompt_bundle_id {
            mismatches.push("prompt_bundle_id");
        }
        if existing_hash != policy_hash {
            mismatches.push("policy_hash");
        }
        if !mismatches.is_empty() {
            return Err(Error::RationaleProvenanceConflict(format!(
                "rationale {rationale_id} already persisted with different {fields}",
                fields = mismatches.join(", ")
            )));
        }
    }

    Ok(rationale_id)
}

/// Look up a rationale by its content-addressed id. Returns `None`
/// if no row matches.
pub fn get_rationale(conn: &Connection, rationale_id: &str) -> Result<Option<RationaleRow>> {
    match conn.query_row(
        "SELECT rationale_id, rationale_text, provider, model
         FROM reasoning_rationales WHERE rationale_id = ?1",
        params![rationale_id],
        |r| {
            Ok(RationaleRow {
                rationale_id: r.get(0)?,
                rationale_text: r.get(1)?,
                provider: r.get(2)?,
                model: r.get(3)?,
            })
        },
    ) {
        Ok(row) => Ok(Some(row)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    fn fresh() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    #[test]
    fn rationale_id_matches_sha256_of_text() {
        // Known vector: sha256("abc") =
        // ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            rationale_id_for("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn insert_round_trips_two_distinct_rationales() {
        let conn = fresh();
        let id_a = insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash-a",
            "first rationale",
            1000,
        )
        .unwrap();
        let id_b = insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash-a",
            "second rationale — different bytes",
            1001,
        )
        .unwrap();
        assert_ne!(id_a, id_b, "different texts must produce different ids");

        let row_a = get_rationale(&conn, &id_a).unwrap().expect("row a");
        assert_eq!(row_a.rationale_text, "first rationale");
        assert_eq!(row_a.provider, "openai");
        assert_eq!(row_a.model, "gpt-5");

        let row_b = get_rationale(&conn, &id_b).unwrap().expect("row b");
        assert_eq!(row_b.rationale_text, "second rationale — different bytes");
    }

    #[test]
    fn inserting_same_text_twice_is_idempotent() {
        let conn = fresh();
        let id1 = insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash",
            "the same rationale text",
            100,
        )
        .unwrap();
        let id2 = insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash",
            "the same rationale text",
            200, // different generated_at
        )
        .unwrap();
        assert_eq!(id1, id2, "content-addressed id must match");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reasoning_rationales WHERE rationale_id = ?1",
                params![id1],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "replay must not produce a second row");
    }

    #[test]
    fn get_rationale_returns_none_for_unknown_id() {
        let conn = fresh();
        assert!(get_rationale(&conn, "deadbeef").unwrap().is_none());
    }

    #[test]
    fn same_text_with_divergent_provenance_is_rejected() {
        let conn = fresh();
        insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash-1",
            "the exact same rationale text",
            100,
        )
        .unwrap();
        // Second call: same text (same content hash), but a
        // different `policy_hash`. This would previously be
        // silently aliased to the first row. Now it MUST error.
        let err = insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash-2-different",
            "the exact same rationale text",
            200,
        )
        .expect_err("divergent provenance on an existing content-hash must error");
        match err {
            crate::error::Error::RationaleProvenanceConflict(msg) => {
                assert!(
                    msg.contains("policy_hash"),
                    "error should list the conflicting fields: {msg}"
                );
            }
            other => panic!("expected RationaleProvenanceConflict, got {other}"),
        }

        // Sanity: still only one row in the table.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM reasoning_rationales", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}
