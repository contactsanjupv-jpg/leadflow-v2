//! Account/org administration: current user, members, audit log, notifications, billing status
//! and Paddle webhook, monitoring jobs.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit;
use crate::auth;
use crate::billing::{self, ProcessOutcome, SignatureError};
use crate::http::authed_user::{AuthedUser, Role};
use crate::http::errors::AppError;
use crate::http::state::AppState;
use crate::http::util::*;
use crate::rbac::{can_assign_role, Permission};

fn role_str(r: &Role) -> &'static str {
    match r {
        Role::Owner => "OWNER",
        Role::Admin => "ADMIN",
        Role::Member => "MEMBER",
        Role::Reviewer => "REVIEWER",
    }
}

pub async fn me(user: AuthedUser, State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let row: (String, String) = sqlx::query_as(
        "SELECT u.email, o.name FROM users u, organizations o WHERE u.id = $1 AND o.id = $2",
    )
    .bind(user.user_id)
    .bind(user.organization_id)
    .fetch_one(&state.pool)
    .await?;
    let entitled = billing::is_entitled(&state.pool, user.organization_id).await?;
    Ok(Json(
        json!({ "user_id": user.user_id, "email": row.0, "organization_id": user.organization_id, "organization_name": row.1, "role": role_str(&user.role), "entitled": entitled || !state.billing.enforce_entitlements }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewMember {
    pub email: String,
    pub password: String,
    pub role: String,
}

pub async fn add_member(
    user: AuthedUser,
    State(state): State<AppState>,
    Json(req): Json<NewMember>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    user.require(Permission::ManageMembers)?;
    let role = match req.role.to_ascii_uppercase().as_str() {
        "OWNER" => Role::Owner,
        "ADMIN" => Role::Admin,
        "MEMBER" => Role::Member,
        "REVIEWER" => Role::Reviewer,
        _ => {
            return Err(AppError::Validation(
                "role must be OWNER, ADMIN, MEMBER or REVIEWER".into(),
            ))
        }
    };
    if !can_assign_role(&user.role, &role) {
        return Err(AppError::Authorization);
    }
    if req.password.len() < 12 {
        return Err(AppError::Validation(
            "password must be at least 12 characters".into(),
        ));
    }
    let email = req.email.trim().to_ascii_lowercase();
    if !email.contains('@') {
        return Err(AppError::Validation("invalid email".into()));
    }
    let hash = auth::hash_password(&req.password).map_err(AppError::Internal)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email = $1)")
        .bind(&email)
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Err(AppError::Conflict(
            "an account with this email already exists".into(),
        ));
    }
    let uid: Uuid =
        sqlx::query_scalar("INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id")
            .bind(&email)
            .bind(hash)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3::membership_role)").bind(user.organization_id).bind(uid).bind(role_str(&role)).execute(&mut *tx).await?;
    audit::record(
        &mut tx,
        user.organization_id,
        Some(user.user_id),
        "member.add",
        "user",
        Some(uid),
        None,
        Some(json!({ "email": email, "role": role_str(&role) })),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "user_id": uid, "email": email, "role": role_str(&role) })),
    ))
}

pub async fn list_members(
    user: AuthedUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ManageMembers)?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at), '[]'::jsonb) FROM (SELECT u.id AS user_id, u.email, m.role::text AS role, m.created_at FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.organization_id = $1) t",
    ).bind(user.organization_id).fetch_one(&state.pool).await?;
    Ok(Json(v))
}

pub async fn list_audit(
    user: AuthedUser,
    State(state): State<AppState>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewAudit)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (SELECT id, actor_user_id, action, resource_type, resource_id, before_state, after_state, created_at FROM audit_logs ORDER BY created_at DESC, id LIMIT $1 OFFSET $2) t",
    ).bind(page.limit()).bind(page.offset()).fetch_one(&mut *tx).await?;
    Ok(Json(v))
}

pub async fn list_notifications(
    user: AuthedUser,
    State(state): State<AppState>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (SELECT id, event_type, payload, read_at, created_at FROM notifications WHERE user_id IS NULL OR user_id = $3 ORDER BY created_at DESC, id LIMIT $1 OFFSET $2) t",
    ).bind(page.limit()).bind(page.offset()).bind(user.user_id).fetch_one(&mut *tx).await?;
    Ok(Json(v))
}

