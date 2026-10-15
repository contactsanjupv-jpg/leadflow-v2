//! Source-data endpoints: vendors, contracts + rates, invoices, manifests, weight tickets.
//! Every handler declares its RBAC permission, runs in a tenant-scoped transaction, verifies
//! ownership of every referenced id (FK checks bypass RLS), and writes an audit row in the same
//! transaction. Money and quantities arrive as decimal strings (`ExactDecimal`) and leave as
//! `NUMERIC(14,4)` text, so no float ever touches them.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit;
use crate::common::money::ExactDecimal;
use crate::http::authed_user::AuthedUser;
use crate::http::errors::AppError;
use crate::http::state::AppState;
use crate::http::util::*;
use crate::rbac::Permission;

type Created = Result<(StatusCode, Json<Value>), AppError>;

// ------------------------------------------------------------------ vendors

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewVendor {
    pub name: String,
}

pub async fn create_vendor(user: AuthedUser, State(state): State<AppState>, Json(req): Json<NewVendor>) -> Created {
    user.require(Permission::ManageData)?;
    let name = clean_text("name", &req.name, 200)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO vendors (organization_id, name) VALUES ($1, $2) RETURNING id").bind(user.organization_id).bind(&name).fetch_one(&mut *tx).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "vendor.create", "vendor", Some(id), None, Some(json!({ "name": name }))).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name }))))
}

pub async fn list_vendors(user: AuthedUser, State(state): State<AppState>, Query(page): Query<Page>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.name, t.id), '[]'::jsonb) FROM (SELECT id, name, created_at FROM vendors ORDER BY name, id LIMIT $1 OFFSET $2) t")
        .bind(page.limit())
        .bind(page.offset())
        .fetch_one(&mut *tx)
        .await?;
    Ok(Json(v))
}

// ---------------------------------------------------------------- contracts

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewContract {
    pub vendor_id: Uuid,
    pub name: String,
    pub source_document_id: Option<Uuid>,
}

const CONTRACT_DETAIL_SQL: &str = r#"
SELECT to_jsonb(c) || jsonb_build_object('rates', coalesce((
    SELECT jsonb_agg(to_jsonb(r) ORDER BY r.effective_from, r.id) FROM (
        SELECT cr.id, cr.waste_stream, cr.unit, cr.unit_rate::text AS unit_rate, cr.facility_id, cr.effective_from, cr.effective_to,
               (SELECT coalesce(jsonb_agg(jsonb_build_object('fee_code', f.fee_code, 'description', f.description, 'amount', f.amount::text) ORDER BY f.fee_code), '[]'::jsonb)
                  FROM contract_authorized_fees f WHERE f.contract_rate_id = cr.id) AS authorized_fees
          FROM contract_rates cr WHERE cr.contract_id = c.id) r), '[]'::jsonb))
FROM (SELECT id, vendor_id, name, source_document_id, created_at FROM contracts WHERE id = $1) c"#;

pub async fn create_contract(user: AuthedUser, State(state): State<AppState>, Json(req): Json<NewContract>) -> Created {
    user.require(Permission::ManageData)?;
    let name = clean_text("name", &req.name, 200)?;
    let mut tx = scoped_tx(&state, &user).await?;
    ensure_owned(&mut tx, "vendors", req.vendor_id).await?;
    if let Some(doc) = req.source_document_id {
        ensure_owned(&mut tx, "documents", doc).await?;
    }
    let id: Uuid = sqlx::query_scalar("INSERT INTO contracts (organization_id, vendor_id, name, source_document_id) VALUES ($1, $2, $3, $4) RETURNING id")
        .bind(user.organization_id).bind(req.vendor_id).bind(&name).bind(req.source_document_id).fetch_one(&mut *tx).await?;
    if let Some(d) = req.source_document_id { crate::http::documents::mark_used(&mut tx, d, "a contract").await?; }
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "contract.create", "contract", Some(id), None, Some(json!({ "vendor_id": req.vendor_id, "name": name }))).await?;
    let body = json_one(&mut tx, CONTRACT_DETAIL_SQL, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(body)))
}

pub async fn list_contracts(user: AuthedUser, State(state): State<AppState>, Query(page): Query<Page>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.name, t.id), '[]'::jsonb)
         FROM (SELECT c.id, c.name, c.vendor_id, v.name AS vendor_name, c.created_at FROM contracts c JOIN vendors v ON v.id = c.vendor_id ORDER BY c.name, c.id LIMIT $1 OFFSET $2) t",
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}

