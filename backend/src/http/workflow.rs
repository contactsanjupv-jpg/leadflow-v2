//! Review queue, evidence packages, disputes and credit/recovery tracking.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit;
use crate::common::money::ExactDecimal;
use crate::discrepancies::{transition_allowed as review_allowed, ReviewAction};
use crate::disputes::{transition_allowed as dispute_allowed, DisputeStatus};
use crate::http::authed_user::AuthedUser;
use crate::http::errors::AppError;
use crate::http::state::AppState;
use crate::http::util::*;
use crate::rbac::Permission;
use crate::reconciliation::model::ReviewStatus;
use crate::reconciliation::service::refresh_reconciliation_status;

const DISCREPANCY_SQL: &str = r#"
SELECT to_jsonb(d) FROM (
  SELECT d.id, d.reconciliation_id, r.invoice_id, i.invoice_number, i.vendor_id, v.name AS vendor_name,
         d.discrepancy_type::text AS discrepancy_type, d.rule_id, d.description, d.amount::text AS amount, d.confidence,
         d.review_status::text AS review_status, d.reviewed_by, d.reviewed_at, d.review_notes, d.created_at
    FROM discrepancies d JOIN reconciliations r ON r.id = d.reconciliation_id JOIN invoices i ON i.id = r.invoice_id JOIN vendors v ON v.id = i.vendor_id
   WHERE d.id = $1) d"#;

#[derive(Deserialize)]
pub struct DiscrepancyQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Discrepancies of the LATEST reconciliation of each invoice (superseded runs are history).
pub async fn list_discrepancies(user: AuthedUser, State(state): State<AppState>, Query(q): Query<DiscrepancyQuery>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let status = match q.status.as_deref() {
        Some(s) => Some(ReviewStatus::from_db(&s.to_ascii_uppercase()).ok_or_else(|| AppError::Validation("unknown status filter".into()))?.as_db()),
        None => None,
    };
    let page = Page { limit: q.limit, offset: q.offset };
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        r#"WITH latest AS (SELECT DISTINCT ON (invoice_id) id FROM reconciliations ORDER BY invoice_id, created_at DESC, id DESC)
           SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (
             SELECT d.id, r.invoice_id, i.invoice_number, v.name AS vendor_name, d.discrepancy_type::text AS discrepancy_type, d.rule_id, d.description,
                    d.amount::text AS amount, d.confidence, d.review_status::text AS review_status, d.created_at
               FROM discrepancies d JOIN latest l ON l.id = d.reconciliation_id JOIN reconciliations r ON r.id = d.reconciliation_id
               JOIN invoices i ON i.id = r.invoice_id JOIN vendors v ON v.id = i.vendor_id
              WHERE ($3::text IS NULL OR d.review_status = $3::review_status)
              ORDER BY d.created_at DESC, d.id LIMIT $1 OFFSET $2) t"#,
    )
    .bind(page.limit())
    .bind(page.offset())
    .bind(status)
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}

pub async fn get_discrepancy(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, DISCREPANCY_SQL, id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub action: String,
    pub notes: Option<String>,
}

pub async fn review_discrepancy(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>, Json(req): Json<ReviewRequest>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ReviewDiscrepancies)?;
    let action = ReviewAction::parse(&req.action).ok_or_else(|| AppError::Validation("action must be one of start_review, confirm, reject, needs_information, reopen".into()))?;
    let notes = req.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.to_string());
    if action.requires_notes() && notes.is_none() {
        return Err(AppError::Validation("notes are required for this action".into()));
    }
    if notes.as_deref().map_or(false, |n| n.chars().count() > 2000) {
        return Err(AppError::Validation("notes must be at most 2000 characters".into()));
    }
    let mut tx = scoped_tx(&state, &user).await?;
    let row: Option<(String, Uuid)> = sqlx::query_as("SELECT review_status::text, reconciliation_id FROM discrepancies WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let (cur, reconciliation_id) = row.ok_or(AppError::NotFound)?;
    let from = ReviewStatus::from_db(&cur).ok_or_else(|| AppError::Internal(anyhow::anyhow!("unknown stored review status")))?;
    if !review_allowed(from, action) {
        return Err(AppError::Conflict(format!("cannot {} a discrepancy that is {}", req.action, cur)));
    }
    let to = action.target();
    sqlx::query("UPDATE discrepancies SET review_status = $2::review_status, reviewed_by = $3, reviewed_at = now(), review_notes = COALESCE($4, review_notes), updated_at = now() WHERE id = $1")
        .bind(id).bind(to.as_db()).bind(user.user_id).bind(&notes).execute(&mut *tx).await?;
    refresh_reconciliation_status(&mut tx, reconciliation_id).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), &format!("discrepancy.{}", req.action), "discrepancy", Some(id),
        Some(json!({ "review_status": cur })), Some(json!({ "review_status": to.as_db(), "notes": notes }))).await?;
    let body = json_one(&mut tx, DISCREPANCY_SQL, id).await?;
    tx.commit().await?;
    Ok(Json(body))
}

