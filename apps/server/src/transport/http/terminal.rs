//! HTTP + WebSocket transport for Terminal (M4 Stage 3).
//!
//! Terminal shells run on the Local pipe backend: `bash` (git bash on
//! Windows) is spawned with stdin/stdout/stderr pipes; there is no ConPTY or
//! tty anywhere. Scrollback lives in a bounded log stream. WebSocket frames
//! carry binary PTY input/output plus typed JSON control frames. Access is
//! gated by a hashed, single-use, actor-and-origin-bound ticket whose raw token
//! is returned once and never persisted.

use std::collections::BTreeMap;

use axum::{
    Extension, Json,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
};
use base64::Engine;
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    AppState,
    modules::runtime::interface::{
        ExecutionEnvironment, LogCursor, RelativeWorkingDirectory, TerminalOwner,
        TerminalProjection, TerminalSignal, TerminalSize, TerminalSpec, TerminalTicket,
        TerminalTicketRequest,
    },
    platform::id::{ProjectId, RuntimeId, SessionId, TerminalId},
    transport::http::{
        auth::authenticate,
        dto::DataResponse,
        problem::{Problem, codes, map_runtime_error},
        request_id::RequestContext,
    },
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTerminalRequest {
    pub runtime_id: String,
    pub owner: TerminalOwnerInput,
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
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum TerminalOwnerInput {
    Project(String),
    Session(String),
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
    Extension(context): Extension<RequestContext>,
    Json(body): Json<CreateTerminalRequest>,
) -> Result<(StatusCode, Json<DataResponse<TerminalProjection>>), Problem> {
    let auth = authenticate(&state, &headers).await?;
    let owner = parse_owner(&body.owner)?;
    let size = TerminalSize::new(body.size.cols, body.size.rows)
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid terminal size"))?;
    let working_directory = RelativeWorkingDirectory::new(&body.working_directory)
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid working directory"))?;
    let ordinary = body.environment.map(|value| value.ordinary).unwrap_or_default();
    let environment = ExecutionEnvironment::new(ordinary, Vec::new())
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid environment"))?;
    let runtime_id: RuntimeId = body
        .runtime_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid runtime id"))?;
    let spec = TerminalSpec {
        id: TerminalId::new(),
        runtime_id,
        owner,
        working_directory,
        environment,
        size,
    };
    let _ = (auth, context);
    let data = state
        .runtime()
        .create_terminal(spec)
        .await
        .map_err(map_runtime_error)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data })))
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
    let owner = parse_owner(&query.owner())?;
    let data = state
        .runtime()
        .list_terminals(owner)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[derive(Debug, Deserialize)]
pub struct ListTerminalsQuery {
    pub owner_kind: String,
    pub owner_id: String,
}

impl ListTerminalsQuery {
    fn owner(&self) -> TerminalOwnerInput {
        match self.owner_kind.as_str() {
            "project" => TerminalOwnerInput::Project(self.owner_id.clone()),
            "session" => TerminalOwnerInput::Session(self.owner_id.clone()),
            other => TerminalOwnerInput::Project(other.to_owned()),
        }
    }
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
    Extension(context): Extension<RequestContext>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<(StatusCode, Json<DataResponse<TerminalTicket>>), Problem> {
    let auth = authenticate(&state, &headers).await?;
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
    let _ = context;
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
    let _auth = authenticate(&state, &headers).await?;
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
    let _auth = authenticate(&state, &headers).await?;
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
    let _auth = authenticate(&state, &headers).await?;
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
    responses((status = 200, body = DataResponse<crate::modules::runtime::interface::LogRange>), (status = 401, body = Problem))
)]
pub async fn terminal_scrollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<ScrollbackQuery>,
) -> Result<Json<DataResponse<crate::modules::runtime::interface::LogRange>>, Problem> {
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
        next_cursor = range.stream.next_cursor;
        if range
            .chunks
            .iter()
            .map(|chunk| chunk.text.len())
            .sum::<usize>()
            < 256 * 1024
        {
            break;
        }
        if next_cursor.value() >= range.stream.next_cursor.value() {
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
                        if state
                            .runtime()
                            .write_terminal_input(terminal_id, bytes.to_vec())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<TerminalControl>(text.as_str()) {
                            Ok(TerminalControl::Input { bytes_base64 }) => {
                                if let Some(decoded) = base64_decode(&bytes_base64) {
                                    let _ = state
                                        .runtime()
                                        .write_terminal_input(terminal_id, decoded)
                                        .await;
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
                                let _ = socket
                                    .send(Message::Text(
                                        serde_json::json!({"kind": "error", "detail": "invalid control frame"})
                                            .to_string()
                                            .into(),
                                    ))
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
                last_cursor = projection.stream.next_cursor;
                let status = state.runtime().terminal(terminal_id).await;
                if let Ok(terminal) = status
                    && matches!(
                        terminal.status,
                        crate::modules::runtime::interface::TerminalStatus::Exited
                            | crate::modules::runtime::interface::TerminalStatus::Failed
                            | crate::modules::runtime::interface::TerminalStatus::Lost
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
    error: &crate::modules::runtime::interface::RuntimeError,
) -> Result<(), ()> {
    let message = serde_json::json!({
        "kind": "error",
        "code": error.code().as_str(),
        "detail": error.to_string(),
    });
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .map_err(|_| ())
}

fn base64_decode(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()
}

#[allow(clippy::result_large_err)]
fn parse_cursor(value: Option<String>) -> Result<LogCursor, Problem> {
    match value {
        None => Ok(LogCursor::ZERO),
        Some(raw) => raw
            .parse::<u64>()
            .map(LogCursor::new)
            .map_err(|_| Problem::from_code(codes::TIMELINE_CURSOR_INVALID, "invalid cursor")),
    }
}

#[allow(clippy::result_large_err)]
fn origin_for(headers: &HeaderMap) -> Result<String, Problem> {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| Problem::from_code(codes::TERMINAL_TICKET_INVALID, "missing Origin"))
}

#[allow(clippy::result_large_err)]
fn parse_owner(input: &TerminalOwnerInput) -> Result<TerminalOwner, Problem> {
    match input {
        TerminalOwnerInput::Project(id) => {
            let project_id: ProjectId = id
                .parse()
                .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid project id"))?;
            Ok(TerminalOwner::Project(project_id))
        }
        TerminalOwnerInput::Session(id) => {
            let session_id: SessionId = id
                .parse()
                .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid session id"))?;
            Ok(TerminalOwner::Session(session_id))
        }
    }
}