pub async fn get_contract(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, CONTRACT_DETAIL_SQL, id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewFee {
    pub fee_code: String,
    pub description: Option<String>,
    pub amount: ExactDecimal,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRate {
    pub waste_stream: String,
    pub unit: String,
    pub unit_rate: ExactDecimal,
    pub facility_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    #[serde(default)]
    pub authorized_fees: Vec<NewFee>,
}

pub async fn create_rate(user: AuthedUser, State(state): State<AppState>, Path(contract_id): Path<Uuid>, Json(req): Json<NewRate>) -> Created {
    user.require(Permission::ManageData)?;
    let waste_stream = clean_text("waste_stream", &req.waste_stream, 100)?;
    let unit = clean_text("unit", &req.unit, 50)?;
    if req.unit_rate.get() < Decimal::ZERO {
        return Err(AppError::Validation("unit_rate must not be negative".into()));
    }
    check_date("effective_from", req.effective_from)?;
    if let Some(to) = req.effective_to { check_date("effective_to", to)?; }
    if req.effective_to.map_or(false, |to| to < req.effective_from) {
        return Err(AppError::Validation("effective_to must not be before effective_from".into()));
    }
    let mut fees = Vec::with_capacity(req.authorized_fees.len());
    for f in &req.authorized_fees {
        if f.amount.get() < Decimal::ZERO {
            return Err(AppError::Validation("authorized fee amounts must not be negative".into()));
        }
        fees.push((clean_text("fee_code", &f.fee_code, 50)?, f.description.as_deref().map(|d| d.trim().to_string()).filter(|d| !d.is_empty()), f.amount.get()));
    }

    let mut tx = scoped_tx(&state, &user).await?;
    ensure_owned(&mut tx, "contracts", contract_id).await?;
    let vendor_id: Uuid = sqlx::query_scalar("SELECT vendor_id FROM contracts WHERE id = $1").bind(contract_id).fetch_one(&mut *tx).await?;
    let rate_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contract_rates (organization_id, contract_id, vendor_id, facility_id, waste_stream, unit, unit_rate, effective_from, effective_to)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(user.organization_id).bind(contract_id).bind(vendor_id).bind(req.facility_id).bind(&waste_stream).bind(&unit)
    .bind(req.unit_rate.get()).bind(req.effective_from).bind(req.effective_to)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| conflict_on_constraint(e, "an overlapping rate already exists for this vendor, waste stream and facility"))?;
    for (code, description, amount) in &fees {
        sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, description, amount) VALUES ($1, $2, $3, $4)")
            .bind(rate_id).bind(code).bind(description).bind(amount).execute(&mut *tx).await
            .map_err(|e| conflict_on_constraint(e, "duplicate fee_code within this rate"))?;
    }
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "contract_rate.create", "contract_rate", Some(rate_id), None,
        Some(json!({ "contract_id": contract_id, "waste_stream": waste_stream, "unit": unit, "unit_rate": req.unit_rate.get().to_string(), "effective_from": req.effective_from, "effective_to": req.effective_to }))).await?;
    let body = json_one(&mut tx, CONTRACT_DETAIL_SQL, contract_id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(body)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndRate {
    pub effective_to: NaiveDate,
}

/// Rates are otherwise immutable: a price change is a new rate with its own effective window,
/// which keeps the history the reconciliation engine relies on. Only the end date can change.
pub async fn end_rate(user: AuthedUser, State(state): State<AppState>, Path(rate_id): Path<Uuid>, Json(req): Json<EndRate>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ManageData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    check_date("effective_to", req.effective_to)?;
    let row: Option<(NaiveDate, Option<NaiveDate>, Uuid)> = sqlx::query_as("SELECT effective_from, effective_to, contract_id FROM contract_rates WHERE id = $1 FOR UPDATE").bind(rate_id).fetch_optional(&mut *tx).await?;
    let (from, old_to, contract_id) = row.ok_or(AppError::NotFound)?;
    if req.effective_to < from {
        return Err(AppError::Validation("effective_to must not be before effective_from".into()));
    }
    sqlx::query("UPDATE contract_rates SET effective_to = $2 WHERE id = $1").bind(rate_id).bind(req.effective_to).execute(&mut *tx).await
        .map_err(|e| conflict_on_constraint(e, "the new end date overlaps another rate"))?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "contract_rate.set_end_date", "contract_rate", Some(rate_id),
        Some(json!({ "effective_to": old_to })), Some(json!({ "effective_to": req.effective_to }))).await?;
    let body = json_one(&mut tx, CONTRACT_DETAIL_SQL, contract_id).await?;
    tx.commit().await?;
    Ok(Json(body))
}

