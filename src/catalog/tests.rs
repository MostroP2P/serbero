#![allow(clippy::unwrap_used)] // test helpers

use std::path::PathBuf;

use super::*;

fn english() -> Catalog {
    Catalogs::embedded().unwrap().get("en").unwrap().clone()
}

fn ars(value: &str) -> Amount<'_> {
    Amount {
        value,
        currency: "ARS",
    }
}

const MINIMAL: &str = r#"
name = "Test"
[format]
thousands_separator = "."
decimal_separator = ","
[words]
fund_action = []
verdict = []
[templates]
"#;

fn with_templates(templates: &str) -> String {
    format!("{MINIMAL}{templates}")
}

#[test]
fn english_is_embedded() {
    assert!(embedded_codes().contains(&"en"));
    assert_eq!(english().name, "English");
}

#[test]
fn every_embedded_template_renders_with_and_without_an_amount() {
    for catalog in Catalogs::embedded().unwrap().iter() {
        for id in catalog.template_ids() {
            let with = catalog.render(id, Some(ars("50000"))).unwrap();
            let without = catalog.render(id, None).unwrap();
            for text in [&with, &without] {
                assert!(!text.contains('{'), "{}/{id}: {text}", catalog.code);
            }
            assert!(!without.contains("ARS"), "{}/{id}", catalog.code);
        }
    }
}

#[test]
fn amount_is_rendered_with_the_language_separators() {
    let text = english()
        .render("ask_seller_received", Some(ars("50000")))
        .unwrap();

    assert_eq!(
        text,
        "Has the 50,000 ARS payment for this order arrived in your account?"
    );
}

#[test]
fn a_missing_amount_uses_the_noamount_form() {
    let text = english().render("ask_seller_received", None).unwrap();

    assert_eq!(
        text,
        "Has the payment for this order arrived in your account?"
    );
}

#[test]
fn amounts_are_grouped_by_thousands() {
    let en = english();
    let dotted = Catalog::parse("xx", &with_templates("")).unwrap();

    assert_eq!(en.format_amount(ars("5")), "5 ARS");
    assert_eq!(en.format_amount(ars("1000")), "1,000 ARS");
    assert_eq!(en.format_amount(ars("1234567.5")), "1,234,567.5 ARS");
    assert_eq!(dotted.format_amount(ars("50000")), "50.000 ARS");
    assert_eq!(dotted.format_amount(ars("1250.75")), "1.250,75 ARS");
    assert_eq!(en.format_amount(ars("1e5")), "1e5 ARS", "kept as is");
    assert_eq!(en.format_amount(ars("")), " ARS");
}

#[test]
fn the_opening_is_intro_then_the_question() {
    let en = english();

    let text = en.render_opening("ask_buyer_sent", None).unwrap();

    assert_eq!(
        text,
        format!(
            "{}\n\n{}",
            en.template("intro").unwrap(),
            en.template("ask_buyer_sent_noamount").unwrap()
        )
    );
}

#[test]
fn an_unknown_template_is_an_error() {
    assert!(english().render("no_such_template", None).is_err());
}

#[test]
fn malformed_catalogs_are_rejected() {
    let cases = [
        ("name = \"\"\n", "name is empty"),
        ("a = \"\"", "template a is empty"),
        ("a = \"Hi {name}\"", "placeholder other than"),
        ("a = \"{amount}\"", "has no a_noamount"),
        (
            "a = \"{amount}\"\na_noamount = \"{amount}\"",
            "must not use {amount}",
        ),
    ];
    for (body, expected) in cases {
        let text = if body.starts_with("name") {
            MINIMAL.replace("name = \"Test\"\n", body)
        } else {
            with_templates(body)
        };

        let err = Catalog::parse("xx", &text).unwrap_err().to_string();

        assert!(err.contains(expected), "{err} should contain {expected}");
    }
    assert!(Catalog::parse("xx", "name = 1").is_err());
    assert!(Catalogs::from_sources(std::iter::empty()).is_err());
}

#[test]
fn a_new_language_file_needs_no_code_change() {
    let dir = std::env::temp_dir().join(format!("serbero-catalogs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(manifest.join("messages/en.toml"), dir.join("en.toml")).unwrap();
    std::fs::copy(
        manifest.join("tests/fixtures/catalogs/fr.toml"),
        dir.join("fr.toml"),
    )
    .unwrap();
    std::fs::write(dir.join("README.md"), "not a catalog").unwrap();

    let catalogs = Catalogs::load_dir(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    let catalogs = catalogs.unwrap();

    assert_eq!(catalogs.codes().collect::<Vec<_>>(), ["en", "fr"]);
    let fr = catalogs.get("fr").unwrap();
    assert_eq!(fr.name, "French");
    assert_eq!(
        fr.render("ask_seller_received", Some(ars("50000")))
            .unwrap(),
        "Le paiement de 50 000 ARS est-il arrivé sur votre compte ?"
    );
}
