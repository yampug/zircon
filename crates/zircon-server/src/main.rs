mod completion;
mod crystal_cli;
mod definition;
mod diagnostics;
mod hover;
mod index;
mod resolver;
mod symbols;
mod uri;
mod workspace;

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::info;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CompletionOptions, CompletionParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentSymbolParams,
    GotoDefinitionParams, HoverParams, InitializeParams, OneOf, PublishDiagnosticsParams,
    SaveOptions, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, WorkDoneProgressOptions, WorkspaceSymbolParams,
};
use tree_sitter::Parser;

use crate::diagnostics::DiagnosticStore;
use crate::index::DocumentIndex;
use crate::workspace::Workspace;

const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(150);

/// Holds all server state across requests.
struct ServerState {
    index: DocumentIndex,
    /// In-memory contents for open documents (path → source).
    open_docs: HashMap<PathBuf, String>,
    /// Tree-sitter parser for ad-hoc parsing (e.g., cursor node lookup).
    parser: Parser,
    /// Workspace root path.
    _root: Option<PathBuf>,
    /// Paths needing diagnostic re-publish, mapped to when the change occurred.
    pending_diagnostics: HashMap<PathBuf, Instant>,
    /// Merged diagnostic store (syntax + compiler).
    diagnostic_store: DiagnosticStore,
    /// Sender for background compiler diagnostic results.
    compiler_tx: crossbeam_channel::Sender<(PathBuf, Vec<lsp_types::Diagnostic>)>,
    /// Receiver for background compiler diagnostic results.
    compiler_rx: crossbeam_channel::Receiver<(PathBuf, Vec<lsp_types::Diagnostic>)>,
}

impl ServerState {
    fn new(root: Option<PathBuf>) -> Self {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&lang)
            .expect("failed to load Crystal grammar");

        let (compiler_tx, compiler_rx) = crossbeam_channel::unbounded();

        let mut state = ServerState {
            index: DocumentIndex::new(),
            open_docs: HashMap::new(),
            parser,
            _root: root.clone(),
            pending_diagnostics: HashMap::new(),
            diagnostic_store: DiagnosticStore::new(),
            compiler_tx,
            compiler_rx,
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

    /// Schedule a diagnostic update for a file (debounced).
    fn schedule_diagnostics(&mut self, path: PathBuf) {
        self.pending_diagnostics.insert(path, Instant::now());
    }

    /// Return the earliest deadline at which pending diagnostics should fire.
    fn next_diagnostic_deadline(&self) -> Option<Instant> {
        self.pending_diagnostics
            .values()
            .min()
            .map(|t| *t + DIAGNOSTICS_DEBOUNCE)
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
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(lsp_types::TextDocumentSyncSaveOptions::SaveOptions(
                    SaveOptions {
                        include_text: Some(false),
                    },
                )),
                ..Default::default()
            },
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
    // Clone the receiver so the select doesn't borrow state.
    let compiler_rx = state.compiler_rx.clone();
    loop {
        let mut sel = crossbeam_channel::Select::new();
        let lsp_idx = sel.recv(&connection.receiver);
        let comp_idx = sel.recv(&compiler_rx);

        let oper = match state.next_diagnostic_deadline() {
            Some(deadline) => match sel.select_deadline(deadline) {
                Ok(oper) => oper,
                Err(_) => {
                    publish_pending_diagnostics(connection, state)?;
                    continue;
                }
            },
            None => sel.select(),
        };

        if oper.index() == lsp_idx {
            let msg = match oper.recv(&connection.receiver) {
                Ok(msg) => msg,
                Err(_) => return Ok(()),
            };
            match msg {
                Message::Request(req) => {
                    if connection.handle_shutdown(&req)? {
                        return Ok(());
                    }
                    handle_request(connection, state, req)?;
                }
                Message::Notification(notif) => {
                    handle_notification(connection, state, notif)?;
                }
                Message::Response(_resp) => {}
            }
        } else if oper.index() == comp_idx {
            if let Ok((path, diags)) = oper.recv(&compiler_rx) {
                state.diagnostic_store.set_compiler(&path, diags);
                if let Some(u) = uri::from_path(&path) {
                    let merged = state.diagnostic_store.merged(&path);
                    send_diagnostics(connection, u, merged)?;
                }
            }
        }
    }
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
            let params: HoverParams = serde_json::from_value(req.params)?;
            let source = params
                .text_document_position_params
                .text_document
                .uri
                .as_str()
                .strip_prefix("file://")
                .and_then(|p| state.get_source(Path::new(p)));
            let result =
                hover::handle(&state.index, &mut state.parser, params, source.as_deref());
            let resp = Response::new_ok(id, result);
            connection.sender.send(Message::Response(resp))?;
        }
        "textDocument/completion" => {
            let params: CompletionParams = serde_json::from_value(req.params)?;
            let source = params
                .text_document_position
                .text_document
                .uri
                .as_str()
                .strip_prefix("file://")
                .and_then(|p| state.get_source(Path::new(p)));
            let result =
                completion::handle(&state.index, &mut state.parser, params, source.as_deref());
            let resp = Response::new_ok(id, result);
            connection.sender.send(Message::Response(resp))?;
        }
        "textDocument/documentSymbol" => {
            let params: DocumentSymbolParams = serde_json::from_value(req.params)?;
            let result = symbols::handle_document_symbols(&state.index, params);
            let resp = Response::new_ok(id, result);
            connection.sender.send(Message::Response(resp))?;
        }
        "workspace/symbol" => {
            let params: WorkspaceSymbolParams = serde_json::from_value(req.params)?;
            let result = symbols::handle_workspace_symbols(&state.index, params);
            let resp = Response::new_ok(id, result);
            connection.sender.send(Message::Response(resp))?;
        }
        _ => {
            info!("unhandled request: {}", method);
            send_null_response(connection, id)?;
        }
    }

    Ok(())
}

