//! HTTP and WebSocket transport for Runtime-owned Terminals.
//!
//! Terminal shells run on the Local pipe backend: `bash` (git bash on
//! Windows) is spawned with stdin/stdout/stderr pipes; there is no ConPTY or
//! tty anywhere. Scrollback lives in a bounded log stream. WebSocket frames
//! carry binary PTY input/output plus typed JSON control frames. Access is
//! gated by a hashed, single-use, actor-and-origin-bound ticket whose raw token
//! is returned once and never persisted.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
};
use base64::Engine;
use futures_util::SinkExt;
use janus_infrastructure::id::{ProjectId, TerminalId};
use janus_runtime::interface::{
    ExecutionEnvironment, LogCursor, RelativeWorkingDirectory, TerminalProjection, TerminalSignal,
    TerminalSize, TerminalTicket, TerminalTicketRequest,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    AppState,
    transport::http::{
        auth::authenticate,
        dto::DataResponse,
        problem::{Problem, codes, map_runtime_error},
    },
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTerminalRequest {
    pub project_id: String,
    #[serde(default = "default_cwd")]
    pub working_directory: String,
    #[serde(default)]
    pub environment: Option<EnvironmentInput>,
    pub size: TerminalSizeInput,
}

fn default_cwd() -> String {
    ".".to_owned()
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TerminalSizeInput {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnvironmentInput {
    #[serde(default)]
    pub ordinary: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResizeTerminalRequest {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SignalTerminalRequest {
    pub signal: TerminalSignal,
}

#[derive(Debug, Deserialize)]
pub struct ScrollbackQuery {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectQuery {
    pub token: String,
    #[serde(default)]
    pub after: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/v1/terminals",
    request_body = CreateTerminalRequest,
    responses((status = 201, body = DataResponse<TerminalProjection>), (status = 401, body = Problem))
)]
pub async fn create_terminal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateTerminalRequest>,
) -> Result<(StatusCode, Json<DataResponse<TerminalProjection>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let project_id: ProjectId = body
        .project_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid project id"))?;
    let size = TerminalSize::new(body.size.cols, body.size.rows)
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal size"))?;
    let working_directory = RelativeWorkingDirectory::new(&body.working_directory)
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid working directory"))?;
    let ordinary = body
        .environment
        .map(|value| value.ordinary)
        .unwrap_or_default();
    let environment = ExecutionEnvironment::new(ordinary, Vec::new())
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid environment"))?;
    let data = state
        .application()
        .create_project_terminal(
            &auth.owner_id,
            project_id,
            working_directory,
            environment,
            size,
        )
        .await
        .map_err(map_project_terminal_error)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data })))
}

fn map_project_terminal_error(
    error: crate::application::project_terminal::ProjectTerminalError,
) -> Problem {
    match error {
        crate::application::project_terminal::ProjectTerminalError::Projects(error) => {
            Problem::from_code(error.code(), error.to_string())
        }
        crate::application::project_terminal::ProjectTerminalError::Runtime(error) => {
            map_runtime_error(error)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/terminals",
    responses((status = 200, body = DataResponse<Vec<TerminalProjection>>,), (status = 401, body = Problem))
)]
pub async fn list_terminals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListTerminalsQuery>,
) -> Result<Json<DataResponse<Vec<TerminalProjection>>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let project_id: ProjectId = query
        .project_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid project id"))?;
    let data = state
        .runtime()
        .list_terminals(project_id)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[derive(Debug, Deserialize)]
pub struct ListTerminalsQuery {
    pub project_id: String,
}

#[utoipa::path(
    post,
    path = "/api/v1/terminals/{id}/tickets",
    params(("id" = String, Path)),
    responses((status = 201, body = DataResponse<TerminalTicket>), (status = 401, body = Problem))
)]
pub async fn issue_terminal_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<(StatusCode, Json<DataResponse<TerminalTicket>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let terminal_id: TerminalId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal id"))?;
    let origin = origin_for(&headers)?;
    let request = TerminalTicketRequest {
        terminal_id,
        actor_id: auth.owner_id.clone(),
        origin,
    };
    let data = state
        .runtime()
        .issue_terminal_ticket(request)
        .await
        .map_err(map_runtime_error)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data })))
}

