// lsp_types::Uri holds a Cell (from fluent_uri) but hashes only its string, so it is a safe key.
#![allow(clippy::mutable_key_type)]

use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::slice;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use gradle_catalog_lsp::catalog::{self, Entry, Module};
use gradle_catalog_lsp::{repositories, versions};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InlayHint, InlayHintLabel, InlayHintParams,
    InlayHintTooltip, MarkupContent, MarkupKind, OneOf, Position, Range, ServerCapabilities,
    ServerInfo, TextDocumentPositionParams, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};
use serde_json::from_value;

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
    /// A clock set back since the fetch makes it stale.
    fn fresh(&self) -> bool {
        let ttl = if self.versions.is_empty() {
            MISSING_TTL
        } else {
            FOUND_TTL
        };
        self.at.elapsed().is_ok_and(|age| age < delay(ttl))
    }
}

type Cache = Arc<Mutex<HashMap<Module, Fetched>>>;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let InitializeParams {
        capabilities,
        initialization_options,
        ..
    } = from_value(initialize_params).unwrap_or_default();
    let refresh_support = capabilities
        .workspace
        .and_then(|workspace| workspace.inlay_hint?.refresh_support)
        .unwrap_or(false);
    repositories::configure(initialization_options.as_ref());
    let initialize_result = InitializeResult {
        capabilities: ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncKind::FULL.into()),
            inlay_hint_provider: Some(OneOf::Left(true)),
            code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            ..Default::default()
        },
        server_info: Some(ServerInfo {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(initialize_id, serde_json::to_value(initialize_result)?)?;

    // Zed only asks for hints on open, edit and scroll, so a catalog left open would never show a
    // release. The thread holds a sender, which `io_threads.join()` waits for: dropping `stop` ends it.
    let (stop, stopped) = mpsc::channel::<()>();
    if refresh_support {
        let sender = connection.sender.clone();
        thread::spawn(move || {
            let mut id = 0;
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(delay(REFRESH_PERIOD)) {
                id += 1;
                let refresh = Request::new(id.into(), "workspace/inlayHint/refresh".into(), ());
                let _ = sender.send(refresh.into());
            }
        });
    }

    let mut documents: HashMap<Uri, String> = HashMap::new();
    let cache = Cache::default();
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                handle(&connection, &documents, &cache, request);
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
            let hovered = catalog::parse(text)
                .into_iter()
                .map(|entry| (range(text, &entry), entry))
                .find(|(range, _)| (range.start..=range.end).contains(&position));
            let Some((range, entry)) = hovered else {
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

fn inlay_hints(text: &str, cache: &Cache) -> Vec<InlayHint> {
    let entries = catalog::parse(text);
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
    let edits: Vec<TextEdit> = catalog::parse(text)
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
