use std::collections::HashMap;
use std::time::Duration;

use super::*;

const SAMPLE: &str = include_str!("../../config.sample.toml");
const PRIVATE_KEY: &str = "4444444444444444444444444444444444444444444444444444444444444444";
const MOSTRO: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

fn base_env() -> impl Fn(&str) -> Option<String> {
    env(&[("SERBERO_PRIVATE_KEY", PRIVATE_KEY)])
}

fn minimal(extra: &str) -> String {
    format!("[mostro]\npubkey = \"{MOSTRO}\"\nrelays = [\"wss://relay.example\"]\n{extra}")
}

fn error_of(text: &str, env: impl Fn(&str) -> Option<String>) -> String {
    Settings::parse(text, env).unwrap_err().to_string()
}

#[test]
fn sample_config_loads() {
    let settings = Settings::parse(SAMPLE, base_env()).unwrap();

    let config = &settings.config;
    assert_eq!(config.solvers.len(), 2);
    assert_eq!(config.solvers[0].permission, Permission::Write);
    assert_eq!(config.notify.renotify_after, Duration::from_secs(900));
    assert_eq!(config.mediation.quiet_period, Duration::from_secs(20));
    assert!(!config.mediation.enabled);
    assert_eq!(settings.secrets.private_key.expose(), PRIVATE_KEY);
}

#[test]
fn sample_config_ships_no_active_thresholds() {
    let settings = Settings::parse(SAMPLE, base_env()).unwrap();

    assert!(settings.config.judge.active_thresholds().is_none());
}

#[test]
fn sample_thresholds_parse_once_uncommented() {
    let uncommented = SAMPLE
        .replace("# [judge.thresholds", "[judge.thresholds")
        .replace("\n# guide", "\nguide");
    let uncommented = [
        "fact",
        "human_request",
        "fraud",
        "conflict",
        "outside_scope",
    ]
    .iter()
    .fold(uncommented, |text, name| {
        text.replace(&format!("\n# {name} ="), &format!("\n{name} ="))
    });

    let settings = Settings::parse(&uncommented, base_env()).unwrap();

    let thresholds = settings.config.judge.active_thresholds().unwrap();
    assert_eq!(thresholds.guide, 0.90);
    assert!(
        thresholds.validated_languages.is_empty(),
        "no language is validated before T3.10"
    );
}

#[test]
fn minimal_config_uses_spec_defaults() {
    let settings = Settings::parse(&minimal(""), base_env()).unwrap();

    let config = &settings.config;
    assert_eq!(config.serbero.db_path, PathBuf::from("serbero.db"));
    assert_eq!(
        config.mediation.languages,
        serbero_catalog_codes(),
        "defaults to every language with a catalog file"
    );
    assert_eq!(config.mediation.default_language, "en");
    assert_eq!(config.judge.judge_key(), "typesafe/jev-1.13.0");
    assert!(config.solvers.is_empty());
}

#[test]
fn missing_private_key_env_fails_with_variable_name() {
    let err = error_of(&minimal(""), env(&[]));

    assert!(err.contains("SERBERO_PRIVATE_KEY"), "{err}");
    assert!(err.contains("not set"), "{err}");
}

#[test]
fn malformed_private_key_is_rejected_without_echoing_it() {
    let err = error_of(
        &minimal(""),
        env(&[("SERBERO_PRIVATE_KEY", "nsec-not-hex")]),
    );

    assert!(
        err.contains("64-character hex secp256k1 private key"),
        "{err}"
    );
    assert!(!err.contains("nsec-not-hex"), "secret leaked: {err}");
}

#[test]
fn enabled_mediation_requires_judge_key() {
    let err = error_of(&minimal("[mediation]\nenabled = true\n"), base_env());

    assert!(err.contains("TYPESAFE_API_KEY"), "{err}");
}

#[test]
fn enabled_mediation_with_recorded_judge_needs_no_key() {
    let text = minimal("[mediation]\nenabled = true\n[judge]\nprovider = \"recorded\"\n");

    assert!(Settings::parse(&text, base_env()).is_ok());
}

#[test]
fn judge_key_is_read_when_present() {
    let settings = Settings::parse(
        &minimal("[mediation]\nenabled = true\n"),
        env(&[
            ("SERBERO_PRIVATE_KEY", PRIVATE_KEY),
            ("TYPESAFE_API_KEY", "ts-key"),
        ]),
    )
    .unwrap();

    assert_eq!(settings.secrets.judge_api_key.unwrap().expose(), "ts-key");
}

