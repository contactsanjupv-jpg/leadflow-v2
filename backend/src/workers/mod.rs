//! Background monitoring worker (spec: continuous monitoring).
//!
//! A scheduler enqueues one RECONCILE_PENDING job per entitled organization every
//! MONITOR_INTERVAL_SECS; the worker claims jobs through the SECURITY DEFINER function
//! `claim_monitoring_job()` (FOR UPDATE SKIP LOCKED, safe with several replicas), re-runs the
//! deterministic reconciliation for every invoice whose inputs are new or changed, raises a
//! notification when findings appear, and retries failures with quadratic backoff
//! (`finish_monitoring_job`). Each invoice runs in its own tenant-scoped transaction.

use std::time::Duration;

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::http::errors::AppError;
use crate::reconciliation::service;

pub const JOB_RECONCILE_PENDING: &str = "RECONCILE_PENDING";

/// Enqueue a job unless one is already pending/running for the organization (partial unique index).
pub async fn enqueue_reconcile_pending(tx: &mut Transaction<'_, Postgres>, organization_id: Uuid) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        "INSERT INTO monitoring_jobs (organization_id, job_type) VALUES ($1, $2)
         ON CONFLICT (organization_id, job_type) WHERE status IN ('PENDING', 'RUNNING') DO NOTHING",
    )
    .bind(organization_id)
    .bind(JOB_RECONCILE_PENDING)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Invoices that have never been reconciled, or whose inputs (invoice, rate, manifest, weight
/// tickets) changed after their latest reconciliation. Already-current invoices are skipped, so
/// repeated monitoring never churns the review queue.
pub async fn invoices_needing_reconciliation(tx: &mut Transaction<'_, Postgres>, limit: i64) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        r#"SELECT i.id FROM invoices i
           LEFT JOIN LATERAL (SELECT created_at FROM reconciliations r WHERE r.invoice_id = i.id ORDER BY created_at DESC, id DESC LIMIT 1) lr ON true
           WHERE lr.created_at IS NULL
              OR i.updated_at > lr.created_at
              OR EXISTS (SELECT 1 FROM contract_rates cr WHERE cr.vendor_id = i.vendor_id AND cr.waste_stream = i.waste_stream AND cr.created_at > lr.created_at)
              OR EXISTS (SELECT 1 FROM manifests m WHERE m.id = i.manifest_id AND m.created_at > lr.created_at)
              OR EXISTS (SELECT 1 FROM weight_records w WHERE w.manifest_id = i.manifest_id AND w.created_at > lr.created_at)
           ORDER BY i.created_at LIMIT $1"#,
    )
    .bind(limit)
    .fetch_all(&mut **tx)
    .await
}

/// Execute one RECONCILE_PENDING job for an organization. Returns the number of invoices processed.
pub async fn run_reconcile_pending(pool: &PgPool, organization_id: Uuid, enforce_entitlements: bool) -> Result<usize, AppError> {
    if enforce_entitlements && !crate::billing::is_entitled(pool, organization_id).await? {
        return Ok(0); // unentitled organizations are skipped, not failed
    }
    let mut tx = crate::tenants::begin_scoped_tx(pool, organization_id).await?;
    let ids = invoices_needing_reconciliation(&mut tx, 100).await?;
    tx.rollback().await?;

    let mut processed = 0usize;
    let mut new_findings = 0usize;
    let mut total_amount = crate::common::money::Money::zero();
    for id in &ids {
        let mut tx = crate::tenants::begin_scoped_tx(pool, organization_id).await?;
        let outcome = service::run_for_invoice(&mut tx, organization_id, *id).await?;
        crate::audit::record(&mut tx, organization_id, None, "reconciliation.run", "reconciliation", Some(outcome.reconciliation_id), None,
            Some(serde_json::json!({ "invoice_id": id, "trigger": "monitoring", "discrepancy_count": outcome.discrepancy_ids.len() }))).await?;
        tx.commit().await?;
        processed += 1;
        if !outcome.result.discrepancies.is_empty() {
            new_findings += outcome.result.discrepancies.len();
            total_amount = total_amount + outcome.result.discrepancy_amount;
        }
    }
    if new_findings > 0 {
        let mut tx = crate::tenants::begin_scoped_tx(pool, organization_id).await?;
        sqlx::query("INSERT INTO notifications (organization_id, event_type, payload) VALUES ($1, 'discrepancies.detected', $2)")
            .bind(organization_id)
            .bind(serde_json::json!({ "invoices_processed": processed, "discrepancy_count": new_findings, "discrepancy_amount": total_amount.to_wire_string() }))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(processed)
}

/// Claim and execute at most one job. Returns true if a job was claimed.
pub async fn run_one_job(pool: &PgPool, enforce_entitlements: bool) -> Result<bool, sqlx::Error> {
    let claimed: Option<(Uuid, Uuid, String, Option<serde_json::Value>, i32)> = sqlx::query_as("SELECT job_id, org_id, job_kind, job_payload, job_attempts FROM claim_monitoring_job()").fetch_optional(pool).await?;
    let Some((job_id, org_id, kind, _payload, _attempts)) = claimed else { return Ok(false) };
    let result = match kind.as_str() {
        JOB_RECONCILE_PENDING => run_reconcile_pending(pool, org_id, enforce_entitlements).await.map(|_| ()).map_err(|e| e.to_string()),
        other => Err(format!("unknown job type {other}")),
    };
    let err = result.err();
    if let Some(e) = &err {
        tracing::warn!(job_id = %job_id, error = %e, "monitoring job failed");
    }
    sqlx::query("SELECT finish_monitoring_job($1, $2)").bind(job_id).bind(err).execute(pool).await?;
    Ok(true)
}

/// Enqueue a job for every organization (scheduler tick). `organizations` has no RLS; the
/// INSERT into monitoring_jobs goes through a tenant-scoped transaction like everything else.
pub async fn schedule_all(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let orgs: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM organizations").fetch_all(pool).await?;
    let mut queued = 0;
    for org in orgs {
        let mut tx = crate::tenants::begin_scoped_tx(pool, org).await?;
        if enqueue_reconcile_pending(&mut tx, org).await? { queued += 1; }
        tx.commit().await?;
    }
    Ok(queued)
}

pub fn spawn(pool: PgPool, poll: Duration, schedule_every: Duration, enforce_entitlements: bool) {
    let worker_pool = pool.clone();
    tokio::spawn(async move {
        loop {
            match run_one_job(&worker_pool, enforce_entitlements).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => tracing::error!(error = %e, "worker loop error"),
            }
            tokio::time::sleep(poll).await;
        }
    });
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(schedule_every).await;
            if let Err(e) = schedule_all(&pool).await {
                tracing::error!(error = %e, "monitoring scheduler error");
            }
        }
    });
}
