// lsp_types::Uri holds a Cell (from fluent_uri) but hashes only its string, so it is a safe key.
#![allow(clippy::mutable_key_type)]

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::slice;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use gradle_catalog_lsp::catalog::{self, Entry, Module};
use gradle_catalog_lsp::osv::{self, Advisory, Severity};
use gradle_catalog_lsp::{repositories, versions};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeDescription, CompletionItem, CompletionOptions,
    CompletionParams, CompletionTextEdit, Diagnostic, DiagnosticOptions,
    DiagnosticServerCapabilities, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentDiagnosticParams, Hover,
    HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
    InlayHint, InlayHintLabel, InlayHintParams, InlayHintTooltip, MarkupContent, MarkupKind,
    NumberOrString, OneOf, Position, Range, ServerCapabilities, ServerInfo,
    TextDocumentPositionParams, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};
use serde_json::{Value, from_value, json};

const FOUND_TTL: Duration = Duration::from_secs(60 * 60);
const MISSING_TTL: Duration = Duration::from_secs(5 * 60);
const REFRESH_PERIOD: Duration = Duration::from_secs(5 * 60);

/// The tests shorten the expiries and the refresh period to `GRADLE_CATALOG_LSP_TTL_MS`
/// milliseconds.
fn delay(default: Duration) -> Duration {
    env::var("GRADLE_CATALOG_LSP_TTL_MS")
        .ok()
        .and_then(|millis| millis.parse().ok())
        .map_or(default, Duration::from_millis)
}

struct Fetched {
    versions: Vec<String>,
    repository: Option<&'static str>,
    // Not an `Instant`, which stops while a Mac sleeps: a night away would not count.
    at: SystemTime,
}

impl Fetched {
    fn fresh(&self) -> bool {
        fresh(self.at, !self.versions.is_empty())
    }
}

struct Checked {
    /// `None` when OSV failed.
    advisories: Option<Vec<Advisory>>,
    at: SystemTime,
}

impl Checked {
    fn fresh(&self) -> bool {
        fresh(self.at, self.advisories.is_some())
    }
}

/// A clock set back since `at` makes it stale.
fn fresh(at: SystemTime, found: bool) -> bool {
    let ttl = if found { FOUND_TTL } else { MISSING_TTL };
    at.elapsed().is_ok_and(|age| age < delay(ttl))
}

type Cache = Arc<Mutex<HashMap<Module, Fetched>>>;
type Query = (Module, String);
type Advisories = Arc<Mutex<HashMap<Query, Checked>>>;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    // lsp-types reads the `workspace.diagnostics` capability of the specification as `diagnostic`.
    let diagnostic_refresh = initialize_params
        .pointer("/capabilities/workspace/diagnostics/refreshSupport")
        == Some(&Value::Bool(true));
    let InitializeParams {
        capabilities,
        initialization_options,
        ..
    } = from_value(initialize_params).unwrap_or_default();
    let hint_refresh = capabilities
        .workspace
        .and_then(|workspace| workspace.inlay_hint?.refresh_support)
        .unwrap_or(false);
    repositories::configure(initialization_options.as_ref());
    osv::configure(initialization_options.as_ref());
    let initialize_result = InitializeResult {
        capabilities: ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncKind::FULL.into()),
            inlay_hint_provider: Some(OneOf::Left(true)),
            code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            // `"` so that an empty version gets the list as soon as it is opened.
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some(vec!["\"".to_string()]),
                ..Default::default()
            }),
            diagnostic_provider: osv::enabled()
                .then(|| DiagnosticServerCapabilities::Options(DiagnosticOptions::default())),
            ..Default::default()
        },
        server_info: Some(ServerInfo {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(initialize_id, serde_json::to_value(initialize_result)?)?;

    // Zed only asks for hints and diagnostics on open, edit and scroll, so a catalog left open would
    // never show a release or an advisory. The thread holds a sender, which `io_threads.join()`
    // waits for: dropping `stop` ends it.
    let (stop, stopped) = mpsc::channel::<()>();
    let refreshes: Vec<&str> = [
        (hint_refresh, "workspace/inlayHint/refresh"),
        (
            diagnostic_refresh && osv::enabled(),
            "workspace/diagnostic/refresh",
        ),
    ]
    .into_iter()
    .filter_map(|(supported, method)| supported.then_some(method))
    .collect();
    if !refreshes.is_empty() {
        let sender = connection.sender.clone();
        thread::spawn(move || {
            let mut id = 0;
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(delay(REFRESH_PERIOD)) {
                for method in &refreshes {
                    id += 1;
                    let refresh = Request::new(id.into(), method.to_string(), ());
                    let _ = sender.send(refresh.into());
                }
            }
        });
    }

    let mut documents: HashMap<Uri, String> = HashMap::new();
    let cache = Cache::default();
    let advisories = Advisories::default();
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                handle(&connection, &documents, &cache, &advisories, request);
            }
            Message::Notification(notification) => track(&mut documents, notification),
            Message::Response(_) => {}
        }
    }
    drop(connection);
    drop(stop);
    io_threads.join()?;
    Ok(())
}