// ------------------------------------------------------------------ evidence

async fn build_evidence(tx: &mut Tx, discrepancy_id: Uuid) -> Result<Value, AppError> {
    let discrepancy = json_one(tx, DISCREPANCY_SQL, discrepancy_id).await?;
    let rec_id: Uuid = sqlx::query_scalar("SELECT reconciliation_id FROM discrepancies WHERE id = $1").bind(discrepancy_id).fetch_one(&mut **tx).await?;
    let reconciliation: Value = sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM (SELECT id, invoice_id, contract_rate_id, manifest_id, weight_record_id, expected_amount::text AS expected_amount, invoiced_amount::text AS invoiced_amount,
                discrepancy_amount::text AS discrepancy_amount, tolerances_applied, calculation_trace, engine_version, created_at FROM reconciliations WHERE id = $1) r",
    ).bind(rec_id).fetch_one(&mut **tx).await?;
    let invoice_id: Uuid = discrepancy["invoice_id"].as_str().and_then(|s| s.parse().ok()).ok_or(AppError::NotFound)?;
    let invoice: Value = sqlx::query_scalar(
        "SELECT to_jsonb(i) || jsonb_build_object('lines', (SELECT coalesce(jsonb_agg(jsonb_build_object('description', l.description, 'fee_code', l.fee_code, 'quantity', l.quantity::text, 'unit', l.unit,
                'unit_price', l.unit_price::text, 'line_total', l.line_total::text) ORDER BY l.id), '[]'::jsonb) FROM invoice_line_items l WHERE l.invoice_id = i.id))
         FROM (SELECT id, invoice_number, invoice_date, service_date, waste_stream, total::text AS total, source_document_id FROM invoices WHERE id = $1) i",
    ).bind(invoice_id).fetch_one(&mut **tx).await?;
    let rate: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(cr) || jsonb_build_object('authorized_fees', (SELECT coalesce(jsonb_agg(jsonb_build_object('fee_code', f.fee_code, 'description', f.description, 'amount', f.amount::text) ORDER BY f.fee_code), '[]'::jsonb)
                FROM contract_authorized_fees f WHERE f.contract_rate_id = cr.id), 'contract_name', (SELECT name FROM contracts WHERE id = cr.contract_id), 'contract_source_document_id', (SELECT source_document_id FROM contracts WHERE id = cr.contract_id))
         FROM (SELECT id, contract_id, waste_stream, unit, unit_rate::text AS unit_rate, effective_from, effective_to FROM contract_rates WHERE id = (SELECT contract_rate_id FROM reconciliations WHERE id = $1)) cr",
    ).bind(rec_id).fetch_optional(&mut **tx).await?;
    let manifest: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(m) FROM (SELECT id, manifest_number, waste_stream, container_count, quantity::text AS quantity, unit, shipment_date, source_document_id FROM manifests WHERE id = (SELECT manifest_id FROM reconciliations WHERE id = $1)) m",
    ).bind(rec_id).fetch_optional(&mut **tx).await?;
    let tickets: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(w) ORDER BY w.ticket_date, w.id), '[]'::jsonb) FROM (SELECT id, ticket_number, net_weight::text AS net_weight, unit, container_count, ticket_date, source_document_id
           FROM weight_records WHERE manifest_id = (SELECT manifest_id FROM reconciliations WHERE id = $1)) w",
    ).bind(rec_id).fetch_one(&mut **tx).await?;
    let documents: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(d) ORDER BY d.created_at), '[]'::jsonb) FROM (SELECT id, document_type::text AS document_type, original_filename, sha256, created_at FROM documents WHERE id IN (
            SELECT source_document_id FROM invoices WHERE id = $1 UNION SELECT source_document_id FROM manifests WHERE id = (SELECT manifest_id FROM reconciliations WHERE id = $2)
            UNION SELECT source_document_id FROM weight_records WHERE manifest_id = (SELECT manifest_id FROM reconciliations WHERE id = $2)
            UNION SELECT source_document_id FROM contracts WHERE id = (SELECT contract_id FROM contract_rates WHERE id = (SELECT contract_rate_id FROM reconciliations WHERE id = $2)))) d",
    ).bind(invoice_id).bind(rec_id).fetch_one(&mut **tx).await?;
    Ok(json!({
        "format_version": 1,
        "discrepancy": discrepancy,
        "reconciliation": reconciliation,
        "invoice": invoice,
        "contract_rate": rate,
        "manifest": manifest,
        "weight_tickets": tickets,
        "source_documents": documents,
    }))
}

