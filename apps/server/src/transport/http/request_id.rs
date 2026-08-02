use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use http::{HeaderValue, header::HeaderName};

use janus_infrastructure::id::RequestId;

pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: String,
}

pub async fn middleware(mut request: Request<Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| RequestId::new().to_string());
    request.extensions_mut().insert(RequestContext {
        request_id: request_id.clone(),
    });
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(&X_REQUEST_ID, value);
    }
    response
}