// ------------------------------------------------------------------ invoices

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewLine {
    pub description: String,
    pub fee_code: Option<String>,
    pub quantity: ExactDecimal,
    pub unit: String,
    pub unit_price: ExactDecimal,
    pub line_total: ExactDecimal,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewInvoice {
    pub vendor_id: Uuid,
    pub invoice_number: String,
    pub invoice_date: NaiveDate,
    pub service_date: NaiveDate,
    pub waste_stream: String,
    pub facility_id: Option<Uuid>,
    pub manifest_id: Option<Uuid>,
    pub source_document_id: Option<Uuid>,
    pub total: ExactDecimal,
    pub line_items: Vec<NewLine>,
}

const INVOICE_DETAIL_SQL: &str = r#"
SELECT to_jsonb(i) || jsonb_build_object(
  'lines', (SELECT coalesce(jsonb_agg(to_jsonb(l) ORDER BY l.id), '[]'::jsonb) FROM (
      SELECT id, description, fee_code, quantity::text AS quantity, unit, unit_price::text AS unit_price, line_total::text AS line_total
        FROM invoice_line_items WHERE invoice_id = i.id) l),
  'latest_reconciliation', (SELECT to_jsonb(r) FROM (
      SELECT id, review_status::text AS review_status, expected_amount::text AS expected_amount, invoiced_amount::text AS invoiced_amount,
             discrepancy_amount::text AS discrepancy_amount, created_at
        FROM reconciliations WHERE invoice_id = i.id ORDER BY created_at DESC, id DESC LIMIT 1) r))
FROM (SELECT id, vendor_id, invoice_number, invoice_date, service_date, facility_id, waste_stream, total::text AS total,
             source_document_id, manifest_id, created_at, updated_at FROM invoices WHERE id = $1) i"#;

pub async fn create_invoice(user: AuthedUser, State(state): State<AppState>, Json(req): Json<NewInvoice>) -> Created {
    user.require(Permission::ManageData)?;
    let invoice_number = clean_text("invoice_number", &req.invoice_number, 100)?;
    let waste_stream = clean_text("waste_stream", &req.waste_stream, 100)?;
    check_date("invoice_date", req.invoice_date)?;
    check_date("service_date", req.service_date)?;
    if req.line_items.is_empty() {
        return Err(AppError::Validation("an invoice needs at least one line item".into()));
    }
    if req.line_items.len() > 500 {
        return Err(AppError::Validation("too many line items (max 500)".into()));
    }
    let mut lines = Vec::with_capacity(req.line_items.len());
    for l in &req.line_items {
        let fee_code = match &l.fee_code {
            Some(c) => Some(clean_text("fee_code", c, 50)?),
            None => None,
        };
        lines.push((clean_text("description", &l.description, 500)?, fee_code, l.quantity.get(), clean_text("unit", &l.unit, 50)?, l.unit_price.get(), l.line_total.get()));
    }

    let mut tx = scoped_tx(&state, &user).await?;
    ensure_owned(&mut tx, "vendors", req.vendor_id).await?;
    if let Some(m) = req.manifest_id { ensure_owned(&mut tx, "manifests", m).await?; }
    if let Some(d) = req.source_document_id { ensure_owned(&mut tx, "documents", d).await?; }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO invoices (organization_id, vendor_id, invoice_number, invoice_date, service_date, facility_id, waste_stream, total, source_document_id, manifest_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(user.organization_id).bind(req.vendor_id).bind(&invoice_number).bind(req.invoice_date).bind(req.service_date)
    .bind(req.facility_id).bind(&waste_stream).bind(req.total.get()).bind(req.source_document_id).bind(req.manifest_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| conflict_on_constraint(e, "this vendor already has an invoice with that number"))?;
    for (description, fee_code, quantity, unit, unit_price, line_total) in &lines {
        sqlx::query("INSERT INTO invoice_line_items (invoice_id, organization_id, description, fee_code, quantity, unit, unit_price, line_total) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(id).bind(user.organization_id).bind(description).bind(fee_code).bind(quantity).bind(unit).bind(unit_price).bind(line_total)
            .execute(&mut *tx).await?;
    }
    if let Some(d) = req.source_document_id { crate::http::documents::mark_used(&mut tx, d, "an invoice").await?; }
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "invoice.create", "invoice", Some(id), None,
        Some(json!({ "vendor_id": req.vendor_id, "invoice_number": invoice_number, "total": req.total.get().to_string(), "line_count": lines.len(), "manifest_id": req.manifest_id }))).await?;
    let body = json_one(&mut tx, INVOICE_DETAIL_SQL, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(body)))
}

pub async fn list_invoices(user: AuthedUser, State(state): State<AppState>, Query(page): Query<Page>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.invoice_date DESC, t.id), '[]'::jsonb) FROM (
             SELECT i.id, i.invoice_number, i.invoice_date, i.service_date, i.waste_stream, i.total::text AS total, i.vendor_id, v.name AS vendor_name,
                    i.manifest_id, lr.review_status::text AS review_status, lr.discrepancy_amount::text AS discrepancy_amount
               FROM invoices i JOIN vendors v ON v.id = i.vendor_id
               LEFT JOIN LATERAL (SELECT review_status, discrepancy_amount FROM reconciliations r WHERE r.invoice_id = i.id ORDER BY created_at DESC, id DESC LIMIT 1) lr ON true
              ORDER BY i.invoice_date DESC, i.id LIMIT $1 OFFSET $2) t"#,
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}

