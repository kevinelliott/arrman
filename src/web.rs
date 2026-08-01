//! THESIS: One browser watch surface joins live Compose intent, Docker state,
//! protected operations, integrations, and recoverable setup bundles.
//! OWN-WORLD: Ink-dark instrument fields, steel rules, sea-glass focus, explicit
//! state words, square geometry, and a persistent shared command channel.
//! STORY: Authenticate, read host condition, select a service, inspect its
//! interlocks, act with exact scope, and follow the result without losing place.
//! FIRST VIEWPORT: Host and command rail above the fleet ledger and selected
//! service workbench, with the shared event strip anchored below.
//! FORM: Sectional Engineering Watch station, grounded direction 3, seed 790992d1.
//! FINISH: unreviewed and undocumented is unfinished; this surface ends with
//! responsive screenshots, a finish verdict, and reconciled DESIGN.md guidance.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_stream::stream;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath, Query, State};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE,
};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, oneshot};
use tokio_util::io::ReaderStream;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use url::Url;

use crate::backup::{BackupRecord, BundleInspection};
use crate::control::{ActiveOperation, ActivityLevel, BackupHealth, ControlPlane, is_busy_error};
use crate::diagnostics::{diagnose, scan_all};
use crate::integrations::{apply_integration_plan, integration_plan};
use crate::model::{
    CpuTimes, DependencySummary, Diagnosis, HostStats, IntegrationPlan, PortSpec, ServiceAction,
    ServiceRow, Snapshot,
};

const INDEX_HTML: &str = include_str!("web_assets/index.html");
const APP_CSS: &str = include_str!("web_assets/app.css");
const APP_JS: &str = include_str!("web_assets/app.js");
const FAVICON_SVG: &str = include_str!("web_assets/favicon.svg");
const RESTORE_TICKET_TTL: Duration = Duration::from_secs(30 * 60);
const DOWNLOAD_TICKET_TTL: Duration = Duration::from_secs(2 * 60);
const MAX_RESTORE_TICKETS: usize = 16;
const MAX_DOWNLOAD_TICKETS: usize = 64;

#[derive(Clone, Debug)]
pub struct WebConfig {
    pub bind: SocketAddr,
    pub token_file: PathBuf,
    pub refresh_interval: Duration,
    pub demo: bool,
}

pub struct WebServerHandle {
    pub address: SocketAddr,
    pub token_file: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<Result<()>>>,
}

#[derive(Clone)]
struct WebState {
    control: ControlPlane,
    token: Arc<Vec<u8>>,
    token_hint: String,
    refresh_interval: Duration,
    demo: bool,
    latest: Arc<RwLock<Option<RealtimeSnapshot>>>,
    events: broadcast::Sender<WireEvent>,
    tickets: Arc<Mutex<HashMap<String, RestoreTicket>>>,
    download_tickets: Arc<Mutex<HashMap<String, DownloadTicket>>>,
}

#[derive(Clone, Debug)]
struct RestoreTicket {
    path: PathBuf,
    expires_at: Instant,
    temporary: bool,
}

#[derive(Clone, Debug)]
struct DownloadTicket {
    path: PathBuf,
    file_name: String,
    size: u64,
    expires_at: Instant,
}

#[derive(Clone, Debug, Serialize)]
struct WireEvent {
    kind: String,
    data: Value,
}

#[derive(Clone, Debug, Serialize)]
struct RealtimeSnapshot {
    sampled_at: String,
    snapshot: Snapshot,
    active_operation: Option<ActiveOperation>,
}

#[derive(Debug, Serialize)]
struct MetaResponse {
    application: &'static str,
    version: &'static str,
    authentication: &'static str,
    token_hint: String,
    refresh_milliseconds: u128,
    demo: bool,
}

#[derive(Debug, Serialize)]
struct AcceptedOperation {
    accepted: bool,
    operation: ActiveOperation,
}

#[derive(Debug, Serialize)]
struct IntegrationResponse {
    generated_at: String,
    pending: Vec<PublicIntegrationChange>,
    blocked: Vec<crate::model::BlockedIntegration>,
}

#[derive(Debug, Serialize)]
struct PublicIntegrationChange {
    target: String,
    service: String,
    kind: String,
    action: String,
    summary: String,
    manual: String,
}

#[derive(Debug, Serialize)]
struct BackupResponse {
    #[serde(flatten)]
    health: BackupHealth,
    sensitive: bool,
    records: Vec<BackupRecord>,
}

#[derive(Debug, Serialize)]
struct RestoreInspectionResponse {
    ticket: String,
    expires_in_seconds: u64,
    inspection: BundleInspection,
}

#[derive(Debug, Serialize)]
struct DeleteResponse {
    deleted: bool,
    id: String,
}

#[derive(Debug, Serialize)]
struct DownloadTicketResponse {
    url: String,
    expires_in_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct ScopeQuery {
    action: String,
}

#[derive(Debug, Deserialize)]
struct ServiceActionRequest {
    action: String,
}

#[derive(Debug, Deserialize)]
struct PortChangeRequest {
    port_index: usize,
    port: u16,
}

#[derive(Debug, Deserialize)]
struct UrlImportRequest {
    url: String,
}

#[derive(Debug, Deserialize)]
struct RestoreRequest {
    ticket: String,
    confirmation: String,
}

#[derive(Debug, Deserialize)]
struct DeleteRequest {
    confirmation: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    recovery: Option<String>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            recovery: None,
        }
    }

    fn conflict(error: &anyhow::Error) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: format!("{error}"),
            recovery: Some("Wait for the active operation to finish, then retry.".to_string()),
        }
    }

    fn internal(error: anyhow::Error, recovery: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: sanitize_error(&format!("{error:#}")),
            recovery: Some(recovery.into()),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            recovery: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.message,
                "recovery": self.recovery,
            })),
        )
            .into_response()
    }
}