/// Snapshot the complete evidence for a discrepancy. The JSON is what a reviewer can attach to
/// a vendor dispute. No PDF is rendered (the `pdf_object_storage_key` column stays empty).
pub async fn generate_evidence(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<(StatusCode, Json<Value>), AppError> {
    user.require(Permission::ReviewDiscrepancies)?;
    let mut tx = scoped_tx(&state, &user).await?;
    ensure_owned(&mut tx, "discrepancies", id).await?;
    let content = build_evidence(&mut tx, id).await?;
    let pkg_id: Uuid = sqlx::query_scalar("INSERT INTO evidence_packages (organization_id, discrepancy_id, generated_by, content) VALUES ($1, $2, $3, $4) RETURNING id")
        .bind(user.organization_id).bind(id).bind(user.user_id).bind(&content).fetch_one(&mut *tx).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "evidence.generate", "evidence_package", Some(pkg_id), None, Some(json!({ "discrepancy_id": id }))).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": pkg_id, "discrepancy_id": id, "content": content }))))
}

pub async fn latest_evidence(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(e) FROM (SELECT id, discrepancy_id, generated_by, content, created_at FROM evidence_packages WHERE discrepancy_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1) e")
        .bind(id).fetch_optional(&mut *tx).await?;
    Ok(Json(v.ok_or(AppError::NotFound)?))
}

/// Download the latest evidence package as a PDF rendered from the stored snapshot.
pub async fn evidence_pdf(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<axum::response::Response, AppError> {
    use axum::http::header;
    use axum::response::IntoResponse;
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let row: Option<(Uuid, Value, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT id, content, created_at FROM evidence_packages WHERE discrepancy_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1").bind(id).fetch_optional(&mut *tx).await?;
    let (pkg_id, content, created_at) = row.ok_or(AppError::NotFound)?;
    let org_name: String = sqlx::query_scalar("SELECT name FROM organizations WHERE id = $1").bind(user.organization_id).fetch_one(&mut *tx).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "evidence.pdf_download", "evidence_package", Some(pkg_id), None, Some(json!({ "discrepancy_id": id }))).await?;
    tx.commit().await?;
    let pdf = crate::evidence_pdf::render(&org_name, &pkg_id.to_string(), &created_at.format("%Y-%m-%d %H:%M UTC").to_string(), &content);
    let invoice = content["invoice"]["invoice_number"].as_str().unwrap_or("invoice");
    let safe: String = invoice.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(60).collect();
    Ok((
        [(header::CONTENT_TYPE, "application/pdf".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"evidence-{safe}-{}.pdf\"", &pkg_id.to_string()[..8])), (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string())],
        pdf,
    ).into_response())
}

// ------------------------------------------------------------------- disputes

const DISPUTE_SQL: &str = r#"
SELECT to_jsonb(x) FROM (
  SELECT d.id, d.discrepancy_id, d.vendor_id, v.name AS vendor_name, d.status::text AS status, d.disputed_amount::text AS disputed_amount, d.confirmed_amount::text AS confirmed_amount,
         d.submitted_at, d.resolved_at, d.created_at, d.updated_at,
         (SELECT to_jsonb(c) FROM (SELECT id, identified_amount::text AS identified_amount, confirmed_amount::text AS confirmed_amount, recovered_amount::text AS recovered_amount,
                 credit_memo_reference, verified_by, verified_at FROM credits WHERE dispute_id = d.id ORDER BY created_at DESC LIMIT 1) c) AS credit
    FROM disputes d JOIN vendors v ON v.id = d.vendor_id WHERE d.id = $1) x"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDispute {
    pub discrepancy_id: Uuid,
}