pub async fn get_invoice(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, INVOICE_DETAIL_SQL, id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkManifest {
    pub manifest_id: Option<Uuid>,
}

pub async fn link_invoice_manifest(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>, Json(req): Json<LinkManifest>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ManageData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    ensure_owned(&mut tx, "invoices", id).await?;
    if let Some(m) = req.manifest_id { ensure_owned(&mut tx, "manifests", m).await?; }
    let old: Option<Uuid> = sqlx::query_scalar("SELECT manifest_id FROM invoices WHERE id = $1 FOR UPDATE").bind(id).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE invoices SET manifest_id = $2, updated_at = now() WHERE id = $1").bind(id).bind(req.manifest_id).execute(&mut *tx).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "invoice.link_manifest", "invoice", Some(id), Some(json!({ "manifest_id": old })), Some(json!({ "manifest_id": req.manifest_id }))).await?;
    let body = json_one(&mut tx, INVOICE_DETAIL_SQL, id).await?;
    tx.commit().await?;
    Ok(Json(body))
}

// ----------------------------------------------------------------- manifests

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewManifest {
    pub manifest_number: String,
    pub waste_stream: String,
    pub container_count: Option<i32>,
    pub quantity: ExactDecimal,
    pub unit: String,
    pub shipment_date: NaiveDate,
    pub source_document_id: Option<Uuid>,
}

const MANIFEST_DETAIL_SQL: &str = r#"
SELECT to_jsonb(m) || jsonb_build_object(
  'weight_tickets', (SELECT coalesce(jsonb_agg(to_jsonb(w) ORDER BY w.ticket_date, w.id), '[]'::jsonb) FROM (
      SELECT id, ticket_number, net_weight::text AS net_weight, unit, container_count, ticket_date FROM weight_records WHERE manifest_id = m.id) w),
  'invoices', (SELECT coalesce(jsonb_agg(to_jsonb(i) ORDER BY i.invoice_date, i.id), '[]'::jsonb) FROM (
      SELECT id, invoice_number, invoice_date, total::text AS total FROM invoices WHERE manifest_id = m.id) i))
FROM (SELECT id, manifest_number, waste_stream, container_count, quantity::text AS quantity, unit, shipment_date, source_document_id, created_at FROM manifests WHERE id = $1) m"#;

pub async fn create_manifest(user: AuthedUser, State(state): State<AppState>, Json(req): Json<NewManifest>) -> Created {
    user.require(Permission::ManageData)?;
    let manifest_number = clean_text("manifest_number", &req.manifest_number, 100)?;
    let waste_stream = clean_text("waste_stream", &req.waste_stream, 100)?;
    let unit = clean_text("unit", &req.unit, 50)?;
    check_date("shipment_date", req.shipment_date)?;
    if req.quantity.get() <= Decimal::ZERO {
        return Err(AppError::Validation("quantity must be greater than zero".into()));
    }
    if req.container_count.map_or(false, |c| c < 0) {
        return Err(AppError::Validation("container_count must not be negative".into()));
    }
    let mut tx = scoped_tx(&state, &user).await?;
    if let Some(d) = req.source_document_id { ensure_owned(&mut tx, "documents", d).await?; }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO manifests (organization_id, manifest_number, waste_stream, container_count, quantity, unit, shipment_date, source_document_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(user.organization_id).bind(&manifest_number).bind(&waste_stream).bind(req.container_count).bind(req.quantity.get()).bind(&unit).bind(req.shipment_date).bind(req.source_document_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| conflict_on_constraint(e, "a manifest with that number already exists"))?;
    if let Some(d) = req.source_document_id { crate::http::documents::mark_used(&mut tx, d, "a manifest").await?; }
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "manifest.create", "manifest", Some(id), None,
        Some(json!({ "manifest_number": manifest_number, "waste_stream": waste_stream, "quantity": req.quantity.get().to_string(), "unit": unit }))).await?;
    let body = json_one(&mut tx, MANIFEST_DETAIL_SQL, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(body)))
}

