use std::sync::Arc;

use crate::control::{Control, ControlError};
use axum::extract::{
    Path, Query, State,
    rejection::{JsonRejection, PathRejection, QueryRejection},
};
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};
use sap_core::{
    api::*,
    ids::{AccountId, NodeId, ThreadKey},
};
use serde::Deserialize;

type Node = State<Arc<dyn Control>>;
type JsonBody<T> = Result<Json<T>, JsonRejection>;
type Params<T> = Result<Query<T>, QueryRejection>;
type Segment = Result<Path<String>, PathRejection>;

pub(super) struct Error(ControlError);
impl From<ControlError> for Error {
    fn from(value: ControlError) -> Self {
        Self(value)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(self.0.body()),
        )
            .into_response()
    }
}
fn invalid(message: impl Into<String>) -> Error {
    ControlError::invalid(message).into()
}
fn body<T>(value: JsonBody<T>) -> Result<T, Error> {
    value.map(|Json(v)| v).map_err(|e| invalid(e.body_text()))
}
fn params<T>(value: Params<T>) -> Result<T, Error> {
    value.map(|Query(v)| v).map_err(|e| invalid(e.body_text()))
}
fn segment(value: Segment) -> Result<String, Error> {
    value.map(|Path(v)| v).map_err(|e| invalid(e.body_text()))
}

#[derive(Deserialize)]
pub(super) struct Hours {
    #[serde(default = "default_hours")]
    hours: u32,
}
fn default_hours() -> u32 {
    24
}
#[derive(Deserialize)]
pub(super) struct Active {
    #[serde(default)]
    active: bool,
}
#[derive(Deserialize)]
pub(super) struct Limit {
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    100
}

pub(super) async fn flow(State(c): Node) -> Json<FlowSnapshot> {
    Json(c.flow())
}
pub(super) async fn accounts(State(c): Node) -> Json<Vec<AccountView>> {
    Json(c.accounts())
}
pub(super) async fn patch_account(
    State(c): Node,
    id: Segment,
    patch: JsonBody<AccountPatch>,
) -> Result<Json<AccountView>, Error> {
    Ok(Json(c.patch_account(&AccountId(segment(id)?), body(patch)?)?))
}
pub(super) async fn login(State(c): Node, request: JsonBody<LoginRequest>) -> Result<Json<LoginStart>, Error> {
    Ok(Json(c.start_login(body(request)?).await?))
}
pub(super) async fn account_history(
    State(c): Node,
    id: Segment,
    query: Params<Hours>,
) -> Result<Json<AccountHistory>, Error> {
    Ok(Json(c.account_history(&AccountId(segment(id)?), params(query)?.hours)?))
}
pub(super) async fn threads(State(c): Node, query: Params<Active>) -> Result<Json<Vec<ThreadView>>, Error> {
    Ok(Json(c.threads(params(query)?.active)))
}
pub(super) async fn assign(
    State(c): Node,
    key: Segment,
    request: JsonBody<AssignRequest>,
) -> Result<Json<ThreadView>, Error> {
    // Axum matches the raw segment, then Path decodes once, retaining encoded `/`
    // inside this key. Do not percent-decode again (`%252F` means literal `%2F`).
    let key = ThreadKey::parse(&segment(key)?).ok_or_else(|| invalid("invalid thread key"))?;
    Ok(Json(c.assign_thread(&key, &body(request)?.account)?))
}
pub(super) async fn nodes(State(c): Node) -> Json<Vec<NodeView>> {
    Json(c.nodes())
}
pub(super) async fn conflicts(State(c): Node) -> Json<Vec<sap_core::sync::Conflict>> {
    Json(c.conflicts())
}
pub(super) async fn resolve(
    State(c): Node,
    id: Segment,
    request: JsonBody<ResolveRequest>,
) -> Result<Json<sap_core::sync::Conflict>, Error> {
    let keep: NodeId = body(request)?.keep;
    Ok(Json(c.resolve_conflict(&segment(id)?, &keep)?))
}
pub(super) async fn events(State(c): Node, query: Params<Limit>) -> Result<Json<Vec<EventView>>, Error> {
    Ok(Json(c.events(params(query)?.limit)))
}
pub(super) async fn status(State(c): Node) -> Json<sap_core::reset_aware::StatusResponse> {
    Json(c.reset_aware_status())
}
pub(super) async fn history(
    State(c): Node,
    query: Params<Hours>,
) -> Result<Json<sap_core::reset_aware::HistoryResponse>, Error> {
    Ok(Json(c.reset_aware_history(params(query)?.hours)))
}
pub(super) async fn requests(State(c): Node) -> Json<sap_core::reset_aware::RequestsResponse> {
    Json(c.reset_aware_requests())
}
pub(super) async fn refresh(State(c): Node) -> Json<sap_core::reset_aware::StatusResponse> {
    Json(c.reset_aware_refresh().await)
}

pub(super) async fn not_found() -> Error {
    ControlError::not_found("unknown API route").into()
}
pub(super) async fn method_not_allowed() -> Error {
    ControlError {
        status: 405,
        error: "method_not_allowed",
        message: "method not allowed".into(),
    }
    .into()
}