/// A reader for `*_FILE` secrets that serves `files` as (path, contents, shared).
fn files(
    files: &'static [(&'static str, &'static str, bool)],
) -> impl Fn(&Path) -> std::io::Result<secret_file::SecretFile> {
    move |path| {
        files
            .iter()
            .find(|(p, _, _)| Path::new(p) == path)
            .map(|(_, contents, shared)| secret_file::SecretFile {
                contents: (*contents).to_owned(),
                shared: *shared,
            })
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
    }
}

#[test]
fn secrets_are_read_from_file_variables() {
    let settings = Settings::parse_with(
        &minimal("[mediation]\nenabled = true\n"),
        env(&[
            ("SERBERO_PRIVATE_KEY_FILE", "/run/secrets/key"),
            ("TYPESAFE_API_KEY_FILE", "/run/secrets/ts"),
        ]),
        files(&[
            (
                "/run/secrets/key",
                "4444444444444444444444444444444444444444444444444444444444444444\n",
                false,
            ),
            ("/run/secrets/ts", "ts-key\n", false),
        ]),
    )
    .unwrap();

    assert_eq!(settings.secrets.private_key.expose(), PRIVATE_KEY);
    assert_eq!(settings.secrets.judge_api_key.unwrap().expose(), "ts-key");
    assert!(settings.warnings.is_empty());
}

#[test]
fn shared_secret_file_is_reported_as_a_warning() {
    let settings = Settings::parse_with(
        &minimal(""),
        env(&[("SERBERO_PRIVATE_KEY_FILE", "/run/secrets/key")]),
        files(&[(
            "/run/secrets/key",
            "4444444444444444444444444444444444444444444444444444444444444444",
            true,
        )]),
    )
    .unwrap();

    assert_eq!(settings.warnings.len(), 1);
    assert!(settings.warnings[0].contains("SERBERO_PRIVATE_KEY_FILE"));
}

#[test]
fn missing_private_key_names_both_variables() {
    let err = error_of(&minimal(""), env(&[]));

    assert!(err.contains("SERBERO_PRIVATE_KEY"), "{err}");
    assert!(err.contains("SERBERO_PRIVATE_KEY_FILE"), "{err}");
}

#[test]
fn invalid_key_in_a_file_does_not_leak() {
    let err = Settings::parse_with(
        &minimal(""),
        env(&[("SERBERO_PRIVATE_KEY_FILE", "/run/secrets/key")]),
        files(&[("/run/secrets/key", "nsec-not-hex", false)]),
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("SERBERO_PRIVATE_KEY"), "{err}");
    assert!(!err.contains("nsec-not-hex"), "secret leaked: {err}");
}

#[test]
fn unknown_language_is_rejected() {
    let err = error_of(
        &minimal("[mediation]\nlanguages = [\"en\", \"fr\"]\n"),
        base_env(),
    );

    assert!(err.contains("\"fr\" has no template catalog"), "{err}");
}

#[test]
fn duplicate_language_is_rejected() {
    let err = error_of(
        &minimal("[mediation]\nlanguages = [\"en\", \"es\", \"en\"]\n"),
        base_env(),
    );

    assert!(err.contains("\"en\" is listed twice"), "{err}");
}

#[test]
fn default_language_must_be_enabled() {
    let text = minimal("[mediation]\nlanguages = [\"en\"]\ndefault_language = \"es\"\n");

    let err = error_of(&text, base_env());

    assert!(err.contains("default_language"), "{err}");
}

#[test]
fn bad_mostro_pubkey_is_rejected() {
    let text = "[mostro]\npubkey = \"npub1abc\"\nrelays = [\"wss://relay.example\"]\n";

    let err = error_of(text, base_env());

    assert!(err.contains("mostro.pubkey"), "{err}");
}

#[test]
fn bad_solver_pubkey_names_its_index() {
    let text = minimal(&format!(
        "[[solvers]]\npubkey = \"{MOSTRO}\"\npermission = \"write\"\n\
         [[solvers]]\npubkey = \"xyz\"\npermission = \"read\"\n"
    ));

    let err = error_of(&text, base_env());

    assert!(err.contains("solvers[1].pubkey"), "{err}");
}

#[test]
fn unknown_permission_is_rejected() {
    let text = minimal(&format!(
        "[[solvers]]\npubkey = \"{MOSTRO}\"\npermission = \"admin\"\n"
    ));

    let err = error_of(&text, base_env());

    assert!(err.contains("admin"), "{err}");
}

#[test]
fn bad_duration_is_rejected() {
    let err = error_of(
        &minimal("[notify]\nrenotify_after = \"15 minutes\"\n"),
        base_env(),
    );

    assert!(err.contains("unknown unit"), "{err}");
}

