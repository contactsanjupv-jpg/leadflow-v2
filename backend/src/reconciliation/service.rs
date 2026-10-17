//! Reconciliation workflow service: load the real inputs for an invoice (rate, manifest, weight
//! tickets), run the deterministic engine, and persist the reconciliation plus one
//! `discrepancies` row per finding. Shared by the HTTP handler and the monitoring worker so
//! both take exactly the same path. The engine itself is untouched.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::common::money::Money;
use crate::http::errors::AppError;
use crate::reconciliation::engine;
use crate::reconciliation::model::*;

pub struct RunOutcome {
    pub reconciliation_id: Uuid,
    pub result: ReconciliationResult,
    pub discrepancy_ids: Vec<Uuid>,
    pub manifest_id: Option<Uuid>,
    pub weight_ticket_count: usize,
}

type InvoiceRow = (
    Uuid,
    Uuid,
    String,
    NaiveDate,
    NaiveDate,
    Option<Uuid>,
    Option<String>,
    Decimal,
    Option<Uuid>,
    Option<Uuid>,
);
type LineRow = (
    Uuid,
    String,
    Option<String>,
    Decimal,
    String,
    Decimal,
    Decimal,
);
type RateRow = (
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    String,
    Decimal,
    NaiveDate,
    Option<NaiveDate>,
);
type ManifestRow = (
    Uuid,
    String,
    String,
    Option<i32>,
    Decimal,
    String,
    NaiveDate,
    Option<Uuid>,
);
type TicketRow = (
    Uuid,
    Option<Uuid>,
    String,
    Decimal,
    String,
    Option<i32>,
    NaiveDate,
    Option<Uuid>,
);
type PrevDiscrepancyRow = (
    String,
    String,
    String,
    String,
    String,
    Option<Uuid>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
);

/// Load a rate's authorized fees and build the domain `ContractRate`.
async fn load_rate(
    tx: &mut Transaction<'_, Postgres>,
    row: RateRow,
) -> Result<ContractRate, AppError> {
    let (id, vendor_id, facility_id, waste_stream, unit, unit_rate, effective_from, effective_to) =
        row;
    let fees: Vec<(String, Option<String>, Decimal)> =
        sqlx::query_as("SELECT fee_code, description, amount FROM contract_authorized_fees WHERE contract_rate_id = $1 ORDER BY fee_code")
            .bind(id)
            .fetch_all(&mut **tx)
            .await?;
    Ok(ContractRate {
        id,
        vendor_id,
        waste_stream,
        facility_id,
        unit,
        effective_from,
        effective_to,
        unit_rate: Money::from_decimal(unit_rate),
        authorized_fees: fees
            .into_iter()
            .map(|(fee_code, description, amount)| AuthorizedFee {
                fee_code,
                description: description.unwrap_or_default(),
                amount: Money::from_decimal(amount),
            })
            .collect(),
    })
}

