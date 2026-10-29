use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{self, Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{env, fs, thread};

use lsp_server::{Message, Notification, Request, RequestId, Response, ResponseError};
use serde_json::{Value, json};

/// Versions expire, and the server asks for a refresh, after this delay.
const TTL: Duration = Duration::from_millis(200);

const CATALOG_URI: &str = "file:///project/gradle/libs.versions.toml";

const CATALOG: &str = r#"[versions]
kotlin = "2.0.21"

[libraries]
stdlib = { module = "org.jetbrains.kotlin:kotlin-stdlib", version.ref = "kotlin" }
"🚀" = "com.squareup.okhttp3:okhttp:4.11.0"
core = { group = "androidx.core", name = "core-ktx", version = "1.13.1" }
missing = "com.example:missing:1.0"

[plugins]
kotlin-android = { id = "org.jetbrains.kotlin.android", version.ref = "kotlin" }
"#;

type Artifacts = &'static [(&'static str, &'static [&'static str])];

const FIRST_REPOSITORY: Artifacts = &[
    (
        "org/jetbrains/kotlin/kotlin-stdlib",
        &["2.0.21", "2.1.0", "2.2.0"],
    ),
    ("androidx/core/core-ktx", &["1.12.0", "1.13.1"]),
];

const SECOND_REPOSITORY: Artifacts = &[
    (
        "com/squareup/okhttp3/okhttp",
        &["4.11.0", "4.12.0", "5.0.0-alpha.14"],
    ),
    (
        "org/jetbrains/kotlin/android/org.jetbrains.kotlin.android.gradle.plugin",
        &["2.0.21", "2.1.0"],
    ),
];

const PRIVATE_REPOSITORY: Artifacts = &[("com/example/missing", &["1.0", "1.1"])];

/// `acme:s3cr3t`, as Basic authentication.
const AUTHORIZATION: &str = "Basic YWNtZTpzM2NyM3Q=";

/// Serves `maven-metadata.xml` like a Maven repository, and 404 for anything else.
struct Repository {
    url: String,
    artifacts: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// The request and header lines received, oldest first.
    received: Arc<Mutex<Vec<String>>>,
}