#[test]
fn zero_duration_is_rejected() {
    let err = error_of(&minimal("[mediation]\nquiet_period = \"0s\"\n"), base_env());

    assert!(
        err.contains("mediation.quiet_period must be greater than 0"),
        "{err}"
    );
}

#[test]
fn relay_without_websocket_scheme_is_rejected() {
    let text = format!("[mostro]\npubkey = \"{MOSTRO}\"\nrelays = [\"https://relay.example\"]\n");

    let err = error_of(&text, base_env());

    assert!(err.contains("wss://"), "{err}");
}

#[test]
fn misspelled_field_is_rejected() {
    let err = error_of(&minimal("[notify]\nrenotify_afer = \"15m\"\n"), base_env());

    assert!(err.contains("renotify_afer"), "{err}");
}

#[test]
fn unknown_provider_is_rejected() {
    let err = error_of(&minimal("[judge]\nprovider = \"acme\"\n"), base_env());

    assert!(err.contains("\"acme\" is unknown"), "{err}");
}

#[test]
fn threshold_out_of_range_is_rejected() {
    let text = minimal(
        "[judge.thresholds.\"typesafe/jev-1.13.0\"]\n\
         guide = 1.5\nfact = 0.8\nhuman_request = 0.8\nfraud = 0.6\nconflict = 0.75\noutside_scope = 0.8\n",
    );

    let err = error_of(&text, base_env());

    assert!(err.contains("guide = 1.5"), "{err}");
}

#[test]
fn choice_thresholds_at_or_below_one_half_are_rejected() {
    let text = minimal(
        "[judge.thresholds.\"typesafe/jev-1.13.0\"]\n\
         guide = 0.9\nfact = 0.5\nhuman_request = 0.8\nfraud = 0.4\nconflict = 0.75\noutside_scope = 0.8\n",
    );

    let err = error_of(&text, base_env());

    assert!(err.contains("fact = 0.5 must be above 0.5"), "{err}");
}

#[test]
fn validated_languages_are_read_from_the_thresholds() {
    let text = minimal(
        "[judge.thresholds.\"typesafe/jev-1.13.0\"]\n\
         guide = 0.9\nfact = 0.8\nhuman_request = 0.8\nfraud = 0.6\nconflict = 0.75\noutside_scope = 0.8\n\
         validated_languages = [\"en\", \"es\"]\n",
    );

    let settings = Settings::parse(&text, base_env()).unwrap();

    assert_eq!(
        settings
            .config
            .judge
            .active_thresholds()
            .unwrap()
            .validated_languages,
        ["en", "es"]
    );
}

#[test]
fn a_validated_language_without_a_catalog_is_rejected() {
    let text = minimal(
        "[judge.thresholds.\"typesafe/jev-1.13.0\"]\n\
         guide = 0.9\nfact = 0.8\nhuman_request = 0.8\nfraud = 0.6\nconflict = 0.75\noutside_scope = 0.8\n\
         validated_languages = [\"en\", \"xx\"]\n",
    );

    let err = error_of(&text, base_env());

    assert!(
        err.contains("validated_languages: \"xx\" has no template catalog"),
        "{err}"
    );
}

#[test]
fn thresholds_for_another_model_are_not_active() {
    let text = minimal(
        "[judge.thresholds.\"typesafe/jev-0.9\"]\n\
         guide = 0.9\nfact = 0.8\nhuman_request = 0.8\nfraud = 0.6\nconflict = 0.75\noutside_scope = 0.8\n",
    );

    let settings = Settings::parse(&text, base_env()).unwrap();

    assert!(settings.config.judge.active_thresholds().is_none());
}

#[test]
fn secrets_debug_output_is_redacted() {
    let settings = Settings::parse(&minimal(""), base_env()).unwrap();

    let debug = format!("{settings:?}");

    assert!(!debug.contains(PRIVATE_KEY), "secret leaked: {debug}");
    assert!(debug.contains("<redacted>"));
}

#[test]
fn empty_private_key_env_name_is_rejected() {
    let text = format!("[serbero]\nprivate_key_env = \"\"\n{}", minimal(""));

    let err = error_of(&text, base_env());

    assert!(
        err.contains("serbero.private_key_env must name an environment variable"),
        "{err}"
    );
}

#[test]
fn malformed_judge_key_env_name_is_rejected() {
    let err = error_of(
        &minimal("[judge]\napi_key_env = \"TYPESAFE KEY\"\n"),
        base_env(),
    );

    assert!(err.contains("judge.api_key_env"), "{err}");
}

