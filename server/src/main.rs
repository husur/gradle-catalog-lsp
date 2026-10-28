// lsp_types::Uri holds a Cell (from fluent_uri) but hashes only its string, so it is a safe key.
#![allow(clippy::mutable_key_type)]

mod catalog;
mod versions;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread;

use catalog::{Entry, Module};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, InlayHint,
    InlayHintLabel, InlayHintParams, OneOf, Position, Range, ServerCapabilities, TextDocumentSyncKind,
    TextEdit, Uri, WorkspaceEdit,
};
use serde_json::from_value;

// ponytail: versions stay cached until the server restarts, add a TTL if long sessions miss releases
type Cache = Arc<Mutex<HashMap<Module, Vec<String>>>>;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncKind::FULL.into()),
        inlay_hint_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        ..Default::default()
    };
    connection.initialize(serde_json::to_value(capabilities)?)?;

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
    io_threads.join()?;
    Ok(())
}

fn handle(connection: &Connection, documents: &HashMap<Uri, String>, cache: &Cache, request: Request) {
    let Request { id, method, params } = request;
    let response = match method.as_str() {
        "textDocument/inlayHint" => {
            let Ok(params) = from_value::<InlayHintParams>(params) else {
                return invalid(connection, id);
            };
            let text = documents.get(&params.text_document.uri).cloned().unwrap_or_default();
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
        _ => Response::new_err(id, ErrorCode::MethodNotFound as i32, method),
    };
    let _ = connection.sender.send(response.into());
}

fn invalid(connection: &Connection, id: lsp_server::RequestId) {
    let response = Response::new_err(id, ErrorCode::InvalidParams as i32, "invalid params".into());
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
                tooltip: None,
                padding_left: Some(true),
                padding_right: None,
                data: None,
            })
        })
        .collect()
}

fn code_actions(documents: &HashMap<Uri, String>, cache: &Cache, params: CodeActionParams) -> Vec<CodeActionOrCommand> {
    let uri = params.text_document.uri;
    let Some(text) = documents.get(&uri) else {
        return vec![];
    };
    let lines = params.range.start.line..=params.range.end.line;
    let cache = cache.lock().unwrap();
    catalog::parse(text)
        .iter()
        .filter(|entry| lines.contains(&position(text, entry.span.start).line))
        .filter_map(|entry| {
            let latest = update(entry, &cache)?;
            let edit = TextEdit::new(range(text, entry), latest.to_string());
            Some(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Update to {latest}"),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                    ..Default::default()
                }),
                is_preferred: Some(true),
                ..Default::default()
            }))
        })
        .collect()
}

fn resolve(entries: &[Entry], cache: &Cache) {
    let missing: HashSet<&Module> = {
        let cache = cache.lock().unwrap();
        entries
            .iter()
            .flat_map(|entry| &entry.modules)
            .filter(|module| !cache.contains_key(*module))
            .collect()
    };
    thread::scope(|scope| {
        for module in missing {
            scope.spawn(move || {
                let available = versions::fetch(module);
                cache.lock().unwrap().insert(module.clone(), available);
            });
        }
    });
}

/// A shared `version.ref` only moves when every module it pins has a newer
/// version, and then only as far as the slowest one.
fn update<'a>(entry: &Entry, cache: &'a HashMap<Module, Vec<String>>) -> Option<&'a str> {
    let latests: Option<Vec<&str>> = entry
        .modules
        .iter()
        .filter_map(|module| cache.get(module).filter(|available| !available.is_empty()))
        .map(|available| versions::latest(&entry.version, available))
        .collect();
    latests?.into_iter().min_by(|a, b| versions::compare(a, b))
}

fn range(text: &str, entry: &Entry) -> Range {
    Range::new(position(text, entry.span.start), position(text, entry.span.end))
}

fn position(text: &str, offset: usize) -> Position {
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Position::new(
        before.matches('\n').count() as u32,
        before[line_start..].encode_utf16().count() as u32,
    )
}