fn handle(
    connection: &Connection,
    documents: &HashMap<Uri, String>,
    cache: &Cache,
    advisories: &Advisories,
    request: Request,
) {
    let Request { id, method, params } = request;
    let response = match method.as_str() {
        "textDocument/inlayHint" => {
            let Ok(params) = from_value::<InlayHintParams>(params) else {
                return invalid(connection, id);
            };
            let text = documents
                .get(&params.text_document.uri)
                .cloned()
                .unwrap_or_default();
            let (sender, cache) = (connection.sender.clone(), cache.clone());
            thread::spawn(move || {
                let hints = inlay_hints(&text, &cache);
                let _ = sender.send(Response::new_ok(id, hints).into());
            });
            return;
        }
        "textDocument/diagnostic" => {
            let Ok(params) = from_value::<DocumentDiagnosticParams>(params) else {
                return invalid(connection, id);
            };
            let text = documents
                .get(&params.text_document.uri)
                .cloned()
                .unwrap_or_default();
            let (sender, cache, advisories) =
                (connection.sender.clone(), cache.clone(), advisories.clone());
            thread::spawn(move || {
                let items = diagnostics(&text, &cache, &advisories);
                let report = json!({ "kind": "full", "items": items });
                let _ = sender.send(Response::new_ok(id, report).into());
            });
            return;
        }
        "textDocument/codeAction" => {
            let Ok(params) = from_value::<CodeActionParams>(params) else {
                return invalid(connection, id);
            };
            Response::new_ok(id, code_actions(documents, cache, params))
        }
        "textDocument/hover" => {
            let Ok(params) = from_value::<HoverParams>(params) else {
                return invalid(connection, id);
            };
            let TextDocumentPositionParams {
                text_document,
                position,
            } = params.text_document_position_params;
            let text = documents.get(&text_document.uri).map_or("", String::as_str);
            let Some((range, entry)) = under(text, pinned(text), position) else {
                return send(connection, Response::new_ok(id, ()));
            };
            let (sender, cache) = (connection.sender.clone(), cache.clone());
            thread::spawn(move || {
                resolve(slice::from_ref(&entry), &cache);
                let hover = hover(&entry, range, &cache.lock().unwrap());
                let _ = sender.send(Response::new_ok(id, hover).into());
            });
            return;
        }
        "textDocument/completion" => {
            let Ok(params) = from_value::<CompletionParams>(params) else {
                return invalid(connection, id);
            };
            let TextDocumentPositionParams {
                text_document,
                position,
            } = params.text_document_position;
            let text = documents.get(&text_document.uri).map_or("", String::as_str);
            let Some((range, entry)) = under(text, catalog::parse(text), position) else {
                return send(connection, Response::new_ok(id, json!([])));
            };
            let (sender, cache) = (connection.sender.clone(), cache.clone());
            thread::spawn(move || {
                resolve(slice::from_ref(&entry), &cache);
                let items = completions(&entry, range, position, &cache.lock().unwrap());
                let _ = sender.send(Response::new_ok(id, items).into());
            });
            return;
        }
        _ => Response::new_err(id, ErrorCode::MethodNotFound as i32, method),
    };
    send(connection, response);
}

fn invalid(connection: &Connection, id: lsp_server::RequestId) {
    let response = Response::new_err(id, ErrorCode::InvalidParams as i32, "invalid params".into());
    send(connection, response);
}