pub async fn create_dispute(user: AuthedUser, State(state): State<AppState>, Json(req): Json<NewDispute>) -> Result<(StatusCode, Json<Value>), AppError> {
    user.require(Permission::ManageDisputes)?;
    let mut tx = scoped_tx(&state, &user).await?;
    ensure_owned(&mut tx, "discrepancies", req.discrepancy_id).await?;
    let row: (String, Decimal, Uuid, Uuid) = sqlx::query_as(
        "SELECT d.review_status::text, d.amount, d.reconciliation_id, i.vendor_id FROM discrepancies d JOIN reconciliations r ON r.id = d.reconciliation_id JOIN invoices i ON i.id = r.invoice_id WHERE d.id = $1 FOR UPDATE OF d",
    ).bind(req.discrepancy_id).fetch_one(&mut *tx).await?;
    let (status, amount, reconciliation_id, vendor_id) = row;
    if status != "CONFIRMED" {
        return Err(AppError::Conflict("only a CONFIRMED discrepancy can be disputed".into()));
    }
    if amount <= Decimal::ZERO {
        return Err(AppError::Conflict("a discrepancy with no recoverable dollar amount cannot be disputed".into()));
    }
    let active: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM disputes WHERE discrepancy_id = $1 AND status <> 'CLOSED')").bind(req.discrepancy_id).fetch_one(&mut *tx).await?;
    if active {
        return Err(AppError::Conflict("an open dispute already exists for this discrepancy".into()));
    }
    let id: Uuid = sqlx::query_scalar("INSERT INTO disputes (organization_id, discrepancy_id, vendor_id, disputed_amount) VALUES ($1, $2, $3, $4) RETURNING id")
        .bind(user.organization_id).bind(req.discrepancy_id).bind(vendor_id).bind(amount).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO credits (organization_id, dispute_id, identified_amount) VALUES ($1, $2, $3)").bind(user.organization_id).bind(id).bind(amount).execute(&mut *tx).await?;
    sqlx::query("UPDATE discrepancies SET review_status = 'DISPUTED', updated_at = now() WHERE id = $1").bind(req.discrepancy_id).execute(&mut *tx).await?;
    refresh_reconciliation_status(&mut tx, reconciliation_id).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "dispute.create", "dispute", Some(id), None, Some(json!({ "discrepancy_id": req.discrepancy_id, "disputed_amount": amount.to_string() }))).await?;
    let body = json_one(&mut tx, DISPUTE_SQL, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(body)))
}

pub async fn list_disputes(user: AuthedUser, State(state): State<AppState>, Query(page): Query<Page>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (
           SELECT d.id, d.discrepancy_id, v.name AS vendor_name, d.status::text AS status, d.disputed_amount::text AS disputed_amount, d.confirmed_amount::text AS confirmed_amount, d.created_at
             FROM disputes d JOIN vendors v ON v.id = d.vendor_id ORDER BY d.created_at DESC, d.id LIMIT $1 OFFSET $2) t",
    ).bind(page.limit()).bind(page.offset()).fetch_one(&mut *tx).await?;
    Ok(Json(v))
}

