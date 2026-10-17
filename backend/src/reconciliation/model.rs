//! Domain model for the reconciliation engine.

use crate::common::money::Money;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedFee {
    pub fee_code: String,
    pub description: String,
    pub amount: Money,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractRate {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub waste_stream: String,
    pub facility_id: Option<Uuid>,
    pub unit_rate: Money,
    pub unit: String,
    pub authorized_fees: Vec<AuthorizedFee>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}

impl ContractRate {
    pub fn covers_date(&self, date: NaiveDate) -> bool {
        date >= self.effective_from && self.effective_to.map_or(true, |end| date <= end)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceLineItem {
    pub id: Uuid,
    pub description: String,
    pub fee_code: Option<String>,
    pub quantity: Decimal,
    pub unit: String,
    pub unit_price: Money,
    pub line_total: Money,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub vendor_id: Uuid,
    pub invoice_number: String,
    pub invoice_date: NaiveDate,
    pub service_date: NaiveDate,
    pub facility_id: Option<Uuid>,
    pub waste_stream: String,
    pub line_items: Vec<InvoiceLineItem>,
    pub total: Money,
    pub source_document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub manifest_number: String,
    pub waste_stream: String,
    pub container_count: Option<i32>,
    pub quantity: Decimal,
    pub unit: String,
    pub shipment_date: NaiveDate,
    pub source_document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightTicket {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub manifest_id: Option<Uuid>,
    pub ticket_number: String,
    pub net_weight: Decimal,
    pub unit: String,
    pub container_count: Option<i32>,
    pub ticket_date: NaiveDate,
    pub source_document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedInvoice {
    pub id: Uuid,
    pub invoice_number: String,
    pub line_items: Vec<InvoiceLineItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationInput {
    pub invoice: Invoice,
    /// Earlier-entered invoices from the same vendor for the same service date, used only by the
    /// duplicate-charge rule. Empty when there are none.
    #[serde(default)]
    pub related_invoices: Vec<RelatedInvoice>,
    /// The rate whose effective window covers the service date (facility-specific preferred).
    pub applicable_rate: Option<ContractRate>,
    /// When no rate covers the service date but an EXPIRED rate exists for the same vendor,
    /// waste stream and facility scope, it is carried here for reporting only. It is never used
    /// to price the invoice.
    #[serde(default)]
    pub stale_rate: Option<ContractRate>,
    pub manifest: Option<Manifest>,
    pub weight_ticket: Option<WeightTicket>,
    pub tolerances: Tolerances,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Tolerances {
    pub weight_tolerance_percent: Decimal,
    pub quantity_tolerance: Decimal,
    pub rounding_tolerance: Money,
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            weight_tolerance_percent: Decimal::new(2, 0),
            quantity_tolerance: Decimal::ZERO,
            rounding_tolerance: Money::from_str_exact("0.01").unwrap(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscrepancyType {
    QuantityMismatch,
    RateMismatch,
    UnauthorizedFee,
    DuplicateCharge,
    WrongWasteStream,
    WrongServiceDate,
    WrongFacility,
    ContractTermMismatch,
    WeightMismatch,
    ContainerCountMismatch,
    MissingSupport,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscrepancyLine {
    pub discrepancy_type: DiscrepancyType,
    pub description: String,
    pub amount: Money,
    pub rule_id: String,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewStatus {
    New,
    UnderReview,
    Confirmed,
    Rejected,
    NeedsInformation,
    Disputed,
    Resolved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub invoice_id: Uuid,
    pub expected_amount: Money,
    pub invoiced_amount: Money,
    pub discrepancy_amount: Money,
    pub discrepancies: Vec<DiscrepancyLine>,
    pub tolerances_applied: Tolerances,
    pub review_status: ReviewStatus,
    pub calculation_trace: Vec<String>,
}

impl ReconciliationResult {
    pub fn has_material_discrepancy(&self) -> bool {
        !self.discrepancy_amount.is_zero()
    }
}

// ---- Database enum mappings (PostgreSQL enum labels are UPPER_SNAKE_CASE) ----

impl ReviewStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            ReviewStatus::New => "NEW",
            ReviewStatus::UnderReview => "UNDER_REVIEW",
            ReviewStatus::Confirmed => "CONFIRMED",
            ReviewStatus::Rejected => "REJECTED",
            ReviewStatus::NeedsInformation => "NEEDS_INFORMATION",
            ReviewStatus::Disputed => "DISPUTED",
            ReviewStatus::Resolved => "RESOLVED",
        }
    }

    pub fn from_db(s: &str) -> Option<ReviewStatus> {
        Some(match s {
            "NEW" => ReviewStatus::New,
            "UNDER_REVIEW" => ReviewStatus::UnderReview,
            "CONFIRMED" => ReviewStatus::Confirmed,
            "REJECTED" => ReviewStatus::Rejected,
            "NEEDS_INFORMATION" => ReviewStatus::NeedsInformation,
            "DISPUTED" => ReviewStatus::Disputed,
            "RESOLVED" => ReviewStatus::Resolved,
            _ => return None,
        })
    }

    /// Roll a set of discrepancy statuses up into the parent reconciliation's status.
    /// Anything still unreviewed dominates; an empty set means nothing was found (RESOLVED).
    pub fn rollup(statuses: &[ReviewStatus]) -> ReviewStatus {
        let has = |s: ReviewStatus| statuses.contains(&s);
        if has(ReviewStatus::New) {
            ReviewStatus::New
        } else if has(ReviewStatus::UnderReview) {
            ReviewStatus::UnderReview
        } else if has(ReviewStatus::NeedsInformation) {
            ReviewStatus::NeedsInformation
        } else if has(ReviewStatus::Disputed) {
            ReviewStatus::Disputed
        } else if has(ReviewStatus::Confirmed) {
            ReviewStatus::Confirmed
        } else {
            ReviewStatus::Resolved
        }
    }
}

impl DiscrepancyType {
    pub fn as_db(&self) -> &'static str {
        match self {
            DiscrepancyType::QuantityMismatch => "QUANTITY_MISMATCH",
            DiscrepancyType::RateMismatch => "RATE_MISMATCH",
            DiscrepancyType::UnauthorizedFee => "UNAUTHORIZED_FEE",
            DiscrepancyType::DuplicateCharge => "DUPLICATE_CHARGE",
            DiscrepancyType::WrongWasteStream => "WRONG_WASTE_STREAM",
            DiscrepancyType::WrongServiceDate => "WRONG_SERVICE_DATE",
            DiscrepancyType::WrongFacility => "WRONG_FACILITY",
            DiscrepancyType::ContractTermMismatch => "CONTRACT_TERM_MISMATCH",
            DiscrepancyType::WeightMismatch => "WEIGHT_MISMATCH",
            DiscrepancyType::ContainerCountMismatch => "CONTAINER_COUNT_MISMATCH",
            DiscrepancyType::MissingSupport => "MISSING_SUPPORT",
            DiscrepancyType::Other => "OTHER",
        }
    }
}

impl Confidence {
    pub fn as_db(&self) -> &'static str {
        match self {
            Confidence::High => "HIGH",
            Confidence::Medium => "MEDIUM",
            Confidence::Low => "LOW",
        }
    }
}

#[cfg(test)]
mod db_mapping_tests {
    use super::*;

    #[test]
    fn review_status_round_trips_through_db_labels() {
        for s in [
            ReviewStatus::New,
            ReviewStatus::UnderReview,
            ReviewStatus::Confirmed,
            ReviewStatus::Rejected,
            ReviewStatus::NeedsInformation,
            ReviewStatus::Disputed,
            ReviewStatus::Resolved,
        ] {
            assert_eq!(ReviewStatus::from_db(s.as_db()), Some(s));
        }
        assert_eq!(ReviewStatus::from_db("bogus"), None);
    }

    #[test]
    fn rollup_prioritises_unreviewed_work() {
        use ReviewStatus::*;
        assert_eq!(ReviewStatus::rollup(&[]), Resolved);
        assert_eq!(ReviewStatus::rollup(&[Confirmed, New]), New);
        assert_eq!(
            ReviewStatus::rollup(&[Confirmed, UnderReview, Rejected]),
            UnderReview
        );
        assert_eq!(
            ReviewStatus::rollup(&[Confirmed, NeedsInformation]),
            NeedsInformation
        );
        assert_eq!(ReviewStatus::rollup(&[Confirmed, Disputed]), Disputed);
        assert_eq!(ReviewStatus::rollup(&[Confirmed, Rejected]), Confirmed);
        assert_eq!(ReviewStatus::rollup(&[Rejected, Rejected]), Resolved);
    }
}
