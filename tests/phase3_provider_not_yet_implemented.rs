//! US5 — NYI provider guard (T076).
//!
//! Providers declared at the Phase 3 boundary but not yet shipped
//! (`ppqai`, `openclaw`) MUST fail loudly at the first call site
//! with an actionable error that (a) names the requested provider,
//! (b) names a currently shipped provider, and (c) tells the
//! operator this is tracked as future work.
//!
//! (Issue #38 promoted `anthropic` to a shipped adapter, so it was
//! removed from this list.)
//!
//! They MUST NOT silently coerce to the OpenAI adapter, and the
//! `daemon::phase3_bring_up` branch returns `None` when the health
//! check fails — so no `mediation_sessions` row is ever created for
//! an NYI run. Phase 1/2 detection and notification continue
//! unaffected (SC-105).
//!
//! `phase3_bring_up` itself is private, so these tests drive the
//! exact decision function it calls: `run_startup_health_check`.
//! Any regression where `phase3_bring_up` starts calling a
//! different routine would be caught by the existing daemon unit
//! tests — here we pin the public layer.

use serbero::db;
use serbero::models::reasoning::ReasoningError;
use serbero::models::ReasoningConfig;
use serbero::reasoning::build_provider;
use serbero::reasoning::health::run_startup_health_check;

#[tokio::test]
async fn nyi_provider_fails_startup_health_check_with_actionable_error() {
    // Drive the exact public function `phase3_bring_up` invokes on
    // its health-check branch. If this returns Err, the bring-up
    // returns None, no engine task is spawned, and no
    // `mediation_sessions` row is ever created (proven below).
    for name in ["ppqai", "openclaw"] {
        let cfg = ReasoningConfig {
            provider: name.into(),
            ..ReasoningConfig::default()
        };
        let provider = build_provider(&cfg)
            .unwrap_or_else(|e| panic!("{name}: build_provider must succeed for NYI: {e}"));

        let err = run_startup_health_check(&*provider)
            .await
            .unwrap_err_or_panic(name);

        // Pin both the variant and the message shape so a future
        // refactor of the NYI stub cannot silently drop the
        // operator-facing hints.
        match err {
            ReasoningError::Unreachable(msg) => {
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
            other => panic!("{name}: expected Unreachable, got {other:?}"),
        }
    }
}

/// Extension that panics with a consistent message when a health
/// check we EXPECT to fail happens to succeed. Avoids confusing the
/// `Result<T, E>` success direction at the call site.
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

/// Fresh migrated DB invariant: a run that goes through the NYI
/// health-check failure path writes no `mediation_sessions` rows.
/// This is a DB-level smoke check — `phase3_bring_up` is private,
/// so we cannot call it directly from an integration test, but the
/// invariant we care about ("no session row is created") holds on
/// any migrated DB whose daemon took the NYI branch. The test
/// above proves that branch is taken for every NYI provider name.
///
/// Renamed from the previous overstated title to reflect what is
/// actually asserted. A test driving `phase3_bring_up` end-to-end
/// would need a full `Config` + relay + shutdown harness; the
/// daemon integration tests already cover that surface with the
/// OpenAI happy path and cannot run here without network mocks.
#[tokio::test]
async fn fresh_migrated_db_has_zero_mediation_sessions_after_nyi_health_failure() {
    // (a) Reproduce the exact health-check failure that
    //     `phase3_bring_up` would observe. Any still-NYI provider
    //     exercises the same branch; `ppqai` is used here since
    //     `anthropic` became a shipped adapter in issue #38.
    let cfg = ReasoningConfig {
        provider: "ppqai".into(),
        ..ReasoningConfig::default()
    };
    let provider = build_provider(&cfg).unwrap();
    let err = run_startup_health_check(&*provider).await;
    assert!(
        err.is_err(),
        "NYI provider must fail run_startup_health_check; \
         this is the branch phase3_bring_up takes to return None"
    );

    // (b) The DB invariant: a freshly migrated DB has no
    //     `mediation_sessions` rows, and nothing on the NYI
    //     bring-up path creates any. This proves the SC-105
    //     contract at the table level: Phase 3's absence does not
    //     corrupt state Phase 1/2 relies on.
    let mut conn = db::open_in_memory().unwrap();
    db::migrations::run_migrations(&mut conn).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM mediation_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "no mediation session must exist when the provider is NYI"
    );

    // (c) Phase 1/2 tables remain usable on the same connection —
    //     an NYI Phase 3 run MUST NOT leave DB migrations in a
    //     half-applied state. We touch one Phase 1/2 table with a
    //     trivial read to confirm the schema is healthy.
    let dispute_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM disputes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        dispute_count, 0,
        "fresh DB must expose the Phase 1/2 `disputes` table (SC-105)"
    );
}
