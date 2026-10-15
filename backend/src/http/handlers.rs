use axum::extract::State;
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth;
use crate::http::authed_user::AuthedUser;
use crate::http::errors::AppError;
use crate::http::state::AppState;
use crate::http::util::{json_one, require_entitlement, scoped_tx, Page};
use crate::rbac::Permission;
use crate::reconciliation::{model::ReconciliationResult, service};

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

#[derive(Deserialize)]
pub struct SignupRequest {
    pub organization_name: String,
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct SignupResponse {
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub session_token: String,
}

pub async fn signup(State(state): State<AppState>, Json(req): Json<SignupRequest>) -> Result<Json<SignupResponse>, AppError> {
    if req.password.len() < 12 {
        return Err(AppError::Validation("password must be at least 12 characters".into()));
    }
    let email = req.email.trim().to_ascii_lowercase();
    if !email.contains('@') {
        return Err(AppError::Validation("invalid email".into()));
    }
    let organization_name = crate::http::util::clean_text("organization_name", &req.organization_name, 200)?;

    let password_hash = auth::hash_password(&req.password).map_err(AppError::Internal)?;
    let mut tx = state.pool.begin().await?;

    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = $1").bind(&email).fetch_optional(&mut *tx).await?;
    if existing.is_some() {
        return Err(AppError::Conflict("an account with this email already exists".into()));
    }

    let org_id: Uuid = sqlx::query_scalar("INSERT INTO organizations (name) VALUES ($1) RETURNING id").bind(&organization_name).fetch_one(&mut *tx).await?;
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id")
        .bind(&email)
        .bind(&password_hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| crate::http::util::conflict_on_constraint(e, "an account with this email already exists"))?;
    sqlx::query("INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'OWNER')").bind(org_id).bind(user_id).execute(&mut *tx).await?;

    let (token, token_hash) = auth::generate_session_token();
    let expires_at = Utc::now() + Duration::hours(state.session_ttl_hours);
    sqlx::query("INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)").bind(user_id).bind(&token_hash).bind(expires_at).execute(&mut *tx).await?;

    tx.commit().await?;
    Ok(Json(SignupResponse { organization_id: org_id, user_id, session_token: token }))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub session_token: String,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> Result<Json<LoginResponse>, AppError> {
    let row: Option<(Uuid, String)> = sqlx::query_as("SELECT id, password_hash FROM users WHERE lower(email) = $1")
        .bind(req.email.trim().to_ascii_lowercase())
        .fetch_optional(&state.pool)
        .await?;

    // Identical error for "no such user" and "wrong password" (account enumeration defense).
    let (user_id, password_hash) = row.ok_or(AppError::Authentication)?;
    let ok = auth::verify_password(&req.password, &password_hash).map_err(AppError::Internal)?;
    if !ok {
        return Err(AppError::Authentication);
    }

    let (token, token_hash) = auth::generate_session_token();
    let expires_at = Utc::now() + Duration::hours(state.session_ttl_hours);
    sqlx::query("INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)").bind(user_id).bind(&token_hash).bind(expires_at).execute(&state.pool).await?;

    Ok(Json(LoginResponse { session_token: token }))
}

pub async fn logout(user: AuthedUser, State(state): State<AppState>, headers: axum::http::HeaderMap) -> Result<Json<serde_json::Value>, AppError> {
    if let Some(token) = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|s| s.strip_prefix("Bearer ")) {
        let hash = auth::hash_token(token);
        sqlx::query("UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND user_id = $2").bind(hash).bind(user.user_id).execute(&state.pool).await?;
    }
    Ok(Json(json!({ "status": "logged_out" })))
}

#[derive(Deserialize)]
pub struct RunReconciliationRequest {
    pub invoice_id: Uuid,
}

#[derive(Serialize)]
pub struct RunReconciliationResponse {
    pub reconciliation_id: Uuid,
    pub discrepancy_ids: Vec<Uuid>,
    pub manifest_id: Option<Uuid>,
    pub weight_ticket_count: usize,
    #[serde(flatten)]
    pub result: ReconciliationResult,
}

/// Run (or re-run) the reconciliation for one invoice through the real workflow: the invoice's
/// linked manifest and weight tickets are loaded and given to the engine, and every finding is
/// persisted as a reviewable `discrepancies` row.
pub async fn run_reconciliation(user: AuthedUser, State(state): State<AppState>, Json(req): Json<RunReconciliationRequest>) -> Result<Json<RunReconciliationResponse>, AppError> {
    user.require(Permission::ManageData)?;
    require_entitlement(&state, user.organization_id).await?;
    let mut tx = scoped_tx(&state, &user).await?;
    let outcome = service::run_for_invoice(&mut tx, user.organization_id, req.invoice_id).await?;
    crate::audit::record(
        &mut tx, user.organization_id, Some(user.user_id), "reconciliation.run", "reconciliation", Some(outcome.reconciliation_id), None,
        Some(json!({ "invoice_id": req.invoice_id, "discrepancy_amount": outcome.result.discrepancy_amount.to_wire_string(), "discrepancy_count": outcome.discrepancy_ids.len() })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(RunReconciliationResponse {
        reconciliation_id: outcome.reconciliation_id,
        discrepancy_ids: outcome.discrepancy_ids,
        manifest_id: outcome.manifest_id,
        weight_ticket_count: outcome.weight_ticket_count,
        result: outcome.result,
    }))
}

const RECONCILIATION_DETAIL_SQL: &str = r#"
SELECT to_jsonb(r) || jsonb_build_object('discrepancies', (SELECT coalesce(jsonb_agg(to_jsonb(d) ORDER BY d.created_at, d.id), '[]'::jsonb) FROM (
    SELECT id, discrepancy_type::text AS discrepancy_type, rule_id, description, amount::text AS amount, confidence, review_status::text AS review_status,
           reviewed_by, reviewed_at, review_notes, created_at FROM discrepancies WHERE reconciliation_id = r.id) d))
FROM (SELECT id, invoice_id, contract_rate_id, manifest_id, weight_record_id, expected_amount::text AS expected_amount, invoiced_amount::text AS invoiced_amount,
             discrepancy_amount::text AS discrepancy_amount, review_status::text AS review_status, tolerances_applied, calculation_trace, engine_version, created_at
        FROM reconciliations WHERE id = $1) r"#;

pub async fn get_reconciliation(user: AuthedUser, State(state): State<AppState>, axum::extract::Path(id): axum::extract::Path<Uuid>) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, RECONCILIATION_DETAIL_SQL, id).await?))
}

