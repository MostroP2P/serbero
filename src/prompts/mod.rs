//! Prompt bundle loading and pinning.
//!
//! Implements the `[prompts]` config surface defined in
//! `contracts/prompt-bundle.md`. On startup (and on operator-triggered
//! config reload) the loader reads the five configured files into a
//! single `PromptBundle`, computes a deterministic `policy_hash` over
//! their bytes, and returns the bundle. Missing or unreadable files
//! MUST fail loudly — the caller leaves Phase 3 disabled for the run.

pub mod hash;

use std::path::Path;

use crate::error::{Error, Result};
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

    let policy_hash = hash::policy_hash(
        &system,
        &classification,
        &escalation,
        &mediation_style,
        &message_templates,
    );

    Ok(PromptBundle {
        id: "phase3-default".to_string(),
        policy_hash,
        system,
        classification,
        escalation,
        mediation_style,
        message_templates,
    })
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
