pub mod admin;
pub mod authed_user;
pub mod data;
pub mod documents;
pub mod errors;
pub mod handlers;
pub mod state;
pub mod util;
pub mod workflow;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderValue, Method};
use axum::routing::{get, post, put};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use state::AppState;

pub fn build_router(state: AppState) -> Router {
    build_router_with_cors(state, &[])
}

/// `cors_origins` empty => no CORS headers at all (same-origin / server-to-server only).
pub fn build_router_with_cors(state: AppState, cors_origins: &[String]) -> Router {
    let upload_limit = state.max_upload_bytes + 1024;
    let upload = Router::new()
        .route("/api/v1/documents", post(documents::upload_document))
        .layer(DefaultBodyLimit::max(upload_limit));

    let mut app = Router::new()
        .route("/api/v1/health", get(handlers::health))
        .route("/api/v1/auth/signup", post(handlers::signup))
        .route("/api/v1/auth/login", post(handlers::login))
        .route("/api/v1/auth/logout", post(handlers::logout))
        .route("/api/v1/me", get(admin::me))
        .route(
            "/api/v1/members",
            get(admin::list_members).post(admin::add_member),
        )
        .route("/api/v1/audit", get(admin::list_audit))
        .route("/api/v1/notifications", get(admin::list_notifications))
        .route(
            "/api/v1/notifications/:id/read",
            post(admin::mark_notification_read),
        )
        .route(
            "/api/v1/vendors",
            get(data::list_vendors).post(data::create_vendor),
        )
        .route(
            "/api/v1/contracts",
            get(data::list_contracts).post(data::create_contract),
        )
        .route("/api/v1/contracts/:id", get(data::get_contract))
        .route("/api/v1/contracts/:id/rates", post(data::create_rate))
        .route("/api/v1/rates/:id/end", put(data::end_rate))
        .route(
            "/api/v1/invoices",
            get(data::list_invoices).post(data::create_invoice),
        )
        .route("/api/v1/invoices/:id", get(data::get_invoice))
        .route(
            "/api/v1/invoices/:id/manifest",
            put(data::link_invoice_manifest),
        )
        .route(
            "/api/v1/manifests",
            get(data::list_manifests).post(data::create_manifest),
        )
        .route("/api/v1/manifests/:id", get(data::get_manifest))
        .route(
            "/api/v1/weight-tickets",
            get(data::list_weight_tickets).post(data::create_weight_ticket),
        )
        .route("/api/v1/weight-tickets/:id", get(data::get_weight_ticket))
        .route("/api/v1/documents", get(documents::list_documents))
        .route("/api/v1/documents/:id", get(documents::get_document))
        .route(
            "/api/v1/documents/:id/resolve",
            post(documents::resolve_document),
        )
        .route(
            "/api/v1/documents/:id/download",
            get(documents::download_document),
        )
        .route(
            "/api/v1/reconciliations",
            get(handlers::list_reconciliations),
        )
        .route(
            "/api/v1/reconciliations/run",
            post(handlers::run_reconciliation),
        )
        .route(
            "/api/v1/reconciliations/:id",
            get(handlers::get_reconciliation),
        )
        .route("/api/v1/dashboard", get(handlers::get_dashboard))
        .route("/api/v1/discrepancies", get(workflow::list_discrepancies))
        .route("/api/v1/discrepancies/:id", get(workflow::get_discrepancy))
        .route(
            "/api/v1/discrepancies/:id/review",
            post(workflow::review_discrepancy),
        )
        .route(
            "/api/v1/discrepancies/:id/evidence",
            get(workflow::latest_evidence).post(workflow::generate_evidence),
        )
        .route(
            "/api/v1/discrepancies/:id/evidence/pdf",
            get(workflow::evidence_pdf),
        )
        .route(
            "/api/v1/disputes",
            get(workflow::list_disputes).post(workflow::create_dispute),
        )
        .route("/api/v1/disputes/:id", get(workflow::get_dispute))
        .route(
            "/api/v1/disputes/:id/transition",
            post(workflow::transition_dispute),
        )
        .route("/api/v1/recovery", get(workflow::recovery_summary))
        .route("/api/v1/billing/status", get(admin::billing_status))
        .route(
            "/api/v1/billing/checkout-context",
            get(admin::checkout_context),
        )
        .route(
            "/api/v1/billing/paddle/webhook",
            post(admin::paddle_webhook),
        )
        .route("/api/v1/monitoring/run", post(admin::enqueue_monitoring))
        .route("/api/v1/monitoring/jobs", get(admin::list_monitoring_jobs))
        .merge(upload)
        // Default JSON/body cap for every other route (uploads have their own limit above).
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(TraceLayer::new_for_http());

    if !cors_origins.is_empty() {
        let origins: Vec<HeaderValue> =
            cors_origins.iter().filter_map(|o| o.parse().ok()).collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::POST, Method::PUT])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        );
    }
    app.with_state(state)
}
