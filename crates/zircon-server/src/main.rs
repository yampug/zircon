mod index;
mod workspace;

use std::error::Error;

use log::info;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CompletionOptions, InitializeParams, OneOf, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, WorkDoneProgressOptions,
};

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    env_logger::init();
    info!("zircon-server starting");

    let (connection, io_threads) = Connection::stdio();

    let server_capabilities = serde_json::to_value(capabilities())?;
    let init_params = connection.initialize(server_capabilities)?;
    let init_params: InitializeParams = serde_json::from_value(init_params)?;

    let root_uri = init_params
        .root_uri
        .as_ref()
        .map(|u| u.as_str().to_string());
    info!("initialized with root_uri: {:?}", root_uri);

    main_loop(&connection)?;

    io_threads.join()?;
    info!("zircon-server shut down cleanly");
    Ok(())
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
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

fn main_loop(connection: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                handle_request(connection, req)?;
            }
            Message::Notification(notif) => {
                handle_notification(connection, notif)?;
            }
            Message::Response(_resp) => {}
        }
    }
    Ok(())
}

fn handle_request(
    connection: &Connection,
    req: Request,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let id = req.id.clone();
    let method = req.method.as_str();

    match method {
        "textDocument/definition" => {
            send_empty_response(connection, id)?;
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
    _connection: &Connection,
    notif: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match notif.method.as_str() {
        "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didSave"
        | "textDocument/didClose" => {
            info!("document notification: {}", notif.method);
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
