use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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

/// Serves `maven-metadata.xml` like a Maven repository, and 404 for anything else.
struct Repository {
    url: String,
    artifacts: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl Repository {
    fn new(artifacts: Artifacts) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let repository = Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            artifacts: Arc::default(),
        };
        for (artifact, versions) in artifacts {
            repository.publish(artifact, versions);
        }
        let served = Arc::clone(&repository.artifacts);
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let request: Vec<String> = BufReader::new(&stream)
                    .lines()
                    .map_while(Result::ok)
                    .take_while(|line| !line.is_empty())
                    .collect();
                let path = request
                    .first()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or_default();
                let response = match served.lock().unwrap().get(path) {
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
    fn spawn(repositories: &[&Repository]) -> Self {
        let urls: Vec<&str> = repositories.iter().map(|r| r.url.as_str()).collect();
        let mut process = Command::new(env!("CARGO_BIN_EXE_gradle-catalog-lsp"))
            .env("GRADLE_CATALOG_LSP_REPOSITORIES", urls.join(","))
            .env("GRADLE_CATALOG_LSP_TTL_MS", TTL.as_millis().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
        let first = Repository::new(FIRST_REPOSITORY);
        let second = Repository::new(SECOND_REPOSITORY);
        Self::start_with(&[&first, &second], json!({}))
    }

    fn start_with(repositories: &[&Repository], capabilities: Value) -> Self {
        let mut server = Self::spawn(repositories);
        server.initialize(capabilities);
        server.open(CATALOG_URI, CATALOG);
        server
    }

    fn initialize(&mut self, capabilities: Value) -> Value {
        let result = self
            .request("initialize", json!({ "capabilities": capabilities }))
            .unwrap();
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
}

impl Drop for Server {
    /// Closing stdin ends the server normally, which is when it writes its coverage data.
    fn drop(&mut self) {
        drop(self.process.stdin.take());
        self.exited();
    }
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
    let mut server = Server::spawn(&[]);
    let result = server.initialize(json!({}));
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
    let first = Repository::new(FIRST_REPOSITORY);
    let mut server = Server::start_with(&[&first, &Repository::new(SECOND_REPOSITORY)], json!({}));
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
    let first = Repository::new(FIRST_REPOSITORY);
    let second = Repository::new(SECOND_REPOSITORY);
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
    let first = Repository::new(FIRST_REPOSITORY);
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