impl WebServerHandle {
    pub fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.join_inner()
    }

    pub fn wait(mut self) -> Result<()> {
        self.join_inner()
    }

    fn join_inner(&mut self) -> Result<()> {
        match self.join.take() {
            Some(join) => join
                .join()
                .map_err(|_| anyhow!("web server thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for WebServerHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn start(control: ControlPlane, config: WebConfig) -> Result<WebServerHandle> {
    let token = Arc::new(read_or_create_token(&config.token_file)?);
    let token_hint = config.token_file.display().to_string();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let thread_config = config.clone();
    let join = thread::spawn(move || -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("docker-services-web")
            .build()?;
        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::bind(thread_config.bind).await {
                Ok(listener) => listener,
                Err(error) => {
                    let message = format!(
                        "could not bind web server to {}: {error}",
                        thread_config.bind
                    );
                    let _ = started_tx.send(Err(message.clone()));
                    bail!(message);
                }
            };
            let address = listener.local_addr()?;
            let (event_tx, _) = broadcast::channel(512);
            let state = WebState {
                control,
                token,
                token_hint,
                refresh_interval: thread_config.refresh_interval,
                demo: thread_config.demo,
                latest: Arc::new(RwLock::new(None)),
                events: event_tx,
                tickets: Arc::new(Mutex::new(HashMap::new())),
                download_tickets: Arc::new(Mutex::new(HashMap::new())),
            };
            let router = router(state.clone());
            tokio::spawn(snapshot_loop(state.clone()));
            tokio::spawn(activity_loop(state));
            let _ = started_tx.send(Ok(address));
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    tokio::select! {
                        _ = shutdown_rx => {},
                        _ = tokio::signal::ctrl_c() => {},
                    }
                })
                .await
                .context("web server stopped unexpectedly")
        })
    });
    let address = started_rx
        .recv()
        .context("web server did not report startup")?
        .map_err(anyhow::Error::msg)?;
    Ok(WebServerHandle {
        address,
        token_file: config.token_file,
        shutdown: Some(shutdown_tx),
        join: Some(join),
    })
}

fn router(state: WebState) -> Router {
    let protected = Router::new()
        .route("/api/state", get(get_state))
        .route("/api/events", get(get_events))
        .route("/api/services/{service}/scope", get(get_action_scope))
        .route("/api/services/{service}/action", post(post_service_action))
        .route("/api/services/{service}/port", post(post_port_change))
        .route("/api/diagnostics/{service}", post(post_diagnose))
        .route("/api/diagnostics", post(post_scan_all))
        .route(
            "/api/integrations",
            get(get_integrations).post(post_apply_integrations),
        )
        .route("/api/backups", get(get_backups).post(post_backup))
        .route("/api/backups/{id}/download", get(get_backup_download))
        .route(
            "/api/backups/{id}/download-ticket",
            post(post_download_ticket),
        )
        .route("/api/backups/{id}/inspect", post(post_backup_inspect))
        .route("/api/backups/{id}", delete(delete_backup))
        .route("/api/setup/inspect/upload", post(post_inspect_upload))
        .route("/api/setup/inspect/url", post(post_inspect_url))
        .route("/api/setup/restore", post(post_restore))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(css))
        .route("/app.js", get(javascript))
        .route("/favicon.svg", get(favicon))
        .route("/download/{ticket}", get(get_ticket_download))
        .route("/api/meta", get(get_meta))
        .merge(protected)
        .fallback(not_found)
        .with_state(state.clone())
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(
            state
                .control
                .backup_store()
                .policy()
                .max_import_bytes
                .saturating_add(2 * 1024 * 1024) as usize,
        ))
        .layer(CatchPanicLayer::new())
        .layer(middleware::from_fn(security_headers))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn css() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "text/css; charset=utf-8"),
            (CACHE_CONTROL, "no-cache"),
        ],
        APP_CSS,
    )
}

async fn javascript() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (CACHE_CONTROL, "no-cache"),
        ],
        APP_JS,
    )
}

async fn favicon() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (CACHE_CONTROL, "no-cache"),
        ],
        FAVICON_SVG,
    )
}

async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":"The requested watch endpoint does not exist."})),
    )
}

async fn get_meta(State(state): State<WebState>) -> Json<MetaResponse> {
    Json(MetaResponse {
        application: "docker-services",
        version: env!("CARGO_PKG_VERSION"),
        authentication: "bearer-token",
        token_hint: state.token_hint,
        refresh_milliseconds: state.refresh_interval.as_millis(),
        demo: state.demo,
    })
}

async fn get_state(State(state): State<WebState>) -> Result<Json<RealtimeSnapshot>, ApiError> {
    if let Some(snapshot) = state
        .latest
        .read()
        .unwrap_or_else(|item| item.into_inner())
        .clone()
    {
        return Ok(Json(snapshot));
    }
    let snapshot = build_realtime_snapshot(state.clone(), None)
        .await
        .map_err(|error| ApiError::internal(error, "Check Docker and Compose, then retry."))?
        .0;
    Ok(Json(snapshot))
}