#[utoipa::path(
    post,
    path = "/api/v1/terminals/{id}/resize",
    params(("id" = String, Path)),
    request_body = ResizeTerminalRequest,
    responses((status = 200, body = DataResponse<TerminalProjection>), (status = 401, body = Problem))
)]
pub async fn resize_terminal(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<ResizeTerminalRequest>,
) -> Result<Json<DataResponse<TerminalProjection>>, Problem> {
    let _auth = authorized(&state, &headers).await?;
    let terminal_id: TerminalId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal id"))?;
    let size = TerminalSize::new(body.cols, body.rows)
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal size"))?;
    let data = state
        .runtime()
        .resize_terminal(terminal_id, size)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/terminals/{id}/signal",
    params(("id" = String, Path)),
    request_body = SignalTerminalRequest,
    responses((status = 204, body = ()), (status = 401, body = Problem))
)]
pub async fn signal_terminal(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<SignalTerminalRequest>,
) -> Result<StatusCode, Problem> {
    let _auth = authorized(&state, &headers).await?;
    let terminal_id: TerminalId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal id"))?;
    state
        .runtime()
        .signal_terminal(terminal_id, body.signal)
        .await
        .map_err(map_runtime_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/terminals/{id}/close",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<TerminalProjection>), (status = 401, body = Problem))
)]
pub async fn close_terminal(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<DataResponse<TerminalProjection>>, Problem> {
    let _auth = authorized(&state, &headers).await?;
    let terminal_id: TerminalId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal id"))?;
    let data = state
        .runtime()
        .close_terminal(terminal_id)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    get,
    path = "/api/v1/terminals/{id}/scrollback",
    params(("id" = String, Path), ("after" = Option<String>, Query), ("limit" = Option<usize>, Query)),
    responses((status = 200, body = DataResponse<janus_runtime::interface::LogRange>), (status = 401, body = Problem))
)]
pub async fn terminal_scrollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<ScrollbackQuery>,
) -> Result<Json<DataResponse<janus_runtime::interface::LogRange>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let terminal_id: TerminalId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal id"))?;
    let after = parse_cursor(query.after)?;
    let data = state
        .runtime()
        .terminal_scrollback(terminal_id, after, query.limit.unwrap_or(1024 * 1024))
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

