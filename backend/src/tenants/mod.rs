//! Tenant context propagation - sets the Postgres session variable RLS checks.
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn begin_scoped_tx<'a>(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)")
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
