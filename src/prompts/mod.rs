//! Prompt bundle loading and pinning.
//!
//! Implements the `[prompts]` config surface defined in
//! `contracts/prompt-bundle.md`. On startup (and on operator-triggered
//! config reload) the loader reads the five configured files into a
//! single `PromptBundle`, computes a deterministic `policy_hash` over
//! their bytes, and returns the bundle. Missing or unreadable files
//! MUST fail loudly — the caller leaves Phase 3 disabled for the run.

pub mod hash;
pub mod self_resolution_parser;

use std::path::Path;

use crate::error::{Error, Result};
use crate::mediation::self_resolution::SelfResolutionTemplates;
use crate::models::PromptsConfig;

/// A loaded, hashed Phase 3 prompt bundle.
#[derive(Debug, Clone)]
pub struct PromptBundle {
    /// Human-readable bundle id (default: `phase3-default`). Used in
    /// audit rows alongside `policy_hash`.
    pub id: String,
    /// Deterministic SHA-256 hash of the bundle bytes (hex, lowercase).
    pub policy_hash: String,
    pub system: String,
    pub classification: String,
    pub escalation: String,
    pub mediation_style: String,
    pub message_templates: String,
    /// Feature 005 — cooperative self-resolution language entries.
    /// Loaded from `prompts/phase3-self-resolution.md`. The bundle's
    /// `policy_hash` extends over the file's bytes so a forensic
    /// replay can reproduce the exact rendered string per session.
    pub self_resolution: SelfResolutionTemplates,
}

/// Load every file referenced by `[prompts]`, compute the bundle
/// hash, and return a `PromptBundle`. Returns `Error::PromptBundleLoad`
/// on any missing or unreadable path.
pub fn load_bundle(config: &PromptsConfig) -> Result<PromptBundle> {
    let system = read_file(&config.system_instructions_path, "system_instructions_path")?;
    let classification = read_file(
        &config.classification_policy_path,
        "classification_policy_path",
    )?;
    let escalation = read_file(&config.escalation_policy_path, "escalation_policy_path")?;
    let mediation_style = read_file(&config.mediation_style_path, "mediation_style_path")?;
    let message_templates = read_file(&config.message_templates_path, "message_templates_path")?;

    // Feature 005: the cooperative self-resolution bundle file lives
    // beside the existing prompt files. Path is derived from the
    // configured `system_instructions_path` (replacing
    // `phase3-system.md` with `phase3-self-resolution.md`) so
    // operators don't need to add a new key for an existing
    // deployment to pick the file up.
    //
    // Backwards-compatibility: a daemon upgrading from before this
    // feature shipped will not yet have the file on disk. Rather
    // than refuse to start, the loader logs a one-line warning and
    // falls back to empty templates — the cooperative-self-resolution
    // policy branch becomes a no-op (the `render_for` helper returns
    // a placeholder that includes a clear operator message), and the
    // legacy cooperative-summary path runs unchanged. The hash
    // includes the (possibly empty) self-resolution bytes so the
    // SC-103 invariant still holds.
    let self_resolution_path = derive_self_resolution_path(&config.system_instructions_path);
    let (self_resolution, self_resolution_raw) = match std::fs::read_to_string(Path::new(
        &self_resolution_path,
    )) {
        Ok(raw) => match self_resolution_parser::parse(&raw) {
            Ok(parsed) => (parsed, raw),
            Err(e) => {
                return Err(Error::PromptBundleLoad(format!(
                    "failed to parse self-resolution templates at {self_resolution_path}: {e}"
                )));
            }
        },
        Err(_) => {
            tracing::warn!(
                path = %self_resolution_path,
                "phase3-self-resolution.md not found; cooperative-self-resolution branch will be inert until the file is added"
            );
            (SelfResolutionTemplates::default(), String::new())
        }
    };

    let policy_hash = hash::policy_hash_v2(
        &system,
        &classification,
        &escalation,
        &mediation_style,
        &message_templates,
        &self_resolution_raw,
    );

    Ok(PromptBundle {
        id: "phase3-default".to_string(),
        policy_hash,
        system,
        classification,
        escalation,
        mediation_style,
        message_templates,
        self_resolution,
    })
}

/// Replace the trailing `phase3-system.md` filename in
/// `system_instructions_path` with `phase3-self-resolution.md`. If
/// the configured path doesn't end in the canonical filename (an
/// operator who renamed the bundle), fall back to a sibling file in
/// the same directory.
fn derive_self_resolution_path(system_path: &str) -> String {
    let p = Path::new(system_path);
    let parent = p.parent().unwrap_or_else(|| Path::new("."));
    parent
        .join("phase3-self-resolution.md")
        .to_string_lossy()
        .into_owned()
}

fn read_file(path: &str, field: &str) -> Result<String> {
    std::fs::read_to_string(Path::new(path)).map_err(|e| {
        Error::PromptBundleLoad(format!(
            "failed to read prompt bundle file for `{field}` at {path}: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn load_bundle_errors_on_missing_path() {
        let cfg = PromptsConfig {
            system_instructions_path: "/no/such/system.md".into(),
            classification_policy_path: "/no/such/classification.md".into(),
            escalation_policy_path: "/no/such/escalation.md".into(),
            mediation_style_path: "/no/such/style.md".into(),
            message_templates_path: "/no/such/templates.md".into(),
        };
        let err = load_bundle(&cfg).unwrap_err();
        assert!(matches!(err, Error::PromptBundleLoad(_)));
    }

    #[test]
    fn load_bundle_happy_path() {
        let a = write_tmp("system body\n");
        let b = write_tmp("classification body\n");
        let c = write_tmp("escalation body\n");
        let d = write_tmp("style body\n");
        let e = write_tmp("templates body\n");
        let cfg = PromptsConfig {
            system_instructions_path: a.path().to_string_lossy().into_owned(),
            classification_policy_path: b.path().to_string_lossy().into_owned(),
            escalation_policy_path: c.path().to_string_lossy().into_owned(),
            mediation_style_path: d.path().to_string_lossy().into_owned(),
            message_templates_path: e.path().to_string_lossy().into_owned(),
        };
        let bundle = load_bundle(&cfg).unwrap();
        assert_eq!(bundle.id, "phase3-default");
        assert_eq!(bundle.system, "system body\n");
        assert_eq!(bundle.policy_hash.len(), 64);
        assert!(bundle.policy_hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