async fn get_events(
    State(state): State<WebState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = state.events.subscribe();
    let initial = state
        .latest
        .read()
        .unwrap_or_else(|item| item.into_inner())
        .clone();
    let activity = state.control.events_since(0);
    let output = stream! {
        if let Some(snapshot) = initial {
            let wire = WireEvent {
                kind: "snapshot".to_string(),
                data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
            };
            yield Ok(wire_to_sse(&wire));
        }
        for item in activity.into_iter().rev().take(100).collect::<Vec<_>>().into_iter().rev() {
            let wire = WireEvent {
                kind: "activity".to_string(),
                data: serde_json::to_value(item).unwrap_or(Value::Null),
            };
            yield Ok(wire_to_sse(&wire));
        }
        loop {
            match receiver.recv().await {
                Ok(wire) => yield Ok(wire_to_sse(&wire)),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let wire = WireEvent {
                        kind: "reload".to_string(),
                        data: json!({"reason":"event stream lagged"}),
                    };
                    yield Ok(wire_to_sse(&wire));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("watch-alive"),
    )
}

async fn get_action_scope(
    State(state): State<WebState>,
    AxumPath(service): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Value>, ApiError> {
    let action = parse_action(&query.action)?;
    let manager = state.control.manager().clone();
    let scope_service = service.clone();
    let scope = tokio::task::spawn_blocking(move || manager.action_scope(&scope_service, action))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry the scope calculation."))?
        .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    Ok(Json(json!({
        "service": service,
        "action": action.label().to_ascii_lowercase(),
        "scope": scope,
    })))
}

async fn post_service_action(
    State(state): State<WebState>,
    AxumPath(service): AxumPath<String>,
    Json(request): Json<ServiceActionRequest>,
) -> Result<(StatusCode, Json<AcceptedOperation>), ApiError> {
    ensure_live(&state)?;
    let action = parse_action(&request.action)?;
    let manager = state.control.manager().clone();
    let mut permit = state
        .control
        .begin_operation(format!("{} {service}", action.label()), "web")
        .map_err(map_operation_error)?;
    let operation = permit.operation().clone();
    tokio::task::spawn_blocking(move || {
        let result = manager.run_service_action(&service, action, |line| permit.progress(line));
        let _ = permit.finish(result);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedOperation {
            accepted: true,
            operation,
        }),
    ))
}

async fn post_port_change(
    State(state): State<WebState>,
    AxumPath(service): AxumPath<String>,
    Json(request): Json<PortChangeRequest>,
) -> Result<(StatusCode, Json<AcceptedOperation>), ApiError> {
    ensure_live(&state)?;
    if request.port == 0 {
        return Err(ApiError::bad_request("Port must be between 1 and 65535."));
    }
    let manager = state.control.manager().clone();
    let mut permit = state
        .control
        .begin_operation(format!("Change {service} port to {}", request.port), "web")
        .map_err(map_operation_error)?;
    let operation = permit.operation().clone();
    tokio::task::spawn_blocking(move || {
        let result = manager.change_port(&service, request.port_index, request.port, |line| {
            permit.progress(line)
        });
        let _ = permit.finish(result);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedOperation {
            accepted: true,
            operation,
        }),
    ))
}

async fn post_diagnose(
    State(state): State<WebState>,
    AxumPath(service): AxumPath<String>,
) -> Result<Json<Diagnosis>, ApiError> {
    let manager = state.control.manager().clone();
    let diagnosis = tokio::task::spawn_blocking(move || -> Result<Diagnosis> {
        let runtime = manager.runtime(None)?;
        let row = runtime
            .services
            .iter()
            .find(|row| row.service == service)
            .ok_or_else(|| anyhow!("service {service:?} is not configured"))?;
        diagnose(row, &runtime.containers, 100)
    })
    .await
    .map_err(|error| ApiError::internal(anyhow!(error), "Retry the log scan."))?
    .map_err(|error| ApiError::internal(error, "Check Docker logs, then retry."))?;
    Ok(Json(diagnosis))
}

async fn post_scan_all(
    State(state): State<WebState>,
) -> Result<Json<BTreeMap<String, Diagnosis>>, ApiError> {
    let manager = state.control.manager().clone();
    let diagnoses = tokio::task::spawn_blocking(move || {
        let runtime = manager.runtime(None)?;
        scan_all(&runtime.services, &runtime.containers)
    })
    .await
    .map_err(|error| ApiError::internal(anyhow!(error), "Retry the fleet scan."))?
    .map_err(|error| ApiError::internal(error, "Check Docker logs, then retry."))?;
    Ok(Json(diagnoses))
}

async fn get_integrations(
    State(state): State<WebState>,
) -> Result<Json<IntegrationResponse>, ApiError> {
    let plan = current_integration_plan(state.control.clone()).await?;
    Ok(Json(public_integration_plan(plan)))
}

async fn post_apply_integrations(
    State(state): State<WebState>,
) -> Result<(StatusCode, Json<AcceptedOperation>), ApiError> {
    ensure_live(&state)?;
    let manager = state.control.manager().clone();
    let mut permit = state
        .control
        .begin_operation("Apply current integration plan", "web")
        .map_err(map_operation_error)?;
    let operation = permit.operation().clone();
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<String> {
            let runtime = manager.runtime(None)?;
            let plan = integration_plan(&manager, &runtime)?;
            apply_integration_plan(&manager, &plan, |line| permit.progress(line))
                .map(|lines| lines.join("\n"))
        })();
        let _ = permit.finish(result);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedOperation {
            accepted: true,
            operation,
        }),
    ))
}