impl Repository {
    /// Answers 401 to requests without the `authorization` header, when there is one.
    fn new(artifacts: Artifacts, authorization: Option<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let repository = Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            artifacts: Arc::default(),
            received: Arc::default(),
        };
        for (artifact, versions) in artifacts {
            repository.publish(artifact, versions);
        }
        let served = Arc::clone(&repository.artifacts);
        let received = Arc::clone(&repository.received);
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let request: Vec<String> = BufReader::new(&stream)
                    .lines()
                    .map_while(Result::ok)
                    .take_while(|line| !line.is_empty())
                    .collect();
                received.lock().unwrap().extend(request.iter().cloned());
                let path = request
                    .first()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or_default();
                let authorized = authorization.is_none_or(|expected| {
                    request.iter().any(|line| {
                        line.split_once(':').is_some_and(|(name, value)| {
                            name.eq_ignore_ascii_case("authorization") && value.trim() == expected
                        })
                    })
                });
                let response = match served.lock().unwrap().get(path) {
                    _ if !authorized => {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                    Some(versions) => {
                        let versions: String = versions
                            .iter()
                            .map(|version| format!("<version>{version}</version>"))
                            .collect();
                        let body = format!(
                            "<metadata><versioning><versions>{versions}</versions></versioning></metadata>"
                        );
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        repository
    }

    fn publish(&self, artifact: &str, versions: &[&str]) {
        self.artifacts.lock().unwrap().insert(
            format!("/{artifact}/maven-metadata.xml"),
            versions.iter().map(|v| v.to_string()).collect(),
        );
    }

    fn empty(&self) {
        self.artifacts.lock().unwrap().clear();
    }

    fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }

    fn authorizations(&self) -> Vec<String> {
        self.received()
            .into_iter()
            .filter(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            .collect()
    }
}

/// The language server binary, driven over stdio like Zed does.
struct Server {
    process: Child,
    output: BufReader<ChildStdout>,
    next_id: i32,
    /// Methods of the requests the server sent, which are answered like Zed does.
    requests: Vec<String>,
}

impl Server {
    fn spawn(repositories: &[&Repository], env: &[(&str, &str)]) -> Self {
        let urls: Vec<&str> = repositories.iter().map(|r| r.url.as_str()).collect();
        let mut process = Command::new(env!("CARGO_BIN_EXE_gradle-catalog-lsp"))
            .env("GRADLE_CATALOG_LSP_REPOSITORIES", urls.join(","))
            .env("GRADLE_CATALOG_LSP_TTL_MS", TTL.as_millis().to_string())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = BufReader::new(process.stdout.take().unwrap());
        Self {
            process,
            output,
            next_id: 0,
            requests: vec![],
        }
    }

    fn start() -> Self {
        let first = Repository::new(FIRST_REPOSITORY, None);
        let second = Repository::new(SECOND_REPOSITORY, None);
        Self::start_with(&[&first, &second], json!({}))
    }

    fn start_with(repositories: &[&Repository], capabilities: Value) -> Self {
        Self::launch(repositories, &[], json!({ "capabilities": capabilities }))
    }

    /// Starts with the `repositories` option of the initialization options set to `configured`.
    fn configured(defaults: &[&Repository], env: &[(&str, &str)], configured: Value) -> Self {
        let options = json!({ "repositories": configured });
        let params = json!({ "capabilities": {}, "initializationOptions": options });
        Self::launch(defaults, env, params)
    }

    fn launch(repositories: &[&Repository], env: &[(&str, &str)], params: Value) -> Self {
        let mut server = Self::spawn(repositories, env);
        server.initialize(params);
        server.open(CATALOG_URI, CATALOG);
        server
    }

    fn initialize(&mut self, params: Value) -> Value {
        let result = self.request("initialize", params).unwrap();
        self.notify("initialized", json!({}));
        result
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, ResponseError> {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.send(Request::new(id.clone(), method.to_string(), params).into());
        loop {
            match Message::read(&mut self.output).unwrap() {
                Some(Message::Response(response)) if response.id == id => {
                    return response.response_result;
                }
                Some(Message::Request(request)) => {
                    self.requests.push(request.method);
                    self.send(Response::new_ok(request.id, ()).into());
                }
                Some(_) => {}
                None => panic!("the server stopped before answering {method}"),
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(Notification::new(method.to_string(), params).into());
    }

    fn send(&mut self, message: Message) {
        message.write(self.process.stdin.as_mut().unwrap()).unwrap();
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "toml", "version": 1, "text": text } }),
        );
    }

    fn hints(&mut self, uri: &str) -> Value {
        self.request(
            "textDocument/inlayHint",
            json!({ "textDocument": { "uri": uri }, "range": range(0, 0, 99, 0) }),
        )
        .unwrap()
    }

    fn actions(&mut self, uri: &str, line: u32) -> Value {
        self.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": range(line, 0, line, 0),
                "context": { "diagnostics": [] },
            }),
        )
        .unwrap()
    }

    fn labels(&mut self) -> Vec<String> {
        let hints = self.hints(CATALOG_URI);
        let hints = hints.as_array().unwrap();
        hints
            .iter()
            .map(|hint| hint["label"].as_str().unwrap().to_string())
            .collect()
    }

    /// Gives up after a few seconds, so that a server that hangs fails the test instead of blocking it.
    fn exited(&mut self) -> Option<ExitStatus> {
        for _ in 0..500 {
            if let Some(status) = self.process.try_wait().unwrap() {
                return Some(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.process.kill();
        let _ = self.process.wait();
        None
    }

    /// Closing stdin ends the server normally, which is when it writes its coverage data.
    fn stop(&mut self) -> String {
        drop(self.process.stdin.take());
        self.exited();
        let mut errors = String::new();
        if let Some(mut stderr) = self.process.stderr.take() {
            let _ = stderr.read_to_string(&mut errors);
        }
        errors
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        eprint!("{}", self.stop());
    }
}

/// A Gradle user home of its own, so that the tests never read yours.
fn gradle_home(test: &str, properties: &str) -> PathBuf {
    let home = env::temp_dir().join(format!("gradle-catalog-lsp-{}-{test}", process::id()));
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("gradle.properties"), properties).unwrap();
    home
}

fn range(start_line: u32, start_character: u32, end_line: u32, end_character: u32) -> Value {
    json!({
        "start": { "line": start_line, "character": start_character },
        "end": { "line": end_line, "character": end_character },
    })
}

fn edit(line: u32, start: u32, end: u32, version: &str) -> Value {
    json!({ "range": range(line, start, line, end), "newText": version })
}

#[test]
fn introduces_itself_and_its_capabilities() {
    let mut server = Server::spawn(&[], &[]);
    let result = server.initialize(json!({ "capabilities": {} }));
    assert_eq!(result["serverInfo"]["name"], "gradle-catalog-lsp");
    assert_eq!(result["capabilities"]["textDocumentSync"], 1);
    assert_eq!(result["capabilities"]["inlayHintProvider"], true);
    assert_eq!(result["capabilities"]["codeActionProvider"], true);
}

#[test]
fn hints_the_newest_version_with_the_edit_that_applies_it() {
    let mut server = Server::start();
    // okhttp only exists in the second repository, and its alpha is skipped for a stable version.
    // The shared `kotlin` ref stops at 2.1.0, the newest version the plugin also has. The emoji key
    // takes two UTF-16 code units, which is how LSP columns are counted.
    assert_eq!(
        server.hints(CATALOG_URI),
        json!([
            {
                "position": { "line": 5, "character": 43 },
                "label": "→ 4.12.0",
                "textEdits": [edit(5, 36, 42, "4.12.0")],
                "tooltip": "Double-click to update to 4.12.0",
                "paddingLeft": true,
            },
            {
                "position": { "line": 1, "character": 17 },
                "label": "→ 2.1.0",
                "textEdits": [edit(1, 10, 16, "2.1.0")],
                "tooltip": "Double-click to update to 2.1.0",
                "paddingLeft": true,
            },
        ])
    );
}

#[test]
fn offers_the_update_as_a_code_action_on_its_line() {
    let mut server = Server::start();
    server.hints(CATALOG_URI);
    assert_eq!(
        server.actions(CATALOG_URI, 5),
        json!([
            {
                "title": "Update to 4.12.0",
                "kind": "quickfix",
                "edit": { "changes": { (CATALOG_URI): [edit(5, 36, 42, "4.12.0")] } },
                "isPreferred": true,
            },
            // The shared `kotlin` ref is a single edit, and `core` (up to date) and `missing` (found
            // nowhere) have none.
            {
                "title": "Update all 2 versions",
                "kind": "source",
                "edit": { "changes": { (CATALOG_URI): [
                    edit(5, 36, 42, "4.12.0"),
                    edit(1, 10, 16, "2.1.0"),
                ] } },
            },
        ])
    );
    assert_eq!(server.actions(CATALOG_URI, 6), json!([]));
}

#[test]
fn offers_no_code_action_before_the_versions_are_fetched() {
    let mut server = Server::start();
    assert_eq!(server.actions(CATALOG_URI, 5), json!([]));
}

#[test]
fn follows_the_catalog_as_it_is_edited_and_closed() {
    let mut server = Server::start();
    let updated = CATALOG.replace("4.11.0", "4.12.0");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": CATALOG_URI, "version": 2 },
            "contentChanges": [{ "text": updated }],
        }),
    );
    let hints = server.hints(CATALOG_URI);
    assert_eq!(hints.as_array().unwrap().len(), 1);
    assert_eq!(hints[0]["label"], "→ 2.1.0");
    let actions = server.actions(CATALOG_URI, 1);
    assert_eq!(actions.as_array().unwrap().len(), 1);
    assert_eq!(actions[0]["title"], "Update to 2.1.0");

    server.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": CATALOG_URI } }),
    );
    assert_eq!(server.hints(CATALOG_URI), json!([]));
    assert_eq!(server.actions(CATALOG_URI, 1), json!([]));
}

