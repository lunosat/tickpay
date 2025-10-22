use std::{net::SocketAddr, time::Duration};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::{net::TcpListener, time::sleep};
use tower_http::{cors::{Any, CorsLayer}, trace::TraceLayer};
use tracing::{error, info, Level};
use uuid::Uuid;

// ===== Models =====

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InvoiceStatus {
    Created,
    Paid,
    Failed,
    Canceled,
    Expired,
    Chargeback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Invoice {
    id: Uuid,
    amount: u64,
    currency: String,
    status: InvoiceStatus,
    webhook_url: String,
    created_at: DateTime<Utc>,
    metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateInvoice {
    amount: u64,
    #[serde(default = "default_currency")] 
    currency: String,
    webhook_url: String,

    /// Milliseconds to wait before emitting the webhook.
    #[serde(default = "default_emit_after_ms")] 
    emit_after_ms: u64,

    /// Final status to emit in the webhook.
    emit_status: EmitStatus,

    /// Arbitrary extra fields you want echoed back.
    #[serde(default)]
    metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")] 
enum EmitStatus {
    Paid,
    Failed,
    Canceled,
    Expired,
    Chargeback,
}

#[derive(Debug, Serialize)]
struct CreateInvoiceResponse {
    id: Uuid,
    status: InvoiceStatus,
    amount: u64,
    currency: String,
    created_at: DateTime<Utc>,
    webhook_url: String,
    checkout_url: String,
    metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct WebhookPayload {
    event: &'static str,             // e.g. "invoice.updated"
    id: Uuid,
    status: InvoiceStatus,
    amount: u64,
    currency: String,
    emitted_at: DateTime<Utc>,
    metadata: serde_json::Value,
}

fn default_currency() -> String { "BRL".to_string() }
fn default_emit_after_ms() -> u64 { 5_000 }

// ===== State =====

#[derive(Clone)]
struct AppState {
    invoices: std::sync::Arc<DashMap<Uuid, Invoice>>, 
    idempotency: std::sync::Arc<DashMap<String, Uuid>>, 
    client: Client,
    webhook_secret: String,
}

// ===== Helpers =====

fn hmac_hex(secret: &str, body: &str) -> String {
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn map_emit_status(s: &EmitStatus) -> InvoiceStatus {
    match s {
        EmitStatus::Paid => InvoiceStatus::Paid,
        EmitStatus::Failed => InvoiceStatus::Failed,
        EmitStatus::Canceled => InvoiceStatus::Canceled,
        EmitStatus::Expired => InvoiceStatus::Expired,
        EmitStatus::Chargeback => InvoiceStatus::Chargeback,
    }
}

// ===== Routes =====

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let state = AppState {
        invoices: std::sync::Arc::new(DashMap::new()),
        idempotency: std::sync::Arc::new(DashMap::new()),
        client: Client::new(),
        webhook_secret: std::env::var("ACQ_WEBHOOK_SECRET").unwrap_or_else(|_| "dev_secret".into()),
    };

    let cors = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_origin(Any);

    let app = Router::new()
        .route("/invoices", post(create_invoice))
        .route("/invoices/:id", get(get_invoice))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let port: u16 = std::env::var("PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await.expect("bind");
    info!(addr = %listener.local_addr().unwrap(), "fake-acquirer listening");
    axum::serve(listener, app)
        .await
        .expect("server");
}

async fn create_invoice(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateInvoice>,
) -> impl IntoResponse {
    // Idempotency (optional)
    if let Some(key) = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()).map(|s| s.to_string()) {
        if let Some(existing_id) = state.idempotency.get(&key).map(|e| *e.value()) {
            if let Some(inv) = state.invoices.get(&existing_id) {
                let resp = CreateInvoiceResponse {
                    id: inv.id,
                    status: inv.status.clone(),
                    amount: inv.amount,
                    currency: inv.currency.clone(),
                    created_at: inv.created_at,
                    webhook_url: inv.webhook_url.clone(),
                    checkout_url: format!("https://checkout.local/invoice/{}", inv.id),
                    metadata: inv.metadata.clone(),
                };
                return (StatusCode::OK, Json(resp));
            }
        }
    }

    let id = Uuid::new_v4();
    let now = Utc::now();

    let invoice = Invoice {
        id,
        amount: payload.amount,
        currency: payload.currency.clone(),
        status: InvoiceStatus::Created,
        webhook_url: payload.webhook_url.clone(),
        created_at: now,
        metadata: payload.metadata.clone(),
    };

    state.invoices.insert(id, invoice.clone());

    // Track idempotency
    if let Some(key) = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()).map(|s| s.to_string()) {
        state.idempotency.insert(key, id);
    }

    // Schedule webhook
    let delay = Duration::from_millis(payload.emit_after_ms);
    let client = state.client.clone();
    let secret = state.webhook_secret.clone();
    let invoices = state.invoices.clone();
    let final_status = map_emit_status(&payload.emit_status);
    let webhook_url = payload.webhook_url.clone();

    tokio::spawn(async move {
        sleep(delay).await;
        let mut inv = match invoices.get(&id) {
            Some(v) => v.clone(),
            None => {
                error!(%id, "invoice not found when emitting webhook");
                return;
            }
        };
        inv.status = final_status.clone();
        invoices.insert(id, inv.clone());

        let body = WebhookPayload {
            event: "invoice.updated",
            id: inv.id,
            status: inv.status.clone(),
            amount: inv.amount,
            currency: inv.currency.clone(),
            emitted_at: Utc::now(),
            metadata: inv.metadata.clone(),
        };

        let json_body = match serde_json::to_string(&body) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "serialize webhook body");
                return;
            }
        };

        let sig = hmac_hex(&secret, &json_body);

        info!(url = %webhook_url, status = ?body.status, "emitting webhook");

        let res = client
            .post(&webhook_url)
            .header("Content-Type", "application/json")
            .header("X-Event", "invoice.updated")
            .header("X-Signature", sig)
            .body(json_body)
            .send()
            .await;

        match res {
            Ok(r) => info!(status = %r.status(), "webhook delivered"),
            Err(e) => error!(error = %e, "webhook delivery failed"),
        }
    });

    let resp = CreateInvoiceResponse {
        id,
        status: InvoiceStatus::Created,
        amount: payload.amount,
        currency: payload.currency,
        created_at: now,
        webhook_url: payload.webhook_url,
        checkout_url: format!("https://checkout.local/invoice/{}", id),
        metadata: payload.metadata,
    };

    (StatusCode::CREATED, Json(resp))
}

async fn get_invoice(State(state): State<AppState>, Path(id): Path<Uuid>) -> impl IntoResponse {
    match state.invoices.get(&id) {
        Some(inv) => (StatusCode::OK, Json(inv.clone())).into_response(),
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "error": "invoice_not_found",
            "message": format!("Invoice {} not found", id)
        }))).into_response(),
    }
}