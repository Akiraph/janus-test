//! Static asset transport for the bundled web client.
//!
//! The deployment image ships the built client next to the binary and points
//! `JANUS_WEB_DIST` at it, so the browser loads the SPA from the same origin it
//! already uses for `/api` and `/health` — the client only ever issues
//! same-origin relative requests.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use super::problem::{Problem, codes};
use crate::AppState;

pub(super) async fn spa(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path();
    // Unmatched public-protocol paths stay protocol errors. Without this guard
    // the SPA shell would answer unknown API routes with 200 and HTML.
    if path.starts_with("/api/") || path.starts_with("/health/") {
        return Problem::from_code(codes::RESOURCE_NOT_FOUND, format!("no route matches {path}"))
            .into_response();
    }

    let Some(dist) = state.config().web_dist.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Client-side routes such as /projects/{id} have no file on disk; the shell
    // resolves them after hydration.
    let files = ServeDir::new(&dist).fallback(ServeFile::new(dist.join("index.html")));
    match files.oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
    }
}
