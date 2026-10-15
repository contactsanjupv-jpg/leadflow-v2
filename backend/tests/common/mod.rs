//! Shared helpers for HTTP-layer integration tests.
//!
//! These tests drive the REAL axum router (real extractors, real auth, real SQL) against a
//! real PostgreSQL database reached through the least-privilege `xflow_app` role, so RLS is
//! active exactly as in production. `DATABASE_URL` must be set and migrations applied.
#![allow(dead_code)]

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use xflow_backend::extraction::{
    ChainExtractor, DocumentExtractor, PdfTextExtractor, PlainTextExtractor,
};
use xflow_backend::http::{
    build_router,
    state::{AppState, BillingSettings},
};
use xflow_backend::storage::LocalFsStorage;

pub const WEBHOOK_SECRET: &str = "test-webhook-secret";

pub struct TestApp {
    pub router: Router,
    pub pool: PgPool,
}

pub struct TestOrg {
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub token: String,
}

pub async fn test_app() -> TestApp {
    let url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run integration tests (see BUILD_NOTES.md)");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect to test database");
    let state = AppState {
        pool: pool.clone(),
        session_ttl_hours: 24,
        storage: Arc::new(LocalFsStorage::new(
            std::env::temp_dir().join(format!("xflow-test-{}", Uuid::new_v4())),
        )),
        extractor: Arc::new(ChainExtractor {
            extractors: vec![
                Arc::new(PdfTextExtractor::new("pdftotext")) as Arc<dyn DocumentExtractor>,
                Arc::new(PlainTextExtractor),
            ],
        }),
        billing: BillingSettings {
            webhook_secret: Some(WEBHOOK_SECRET.to_string()),
            signature_tolerance_secs: 300,
            enforce_entitlements: false,
        },
        max_upload_bytes: 1024 * 1024,
    };
    let router = build_router(state);
    TestApp { router, pool }
}

impl TestApp {
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let resp = self.router.clone().oneshot(req).await.expect("router call");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 10 * 1024 * 1024).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    pub async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, Some(token), None).await
    }

    pub async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(token), Some(body)).await
    }

    /// Sign up a brand-new organization + OWNER user through the real signup endpoint.
    pub async fn signup_org(&self, label: &str) -> TestOrg {
        let email = format!("{}-{}@test.invalid", label, Uuid::new_v4());
        let (status, body) = self
            .call(Method::POST, "/api/v1/auth/signup", None, Some(json!({ "organization_name": format!("{label} org"), "email": email, "password": "a-sufficiently-long-password" })))
            .await;
        assert_eq!(status, StatusCode::OK, "signup failed: {body}");
        TestOrg {
            organization_id: body["organization_id"].as_str().unwrap().parse().unwrap(),
            user_id: body["user_id"].as_str().unwrap().parse().unwrap(),
            token: body["session_token"].as_str().unwrap().to_string(),
        }
    }

    /// Seed the spec section 55 scenario ($700 discrepancy) for `org` through the tenant-scoped
    /// transaction path as the least-privilege role (RLS WITH CHECK applies to the inserts).
    pub async fn seed_worked_example(&self, org: &TestOrg) -> SeededInvoice {
        let vendor_id = Uuid::new_v4();
        let contract_id = Uuid::new_v4();
        let rate_id = Uuid::new_v4();
        let invoice_id = Uuid::new_v4();
        let org_id = org.organization_id;
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&self.pool, org_id)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO vendors (id, organization_id, name) VALUES ($1, $2, 'Test Vendor')",
        )
        .bind(vendor_id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO contracts (id, organization_id, vendor_id, name) VALUES ($1, $2, $3, 'Test Contract')").bind(contract_id).bind(org_id).bind(vendor_id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO contract_rates (id, organization_id, contract_id, vendor_id, waste_stream, unit, unit_rate, effective_from) VALUES ($1, $2, $3, $4, 'F003', 'drum', 400.00, '2026-01-01')")
            .bind(rate_id).bind(org_id).bind(contract_id).bind(vendor_id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, description, amount) VALUES ($1, 'TRANSPORT', 'Transportation', 500.00)").bind(rate_id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO invoices (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total) VALUES ($1, $2, $3, 'INV-1001', '2026-03-15', '2026-03-10', 'F003', 5200.00)")
            .bind(invoice_id).bind(org_id).bind(vendor_id).execute(&mut *tx).await.unwrap();
        for (desc, fee, qty, unit, price, total) in [
            ("Drum disposal", None, 10, "drum", 450, 4500),
            ("Transportation", Some("TRANSPORT"), 1, "flat", 500, 500),
            ("Environmental fee", Some("ENV_FEE"), 1, "flat", 200, 200),
        ] {
            sqlx::query("INSERT INTO invoice_line_items (invoice_id, organization_id, description, fee_code, quantity, unit, unit_price, line_total) VALUES ($1, $2, $3, $4, $5::numeric, $6, $7::numeric, $8::numeric)")
                .bind(invoice_id).bind(org_id).bind(desc).bind(fee).bind(qty).bind(unit).bind(price).bind(total).execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        SeededInvoice {
            vendor_id,
            invoice_id,
        }
    }
}