fn handle_notification(
    connection: &Connection,
    state: &mut ServerState,
    notif: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match notif.method.as_str() {
        "textDocument/didOpen" => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                let source = params.text_document.text;
                state.index.update_file(&path, &source);
                state.open_docs.insert(path.clone(), source);
                state.schedule_diagnostics(path);
            }
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                if let Some(change) = params.content_changes.into_iter().last() {
                    state.index.update_file(&path, &change.text);
                    state.open_docs.insert(path.clone(), change.text);
                    state.schedule_diagnostics(path);
                }
            }
        }
        "textDocument/didClose" => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                state.open_docs.remove(&path);
                state.pending_diagnostics.remove(&path);
                state.diagnostic_store.clear(&path);
                // Re-index from disk so the file stays in the index.
                state.index.index_file(&path);
                // Clear diagnostics for the closed file.
                if let Some(u) = uri::from_path(&path) {
                    send_diagnostics(connection, u, vec![])?;
                }
            }
        }
        "textDocument/didSave" => {
            let params: DidSaveTextDocumentParams = serde_json::from_value(notif.params)?;
            if let Some(path) = uri::to_path(&params.text_document.uri) {
                info!("document saved: {:?}", path);
                // Run Crystal compiler diagnostics in a background thread so the
                // main LSP loop stays responsive (compilation can take seconds).
                let tx = state.compiler_tx.clone();
                std::thread::spawn(move || {
                    let diags = crystal_cli::check_file(&path).unwrap_or_default();
                    let _ = tx.send((path, diags));
                });
            }
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

fn publish_pending_diagnostics(
    connection: &Connection,
    state: &mut ServerState,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let now = Instant::now();
    let ready: Vec<PathBuf> = state
        .pending_diagnostics
        .iter()
        .filter(|(_, time)| now >= **time + DIAGNOSTICS_DEBOUNCE)
        .map(|(path, _)| path.clone())
        .collect();

    for path in ready {
        state.pending_diagnostics.remove(&path);
        let source = state.get_source(&path);
        let syntax_diags = match source {
            Some(src) => diagnostics::extract_syntax_errors(&mut state.parser, &src),
            None => Vec::new(),
        };
        state.diagnostic_store.set_syntax(&path, syntax_diags);
        if let Some(u) = uri::from_path(&path) {
            let merged = state.diagnostic_store.merged(&path);
            send_diagnostics(connection, u, merged)?;
        }
    }
    Ok(())
}

fn send_diagnostics(
    connection: &Connection,
    uri: lsp_types::Uri,
    diagnostics: Vec<lsp_types::Diagnostic>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    let notif = lsp_server::Notification::new(
        "textDocument/publishDiagnostics".to_string(),
        params,
    );
    connection.sender.send(Message::Notification(notif))?;
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
