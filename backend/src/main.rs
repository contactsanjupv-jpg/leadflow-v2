use std::sync::Arc;
use std::time::Duration;

use xflow_backend::config::Config;
use xflow_backend::extraction::{
    ChainExtractor, DocumentExtractor, PdfTextExtractor, PlainTextExtractor,
};
use xflow_backend::http::build_router_with_cors;
use xflow_backend::http::state::{AppState, BillingSettings};
use xflow_backend::storage::LocalFsStorage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await?;

    // Migrations are deliberately NOT run here (spec section 30): this pool connects as the
    // least-privilege `xflow_app` role, which correctly cannot CREATE/ALTER tables.
    // Run migrations via scripts/migrate.sh with a privileged role as a separate deploy step.

    if config.paddle_webhook_secret.is_none() {
        tracing::warn!("PADDLE_WEBHOOK_SECRET is not set: the Paddle webhook endpoint will reject every request (fail closed)");
    }
    if config.worker_enabled {
        xflow_backend::workers::spawn(
            pool.clone(),
            Duration::from_secs(config.worker_poll_secs),
            Duration::from_secs(config.monitor_interval_secs),
            config.billing_enforcement,
        );
    }

    let mut extractors: Vec<Arc<dyn DocumentExtractor>> = Vec::new();
    if config.pdf_extraction_enabled {
        extractors.push(Arc::new(PdfTextExtractor::new(
            config.pdftotext_path.clone(),
        )));
    }
    extractors.push(Arc::new(PlainTextExtractor));

    let state = AppState {
        pool,
        session_ttl_hours: config.session_ttl_hours,
        storage: Arc::new(LocalFsStorage::new(&config.storage_dir)),
        extractor: Arc::new(ChainExtractor { extractors }),
        billing: BillingSettings {
            webhook_secret: config.paddle_webhook_secret.clone(),
            signature_tolerance_secs: config.paddle_signature_tolerance_secs,
            enforce_entitlements: config.billing_enforcement,
        },
        max_upload_bytes: config.max_upload_bytes,
    };
    let app = build_router_with_cors(state, &config.cors_allowed_origins);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!("xflow-backend listening on {}", config.bind_addr);
    axum::serve(listener, app).await?;
    Ok(())
}
