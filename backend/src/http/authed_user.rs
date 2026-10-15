//! Authentication + RBAC extractor (spec sections 8-9).

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use chrono::Utc;
use uuid::Uuid;

use crate::auth;
use crate::http::errors::AppError;
use crate::http::state::AppState;

#[derive(Debug, Clone, sqlx::Type, PartialEq, Eq)]
#[sqlx(type_name = "membership_role", rename_all = "UPPERCASE")]
pub enum Role {
    Owner,
    Admin,
    Member,
    Reviewer,
}

#[derive(Debug, Clone)]
pub struct AuthedUser {
    pub user_id: Uuid,
    pub organization_id: Uuid,
    pub role: Role,
}

impl AuthedUser {
    pub fn require_role(&self, allowed: &[Role]) -> Result<(), AppError> {
        if allowed.contains(&self.role) {
            Ok(())
        } else {
            Err(AppError::Authorization)
        }
    }

    /// Permission check used by every tenant-data handler (see `rbac`).
    pub fn require(&self, permission: crate::rbac::Permission) -> Result<(), AppError> {
        if crate::rbac::role_can(&self.role, permission) {
            Ok(())
        } else {
            Err(AppError::Authorization)
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Authentication)?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Authentication)?;
        let token_hash = auth::hash_token(token);

        let row: (
            Uuid,
            chrono::DateTime<Utc>,
            Option<chrono::DateTime<Utc>>,
            Uuid,
            Role,
        ) = sqlx::query_as(
            "SELECT s.user_id, s.expires_at, s.revoked_at, m.organization_id, m.role
             FROM sessions s
             JOIN memberships m ON m.user_id = s.user_id
             WHERE s.token_hash = $1
             ORDER BY m.created_at ASC
             LIMIT 1",
        )
        .bind(token_hash)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Database)?
        .ok_or(AppError::Authentication)?;
        let (user_id, expires_at, revoked_at, organization_id, role) = row;

        if revoked_at.is_some() || expires_at < Utc::now() {
            return Err(AppError::Authentication);
        }

        Ok(AuthedUser {
            user_id,
            organization_id,
            role,
        })
    }
}