async fn get_backups(State(state): State<WebState>) -> Result<Json<BackupResponse>, ApiError> {
    let health = state
        .control
        .backup_health()
        .map_err(|error| ApiError::internal(error, "Check backup storage, then retry."))?;
    let store = state.control.backup_store().clone();
    let records = tokio::task::spawn_blocking(move || store.list())
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Reload the backup catalog."))?
        .map_err(|error| ApiError::internal(error, "Check backup storage, then retry."))?;
    Ok(Json(BackupResponse {
        health,
        sensitive: true,
        records,
    }))
}

async fn post_backup(
    State(state): State<WebState>,
) -> Result<(StatusCode, Json<AcceptedOperation>), ApiError> {
    ensure_live(&state)?;
    let control = state.control.clone();
    let manager = state.control.manager().clone();
    let store = state.control.backup_store().clone();
    let mut permit = state
        .control
        .begin_operation("Create manual setup bundle", "web")
        .map_err(map_operation_error)?;
    let operation = permit.operation().clone();
    tokio::task::spawn_blocking(move || {
        let result = store.create_bundle(&manager, "manual", |line| permit.progress(line));
        control.record_backup_result(&result);
        let _ = permit.finish(result);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedOperation {
            accepted: true,
            operation,
        }),
    ))
}

async fn get_backup_download(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let store = state.control.backup_store().clone();
    let record = tokio::task::spawn_blocking(move || store.find(&id))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Reload the backup catalog."))?
        .map_err(|error| ApiError::not_found(format!("{error}")))?;
    let file = tokio::fs::File::open(&record.path)
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry the download."))?;
    let disposition = format!("attachment; filename=\"{}\"", record.file_name);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/gzip")
        .header(CONTENT_LENGTH, record.size_bytes.to_string())
        .header(CONTENT_DISPOSITION, disposition)
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry the download."))
}

async fn post_download_ticket(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<DownloadTicketResponse>, ApiError> {
    let store = state.control.backup_store().clone();
    let record = tokio::task::spawn_blocking(move || store.find(&id))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Reload the backup catalog."))?
        .map_err(|error| ApiError::not_found(format!("{error}")))?;
    cleanup_download_tickets(&state);
    let token = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    let mut tickets = state
        .download_tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner());
    if tickets.len() >= MAX_DOWNLOAD_TICKETS
        && let Some(oldest) = tickets
            .iter()
            .min_by_key(|(_, ticket)| ticket.expires_at)
            .map(|(token, _)| token.clone())
    {
        tickets.remove(&oldest);
    }
    tickets.insert(
        token.clone(),
        DownloadTicket {
            path: record.path,
            file_name: record.file_name,
            size: record.size_bytes,
            expires_at: Instant::now() + DOWNLOAD_TICKET_TTL,
        },
    );
    Ok(Json(DownloadTicketResponse {
        url: format!("/download/{token}"),
        expires_in_seconds: DOWNLOAD_TICKET_TTL.as_secs(),
    }))
}

async fn get_ticket_download(
    State(state): State<WebState>,
    AxumPath(ticket): AxumPath<String>,
) -> Result<Response, ApiError> {
    cleanup_download_tickets(&state);
    let ticket = state
        .download_tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner())
        .remove(&ticket)
        .ok_or_else(|| {
            ApiError::not_found("Download authorization expired or was already used.")
        })?;
    if ticket.expires_at <= Instant::now() {
        return Err(ApiError::not_found(
            "Download authorization expired; request a new download.",
        ));
    }
    let file = tokio::fs::File::open(&ticket.path)
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Request a new download."))?;
    let disposition = format!("attachment; filename=\"{}\"", ticket.file_name);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/gzip")
        .header(CONTENT_LENGTH, ticket.size.to_string())
        .header(CONTENT_DISPOSITION, disposition)
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|error| ApiError::internal(anyhow!(error), "Request a new download."))
}