#[test]
fn ignores_files_that_are_not_version_catalogs() {
    let mut server = Server::start();
    let uri = "file:///project/gradle/settings.toml";
    server.open(uri, CATALOG);
    assert_eq!(server.hints(uri), json!([]));
}

#[test]
fn rejects_unknown_methods_and_invalid_params() {
    let mut server = Server::start();
    let code = |result: Result<Value, ResponseError>| result.unwrap_err().code;
    assert_eq!(
        code(server.request("textDocument/hover", json!({}))),
        -32601
    );
    assert_eq!(
        code(server.request("textDocument/inlayHint", json!({}))),
        -32602
    );
    assert_eq!(
        code(server.request("textDocument/codeAction", json!({}))),
        -32602
    );
}

#[test]
fn exits_after_shutdown() {
    let mut server = Server::start();
    assert_eq!(
        server.request("shutdown", Value::Null).unwrap(),
        Value::Null
    );
    server.notify("exit", Value::Null);
    assert!(
        server
            .exited()
            .expect("the server is still running")
            .success()
    );
}

#[test]
fn shows_a_release_published_since_the_last_lookup_once_it_expires() {
    let first = Repository::new(FIRST_REPOSITORY, None);
    let mut server = Server::start_with(
        &[&first, &Repository::new(SECOND_REPOSITORY, None)],
        json!({}),
    );
    assert_eq!(server.labels(), ["→ 4.12.0", "→ 2.1.0"]);
    first.publish("androidx/core/core-ktx", &["1.12.0", "1.13.1", "1.15.0"]);
    first.publish("com/example/missing", &["1.0", "1.1"]);
    thread::sleep(TTL);
    assert_eq!(
        server.labels(),
        ["→ 4.12.0", "→ 1.15.0", "→ 1.1", "→ 2.1.0"]
    );
}