fn send(connection: &Connection, response: Response) {
    let _ = connection.sender.send(response.into());
}

fn track(documents: &mut HashMap<Uri, String>, notification: Notification) {
    let Notification { method, params } = notification;
    match method.as_str() {
        "textDocument/didOpen" => {
            if let Ok(DidOpenTextDocumentParams { text_document }) = from_value(params)
                && text_document.uri.as_str().ends_with(".versions.toml")
            {
                documents.insert(text_document.uri, text_document.text);
            }
        }
        "textDocument/didChange" => {
            if let Ok(mut change) = from_value::<DidChangeTextDocumentParams>(params)
                && let Some(text) = change.content_changes.pop()
                && let Some(document) = documents.get_mut(&change.text_document.uri)
            {
                *document = text.text;
            }
        }
        "textDocument/didClose" => {
            if let Ok(DidCloseTextDocumentParams { text_document }) = from_value(params) {
                documents.remove(&text_document.uri);
            }
        }
        _ => {}
    }
}

/// The entries that pin a version, without the empty ones that only completion fills.
fn pinned(text: &str) -> Vec<Entry> {
    let mut entries = catalog::parse(text);
    entries.retain(|entry| !entry.version.is_empty());
    entries
}

fn under(text: &str, entries: Vec<Entry>, position: Position) -> Option<(Range, Entry)> {
    entries
        .into_iter()
        .map(|entry| (range(text, &entry), entry))
        .find(|(range, _)| (range.start..=range.end).contains(&position))
}

fn inlay_hints(text: &str, cache: &Cache) -> Vec<InlayHint> {
    let entries = pinned(text);
    resolve(&entries, cache);
    let cache = cache.lock().unwrap();
    entries
        .iter()
        .filter_map(|entry| {
            let latest = update(entry, &cache)?;
            Some(InlayHint {
                position: position(text, entry.span.end + 1),
                label: InlayHintLabel::String(format!("→ {latest}")),
                kind: None,
                text_edits: Some(vec![TextEdit::new(range(text, entry), latest.to_string())]),
                tooltip: Some(InlayHintTooltip::String(format!(
                    "Double-click to update to {latest}"
                ))),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            })
        })
        .collect()
}

fn hover(entry: &Entry, range: Range, cache: &HashMap<Module, Fetched>) -> Hover {
    let modules: Vec<String> = entry
        .modules
        .iter()
        .map(|module| describe(module, &entry.version, cache.get(module)))
        .collect();
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: modules.join("\n\n"),
        }),
        range: Some(range),
    }
}

fn describe(module: &Module, version: &str, fetched: Option<&Fetched>) -> String {
    let name = match module.plugin() {
        Some(id) => format!("`{id}`"),
        None => format!("`{}:{}`", module.group, module.name),
    };
    let Some(fetched) = fetched.filter(|fetched| !fetched.versions.is_empty()) else {
        return format!("{name}: not found in any repository");
    };
    let page = fetched
        .repository
        .and_then(|repository| repositories::page(repository, module, version));
    let name = match page {
        Some(page) => format!("[{name}]({page})"),
        None => name,
    };
    let update = versions::latest(version, &fetched.versions);
    let status = match update {
        Some(update) => format!("{name}: update to {update}"),
        None => format!("{name}: up to date"),
    };
    let prerelease = versions::prerelease(version, &fetched.versions).filter(|prerelease| {
        update.is_none_or(|update| versions::compare(prerelease, update).is_gt())
    });
    match prerelease {
        Some(prerelease) => format!("{status}, newest pre-release {prerelease}"),
        None => status,
    }
}