async fn post_backup_inspect(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RestoreInspectionResponse>, ApiError> {
    let store = state.control.backup_store().clone();
    let record = store
        .find(&id)
        .map_err(|error| ApiError::not_found(format!("{error}")))?;
    let path = record.path.clone();
    let inspection = tokio::task::spawn_blocking(move || store.inspect(&path))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry bundle inspection."))?
        .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    Ok(Json(insert_restore_ticket(
        &state,
        record.path,
        inspection,
        false,
    )?))
}

async fn delete_backup(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<DeleteRequest>,
) -> Result<Json<DeleteResponse>, ApiError> {
    ensure_live(&state)?;
    if request.confirmation != id {
        return Err(ApiError::bad_request(
            "Deletion confirmation must exactly match the bundle identifier.",
        ));
    }
    let store = state.control.backup_store().clone();
    let delete_id = id.clone();
    let permit = state
        .control
        .begin_operation(format!("Delete setup bundle {id}"), "web")
        .map_err(map_operation_error)?;
    tokio::task::spawn_blocking(move || permit.finish(store.delete(&delete_id)))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry the deletion."))?
        .map_err(|error| ApiError::internal(error, "Check backup storage, then retry."))?;
    state.control.record(
        "web",
        ActivityLevel::Warning,
        format!("Deleted setup bundle {id}"),
    );
    Ok(Json(DeleteResponse { deleted: true, id }))
}

async fn post_inspect_upload(
    State(state): State<WebState>,
    mut multipart: Multipart,
) -> Result<Json<RestoreInspectionResponse>, ApiError> {
    let (path, file) = state
        .control
        .backup_store()
        .create_staging_file()
        .map_err(|error| ApiError::internal(error, "Check backup storage, then retry."))?;
    let mut file = tokio::fs::File::from_std(file);
    let mut total = 0_u64;
    let maximum = state.control.backup_store().policy().max_import_bytes;
    let operation =
        async {
            let mut found = false;
            while let Some(mut field) = multipart.next_field().await.map_err(|error| {
                ApiError::bad_request(format!("Invalid multipart upload: {error}"))
            })? {
                if field.name() != Some("bundle") {
                    continue;
                }
                if found {
                    return Err(ApiError::bad_request("Upload exactly one setup bundle."));
                }
                found = true;
                while let Some(chunk) = field
                    .next()
                    .await
                    .transpose()
                    .map_err(|error| ApiError::bad_request(format!("Upload failed: {error}")))?
                {
                    total = total.saturating_add(chunk.len() as u64);
                    if total > maximum {
                        return Err(ApiError::bad_request(format!(
                            "Bundle exceeds the {maximum} byte import limit."
                        )));
                    }
                    file.write_all(&chunk)
                        .await
                        .map_err(|error| ApiError::internal(anyhow!(error), "Retry the upload."))?;
                }
            }
            if !found || total == 0 {
                return Err(ApiError::bad_request(
                    "Choose a non-empty .tar.gz setup bundle.",
                ));
            }
            file.flush()
                .await
                .map_err(|error| ApiError::internal(anyhow!(error), "Retry the upload."))?;
            Ok(())
        }
        .await;
    drop(file);
    if let Err(error) = operation {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    let store = state.control.backup_store().clone();
    let inspect_path = path.clone();
    let inspection = tokio::task::spawn_blocking(move || store.inspect(&inspect_path))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry bundle inspection."))?
        .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    Ok(Json(insert_restore_ticket(&state, path, inspection, true)?))
}

async fn post_inspect_url(
    State(state): State<WebState>,
    Json(request): Json<UrlImportRequest>,
) -> Result<Json<RestoreInspectionResponse>, ApiError> {
    let store = state.control.backup_store().clone();
    let url = request.url;
    let (path, file) = store
        .create_staging_file()
        .map_err(|error| ApiError::internal(error, "Check backup storage, then retry."))?;
    drop(file);
    let download_path = path.clone();
    let maximum = store.policy().max_import_bytes;
    let download =
        tokio::task::spawn_blocking(move || download_https_bundle(&url, &download_path, maximum))
            .await
            .map_err(|error| ApiError::internal(anyhow!(error), "Retry the URL import."))?;
    if let Err(error) = download {
        let _ = fs::remove_file(&path);
        return Err(ApiError::bad_request(format!("{error:#}")));
    }
    let inspect_store = store.clone();
    let inspect_path = path.clone();
    let inspection = tokio::task::spawn_blocking(move || inspect_store.inspect(&inspect_path))
        .await
        .map_err(|error| ApiError::internal(anyhow!(error), "Retry bundle inspection."))?
        .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    Ok(Json(insert_restore_ticket(&state, path, inspection, true)?))
}

async fn post_restore(
    State(state): State<WebState>,
    Json(request): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<AcceptedOperation>), ApiError> {
    ensure_live(&state)?;
    if request.confirmation != "RESTORE" {
        return Err(ApiError::bad_request(
            "Type RESTORE exactly to authorize a stopped-stack configuration restore.",
        ));
    }
    cleanup_restore_tickets(&state);
    let ticket = state
        .tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner())
        .get(&request.ticket)
        .cloned()
        .ok_or_else(|| {
            ApiError::not_found("Restore inspection expired or was already consumed.")
        })?;
    if ticket.expires_at <= Instant::now() {
        return Err(ApiError::not_found(
            "Restore inspection expired; inspect the bundle again.",
        ));
    }
    let manager = state.control.manager().clone();
    let store = state.control.backup_store().clone();
    let mut permit = state
        .control
        .begin_operation("Restore setup bundle", "web")
        .map_err(map_operation_error)?;
    let operation = permit.operation().clone();
    state
        .tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner())
        .remove(&request.ticket);
    tokio::task::spawn_blocking(move || {
        let result = store.restore(&manager, &ticket.path, |line| permit.progress(line));
        if ticket.temporary {
            let _ = fs::remove_file(&ticket.path);
        }
        let _ = permit.finish(result);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedOperation {
            accepted: true,
            operation,
        }),
    ))
}

