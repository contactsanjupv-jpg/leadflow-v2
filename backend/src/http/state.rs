use std::sync::Arc;

use sqlx::PgPool;

use crate::extraction::DocumentExtractor;
use crate::storage::ObjectStorage;

#[derive(Clone, Debug)]
pub struct BillingSettings {
    /// Paddle notification-destination secret. When absent the webhook endpoint fails closed.
    pub webhook_secret: Option<String>,
    /// Maximum accepted age/skew of the signed timestamp.
    pub signature_tolerance_secs: i64,
    /// Whether processing endpoints require an entitled subscription.
    pub enforce_entitlements: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub session_ttl_hours: i64,
    pub storage: Arc<dyn ObjectStorage>,
    pub extractor: Arc<dyn DocumentExtractor>,
    pub billing: BillingSettings,
    pub max_upload_bytes: usize,
}