pub async fn list_manifests(user: AuthedUser, State(state): State<AppState>, Query(page): Query<Page>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.shipment_date DESC, t.id), '[]'::jsonb) FROM (
           SELECT id, manifest_number, waste_stream, container_count, quantity::text AS quantity, unit, shipment_date FROM manifests ORDER BY shipment_date DESC, id LIMIT $1 OFFSET $2) t",
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}

pub async fn get_manifest(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, MANIFEST_DETAIL_SQL, id).await?))
}

// ------------------------------------------------------------- weight tickets

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewWeightTicket {
    pub ticket_number: String,
    pub manifest_id: Option<Uuid>,
    pub net_weight: ExactDecimal,
    pub unit: String,
    pub container_count: Option<i32>,
    pub ticket_date: NaiveDate,
    pub source_document_id: Option<Uuid>,
}

const WEIGHT_DETAIL_SQL: &str = "SELECT to_jsonb(w) FROM (SELECT id, ticket_number, manifest_id, net_weight::text AS net_weight, unit, container_count, ticket_date, source_document_id, created_at FROM weight_records WHERE id = $1) w";

pub async fn create_weight_ticket(user: AuthedUser, State(state): State<AppState>, Json(req): Json<NewWeightTicket>) -> Created {
    user.require(Permission::ManageData)?;
    let ticket_number = clean_text("ticket_number", &req.ticket_number, 100)?;
    let unit = clean_text("unit", &req.unit, 50)?;
    check_date("ticket_date", req.ticket_date)?;
    if req.net_weight.get() <= Decimal::ZERO {
        return Err(AppError::Validation("net_weight must be greater than zero".into()));
    }
    if req.container_count.map_or(false, |c| c < 0) {
        return Err(AppError::Validation("container_count must not be negative".into()));
    }
    let mut tx = scoped_tx(&state, &user).await?;
    if let Some(m) = req.manifest_id { ensure_owned(&mut tx, "manifests", m).await?; }
    if let Some(d) = req.source_document_id { ensure_owned(&mut tx, "documents", d).await?; }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO weight_records (organization_id, manifest_id, ticket_number, net_weight, unit, container_count, ticket_date, source_document_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(user.organization_id).bind(req.manifest_id).bind(&ticket_number).bind(req.net_weight.get()).bind(&unit).bind(req.container_count).bind(req.ticket_date).bind(req.source_document_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| conflict_on_constraint(e, "a weight ticket with that number already exists"))?;
    if let Some(d) = req.source_document_id { crate::http::documents::mark_used(&mut tx, d, "a weight ticket").await?; }
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "weight_ticket.create", "weight_ticket", Some(id), None,
        Some(json!({ "ticket_number": ticket_number, "manifest_id": req.manifest_id, "net_weight": req.net_weight.get().to_string(), "unit": unit }))).await?;
    let body = json_one(&mut tx, WEIGHT_DETAIL_SQL, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(body)))
}

pub async fn list_weight_tickets(user: AuthedUser, State(state): State<AppState>, Query(page): Query<Page>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.ticket_date DESC, t.id), '[]'::jsonb) FROM (
           SELECT id, ticket_number, manifest_id, net_weight::text AS net_weight, unit, container_count, ticket_date FROM weight_records ORDER BY ticket_date DESC, id LIMIT $1 OFFSET $2) t",
    )
    .bind(page.limit())
    .bind(page.offset())
    .fetch_one(&mut *tx)
    .await?;
    Ok(Json(v))
}

pub async fn get_weight_ticket(user: AuthedUser, State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    Ok(Json(json_one(&mut tx, WEIGHT_DETAIL_SQL, id).await?))
}