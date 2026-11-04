//! STUB (stage c): CastDesk WebSocket control protocol, spoken by the custom
//! Media3 receiver app. Draft spec: `docs/control-protocol.md`.
//!
//! Testing: the 501 response from `/api/v1/control` is not covered by an
//! automated test (the API router has no tests).

use axum::http::StatusCode;
use axum::response::IntoResponse;
use castdesk_core::not_implemented;

/// Placeholder for the WebSocket upgrade handler.
pub async fn handler() -> impl IntoResponse {
    (StatusCode::NOT_IMPLEMENTED, not_implemented("the WebSocket control protocol", "c"))
}
