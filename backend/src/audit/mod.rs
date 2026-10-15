//! Audit logging (spec: full audit log). Written inside the SAME tenant-scoped transaction as
//! the change it records, so a change and its audit row commit or roll back together.
//! `audit_logs` is append-only for the runtime role (UPDATE/DELETE revoked in migration 0002).

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn record(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    actor_user_id: Option<Uuid>,
    action: &str,
    resource_type: &str,
    resource_id: Option<Uuid>,
    before_state: Option<Value>,
    after_state: Option<Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_logs (organization_id, actor_user_id, action, resource_type, resource_id, before_state, after_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(organization_id)
    .bind(actor_user_id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(before_state)
    .bind(after_state)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
