use gradle_catalog_lsp::catalog::Module;
use gradle_catalog_lsp::repositories::{Repository, page};
use serde_json::{Value, json};

#[test]
fn accepts_a_configured_repository_only_when_its_credentials_stay_safe() {
    let accepted = |entry: Value| {
        let property = |name: &str| {
            ["acmeUsername", "acmePassword"]
                .contains(&name)
                .then(String::new)
        };
        Repository::configured(&entry, property).is_some()
    };
    assert!(accepted(json!({ "url": "https://jitpack.io" })));
    assert!(accepted(
        json!({ "url": "https://nexus.acme.com/maven/", "credentials": "acme" })
    ));
    // Plain HTTP is fine without credentials, or when they don't leave the machine.
    assert!(accepted(json!({ "url": "http://nexus.acme.com/maven" })));
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        assert!(accepted(
            json!({ "url": format!("http://{host}:8081"), "credentials": "acme" })
        ));
    }

    assert!(!accepted(
        json!({ "url": "http://nexus.acme.com/maven", "credentials": "acme" })
    ));
    assert!(!accepted(
        json!({ "url": "https://user:secret@nexus.acme.com/maven" })
    ));
    assert!(!accepted(
        json!({ "url": "https://user@nexus.acme.com/maven" })
    ));
    assert!(!accepted(
        json!({ "url": "https://nexus.acme.com/maven", "credentials": "unknown" })
    ));
    assert!(!accepted(
        json!({ "url": "https://nexus.acme.com/maven", "credentials": 42 })
    ));
    assert!(!accepted(
        json!({ "url": "file:///home/me/.m2/repository" })
    ));
    assert!(!accepted(json!({ "url": "nexus.acme.com/maven" })));
    assert!(!accepted(json!({ "url": 42 })));
    assert!(!accepted(json!({ "credentials": "acme" })));
    assert!(!accepted(json!("https://jitpack.io")));
}

#[test]
fn links_to_the_page_of_the_version_on_the_default_repositories_only() {
    let module = |group: &str, name: &str| Module {
        group: group.to_string(),
        name: name.to_string(),
    };
    let okhttp = module("com.squareup.okhttp3", "okhttp");
    let core = module("androidx.core", "core-ktx");
    let plugin = module(
        "org.jetbrains.kotlin.android",
        "org.jetbrains.kotlin.android.gradle.plugin",
    );
    let central = "https://repo1.maven.org/maven2";
    let google = "https://dl.google.com/dl/android/maven2";
    let portal = "https://plugins.gradle.org/m2";
    assert_eq!(
        page(central, &okhttp, "5.5.1").as_deref(),
        Some("https://central.sonatype.com/artifact/com.squareup.okhttp3/okhttp/5.5.1")
    );
    assert_eq!(
        page(google, &core, "1.19.1").as_deref(),
        Some("https://maven.google.com/web/index.html#androidx.core:core-ktx:1.19.1")
    );
    assert_eq!(
        page(portal, &plugin, "2.4.21").as_deref(),
        Some("https://plugins.gradle.org/plugin/org.jetbrains.kotlin.android/2.4.21")
    );
    // The Plugin Portal only lists the files of a library.
    assert_eq!(page(portal, &okhttp, "5.5.1"), None);
    for repository in [
        "https://jitpack.io",
        "https://nexus.acme.com/maven",
        "http://127.0.0.1:8081",
    ] {
        assert_eq!(page(repository, &okhttp, "5.5.1"), None);
        assert_eq!(page(repository, &plugin, "2.4.21"), None);
    }
}
