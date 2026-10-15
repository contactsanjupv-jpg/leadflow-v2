//! Shared handler helpers.

use serde::Deserialize;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::http::authed_user::AuthedUser;
use crate::http::errors::AppError;
use crate::http::state::AppState;

pub type Tx = Transaction<'static, Postgres>;

pub async fn scoped_tx(state: &AppState, user: &AuthedUser) -> Result<Tx, AppError> {
    Ok(crate::tenants::begin_scoped_tx(&state.pool, user.organization_id).await?)
}

/// Map unique / exclusion constraint violations to 409, everything else to a database error.
pub fn conflict_on_constraint(e: sqlx::Error, message: &str) -> AppError {
    if let Some(db) = e.as_database_error() {
        if matches!(db.code().as_deref(), Some("23505") | Some("23P01")) {
            return AppError::Conflict(message.to_string());
        }
    }
    AppError::Database(e)
}

/// Verify that a referenced row exists AND is visible to the current tenant.
///
/// PostgreSQL foreign-key checks are performed with the table owner's rights and bypass RLS, so
/// a client could otherwise reference another tenant's row by UUID. Every endpoint that accepts
/// an id of a tenant-owned row calls this first. Unknown and foreign ids both give 404.
pub async fn ensure_owned(tx: &mut Tx, table: &'static str, id: Uuid) -> Result<(), AppError> {
    const ALLOWED: [&str; 8] = [
        "vendors",
        "contracts",
        "manifests",
        "documents",
        "invoices",
        "weight_records",
        "discrepancies",
        "disputes",
    ];
    if !ALLOWED.contains(&table) {
        return Err(AppError::Internal(anyhow::anyhow!(
            "ensure_owned called with unsupported table {table}"
        )));
    }
    let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)");
    let exists: bool = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

/// Reject implausible years (see `common::dates`). Used for every date field on every endpoint.
pub fn check_date(field: &str, date: chrono::NaiveDate) -> Result<chrono::NaiveDate, AppError> {
    crate::common::dates::validate_date(field, date).map_err(AppError::Validation)
}

pub fn clean_text(field: &str, value: &str, max_len: usize) -> Result<String, AppError> {
    let v = value.trim();
    if v.is_empty() {
        return Err(AppError::Validation(format!("{field} must not be empty")));
    }
    if v.chars().count() > max_len {
        return Err(AppError::Validation(format!(
            "{field} must be at most {max_len} characters"
        )));
    }
    Ok(v.to_string())
}

#[derive(Debug, Deserialize)]
pub struct Page {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl Page {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
    pub fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

pub async fn json_one(tx: &mut Tx, sql: &str, id: Uuid) -> Result<Value, AppError> {
    let v: Option<Value> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    v.ok_or(AppError::NotFound)
}

/// Entitlement gate for processing actions (server-side; the frontend is never trusted).
pub async fn require_entitlement(state: &AppState, organization_id: Uuid) -> Result<(), AppError> {
    if !state.billing.enforce_entitlements {
        return Ok(());
    }
    if crate::billing::is_entitled(&state.pool, organization_id).await? {
        Ok(())
    } else {
        Err(AppError::PaymentRequired)
    }
}