async fn require_auth(
    State(state): State<WebState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let supplied = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::as_bytes);
    let authorized = supplied.is_some_and(|value| {
        value.len() == state.token.len() && bool::from(value.ct_eq(state.token.as_slice()))
    });
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "Bearer realm=\"docker-services\"")],
            Json(json!({
                "error": "A valid web access token is required.",
                "recovery": format!("Read the private token from {}.", state.token_hint),
            })),
        )
            .into_response();
    }
    next.run(request).await
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let values = [
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' blob:; font-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Permissions-Policy",
            "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
        ),
        ("Cross-Origin-Opener-Policy", "same-origin"),
        ("Cross-Origin-Resource-Policy", "same-origin"),
    ];
    for (name, value) in values {
        if let (Ok(name), Ok(value)) = (
            name.parse::<axum::http::HeaderName>(),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    response
}

async fn snapshot_loop(state: WebState) {
    let mut previous_cpu = None;
    loop {
        match build_realtime_snapshot(state.clone(), previous_cpu).await {
            Ok((snapshot, next_cpu)) => {
                previous_cpu = next_cpu;
                *state
                    .latest
                    .write()
                    .unwrap_or_else(|item| item.into_inner()) = Some(snapshot.clone());
                let _ = state.events.send(WireEvent {
                    kind: "snapshot".to_string(),
                    data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
                });
            }
            Err(error) => {
                let _ = state.events.send(WireEvent {
                    kind: "runtime_error".to_string(),
                    data: json!({
                        "error": sanitize_error(&format!("{error:#}")),
                        "recovery": "Check Docker and Compose; the last good state remains visible."
                    }),
                });
            }
        }
        tokio::time::sleep(state.refresh_interval).await;
    }
}

async fn activity_loop(state: WebState) {
    let mut last_event = 0_u64;
    loop {
        for event in state.control.events_since(last_event) {
            last_event = last_event.max(event.id);
            let _ = state.events.send(WireEvent {
                kind: "activity".to_string(),
                data: serde_json::to_value(event).unwrap_or(Value::Null),
            });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn build_realtime_snapshot(
    state: WebState,
    previous_cpu: Option<CpuTimes>,
) -> Result<(RealtimeSnapshot, Option<CpuTimes>)> {
    let control = state.control.clone();
    let demo = state.demo;
    tokio::task::spawn_blocking(move || {
        let (snapshot, next_cpu) = if demo {
            (demo_snapshot(), previous_cpu)
        } else {
            control
                .manager()
                .snapshot_with_cpu(false, false, previous_cpu)?
        };
        Ok((
            RealtimeSnapshot {
                sampled_at: Utc::now().to_rfc3339(),
                snapshot,
                active_operation: control.status().active_operation,
            },
            next_cpu,
        ))
    })
    .await
    .context("snapshot worker stopped")?
}

async fn current_integration_plan(control: ControlPlane) -> Result<IntegrationPlan, ApiError> {
    tokio::task::spawn_blocking(move || {
        let runtime = control.manager().runtime(None)?;
        integration_plan(control.manager(), &runtime)
    })
    .await
    .map_err(|error| ApiError::internal(anyhow!(error), "Retry integration discovery."))?
    .map_err(|error| ApiError::internal(error, "Check service APIs, then retry."))
}

fn public_integration_plan(plan: IntegrationPlan) -> IntegrationResponse {
    IntegrationResponse {
        generated_at: Utc::now().to_rfc3339(),
        pending: plan
            .changes
            .into_iter()
            .map(|change| PublicIntegrationChange {
                target: change.target,
                service: change.service,
                kind: change.kind,
                action: change.action,
                summary: change.summary,
                manual: change.manual,
            })
            .collect(),
        blocked: plan.blocked,
    }
}

fn parse_action(value: &str) -> Result<ServiceAction, ApiError> {
    match value.to_ascii_lowercase().as_str() {
        "enable" => Ok(ServiceAction::Enable),
        "disable" => Ok(ServiceAction::Disable),
        "restart" => Ok(ServiceAction::Restart),
        _ => Err(ApiError::bad_request(
            "Action must be enable, disable, or restart.",
        )),
    }
}

fn map_operation_error(error: anyhow::Error) -> ApiError {
    if is_busy_error(&error) {
        ApiError::conflict(&error)
    } else {
        ApiError::internal(error, "Check the operation lock and retry.")
    }
}

fn ensure_live(state: &WebState) -> Result<(), ApiError> {
    if state.demo {
        Err(ApiError {
            status: StatusCode::CONFLICT,
            message: "Demo mode is read-only.".to_string(),
            recovery: Some("Start without --demo to manage the live stack.".to_string()),
        })
    } else {
        Ok(())
    }
}

fn insert_restore_ticket(
    state: &WebState,
    path: PathBuf,
    inspection: BundleInspection,
    temporary: bool,
) -> Result<RestoreInspectionResponse, ApiError> {
    cleanup_restore_tickets(state);
    let mut tickets = state
        .tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner());
    if tickets.len() >= MAX_RESTORE_TICKETS {
        return Err(ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "Too many restore inspections are open.".to_string(),
            recovery: Some("Wait for an inspection to expire, then retry.".to_string()),
        });
    }
    let token = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    tickets.insert(
        token.clone(),
        RestoreTicket {
            path,
            expires_at: Instant::now() + RESTORE_TICKET_TTL,
            temporary,
        },
    );
    Ok(RestoreInspectionResponse {
        ticket: token,
        expires_in_seconds: RESTORE_TICKET_TTL.as_secs(),
        inspection,
    })
}

fn cleanup_restore_tickets(state: &WebState) {
    let mut tickets = state
        .tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner());
    let now = Instant::now();
    let expired = tickets
        .iter()
        .filter(|(_, ticket)| ticket.expires_at <= now)
        .map(|(token, ticket)| (token.clone(), ticket.clone()))
        .collect::<Vec<_>>();
    for (token, ticket) in expired {
        tickets.remove(&token);
        if ticket.temporary {
            let _ = fs::remove_file(ticket.path);
        }
    }
}

fn cleanup_download_tickets(state: &WebState) {
    let now = Instant::now();
    state
        .download_tickets
        .lock()
        .unwrap_or_else(|item| item.into_inner())
        .retain(|_, ticket| ticket.expires_at > now);
}

fn wire_to_sse(wire: &WireEvent) -> Event {
    Event::default()
        .event(&wire.kind)
        .data(serde_json::to_string(&wire.data).unwrap_or_else(|_| "null".to_string()))
}

fn read_or_create_token(path: &Path) -> Result<Vec<u8>> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            bail!("web token path {} must not be a symlink", path.display());
        }
        let token = fs::read_to_string(path)
            .with_context(|| format!("could not read web token {}", path.display()))?;
        let token = token.trim().as_bytes().to_vec();
        validate_token(&token)?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        return Ok(token);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut random = [0_u8; 32];
    File::open("/dev/urandom")
        .context("secure system randomness is unavailable")?
        .read_exact(&mut random)?;
    let token = URL_SAFE_NO_PAD.encode(random);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("could not create web token {}", path.display()))?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(token.into_bytes())
}

