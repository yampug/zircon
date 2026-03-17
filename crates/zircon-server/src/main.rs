mod definition;
mod index;
mod resolver;
mod uri;
mod workspace;

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use log::info;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CompletionOptions, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, GotoDefinitionParams, InitializeParams, OneOf, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, WorkDoneProgressOptions,
};
use tree_sitter::Parser;

use crate::index::DocumentIndex;
use crate::workspace::Workspace;

/// Holds all server state across requests.
struct ServerState {
    index: DocumentIndex,
    /// In-memory contents for open documents (path → source).
    open_docs: HashMap<PathBuf, String>,
    /// Tree-sitter parser for ad-hoc parsing (e.g., cursor node lookup).
    parser: Parser,
    /// Workspace root path.
    _root: Option<PathBuf>,
}

impl ServerState {
    fn new(root: Option<PathBuf>) -> Self {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&lang)
            .expect("failed to load Crystal grammar");

        let mut state = ServerState {
            index: DocumentIndex::new(),
            open_docs: HashMap::new(),
            parser,
            _root: root.clone(),
        };

        // Scan workspace and index all Crystal files.
        if let Some(ref root) = root {
            let ws = Workspace::scan(root);
            let paths: Vec<PathBuf> = ws.files.keys().cloned().collect();
            state.index.index_files(&paths);
            info!("indexed {} crystal files", paths.len());
        }

        state
    }

    /// Get source for a file: open doc first, then disk.
    fn get_source(&self, path: &Path) -> Option<String> {
        if let Some(src) = self.open_docs.get(path) {
            return Some(src.clone());
        }
        fs::read_to_string(path).ok()
    }
}

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    env_logger::init();
    info!("zircon-server starting");

    let (connection, io_threads) = Connection::stdio();

    let server_capabilities = serde_json::to_value(capabilities())?;
    let init_params = connection.initialize(server_capabilities)?;
    let init_params: InitializeParams = serde_json::from_value(init_params)?;

    #[allow(deprecated)]
    let root_path = init_params
        .root_uri
        .as_ref()
        .and_then(|u| uri::to_path(u));
    info!("initialized with root: {:?}", root_path);

    let mut state = ServerState::new(root_path);

    main_loop(&connection, &mut state)?;

    io_threads.join()?;
    info!("zircon-server shut down cleanly");
    Ok(())
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::FULL,
        )),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into()]),
            resolve_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions {
                work_done_progress: Some(false),
            },
            ..Default::default()
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    }
}

fn main_loop(
    connection: &Connection,
    state: &mut ServerState,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                handle_request(connection, state, req)?;
            }
            Message::Notification(notif) => {
                handle_notification(state, notif)?;
            }
            Message::Response(_resp) => {}
        }
    }
    Ok(())
}

fn handle_request(
    connection: &Connection,
    state: &mut ServerState,
    req: Request,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let id = req.id.clone();
    let method = req.method.as_str();

    match method {
        "textDocument/definition" => {
            let params: GotoDefinitionParams = serde_json::from_value(req.params)?;
            // Extract source before calling handler to avoid borrow conflict.
            let source = params
                .text_document_position_params
                .text_document
                .uri
                .as_str()
                .strip_prefix("file://")
                .and_then(|p| state.get_source(Path::new(p)));
            let result =
                definition::handle(&state.index, &mut state.parser, params, source.as_deref());
            let resp = Response::new_ok(id, result);
            connection.sender.send(Message::Response(resp))?;
        }
        "textDocument/hover" => {
            send_null_response(connection, id)?;
        }
        "textDocument/completion" => {
            send_empty_response(connection, id)?;
        }
        "textDocument/documentSymbol" => {
            send_empty_response(connection, id)?;
        }
        "workspace/symbol" => {
            send_empty_response(connection, id)?;
        }
        _ => {
            info!("unhandled request: {}", method);
            send_null_response(connection, id)?;
        }
    }

    Ok(())
}

fn handle_notification(
    state: &mut ServerState,
    notif: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match notif.method.as_str() {
        "textDocument/didOpen" => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                let source = params.text_document.text;
                state.index.update_file(&path, &source);
                state.open_docs.insert(path, source);
            }
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                // With FULL sync, the last content change is the full text.
                if let Some(change) = params.content_changes.into_iter().last() {
                    state.index.update_file(&path, &change.text);
                    state.open_docs.insert(path, change.text);
                }
            }
        }
        "textDocument/didClose" => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                state.open_docs.remove(&path);
                // Re-index from disk so the file stays in the index.
                state.index.index_file(&path);
            }
        }
        "textDocument/didSave" => {
            info!("document saved");
        }
        "initialized" => {
            info!("client sent initialized notification");
        }
        _ => {
            info!("unhandled notification: {}", notif.method);
        }
    }
    Ok(())
}

fn send_empty_response(
    connection: &Connection,
    id: RequestId,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let resp = Response::new_ok(id, serde_json::Value::Array(vec![]));
    connection.sender.send(Message::Response(resp))?;
    Ok(())
}

fn send_null_response(
    connection: &Connection,
    id: RequestId,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let resp = Response::new_ok(id, serde_json::Value::Null);
    connection.sender.send(Message::Response(resp))?;
    Ok(())
}