#[test]
fn keeps_the_known_versions_when_the_repositories_stop_answering() {
    let first = Repository::new(FIRST_REPOSITORY, None);
    let second = Repository::new(SECOND_REPOSITORY, None);
    let mut server = Server::start_with(&[&first, &second], json!({}));
    assert_eq!(server.labels(), ["→ 4.12.0", "→ 2.1.0"]);
    first.empty();
    second.empty();
    thread::sleep(TTL);
    assert_eq!(server.labels(), ["→ 4.12.0", "→ 2.1.0"]);
}

#[test]
fn asks_zed_to_refresh_the_hints_when_it_supports_it() {
    let refresh_support = json!({ "workspace": { "inlayHint": { "refreshSupport": true } } });
    let first = Repository::new(FIRST_REPOSITORY, None);
    let mut server = Server::start_with(&[&first], refresh_support);
    thread::sleep(TTL * 3);
    server.hints(CATALOG_URI);
    assert!(server.requests.len() >= 2);
    assert!(
        server
            .requests
            .iter()
            .all(|method| method == "workspace/inlayHint/refresh")
    );

    assert_eq!(
        server.request("shutdown", Value::Null).unwrap(),
        Value::Null
    );
    server.notify("exit", Value::Null);
    assert!(
        server
            .exited()
            .expect("the server is still running")
            .success()
    );
}

#[test]
fn never_asks_a_client_without_refresh_support() {
    let mut server = Server::start();
    thread::sleep(TTL * 3);
    server.hints(CATALOG_URI);
    assert!(server.requests.is_empty());
}

#[test]
fn queries_the_configured_repositories_before_the_default_ones() {
    let first = Repository::new(FIRST_REPOSITORY, None);
    let second = Repository::new(SECOND_REPOSITORY, None);
    let private = Repository::new(
        &[
            ("com/example/missing", &["1.0", "1.1"]),
            ("com/squareup/okhttp3/okhttp", &["4.11.0", "4.13.0"]),
        ],
        None,
    );
    let configured = json!([{ "url": private.url }]);
    let mut server = Server::configured(&[&first, &second], &[], configured);
    // The second repository has okhttp too, up to 4.12.0.
    assert_eq!(server.labels(), ["→ 4.13.0", "→ 1.1", "→ 2.1.0"]);
}