fn validate_token(token: &[u8]) -> Result<()> {
    if token.len() < 32 || token.len() > 512 || token.iter().any(u8::is_ascii_whitespace) {
        bail!("web token must be 32-512 non-whitespace bytes");
    }
    Ok(())
}

pub(crate) fn download_https_bundle(url: &str, destination: &Path, maximum: u64) -> Result<()> {
    let url = Url::parse(url).context("setup URL is invalid")?;
    let pinned_addresses = validate_remote_url(&url)?;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(60))
        .resolver(move |_netloc: &str| Ok(pinned_addresses.clone()))
        .build();
    let response = agent
        .get(url.as_str())
        .set("Accept", "application/gzip, application/octet-stream")
        .call()
        .map_err(|error| anyhow!("setup URL request failed: {error}"))?;
    if !(200..300).contains(&response.status()) {
        bail!("setup URL returned HTTP {}", response.status());
    }
    if let Some(length) = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        && length > maximum
    {
        bail!("remote bundle exceeds the import size limit");
    }
    let mut input = response.into_reader().take(maximum.saturating_add(1));
    let mut output = OpenOptions::new()
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(destination)?;
    let copied = std::io::copy(&mut input, &mut output)?;
    if copied > maximum {
        bail!("remote bundle exceeds the import size limit");
    }
    output.sync_all()?;
    Ok(())
}

fn validate_remote_url(url: &Url) -> Result<Vec<SocketAddr>> {
    if url.scheme() != "https" {
        bail!("setup URLs must use HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("setup URLs must not contain embedded credentials");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("setup URL has no host"))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("could not resolve setup host {host}"))?
        .collect::<Vec<_>>();
    let ips = addresses
        .iter()
        .map(|address| address.ip())
        .collect::<Vec<_>>();
    if ips.is_empty() {
        bail!("setup host did not resolve");
    }
    if ips.iter().any(|address| !is_public_address(*address)) {
        bail!("setup URL resolves to a private, local, or reserved address");
    }
    Ok(addresses)
}

fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_unspecified()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0)
        || (a == 198 && (b == 18 || b == 19)))
        && !(a == 198 && b == 51 && c == 100)
        && !(a == 203 && b == 0 && c == 113)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    !(address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        || segments[0] == 0x2002
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0))
        && address.to_ipv4_mapped().map(is_public_ipv4).unwrap_or(true)
}

fn sanitize_error(value: &str) -> String {
    let mut output = value.to_string();
    for marker in ["api_key=", "apikey=", "token=", "password="] {
        let mut offset = 0;
        while offset < output.len() {
            let Some(relative) = output[offset..].to_ascii_lowercase().find(marker) else {
                break;
            };
            let start = offset + relative + marker.len();
            let end = output[start..]
                .find(|character: char| character.is_whitespace() || character == '&')
                .map(|offset| start + offset)
                .unwrap_or(output.len());
            output.replace_range(start..end, "[REDACTED]");
            offset = start + "[REDACTED]".len();
        }
    }
    output
}