/// Latest reconciliation per invoice (history stays in the table; the list shows current state).
pub async fn list_reconciliations(user: AuthedUser, State(state): State<AppState>, axum::extract::Query(page): axum::extract::Query<Page>) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: serde_json::Value = sqlx::query_scalar(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (
             SELECT * FROM (
               SELECT DISTINCT ON (r.invoice_id) r.id, r.invoice_id, i.invoice_number, i.vendor_id, v.name AS vendor_name, r.expected_amount::text AS expected_amount,
                      r.invoiced_amount::text AS invoiced_amount, r.discrepancy_amount::text AS discrepancy_amount, r.review_status::text AS review_status, r.created_at
                 FROM reconciliations r JOIN invoices i ON i.id = r.invoice_id JOIN vendors v ON v.id = i.vendor_id
                ORDER BY r.invoice_id, r.created_at DESC, r.id DESC) latest
             ORDER BY created_at DESC, id LIMIT $1 OFFSET $2) t"#,
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}

pub async fn get_dashboard(user: AuthedUser, State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    // Only the LATEST reconciliation of each invoice counts, so re-running never double-counts.
    // Amounts are summed by PostgreSQL NUMERIC (exact) and returned as fixed 4-decimal text.
    let v: serde_json::Value = sqlx::query_scalar(
        r#"WITH latest AS (
             SELECT DISTINCT ON (invoice_id) id, invoice_id, review_status FROM reconciliations ORDER BY invoice_id, created_at DESC, id DESC)
           SELECT jsonb_build_object(
             'invoices_analyzed', (SELECT count(*) FROM latest),
             'requiring_review', (SELECT count(*) FROM latest WHERE review_status IN ('NEW', 'UNDER_REVIEW', 'NEEDS_INFORMATION')),
             'potential_discrepancy_total', (SELECT coalesce(sum(d.amount), 0)::numeric(20,4)::text FROM discrepancies d JOIN latest l ON l.id = d.reconciliation_id
                                              WHERE d.amount > 0 AND d.review_status IN ('NEW', 'UNDER_REVIEW', 'NEEDS_INFORMATION')),
             'confirmed_discrepancy_total', (SELECT coalesce(sum(d.amount), 0)::numeric(20,4)::text FROM discrepancies d JOIN latest l ON l.id = d.reconciliation_id
                                              WHERE d.amount > 0 AND d.review_status IN ('CONFIRMED', 'DISPUTED')))"#,
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}