#[test]
fn authenticates_with_the_credentials_gradle_would_use() {
    // The environment wins over `gradle.properties`, which also holds a wrong password here.
    let home = gradle_home("environment", "acmePassword=wrong\n");
    let environment = [
        ("GRADLE_USER_HOME", home.to_str().unwrap()),
        ("ORG_GRADLE_PROJECT_acmeUsername", "acme"),
        ("ORG_GRADLE_PROJECT_acmePassword", "s3cr3t"),
    ];
    let properties = "# acmePassword=wrong\nacmeUsername = acme\nacmePassword=s3cr3t\n";
    let other_home = gradle_home("properties", properties);
    let properties = [("GRADLE_USER_HOME", other_home.to_str().unwrap())];
    for env in [&environment[..], &properties[..]] {
        let first = Repository::new(FIRST_REPOSITORY, None);
        let second = Repository::new(SECOND_REPOSITORY, None);
        let private = Repository::new(PRIVATE_REPOSITORY, Some(AUTHORIZATION));
        let configured = json!([{ "url": private.url, "credentials": "acme" }]);
        let mut server = Server::configured(&[&first, &second], env, configured);
        assert_eq!(server.labels(), ["→ 4.12.0", "→ 1.1", "→ 2.1.0"]);
        assert!(first.authorizations().is_empty());
        assert!(second.authorizations().is_empty());
    }
    fs::remove_dir_all(home).unwrap();
    fs::remove_dir_all(other_home).unwrap();
}

#[test]
fn logs_a_repository_that_refuses_its_credentials() {
    let home = gradle_home("refused", "acmeUsername=acme\nacmePassword=wrong\n");
    let first = Repository::new(FIRST_REPOSITORY, None);
    let second = Repository::new(SECOND_REPOSITORY, None);
    let private = Repository::new(PRIVATE_REPOSITORY, Some(AUTHORIZATION));
    let configured = json!([{ "url": private.url, "credentials": "acme" }]);
    let env = [("GRADLE_USER_HOME", home.to_str().unwrap())];
    let mut server = Server::configured(&[&first, &second], &env, configured);
    assert_eq!(server.labels(), ["→ 4.12.0", "→ 2.1.0"]);
    // The first and second repositories answer 404 for the modules they don't have, which is not
    // worth a line.
    let errors = server.stop();
    let refused = format!(
        "{}/com/example/missing/maven-metadata.xml: http status: 401",
        private.url
    );
    assert!(errors.lines().any(|line| line == refused), "{errors}");
    assert!(
        errors
            .lines()
            .all(|line| line.ends_with(": http status: 401")),
        "{errors}"
    );
    assert!(
        !errors.contains("wrong") && !errors.contains("Basic"),
        "{errors}"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn skips_a_repository_whose_credentials_are_missing() {
    let home = gradle_home("missing", "");
    let first = Repository::new(FIRST_REPOSITORY, None);
    let second = Repository::new(SECOND_REPOSITORY, None);
    let private = Repository::new(PRIVATE_REPOSITORY, Some(AUTHORIZATION));
    let configured = json!([{ "url": private.url, "credentials": "acme" }]);
    let env = [("GRADLE_USER_HOME", home.to_str().unwrap())];
    let mut server = Server::configured(&[&first, &second], &env, configured);
    assert_eq!(server.labels(), ["→ 4.12.0", "→ 2.1.0"]);
    assert!(private.received().is_empty());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn ignores_invalid_entries_and_credentials_written_in_urls() {
    let first = Repository::new(FIRST_REPOSITORY, None);
    let second = Repository::new(SECOND_REPOSITORY, None);
    let exposed = Repository::new(&[("com/example/missing", &["1.0", "9.9"])], None);
    let private = Repository::new(PRIVATE_REPOSITORY, None);
    let configured = json!([
        { "url": exposed.url.replace("http://", "http://acme:s3cr3t@") },
        { "url": 42 },
        "https://jitpack.io",
        { "url": private.url },
    ]);
    let mut server = Server::configured(&[&first, &second], &[], configured);
    let hints = server.hints(CATALOG_URI);
    assert_eq!(server.labels(), ["→ 4.12.0", "→ 1.1", "→ 2.1.0"]);
    assert!(exposed.received().is_empty());
    let output = format!("{hints}{}", server.stop());
    assert!(!output.contains("s3cr3t"));
}