fn demo_snapshot() -> Snapshot {
    let services = vec![
        demo_service(
            "jellyfin",
            "Media server and streaming",
            "running (healthy)",
            true,
            false,
            8096,
            0,
        ),
        demo_service(
            "sonarr",
            "TV series library manager",
            "running (healthy)",
            true,
            false,
            8989,
            0,
        ),
        demo_service(
            "radarr",
            "Movie library manager",
            "running (healthy)",
            true,
            false,
            7878,
            0,
        ),
        demo_service(
            "prowlarr",
            "Indexer manager and proxy",
            "running (healthy)",
            true,
            false,
            9696,
            0,
        ),
        demo_service(
            "sabnzbd",
            "Usenet downloader",
            "running (healthy)",
            true,
            false,
            8080,
            0,
        ),
        demo_service(
            "bazarr",
            "Subtitle manager",
            "running",
            true,
            false,
            6767,
            0,
        ),
        demo_service(
            "seerr",
            "Media request and discovery manager",
            "restarting",
            false,
            false,
            5055,
            1,
        ),
        demo_service(
            "jellyseerr",
            "Jellyfin media request manager",
            "disabled",
            false,
            true,
            5056,
            0,
        ),
        demo_service(
            "spotweb",
            "Spotweb Usenet indexer and Newznab API",
            "running (healthy)",
            true,
            false,
            8085,
            0,
        ),
        demo_service(
            "tdarr",
            "Media transcoding and processing",
            "not created",
            false,
            true,
            8265,
            0,
        ),
        demo_service(
            "tracearr",
            "Playback monitoring and analytics",
            "running",
            true,
            false,
            3000,
            0,
        ),
        demo_service(
            "audiobookshelf",
            "Audiobook and podcast server",
            "not created",
            false,
            true,
            13378,
            0,
        ),
    ];
    Snapshot {
        host: "192.168.1.50".to_string(),
        compose_file: "/root/docker-compose.yml".to_string(),
        project: Some("root".to_string()),
        host_stats: HostStats {
            ip: "192.168.1.50".to_string(),
            cpu_cores: Some(4),
            cpu_percent: Some(18.7),
            load_1m: Some(0.72),
            memory_total: Some(16 * 1024_u64.pow(3)),
            memory_available: Some(8_230_000_000),
            memory_used: Some(8_949_869_184),
            disk_total: Some(20_294_205_440),
            disk_free: Some(1_610_612_736),
            uptime_seconds: Some(1_124_340),
        },
        log_scan: crate::model::LogScanSummary {
            scanned_services: 12,
            issue_count: 1,
        },
        integrations: Default::default(),
        dependency_graph: BTreeMap::from([(
            "spotweb".to_string(),
            DependencySummary {
                depends_on: BTreeMap::from([(
                    "spotweb-mysql".to_string(),
                    "service_healthy".to_string(),
                )]),
                healthcheck: Some(json!({"test":["CMD","curl","-f","http://localhost"]})),
            },
        )]),
        services,
    }
}

fn demo_service(
    service: &str,
    purpose: &str,
    status: &str,
    running: bool,
    disabled: bool,
    port: u16,
    issue_count: usize,
) -> ServiceRow {
    ServiceRow {
        service: service.to_string(),
        purpose: purpose.to_string(),
        image: format!("lscr.io/linuxserver/{service}:latest"),
        status: status.to_string(),
        running,
        disabled,
        configured: true,
        container: if status == "not created" {
            None
        } else {
            Some(service.to_string())
        },
        ports: vec![port.to_string()],
        port_specs: vec![PortSpec {
            published: port.to_string(),
            target: port.to_string(),
            protocol: "tcp".to_string(),
        }],
        urls: vec![format!("http://192.168.1.50:{port}")],
        mounts: 2,
        dependencies_ready: true,
        healthcheck_configured: status.contains("healthy"),
        issue_count,
        ..ServiceRow::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::{BackupPolicy, BackupStore};
    use crate::compose::Manager;
    use tempfile::tempdir;
    use tower::ServiceExt;

    #[test]
    fn token_is_private_and_reused() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("token");
        let first = read_or_create_token(&path).unwrap();
        let second = read_or_create_token(&path).unwrap();
        assert_eq!(first, second);
        assert!(first.len() >= 32);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn remote_url_filter_rejects_local_and_plain_http() {
        assert!(
            validate_remote_url(&Url::parse("http://example.com/setup.tar.gz").unwrap()).is_err()
        );
        assert!(
            validate_remote_url(&Url::parse("https://127.0.0.1/setup.tar.gz").unwrap()).is_err()
        );
        assert!(!is_public_address(IpAddr::V4(Ipv4Addr::new(
            192, 168, 1, 4
        ))));
        assert!(!is_public_address(IpAddr::V4(Ipv4Addr::new(
            198, 51, 100, 8
        ))));
        assert!(!is_public_address(IpAddr::V4(Ipv4Addr::new(
            203, 0, 113, 8
        ))));
        assert!(!is_public_address(IpAddr::V6(
            "2001:db8::8".parse().unwrap()
        )));
        assert!(!is_public_address(IpAddr::V6(
            "64:ff9b::7f00:1".parse().unwrap()
        )));
        assert!(is_public_address(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn error_sanitizer_removes_query_secrets() {
        assert_eq!(
            sanitize_error("failed https://host/?apikey=secret&mode=1"),
            "failed https://host/?apikey=[REDACTED]&mode=1"
        );
    }

    #[tokio::test]
    async fn protected_api_requires_bearer_token_and_static_shell_has_csp() {
        let directory = tempdir().unwrap();
        let manager = Manager::new(directory.path().join("compose.yml"), "host".into());
        let control = ControlPlane::new(
            manager,
            BackupStore::new(BackupPolicy::new(
                directory.path().join("backups"),
                Duration::from_secs(3600),
                4,
            )),
        );
        let (events, _) = broadcast::channel(8);
        let state = WebState {
            control,
            token: Arc::new(b"test-token-with-at-least-32-bytes".to_vec()),
            token_hint: directory.path().join("token").display().to_string(),
            refresh_interval: Duration::from_secs(5),
            demo: true,
            latest: Arc::new(RwLock::new(None)),
            events,
            tickets: Arc::new(Mutex::new(HashMap::new())),
            download_tickets: Arc::new(Mutex::new(HashMap::new())),
        };

        let unauthorized = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert!(
            unauthorized
                .headers()
                .contains_key("content-security-policy")
        );

        let shell = router(state)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(shell.status(), StatusCode::OK);
        assert!(shell.headers().contains_key("content-security-policy"));
        assert_eq!(
            shell
                .headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
    }
}