pub struct SeededInvoice {
    pub vendor_id: Uuid,
    pub invoice_id: Uuid,
}

pub fn have_pdftotext() -> bool {
    std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_ok()
}

impl TestApp {
    /// Like `call` but returns raw bytes + headers (for PDFs / downloads).
    pub async fn call_raw(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if let Some(c) = content_type {
            b = b.header("content-type", c);
        }
        let resp = self
            .router
            .clone()
            .oneshot(b.body(Body::from(body)).unwrap())
            .await
            .expect("router call");
        let (parts, body) = resp.into_parts();
        (
            parts.status,
            parts.headers,
            to_bytes(body, 20 * 1024 * 1024).await.unwrap().to_vec(),
        )
    }
}

/// Paddle-style signature header for `body` at unix time `ts`.
pub fn sign(secret: &str, ts: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{ts}:{body}").as_bytes());
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("ts={ts};h1={hex}")
}

/// POST a (pre-signed or unsigned) webhook body to `app`.
pub async fn webhook(app: &TestApp, body: &str, sig: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/v1/billing/paddle/webhook")
        .header("content-type", "application/json");
    if let Some(s) = sig {
        req = req.header("paddle-signature", s);
    }
    let resp = app
        .router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let st = resp.status();
    let b = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

/// A realistic Paddle Billing `subscription.*` notification. `sig_for` is the organization the
/// backend-issued signature is computed for (normally the same organization).
pub fn paddle_event(
    event_type: &str,
    event_id: &str,
    sub_id: &str,
    status: &str,
    occurred_at: &str,
    org: &TestOrg,
    sig_for: Option<&TestOrg>,
) -> String {
    let mut custom = json!({ "organization_id": org.organization_id });
    if let Some(o) = sig_for {
        custom["organization_sig"] = json!(xflow_backend::billing::checkout_signature(
            WEBHOOK_SECRET,
            o.organization_id
        ));
    }
    json!({
        "event_id": event_id, "event_type": event_type, "occurred_at": occurred_at, "notification_id": format!("ntf_{event_id}"),
        "data": {
            "id": sub_id, "status": status, "customer_id": "ctm_01test", "currency_code": "USD", "collection_mode": "automatic",
            "items": [{ "status": status, "quantity": 1, "price": { "id": "pri_01test_monthly", "product_id": "pro_01test" } }],
            "current_billing_period": { "starts_at": "2026-10-01T00:00:00Z", "ends_at": "2026-11-01T00:00:00Z" },
            "custom_data": custom,
        }
    }).to_string()
}

pub async fn send_signed(app: &TestApp, body: &str) -> (StatusCode, Value) {
    webhook(
        app,
        body,
        Some(sign(WEBHOOK_SECRET, chrono::Utc::now().timestamp(), body)),
    )
    .await
}
