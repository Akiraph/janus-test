use std::{convert::Infallible, time::Duration};

use axum::{
    Extension,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::Stream;
use serde_json::json;

use crate::{
    AppState,
    transport::http::{dto::EventsQuery, problem::Problem, request_id::RequestContext},
};

#[utoipa::path(
    get,
    path = "/api/v1/events",
    params(("after" = Option<String>, Query, description = "Global event cursor")),
    responses(
        (status = 200, description = "Persistent Janus event stream", content_type = "text/event-stream"),
        (status = 400, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
    Extension(context): Extension<RequestContext>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Problem> {
    let header_cursor = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    if let (Some(header), Some(query)) = (&header_cursor, &query.after)
        && header != query
    {
        return Err(with_request(
            Problem::new(
                StatusCode::BAD_REQUEST,
                "CURSOR_MISMATCH",
                "Event cursor mismatch",
                "Last-Event-ID and after must contain the same cursor.",
            ),
            &context,
        ));
    }
    let raw_cursor = header_cursor.or(query.after).unwrap_or_else(|| "0".into());
    let after = raw_cursor.parse::<u64>().map_err(|_| {
        with_request(
            Problem::new(
                StatusCode::BAD_REQUEST,
                "INVALID_CURSOR",
                "Invalid event cursor",
                "The event cursor must be an unsigned decimal string.",
            ),
            &context,
        )
    })?;
    let bounds = state.events().bounds().await.map_err(|error| {
        tracing::error!(request_id = %context.request_id, %error, "read event bounds");
        with_request(
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal server error",
                "The event stream could not be opened.",
            ),
            &context,
        )
    })?;
    if after > bounds.max {
        return Err(with_request(
            Problem::new(
                StatusCode::BAD_REQUEST,
                "EVENT_CURSOR_AHEAD",
                "Event cursor is ahead",
                "The requested cursor is newer than the server high-water mark.",
            )
            .with_cursor(bounds.max),
            &context,
        ));
    }
    if bounds.min > 0 && after.saturating_add(1) < bounds.min {
        return Err(with_request(
            Problem::new(
                StatusCode::CONFLICT,
                "EVENT_CURSOR_EXPIRED",
                "Event cursor expired",
                "The requested cursor is older than the retained event history.",
            )
            .with_cursor(bounds.max),
            &context,
        ));
    }

    let heartbeat = state.config().event_heartbeat;
    let stream_state = state.clone();
    let stream = async_stream::stream! {
        let mut cursor = after;
        let mut receiver = stream_state.events().subscribe();
        loop {
            match stream_state.events().after(cursor, 256).await {
                Ok(batch) if !batch.is_empty() => {
                    for envelope in batch {
                        if let Ok(next_cursor) = envelope.cursor.parse::<u64>() {
                            cursor = next_cursor;
                        }
                        match Event::default().event("janus").id(envelope.cursor.clone()).json_data(&envelope) {
                            Ok(event) => yield Ok(event),
                            Err(error) => {
                                tracing::error!(%error, "serialize SSE event");
                                return;
                            }
                        }
                    }
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(%error, "replay public events");
                    return;
                }
            }

            let cursor_data = json!({ "cursor": cursor.to_string() });
            match Event::default().event("cursor").id(cursor.to_string()).json_data(cursor_data) {
                Ok(event) => yield Ok(event),
                Err(error) => {
                    tracing::error!(%error, "serialize SSE cursor frame");
                    return;
                }
            }

            tokio::select! {
                _ = receiver.recv() => {},
                () = tokio::time::sleep(heartbeat) => {
                    yield Ok(Event::default().comment("heartbeat"));
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(heartbeat.as_secs().max(1)))
            .text("heartbeat"),
    ))
}

fn with_request(mut problem: Problem, context: &RequestContext) -> Problem {
    problem.request_id = Some(context.request_id.clone().into_boxed_str());
    problem
}