fn completions(
    entry: &Entry,
    range: Range,
    cursor: Position,
    cache: &HashMap<Module, Fetched>,
) -> Vec<CompletionItem> {
    let published: Vec<&[String]> = entry
        .modules
        .iter()
        .filter_map(|module| cache.get(module))
        .map(|fetched| fetched.versions.as_slice())
        .filter(|versions| !versions.is_empty())
        .collect();
    let Some((first, others)) = published.split_first() else {
        return vec![];
    };
    let mut common: Vec<&String> = first
        .iter()
        .filter(|version| others.iter().all(|other| other.contains(version)))
        .collect();
    common.sort_by(|a, b| versions::compare(b, a));
    let latest = update(entry, cache);
    // Zed filters the items with the letters and digits right before the cursor, the `0` at the end
    // of `1.13.0`, which would hide or reorder most versions. Starting every filter text with them
    // keeps the whole list in our order until something is typed.
    let before: Vec<u16> = entry
        .version
        .encode_utf16()
        .take((cursor.character - range.start.character) as usize)
        .collect();
    let before = String::from_utf16_lossy(&before);
    let word = before
        .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default();
    common
        .into_iter()
        .enumerate()
        .map(|(index, version)| {
            let mark = if *version == entry.version {
                Some("current")
            } else if Some(version.as_str()) == latest {
                Some("latest")
            } else if versions::is_prerelease(version) {
                Some("pre-release")
            } else {
                None
            };
            CompletionItem {
                label: version.clone(),
                detail: mark.map(str::to_string),
                sort_text: Some(format!("{index:05}")),
                filter_text: Some(format!("{word}{version}")),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                    range,
                    version.clone(),
                ))),
                ..Default::default()
            }
        })
        .collect()
}

fn diagnostics(text: &str, cache: &Cache, advisories: &Advisories) -> Vec<Diagnostic> {
    if !osv::enabled() {
        return vec![];
    }
    let entries = pinned(text);
    resolve(&entries, cache);
    // A module from a repository of the settings stays on the machine, since its coordinates can be
    // confidential. OSV does not track the marker artifacts of plugins.
    let queries: Vec<Vec<Query>> = {
        let cache = cache.lock().unwrap();
        entries
            .iter()
            .map(|entry| {
                entry
                    .modules
                    .iter()
                    .filter(|module| module.plugin().is_none())
                    .filter(|module| {
                        let repository = cache.get(*module).and_then(|fetched| fetched.repository);
                        repository.is_some_and(repositories::is_default)
                    })
                    .map(|module| (module.clone(), entry.version.clone()))
                    .collect()
            })
            .collect()
    };
    check(queries.iter().flatten(), advisories);
    let advisories = advisories.lock().unwrap();
    entries
        .iter()
        .zip(&queries)
        .filter_map(|(entry, queries)| {
            let found = queries
                .iter()
                .filter_map(|query| advisories.get(query)?.advisories.as_deref())
                .flatten();
            diagnostic(text, entry, found)
        })
        .collect()
}

/// One diagnostic per entry, for all the advisories of its modules, the most severe first.
fn diagnostic<'a>(
    text: &str,
    entry: &Entry,
    advisories: impl Iterator<Item = &'a Advisory>,
) -> Option<Diagnostic> {
    // An advisory can cover several modules of a project that share a `version.ref`.
    let mut seen = HashSet::new();
    let mut advisories: Vec<&Advisory> = advisories
        .filter(|advisory| seen.insert(&advisory.id))
        .collect();
    advisories.sort_by_key(|advisory| Reverse(advisory.severity));
    let most_severe = advisories.first()?;
    let serious: Vec<String> = [(Severity::Critical, "critical"), (Severity::High, "high")]
        .into_iter()
        .filter_map(|(severity, label)| {
            let count = advisories
                .iter()
                .filter(|advisory| advisory.severity == severity)
                .count();
            (count > 0).then(|| format!("{count} {label}"))
        })
        .collect();
    let serious = match serious.is_empty() {
        true => String::new(),
        false => format!(" ({})", serious.join(", ")),
    };
    let ids: Vec<&str> = advisories
        .iter()
        .map(|advisory| advisory.id.as_str())
        .collect();
    let vulnerabilities = match ids.len() {
        1 => "1 known vulnerability".to_string(),
        count => format!("{count} known vulnerabilities"),
    };
    let severity = match most_severe.severity >= Severity::High {
        true => DiagnosticSeverity::WARNING,
        false => DiagnosticSeverity::INFORMATION,
    };
    let page = format!("https://osv.dev/vulnerability/{}", most_severe.id);
    Some(Diagnostic {
        range: range(text, entry),
        severity: Some(severity),
        code: Some(NumberOrString::String(most_severe.id.clone())),
        code_description: page.parse().ok().map(|href| CodeDescription { href }),
        source: Some("OSV".to_string()),
        message: format!("{vulnerabilities}{serious}: {}", ids.join(", ")),
        ..Default::default()
    })
}