#[test]
fn zero_private_key_is_rejected() {
    let zeros = "0".repeat(64);

    let err = error_of(
        &minimal(""),
        env(&[("SERBERO_PRIVATE_KEY", zeros.as_str())]),
    );

    assert!(
        err.contains("valid 64-character hex secp256k1 private key"),
        "{err}"
    );
}

#[test]
fn pubkey_off_the_curve_is_rejected() {
    // 64 hex digits, but not the x-coordinate of any secp256k1 point.
    let off_curve = "0".repeat(64);
    let text = format!("[mostro]\npubkey = \"{off_curve}\"\nrelays = [\"wss://relay.example\"]\n");

    let err = error_of(&text, base_env());

    assert!(err.contains("mostro.pubkey"), "{err}");
}

fn serbero_catalog_codes() -> Vec<String> {
    crate::catalog::embedded_codes()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_config_parses_without_its_secrets() {
    let config = Config::parse(&minimal("")).unwrap();

    assert_eq!(config.judge.provider, "typesafe");
}

#[test]
fn a_config_parsed_without_secrets_is_still_validated() {
    let err = Config::parse(&minimal("[mediation]\nlanguages = [\"xx\"]\n")).unwrap_err();

    assert!(
        err.to_string().contains("\"xx\" has no template catalog"),
        "{err}"
    );
}

const OBSERVER: &str = "e493dbf1c10d80f3581e4904930b1404cc6c13900ee0758474fa94abe8c4cd13";

#[test]
fn observers_are_optional() {
    let settings = Settings::parse(&minimal(""), base_env()).unwrap();

    assert!(settings.config.observers.is_empty());
}

#[test]
fn observers_are_read_from_their_own_list() {
    let text = minimal(&format!("[[observers]]\npubkey = \"{OBSERVER}\"\n"));

    let settings = Settings::parse(&text, base_env()).unwrap();

    let pubkeys: Vec<&str> = settings
        .config
        .observers
        .iter()
        .map(|o| o.pubkey.as_str())
        .collect();
    assert_eq!(pubkeys, [OBSERVER]);
}

#[test]
fn an_observer_with_a_bad_pubkey_is_refused() {
    let text = minimal("[[observers]]\npubkey = \"npub1notahexkey\"\n");

    let err = error_of(&text, base_env());

    assert!(err.contains("observers[0].pubkey"), "{err}");
}

#[test]
fn an_observer_that_is_also_a_solver_is_refused() {
    // A solver already gets the full message; as an observer it would get
    // every header twice.
    let text = minimal(&format!(
        "[[solvers]]\npubkey = \"{OBSERVER}\"\npermission = \"write\"\n\
         [[observers]]\npubkey = \"{OBSERVER}\"\n"
    ));

    let err = error_of(&text, base_env());

    assert!(
        err.contains("observers[0].pubkey is also a solver"),
        "{err}"
    );
}

#[test]
fn an_observer_listed_twice_is_refused() {
    let text = minimal(&format!(
        "[[observers]]\npubkey = \"{OBSERVER}\"\n[[observers]]\npubkey = \"{OBSERVER}\"\n"
    ));

    let err = error_of(&text, base_env());

    assert!(err.contains("observers[1].pubkey is listed twice"), "{err}");
}

#[test]
fn the_mostro_node_cannot_be_an_observer() {
    let text = minimal(&format!("[[observers]]\npubkey = \"{MOSTRO}\"\n"));

    let err = error_of(&text, base_env());

    assert!(
        err.contains("observers[0].pubkey is the Mostro node"),
        "{err}"
    );
}

fn serbero_pubkey() -> String {
    nostr_sdk::prelude::Keys::parse(PRIVATE_KEY)
        .unwrap()
        .public_key()
        .to_hex()
}

#[test]
fn serbero_cannot_be_one_of_its_own_solvers() {
    // Serbero registers on Mostro as a solver, so operators may list it in
    // [[solvers]]; it would then send every notification to itself.
    let text = minimal(&format!(
        "[[solvers]]\npubkey = \"{MOSTRO}\"\npermission = \"write\"\n\
         [[solvers]]\npubkey = \"{}\"\npermission = \"read\"\n",
        serbero_pubkey().to_uppercase()
    ));

    let err = error_of(&text, base_env());

    assert!(
        err.contains("solvers[1].pubkey is Serbero's own key"),
        "{err}"
    );
}

#[test]
fn serbero_cannot_be_its_own_observer() {
    let text = minimal(&format!(
        "[[observers]]\npubkey = \"{}\"\n",
        serbero_pubkey()
    ));

    let err = error_of(&text, base_env());

    assert!(
        err.contains("observers[0].pubkey is Serbero's own key"),
        "{err}"
    );
}
