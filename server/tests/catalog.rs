use gradle_catalog_lsp::catalog::parse;

#[test]
fn finds_every_version_and_its_modules() {
    let text = r#"
[versions]
kotlin = "2.0.21"
agp = "8.7.2"
unused = "1.0"
range = "[1.0,2.0)"

[libraries]
stdlib = { module = "org.jetbrains.kotlin:kotlin-stdlib", version.ref = "kotlin" }
core = { group = "androidx.core", name = "core-ktx", version = "1.13.1" }
okhttp = "com.squareup.okhttp3:okhttp:4.12.0"
bom-managed = { group = "androidx.compose.material3", name = "material3" }
nested = { module = "a:b", version = { ref = "range" } }

[plugins]
android-application = { id = "com.android.application", version.ref = "agp" }
kotlin-android = { id = "org.jetbrains.kotlin.android", version.ref = "kotlin" }
ksp = "com.google.devtools.ksp:2.0.21-1.0.27"
"#;
    let found: Vec<(&str, Vec<String>)> = parse(text)
        .into_iter()
        .map(|entry| {
            assert_eq!(text[entry.span.clone()], entry.version);
            let modules = entry
                .modules
                .iter()
                .map(|m| format!("{}:{}", m.group, m.name));
            (&text[entry.span], modules.collect())
        })
        .collect();
    let plugin = |id: &str| format!("{id}:{id}.gradle.plugin");
    assert_eq!(
        found,
        vec![
            ("1.13.1", vec!["androidx.core:core-ktx".to_string()]),
            ("4.12.0", vec!["com.squareup.okhttp3:okhttp".to_string()]),
            ("2.0.21-1.0.27", vec![plugin("com.google.devtools.ksp")]),
            (
                "2.0.21",
                vec![
                    "org.jetbrains.kotlin:kotlin-stdlib".to_string(),
                    plugin("org.jetbrains.kotlin.android"),
                ]
            ),
            ("8.7.2", vec![plugin("com.android.application")]),
        ]
    );
}

fn versions(text: &str) -> Vec<String> {
    parse(text).into_iter().map(|entry| entry.version).collect()
}

#[test]
fn reads_partial_catalogs_and_skips_invalid_ones() {
    assert!(parse("[libraries\nbroken").is_empty());
    assert_eq!(
        versions("[libraries]\nokhttp = \"com.squareup.okhttp3:okhttp:4.12.0\"\n"),
        ["4.12.0"]
    );
    assert_eq!(
        versions("[plugins]\nksp = \"com.google.devtools.ksp:2.0.21-1.0.27\"\n"),
        ["2.0.21-1.0.27"]
    );
}

#[test]
fn skips_versions_whose_text_differs_from_their_value() {
    let text = r#"[libraries]
escaped = { module = "a:b", version = "1.0\u002E1" }
multiline = { module = "c:d", version = """2.0""" }
"#;
    assert!(parse(text).is_empty());
}
