use gradle_catalog_lsp::repositories::Repository;
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