pub async fn mark_notification_read(
    user: AuthedUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let n =
        sqlx::query("UPDATE notifications SET read_at = COALESCE(read_at, now()) WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(Json(json!({ "id": id, "read": true })))
}

// ------------------------------------------------------------------- billing

pub async fn billing_status(
    user: AuthedUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewBilling)?;
    let row: Option<(String, Option<String>, Option<chrono::DateTime<Utc>>)> = sqlx::query_as(
        "SELECT status, plan, current_period_end FROM subscriptions WHERE organization_id = $1",
    )
    .bind(user.organization_id)
    .fetch_optional(&state.pool)
    .await?;
    let (status, plan, end) = row.unwrap_or(("NONE".to_string(), None, None));
    Ok(Json(
        json!({ "status": status, "plan": plan, "current_period_end": end, "entitled": billing::status_is_entitled(&status), "enforced": state.billing.enforce_entitlements }),
    ))
}

/// Everything the browser needs to open a Paddle checkout for THIS organization: the id, the
/// backend-issued signature that proves the link (see `billing::checkout_signature`), and the buyer's
/// email. Owners/admins only. The Paddle client token and price id are public checkout settings served
/// by the web app; the signing key never leaves the backend.
pub async fn checkout_context(
    user: AuthedUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ManageMembers)?;
    let secret = state.billing.webhook_secret.as_deref().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "billing is not configured (PADDLE_WEBHOOK_SECRET missing)".into(),
        )
    })?;
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(json!({
        "organization_id": user.organization_id,
        "organization_sig": billing::checkout_signature(secret, user.organization_id),
        "email": email,
    })))
}

/// Paddle webhook. Unauthenticated by design: authenticity comes from the HMAC signature over the
/// RAW body. Fails closed (503) if no secret is configured, so an unconfigured deployment can
/// never accept forged subscription events.
pub async fn paddle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, AppError> {
    let secret = state
        .billing
        .webhook_secret
        .as_deref()
        .ok_or_else(|| AppError::ServiceUnavailable("webhook secret not configured".into()))?;
    let sig = headers
        .get("paddle-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Authentication)?;
    match billing::verify_signature(
        sig,
        &body,
        secret,
        Utc::now().timestamp(),
        state.billing.signature_tolerance_secs,
    ) {
        Ok(()) => {}
        Err(
            SignatureError::MissingOrMalformed
            | SignatureError::TimestampOutOfTolerance
            | SignatureError::Mismatch,
        ) => return Err(AppError::Authentication),
    }
    let event: Value = serde_json::from_slice(&body)
        .map_err(|_| AppError::Validation("invalid JSON body".into()))?;
    let outcome = billing::process_event(&state.pool, &event, secret)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "paddle event processing failed");
            AppError::Validation("event could not be processed".into())
        })?;
    let label = match outcome {
        ProcessOutcome::Applied => "applied",
        ProcessOutcome::DuplicateIgnored => "duplicate",
        ProcessOutcome::IgnoredStale => "stale",
        ProcessOutcome::IgnoredUnhandledType => "ignored",
        ProcessOutcome::UnmappedOrganization => "unmapped",
        ProcessOutcome::IgnoredConflictingSubscription => "conflict",
    };
    Ok(Json(json!({ "status": label })))
}

// ---------------------------------------------------------------- monitoring

pub async fn enqueue_monitoring(
    user: AuthedUser,
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    user.require(Permission::ManageData)?;
    require_entitlement(&state, user.organization_id).await?;
    let mut tx = scoped_tx(&state, &user).await?;
    let queued = crate::workers::enqueue_reconcile_pending(&mut tx, user.organization_id).await?;
    audit::record(
        &mut tx,
        user.organization_id,
        Some(user.user_id),
        "monitoring.enqueue",
        "monitoring_job",
        None,
        None,
        Some(json!({ "queued": queued })),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": queued }))))
}

pub async fn list_monitoring_jobs(
    user: AuthedUser,
    State(state): State<AppState>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (SELECT id, job_type, status, attempts, scheduled_for, started_at, completed_at, last_error, created_at FROM monitoring_jobs ORDER BY created_at DESC, id LIMIT $1 OFFSET $2) t",
    ).bind(page.limit()).bind(page.offset()).fetch_one(&mut *tx).await?;
    Ok(Json(v))
}
