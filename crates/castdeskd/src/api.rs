//! Localhost REST control API (bearer-token protected).
//!
//! GET    /api/v1/health                 (no auth)
//! GET    /api/v1/system                 session, encoders, MediaMTX
//! GET    /api/v1/displays
//! POST   /api/v1/displays               body: DisplaySpec
//! GET    /api/v1/displays/{name}
//! DELETE /api/v1/displays/{name}
//! POST   /api/v1/displays/{name}/restart
//! GET    /api/v1/control                STUB (stage c): always 501 Not Implemented
//!
//! Testing: no unit tests for the router. `scripts/selftest-stage-a.sh`
//! covers: 401 without a token, POST /displays, GET /displays/{name},
//! DELETE /displays/{name}. Not covered by any test: /health, /system,
//! GET /displays, /restart, /control.

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use castdesk_core::{ApiError, DisplaySpec, SystemInfo};
use serde_json::json;

use crate::manager::Manager;

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<Manager>,
    pub token: Arc<String>,
}

struct Error(StatusCode, String);

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}

fn bad(e: anyhow::Error) -> Error {
    let msg = format!("{e:#}");
    let code = if msg.contains("no display named") {
        StatusCode::NOT_FOUND
    } else if msg.contains("already exists") {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_REQUEST
    };
    Error(code, msg)
}

async fn auth(State(st): State<AppState>, req: Request, next: Next) -> Result<Response, Error> {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|t| constant_time_eq(t.as_bytes(), st.token.as_bytes()));
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(Error(StatusCode::UNAUTHORIZED, "missing or wrong bearer token".into()))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/system", get(system))
        .route("/displays", get(list).post(add))
        .route("/displays/{name}", get(one).delete(remove))
        .route("/displays/{name}/restart", post(restart))
        .route("/control", get(crate::remote::control_ws::handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .nest(
            castdesk_core::API_PREFIX,
            Router::new().route("/health", get(health)).merge(protected),
        )
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

async fn system(State(st): State<AppState>) -> Json<SystemInfo> {
    let ctx = st.manager.context();
    Json(SystemInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        host: ctx.host.clone(),
        session: ctx.session.clone(),
        encoders: ctx.encoders.clone(),
        mediamtx: ctx.mediamtx.info(),
    })
}

async fn list(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.manager.list().await)
}

async fn one(State(st): State<AppState>, Path(name): Path<String>) -> Result<impl IntoResponse, Error> {
    st.manager
        .get(&name)
        .await
        .map(Json)
        .ok_or_else(|| Error(StatusCode::NOT_FOUND, format!("no display named {name:?}")))
}

async fn add(State(st): State<AppState>, Json(spec): Json<DisplaySpec>) -> Result<impl IntoResponse, Error> {
    let status = st.manager.add(spec).await.map_err(bad)?;
    Ok((StatusCode::CREATED, Json(status)))
}

async fn remove(State(st): State<AppState>, Path(name): Path<String>) -> Result<impl IntoResponse, Error> {
    st.manager.remove(&name).await.map_err(bad)?;
    Ok(Json(json!({ "removed": name })))
}

async fn restart(State(st): State<AppState>, Path(name): Path<String>) -> Result<impl IntoResponse, Error> {
    st.manager.restart(&name).await.map_err(bad)?;
    Ok(Json(json!({ "restarting": name })))
}
