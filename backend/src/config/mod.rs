use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub session_ttl_hours: i64,
    /// Comma-separated allowed browser origins for CORS. Empty = no cross-origin access
    /// (the Next.js app talks to the API server-side through its BFF proxy, so none is needed).
    pub cors_allowed_origins: Vec<String>,
    pub storage_dir: String,
    pub pdftotext_path: String,
    pub pdf_extraction_enabled: bool,
    pub max_upload_bytes: usize,
    pub paddle_webhook_secret: Option<String>,
    pub paddle_signature_tolerance_secs: i64,
    /// When true, processing endpoints require an entitled subscription (server-side check).
    pub billing_enforcement: bool,
    pub worker_enabled: bool,
    pub worker_poll_secs: u64,
    pub monitor_interval_secs: u64,
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => default,
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let _ = dotenvy::dotenv();
        let non_empty = |name: &str| env::var(name).ok().filter(|v| !v.trim().is_empty());
        Ok(Config {
            database_url: env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?,
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            session_ttl_hours: env::var("SESSION_TTL_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(24 * 7),
            cors_allowed_origins: non_empty("CORS_ALLOWED_ORIGINS")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            pdftotext_path: env::var("PDFTOTEXT_PATH").unwrap_or_else(|_| "pdftotext".to_string()),
            pdf_extraction_enabled: env_bool("PDF_EXTRACTION_ENABLED", true),
            storage_dir: env::var("STORAGE_DIR").unwrap_or_else(|_| "./data/uploads".to_string()),
            max_upload_bytes: env::var("MAX_UPLOAD_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(25 * 1024 * 1024),
            paddle_webhook_secret: non_empty("PADDLE_WEBHOOK_SECRET"),
            paddle_signature_tolerance_secs: env::var("PADDLE_SIGNATURE_TOLERANCE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(300),
            billing_enforcement: env_bool("BILLING_ENFORCEMENT", true),
            worker_enabled: env_bool("WORKER_ENABLED", true),
            worker_poll_secs: env::var("WORKER_POLL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(5),
            monitor_interval_secs: env::var("MONITOR_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(900),
        })
    }
}