fn code_actions(
    documents: &HashMap<Uri, String>,
    cache: &Cache,
    params: CodeActionParams,
) -> Vec<CodeActionOrCommand> {
    let uri = params.text_document.uri;
    let Some(text) = documents.get(&uri) else {
        return vec![];
    };
    let lines = params.range.start.line..=params.range.end.line;
    let cache = cache.lock().unwrap();
    let edits: Vec<TextEdit> = pinned(text)
        .iter()
        .filter_map(|entry| {
            let latest = update(entry, &cache)?;
            Some(TextEdit::new(range(text, entry), latest.to_string()))
        })
        .collect();
    let mut actions: Vec<CodeActionOrCommand> = edits
        .iter()
        .filter(|edit| lines.contains(&edit.range.start.line))
        .map(|edit| {
            CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Update to {}", edit.new_text),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(changes(&uri, vec![edit.clone()])),
                is_preferred: Some(true),
                ..Default::default()
            })
        })
        .collect();
    // `source` rather than `source.fixAll`, which Zed would run on save for users who enabled it.
    if !actions.is_empty() && edits.len() >= 2 {
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Update all {} versions", edits.len()),
            kind: Some(CodeActionKind::SOURCE),
            edit: Some(changes(&uri, edits)),
            ..Default::default()
        }));
    }
    actions
}

fn changes(uri: &Uri, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: Some(HashMap::from([(uri.clone(), edits)])),
        ..Default::default()
    }
}

fn resolve(entries: &[Entry], cache: &Cache) {
    let stale: HashSet<&Module> = {
        let cache = cache.lock().unwrap();
        entries
            .iter()
            .flat_map(|entry| &entry.modules)
            .filter(|module| !cache.get(*module).is_some_and(Fetched::fresh))
            .collect()
    };
    thread::scope(|scope| {
        for module in stale {
            scope.spawn(move || {
                let (repository, versions) = repositories::fetch(module).unzip();
                let fetched = Fetched {
                    versions: versions.unwrap_or_default(),
                    repository,
                    at: SystemTime::now(),
                };
                let mut cache = cache.lock().unwrap();
                match cache.get_mut(module) {
                    // Offline, the hints stay rather than disappear.
                    Some(known) if fetched.versions.is_empty() => known.at = fetched.at,
                    _ => {
                        cache.insert(module.clone(), fetched);
                    }
                }
            });
        }
    });
}

fn check<'a>(queries: impl Iterator<Item = &'a Query>, advisories: &Advisories) {
    let stale: HashSet<&Query> = {
        let advisories = advisories.lock().unwrap();
        queries
            .filter(|query| !advisories.get(*query).is_some_and(Checked::fresh))
            .collect()
    };
    thread::scope(|scope| {
        for query in stale {
            scope.spawn(move || {
                let (module, version) = query;
                let checked = Checked {
                    advisories: osv::query(module, version),
                    at: SystemTime::now(),
                };
                let mut advisories = advisories.lock().unwrap();
                match advisories.get_mut(query) {
                    // Offline, the warnings stay rather than disappear.
                    Some(known) if checked.advisories.is_none() => known.at = checked.at,
                    _ => {
                        advisories.insert(query.clone(), checked);
                    }
                }
            });
        }
    });
}

/// A shared `version.ref` only moves when every module it pins has a newer
/// version, and then only as far as the slowest one.
fn update<'a>(entry: &Entry, cache: &'a HashMap<Module, Fetched>) -> Option<&'a str> {
    let latests: Option<Vec<&str>> = entry
        .modules
        .iter()
        .filter_map(|module| {
            cache
                .get(module)
                .filter(|fetched| !fetched.versions.is_empty())
        })
        .map(|fetched| versions::latest(&entry.version, &fetched.versions))
        .collect();
    latests?.into_iter().min_by(|a, b| versions::compare(a, b))
}

fn range(text: &str, entry: &Entry) -> Range {
    Range::new(
        position(text, entry.span.start),
        position(text, entry.span.end),
    )
}

fn position(text: &str, offset: usize) -> Position {
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Position::new(
        before.matches('\n').count() as u32,
        before[line_start..].encode_utf16().count() as u32,
    )
}