/// Run (or re-run) the reconciliation for one invoice inside an already tenant-scoped transaction.
pub async fn run_for_invoice(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    invoice_id: Uuid,
) -> Result<RunOutcome, AppError> {
    let inv: InvoiceRow = sqlx::query_as(
        "SELECT id, vendor_id, invoice_number, invoice_date, service_date, facility_id, waste_stream, total, source_document_id, manifest_id
         FROM invoices WHERE id = $1",
    )
    .bind(invoice_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let (
        _,
        vendor_id,
        invoice_number,
        invoice_date,
        service_date,
        facility_id,
        waste_stream,
        total,
        source_document_id,
        manifest_id,
    ) = inv;

    let lines: Vec<LineRow> = sqlx::query_as(
        "SELECT id, description, fee_code, quantity, unit, unit_price, line_total FROM invoice_line_items WHERE invoice_id = $1 ORDER BY id",
    )
    .bind(invoice_id)
    .fetch_all(&mut **tx)
    .await?;

    let invoice = Invoice {
        id: invoice_id,
        organization_id,
        vendor_id,
        invoice_number,
        invoice_date,
        service_date,
        facility_id,
        waste_stream: waste_stream.unwrap_or_default(),
        line_items: lines
            .into_iter()
            .map(
                |(id, description, fee_code, quantity, unit, unit_price, line_total)| {
                    InvoiceLineItem {
                        id,
                        description,
                        fee_code,
                        quantity,
                        unit,
                        unit_price: Money::from_decimal(unit_price),
                        line_total: Money::from_decimal(line_total),
                    }
                },
            )
            .collect(),
        total: Money::from_decimal(total),
        source_document_id,
    };

    // Rate lookup. Streams are compared by the canonical key (case/whitespace folded; the text
    // as entered is preserved). A rate is APPLICABLE only if its window covers the service date;
    // facility-specific beats vendor-wide; another facility's rate is never a fallback. A blank
    // invoice stream has no key and matches nothing.
    let stream_key = crate::common::waste_stream::key(&invoice.waste_stream);

    let mut applicable_rate: Option<ContractRate> = None;
    let mut stale_rate: Option<ContractRate> = None;
    if let Some(key) = stream_key.as_deref() {
        let covering: Option<RateRow> = sqlx::query_as(
            r#"
            SELECT id, vendor_id, facility_id, waste_stream, unit,
                   unit_rate, effective_from, effective_to
            FROM contract_rates
            WHERE vendor_id = $1
              AND waste_stream_key = $2
              AND effective_from <= $3
              AND (effective_to IS NULL OR effective_to >= $3)
              AND (facility_id IS NULL OR facility_id = $4)
            ORDER BY
              CASE
                WHEN $4 IS NOT NULL AND facility_id = $4 THEN 0
                ELSE 1
              END ASC,
              effective_from DESC
            LIMIT 1
            "#,
        )
        .bind(invoice.vendor_id)
        .bind(key)
        .bind(invoice.service_date)
        .bind(invoice.facility_id)
        .fetch_optional(&mut **tx)
        .await?;
        match covering {
            Some(row) => applicable_rate = Some(load_rate(tx, row).await?),
            None => {
                // Nothing covers the service date. If an EXPIRED rate exists for the same scope,
                // keep it for reporting only (it must never price the invoice).
                let expired: Option<RateRow> = sqlx::query_as(
                    r#"
                    SELECT id, vendor_id, facility_id, waste_stream, unit,
                           unit_rate, effective_from, effective_to
                    FROM contract_rates
                    WHERE vendor_id = $1
                      AND waste_stream_key = $2
                      AND effective_from <= $3
                      AND effective_to IS NOT NULL AND effective_to < $3
                      AND (facility_id IS NULL OR facility_id = $4)
                    ORDER BY
                      CASE
                        WHEN $4 IS NOT NULL AND facility_id = $4 THEN 0
                        ELSE 1
                      END ASC,
                      effective_to DESC
                    LIMIT 1
                    "#,
                )
                .bind(invoice.vendor_id)
                .bind(key)
                .bind(invoice.service_date)
                .bind(invoice.facility_id)
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(row) = expired {
                    stale_rate = Some(load_rate(tx, row).await?);
                }
            }
        }
    }

    // Supporting documents come from the invoice's explicit manifest link.
    let mut manifest: Option<Manifest> = None;
    let mut weight_ticket: Option<WeightTicket> = None;
    let mut first_ticket_id: Option<Uuid> = None;
    let mut ticket_count = 0usize;
    let mut extra_trace: Vec<String> = Vec::new();

    if let Some(mid) = manifest_id {
        let m: Option<ManifestRow> = sqlx::query_as(
            "SELECT id, manifest_number, waste_stream, container_count, quantity, unit, shipment_date, source_document_id FROM manifests WHERE id = $1",
        )
        .bind(mid)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((
            id,
            manifest_number,
            waste_stream,
            container_count,
            quantity,
            unit,
            shipment_date,
            source_document_id,
        )) = m
        {
            manifest = Some(Manifest {
                id,
                organization_id,
                manifest_number,
                waste_stream,
                container_count,
                quantity,
                unit,
                shipment_date,
                source_document_id,
            });
        }

        let tickets: Vec<TicketRow> = sqlx::query_as(
            "SELECT id, manifest_id, ticket_number, net_weight, unit, container_count, ticket_date, source_document_id
             FROM weight_records WHERE manifest_id = $1 ORDER BY ticket_date, created_at, id",
        )
        .bind(mid)
        .fetch_all(&mut **tx)
        .await?;
        if let Some(first) = tickets.first() {
            // A load can be weighed on several tickets: aggregate every ticket in the first
            // ticket's unit. Tickets in a different unit are NOT converted (no invented unit
            // conversion); they are reported in the trace instead.
            let unit = first.4.clone();
            let same: Vec<&TicketRow> = tickets.iter().filter(|t| t.4 == unit).collect();
            let skipped = tickets.len() - same.len();
            let net: Decimal = same.iter().map(|t| t.3).sum();
            let containers: Option<i32> = if same.iter().any(|t| t.5.is_some()) {
                Some(same.iter().filter_map(|t| t.5).sum())
            } else {
                None
            };
            let numbers = same
                .iter()
                .map(|t| t.2.as_str())
                .collect::<Vec<_>>()
                .join("+");
            first_ticket_id = Some(first.0);
            ticket_count = same.len();
            extra_trace.push(format!(
                "Weight support: {} ticket(s) [{}] aggregated to {} {}",
                same.len(),
                numbers,
                net,
                unit
            ));
            if skipped > 0 {
                extra_trace.push(format!("{skipped} weight ticket(s) in a different unit than '{unit}' were not included (no unit conversion is applied)"));
            }
            weight_ticket = Some(WeightTicket {
                id: first.0,
                organization_id,
                manifest_id: Some(mid),
                ticket_number: numbers,
                net_weight: net,
                unit,
                container_count: containers,
                ticket_date: first.6,
                source_document_id: first.7,
            });
        }
    }

    // Earlier-entered invoices from the same vendor for the same service date (RULE_05 cross-invoice check).
    let related_rows: Vec<(Uuid, String, Uuid, String, Option<String>, Decimal, String, Decimal, Decimal)> = sqlx::query_as(
        "SELECT i.id, i.invoice_number, l.id, l.description, l.fee_code, l.quantity, l.unit, l.unit_price, l.line_total
           FROM invoices i JOIN invoice_line_items l ON l.invoice_id = i.id, invoices me
          WHERE me.id = $1 AND i.vendor_id = me.vendor_id AND i.id <> me.id AND i.service_date = me.service_date
            AND (i.invoice_date < me.invoice_date OR (i.invoice_date = me.invoice_date AND (i.created_at, i.id) < (me.created_at, me.id)))
          ORDER BY i.invoice_date, i.created_at, i.id, l.id",
    )
    .bind(invoice_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut related_invoices: Vec<RelatedInvoice> = Vec::new();
    for (rid, number, lid, description, fee_code, quantity, unit, unit_price, line_total) in
        related_rows
    {
        if related_invoices.last().map_or(true, |r| r.id != rid) {
            related_invoices.push(RelatedInvoice {
                id: rid,
                invoice_number: number,
                line_items: Vec::new(),
            });
        }
        related_invoices
            .last_mut()
            .unwrap()
            .line_items
            .push(InvoiceLineItem {
                id: lid,
                description,
                fee_code,
                quantity,
                unit,
                unit_price: Money::from_decimal(unit_price),
                line_total: Money::from_decimal(line_total),
            });
    }

    let input = ReconciliationInput {
        invoice,
        related_invoices,
        applicable_rate,
        stale_rate,
        manifest,
        weight_ticket,
        tolerances: Tolerances::default(),
    };
    let mut result = engine::run(&input);
    for line in extra_trace.into_iter().rev() {
        result.calculation_trace.insert(1, line);
    }

    // Carry review decisions forward from the previous run when the very same finding recurs.
    let previous: Vec<PrevDiscrepancyRow> = sqlx::query_as(
        "SELECT discrepancy_type::text, rule_id, description, amount::text, review_status::text, reviewed_by, reviewed_at, review_notes
         FROM discrepancies
         WHERE reconciliation_id = (SELECT id FROM reconciliations WHERE invoice_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1)",
    )
    .bind(invoice_id)
    .fetch_all(&mut **tx)
    .await?;

    let reconciliation_id: Uuid = sqlx::query_scalar(
        "INSERT INTO reconciliations
           (organization_id, invoice_id, contract_rate_id, manifest_id, weight_record_id, expected_amount, invoiced_amount, discrepancy_amount, review_status, tolerances_applied, calculation_trace)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::review_status, $10, $11) RETURNING id",
    )
    .bind(organization_id)
    .bind(invoice_id)
    .bind(input.applicable_rate.as_ref().map(|r| r.id))
    .bind(input.manifest.as_ref().map(|m| m.id))
    .bind(first_ticket_id)
    .bind(result.expected_amount.as_decimal())
    .bind(result.invoiced_amount.as_decimal())
    .bind(result.discrepancy_amount.as_decimal())
    .bind(result.review_status.as_db())
    .bind(serde_json::to_value(&result.tolerances_applied).map_err(|e| AppError::Internal(e.into()))?)
    .bind(serde_json::to_value(&result.calculation_trace).map_err(|e| AppError::Internal(e.into()))?)
    .fetch_one(&mut **tx)
    .await?;

    let mut discrepancy_ids = Vec::with_capacity(result.discrepancies.len());
    let mut statuses = Vec::with_capacity(result.discrepancies.len());
    for d in &result.discrepancies {
        let amount_text = d.amount.to_wire_string();
        let carried = previous.iter().find(|p| {
            p.0 == d.discrepancy_type.as_db()
                && p.1 == d.rule_id
                && p.2 == d.description
                && p.3 == amount_text
        });
        let (status, reviewed_by, reviewed_at, notes) = match carried {
            Some(p) => (p.4.clone(), p.5, p.6, p.7.clone()),
            None => ("NEW".to_string(), None, None, None),
        };
        statuses.push(ReviewStatus::from_db(&status).unwrap_or(ReviewStatus::New));
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO discrepancies
               (organization_id, reconciliation_id, discrepancy_type, rule_id, description, amount, confidence, review_status, reviewed_by, reviewed_at, review_notes)
             VALUES ($1, $2, $3::discrepancy_type, $4, $5, $6, $7, $8::review_status, $9, $10, $11) RETURNING id",
        )
        .bind(organization_id)
        .bind(reconciliation_id)
        .bind(d.discrepancy_type.as_db())
        .bind(&d.rule_id)
        .bind(&d.description)
        .bind(d.amount.as_decimal())
        .bind(d.confidence.as_db())
        .bind(&status)
        .bind(reviewed_by)
        .bind(reviewed_at)
        .bind(notes)
        .fetch_one(&mut **tx)
        .await?;
        discrepancy_ids.push(id);
    }

    // The engine reports NEW when findings exist; if earlier decisions were carried forward the
    // reconciliation status must reflect them.
    let rolled = ReviewStatus::rollup(&statuses);
    if rolled != result.review_status {
        sqlx::query("UPDATE reconciliations SET review_status = $1::review_status, updated_at = now() WHERE id = $2")
            .bind(rolled.as_db())
            .bind(reconciliation_id)
            .execute(&mut **tx)
            .await?;
        result.review_status = rolled;
    }

    Ok(RunOutcome {
        reconciliation_id,
        result,
        discrepancy_ids,
        manifest_id: input.manifest.as_ref().map(|m| m.id),
        weight_ticket_count: ticket_count,
    })
}

/// Recompute a reconciliation's status from its discrepancies (after a review action).
pub async fn refresh_reconciliation_status(
    tx: &mut Transaction<'_, Postgres>,
    reconciliation_id: Uuid,
) -> Result<ReviewStatus, AppError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT review_status::text FROM discrepancies WHERE reconciliation_id = $1",
    )
    .bind(reconciliation_id)
    .fetch_all(&mut **tx)
    .await?;
    let statuses: Vec<ReviewStatus> = rows
        .iter()
        .filter_map(|r| ReviewStatus::from_db(&r.0))
        .collect();
    let rolled = ReviewStatus::rollup(&statuses);
    sqlx::query("UPDATE reconciliations SET review_status = $1::review_status, updated_at = now() WHERE id = $2")
        .bind(rolled.as_db())
        .bind(reconciliation_id)
        .execute(&mut **tx)
        .await?;
    Ok(rolled)
}