/// WebSocket upgrade for a Terminal. The client supplies a one-use ticket token
/// in the query string plus an optional scrollback resume cursor. After replay,
/// the server streams live scrollback output and accepts binary input or JSON
/// control frames (`input`/`resize`/`signal`/`close`).
#[utoipa::path(
    get,
    path = "/api/v1/terminals/{id}/connect",
    params(("id" = String, Path), ("token" = String, Query), ("after" = Option<String>, Query)),
    responses((status = 101, description = "websocket upgrade"), (status = 401, body = Problem))
)]
pub async fn connect_terminal(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<ConnectQuery>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let requested_id: TerminalId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal id"))?;
    let origin = origin_for(&headers)?;
    let terminal_id = state
        .runtime()
        .consume_terminal_ticket(&query.token, &auth.owner_id, &origin)
        .await
        .map_err(map_runtime_error)?;
    if terminal_id != requested_id {
        return Err(Problem::from_code(
            codes::TERMINAL_TICKET_INVALID,
            "ticket does not match terminal",
        ));
    }
    let resume_after = parse_cursor(query.after)?;
    Ok(ws.on_upgrade(move |socket| drive_terminal(state, terminal_id, resume_after, socket)))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TerminalControl {
    Input { bytes_base64: String },
    Resize { cols: u16, rows: u16 },
    Signal { signal: TerminalSignal },
    Close,
}

async fn drive_terminal(
    state: AppState,
    terminal_id: TerminalId,
    resume_after: LogCursor,
    mut socket: WebSocket,
) {
    // Replay bounded scrollback before live output.
    let mut next_cursor = resume_after;
    loop {
        let range = match state
            .runtime()
            .terminal_scrollback(terminal_id, next_cursor, 256 * 1024)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let _ = send_problem(&mut socket, &error).await;
                break;
            }
        };
        for chunk in &range.chunks {
            if !chunk.text.is_empty()
                && socket
                    .send(Message::Binary(chunk.text.as_bytes().to_vec().into()))
                    .await
                    .is_err()
            {
                return;
            }
        }
        // `stream.next_cursor` is the log's write head, not the end of this
        // page. Resuming from it skipped everything the byte limit left unread,
        // so a terminal with more scrollback than one page replayed as its
        // oldest page followed by a hole up to the moment of connect.
        let mut page_end = next_cursor;
        let mut page_bytes = 0usize;
        for chunk in &range.chunks {
            page_bytes += chunk.text.len();
            page_end = page_end.max(chunk.end_cursor);
        }
        let advanced = page_end > next_cursor;
        next_cursor = page_end;
        if page_bytes < 256 * 1024 || !advanced {
            break;
        }
    }

    // Subscribe to live scrollback additions: poll the projection until it
    // advances, then ship the new chunk. Cheap bounded polling is acceptable
    // because Terminal scrollback is sparse and a low-rate fallback.
    let mut last_cursor = next_cursor;
    loop {
        tokio::select! {
            recv = socket.recv() => {
                match recv {
                    Some(Ok(Message::Binary(bytes))) => {
                        if let Err(error) = state
                            .runtime()
                            .write_terminal_input(terminal_id, bytes.to_vec())
                            .await
                        {
                            let _ = send_problem(&mut socket, &error).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<TerminalControl>(text.as_str()) {
                            Ok(TerminalControl::Input { bytes_base64 }) => {
                                let Some(decoded) = base64_decode(&bytes_base64) else {
                                    let _ = send_error_frame(
                                        &mut socket,
                                        codes::VALIDATION_FAILED,
                                        "control frame bytes_base64 is not valid base64",
                                    )
                                    .await;
                                    continue;
                                };
                                if let Err(error) = state
                                    .runtime()
                                    .write_terminal_input(terminal_id, decoded)
                                    .await
                                {
                                    let _ = send_problem(&mut socket, &error).await;
                                    break;
                                }
                            }
                            Ok(TerminalControl::Resize { cols, rows }) => {
                                if let Ok(size) = TerminalSize::new(cols, rows) {
                                    let _ = state
                                        .runtime()
                                        .resize_terminal(terminal_id, size)
                                        .await;
                                }
                            }
                            Ok(TerminalControl::Signal { signal }) => {
                                let _ = state
                                    .runtime()
                                    .signal_terminal(terminal_id, signal)
                                    .await;
                            }
                            Ok(TerminalControl::Close) => {
                                let _ = state
                                    .runtime()
                                    .close_terminal(terminal_id)
                                    .await;
                                break;
                            }
                            Err(_) => {
                                let _ = send_error_frame(
                                    &mut socket,
                                    codes::VALIDATION_FAILED,
                                    "control frame is not a known terminal control message",
                                )
                                .await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                    Some(Err(_)) => break,
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(120)) => {
                let projection = match state
                    .runtime()
                    .terminal_scrollback(terminal_id, last_cursor, 1024 * 1024)
                    .await
                {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                for chunk in &projection.chunks {
                    if !chunk.text.is_empty()
                        && socket
                            .send(Message::Binary(chunk.text.as_bytes().to_vec().into()))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
                for chunk in &projection.chunks {
                    last_cursor = last_cursor.max(chunk.end_cursor);
                }
                let status = state.runtime().terminal(terminal_id).await;
                if let Ok(terminal) = status
                    && matches!(
                        terminal.status,
                        janus_runtime::interface::TerminalStatus::Exited
                            | janus_runtime::interface::TerminalStatus::Failed
                            | janus_runtime::interface::TerminalStatus::Lost
                    )
                {
                    let _ = socket
                        .send(Message::Text(
                            serde_json::json!({
                                "kind": "exit",
                                "exit": terminal.exit,
                                "status": terminal.status,
                            })
                            .to_string()
                            .into(),
                        ))
                        .await;
                    break;
                }
            }
        }
    }
    let _ = socket.close().await;
}

async fn send_problem(
    socket: &mut WebSocket,
    error: &janus_runtime::interface::RuntimeError,
) -> Result<(), ()> {
    send_error_frame(socket, error.code().as_str(), &error.to_string()).await
}

/// Every failure the client is told about arrives as one frame shape: `kind`
/// `error` plus a stable `code` and a `detail`. Without the `code` the terminal
/// view can only print the sentence, so it cannot tell a rejected frame from a
/// dead shell.
async fn send_error_frame(socket: &mut WebSocket, code: &str, detail: &str) -> Result<(), ()> {
    let message = serde_json::json!({
        "kind": "error",
        "code": code,
        "detail": detail,
    });
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .map_err(|_| ())
}

fn base64_decode(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(value).ok()
}

fn parse_cursor(value: Option<String>) -> Result<LogCursor, Problem> {
    match value {
        None => Ok(LogCursor::ZERO),
        Some(raw) => raw
            .parse::<u64>()
            .map(LogCursor::new)
            .map_err(|_| Problem::from_code(codes::TIMELINE_CURSOR_INVALID, "invalid cursor")),
    }
}

fn origin_for(headers: &HeaderMap) -> Result<String, Problem> {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| Problem::from_code(codes::TERMINAL_TICKET_INVALID, "missing Origin"))
}