pub async fn get_dispute(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, DISPUTE_SQL, id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionRequest {
    pub to: String,
    /// CREDIT_PROMISED: the credit amount the vendor agreed to. CREDIT_RECEIVED: the amount actually received.
    pub amount: Option<ExactDecimal>,
    /// CREDIT_RECEIVED: the vendor's credit memo / reference number.
    pub credit_memo_reference: Option<String>,
}

pub async fn transition_dispute(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>, Json(req): Json<TransitionRequest>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ManageDisputes)?;
    let to = DisputeStatus::from_db(&req.to.to_ascii_uppercase()).ok_or_else(|| AppError::Validation("unknown target status".into()))?;
    if to == DisputeStatus::Verified {
        user.require(Permission::VerifyCredits)?;
    }
    let mut tx = scoped_tx(&state, &user).await?;
    let row: Option<(String, Decimal, Option<Decimal>, Uuid, Uuid)> = sqlx::query_as(
        "SELECT status::text, disputed_amount, confirmed_amount, discrepancy_id, vendor_id FROM disputes WHERE id = $1 FOR UPDATE",
    ).bind(id).fetch_optional(&mut *tx).await?;
    let (cur, disputed, confirmed, discrepancy_id, _) = row.ok_or(AppError::NotFound)?;
    let from = DisputeStatus::from_db(&cur).ok_or_else(|| AppError::Internal(anyhow::anyhow!("unknown stored dispute status")))?;
    if !dispute_allowed(from, to) {
        return Err(AppError::Conflict(format!("a dispute in {cur} cannot move to {}", to.as_db())));
    }
    match to {
        DisputeStatus::CreditPromised => {
            let a = req.amount.ok_or_else(|| AppError::Validation("amount (credit promised by the vendor) is required".into()))?.get();
            if a < Decimal::ZERO || a > disputed {
                return Err(AppError::Validation("the promised credit must be between 0 and the disputed amount".into()));
            }
            sqlx::query("UPDATE disputes SET confirmed_amount = $2 WHERE id = $1").bind(id).bind(a).execute(&mut *tx).await?;
            sqlx::query("UPDATE credits SET confirmed_amount = $2 WHERE dispute_id = $1").bind(id).bind(a).execute(&mut *tx).await?;
        }
        DisputeStatus::CreditReceived => {
            let a = req.amount.ok_or_else(|| AppError::Validation("amount (credit actually received) is required".into()))?.get();
            let memo = req.credit_memo_reference.as_deref().map(str::trim).filter(|m| !m.is_empty()).ok_or_else(|| AppError::Validation("credit_memo_reference is required".into()))?;
            if a < Decimal::ZERO || a > confirmed.unwrap_or(Decimal::ZERO) {
                return Err(AppError::Validation("the received credit must be between 0 and the promised credit".into()));
            }
            if memo.chars().count() > 200 {
                return Err(AppError::Validation("credit_memo_reference must be at most 200 characters".into()));
            }
            sqlx::query("UPDATE credits SET recovered_amount = $2, credit_memo_reference = $3 WHERE dispute_id = $1").bind(id).bind(a).bind(memo).execute(&mut *tx).await?;
        }
        DisputeStatus::Verified => {
            sqlx::query("UPDATE credits SET verified_by = $2, verified_at = now() WHERE dispute_id = $1").bind(id).bind(user.user_id).execute(&mut *tx).await?;
        }
        _ => {}
    }
    sqlx::query("UPDATE disputes SET status = $2::dispute_status, updated_at = now(), submitted_at = CASE WHEN $2 = 'SUBMITTED' THEN now() ELSE submitted_at END,
                    resolved_at = CASE WHEN $2 = 'CLOSED' THEN now() ELSE resolved_at END WHERE id = $1")
        .bind(id).bind(to.as_db()).execute(&mut *tx).await?;
    if to == DisputeStatus::Closed {
        let rec_id: Uuid = sqlx::query_scalar("UPDATE discrepancies SET review_status = 'RESOLVED', updated_at = now() WHERE id = $1 RETURNING reconciliation_id").bind(discrepancy_id).fetch_one(&mut *tx).await?;
        refresh_reconciliation_status(&mut tx, rec_id).await?;
    }
    audit::record(&mut tx, user.organization_id, Some(user.user_id), &format!("dispute.{}", to.as_db().to_ascii_lowercase()), "dispute", Some(id),
        Some(json!({ "status": cur })), Some(json!({ "status": to.as_db(), "amount": req.amount.map(|a| a.get().to_string()) }))).await?;
    let body = json_one(&mut tx, DISPUTE_SQL, id).await?;
    tx.commit().await?;
    Ok(Json(body))
}

/// Recovery summary. "Recovered" counts ONLY credits that a person verified; credits that were
/// received but not yet verified are reported separately, so the headline number is never inflated.
pub async fn recovery_summary(user: AuthedUser, State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        r#"SELECT jsonb_build_object(
             'identified', coalesce(sum(c.identified_amount), 0)::numeric(20,4)::text,
             'confirmed', coalesce(sum(c.confirmed_amount), 0)::numeric(20,4)::text,
             'recovered_verified', coalesce(sum(c.recovered_amount) FILTER (WHERE c.verified_at IS NOT NULL), 0)::numeric(20,4)::text,
             'received_pending_verification', coalesce(sum(c.recovered_amount) FILTER (WHERE c.verified_at IS NULL), 0)::numeric(20,4)::text,
             'unrecovered', (coalesce(sum(c.confirmed_amount), 0) - coalesce(sum(c.recovered_amount) FILTER (WHERE c.verified_at IS NOT NULL), 0))::numeric(20,4)::text,
             'open_disputes', (SELECT count(*) FROM disputes WHERE status <> 'CLOSED'))
           FROM credits c"#,
    ).fetch_one(&mut *tx).await?;
    Ok(Json(v))
}
