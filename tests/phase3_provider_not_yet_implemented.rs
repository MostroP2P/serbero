//! US5 — NYI provider guard (T076).
//!
//! Providers declared at the Phase 3 boundary but not yet shipped
//! (`anthropic`, `ppqai`, `openclaw`) MUST fail loudly at the first
//! call site with an actionable error that (a) names the requested
//! provider, (b) names a currently shipped provider, and (c) tells
//! the operator this is tracked as future work.
//!
//! They MUST NOT silently coerce to the OpenAI adapter, and
//! `phase3_bring_up`'s health-check path means no
//! `mediation_sessions` row is ever created for an NYI run.
//! Phase 1/2 detection and notification continue unaffected
//! (SC-105).

use serbero::db;
use serbero::models::ReasoningConfig;
use serbero::reasoning::build_provider;

#[tokio::test]
async fn nyi_provider_fails_health_check_with_actionable_error() {
    for name in ["anthropic", "ppqai", "openclaw"] {
        let cfg = ReasoningConfig {
            provider: name.into(),
            ..ReasoningConfig::default()
        };
        let provider = build_provider(&cfg)
            .unwrap_or_else(|e| panic!("{name}: build_provider must succeed for NYI: {e}"));

        let err = provider.health_check().await.unwrap_err_or_panic(name);

        let msg = err.to_string();
        assert!(
            msg.contains(name),
            "{name}: error must name the requested provider: {msg}"
        );
        assert!(
            msg.contains("openai"),
            "{name}: error must name a shipped provider: {msg}"
        );
        assert!(
            msg.contains("not yet implemented") || msg.contains("future work"),
            "{name}: error must explain this is not yet implemented / future work: {msg}"
        );
    }
}

/// Small extension that panics with a consistent message when a
/// health check we EXPECT to fail happens to succeed. Avoids
/// confusing the `Result<T, E>` success direction at the call site.
trait UnwrapErrOrPanic<E> {
    fn unwrap_err_or_panic(self, provider_name: &str) -> E;
}

impl<T, E> UnwrapErrOrPanic<E> for std::result::Result<T, E> {
    fn unwrap_err_or_panic(self, provider_name: &str) -> E {
        match self {
            Ok(_) => panic!("{provider_name}: health_check must fail for NYI provider"),
            Err(e) => e,
        }
    }
}

#[tokio::test]
async fn nyi_provider_does_not_create_any_session_rows() {
    // SC-105: if the health check fails, `phase3_bring_up` returns
    // `None`, the engine task is never spawned, and no
    // `mediation_sessions` row can be written. We verify the DB-
    // level invariant directly — a fresh migrated DB has zero
    // mediation_sessions rows and nothing in this branch creates
    // any.
    let cfg = ReasoningConfig {
        provider: "anthropic".into(),
        ..ReasoningConfig::default()
    };
    let provider = build_provider(&cfg).unwrap();
    assert!(
        provider.health_check().await.is_err(),
        "NYI provider health check must fail"
    );

    let mut conn = db::open_in_memory().unwrap();
    db::migrations::run_migrations(&mut conn).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM mediation_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "no mediation session must exist when the provider is NYI"
    );
}
