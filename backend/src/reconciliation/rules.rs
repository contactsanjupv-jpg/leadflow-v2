//! Deterministic reconciliation rules (spec section 18).

use crate::common::money::Money;
use crate::reconciliation::model::*;
use rust_decimal::Decimal;

pub const RULE_RATE_MISMATCH: &str = "RULE_03_RATE_MISMATCH";
pub const RULE_UNAUTHORIZED_FEE: &str = "RULE_04_UNAUTHORIZED_FEE";
pub const RULE_QUANTITY_VS_MANIFEST: &str = "RULE_01_QUANTITY_VS_MANIFEST";
pub const RULE_WEIGHT_MISMATCH: &str = "RULE_02_WEIGHT_MISMATCH";
pub const RULE_STALE_RATE: &str = "RULE_06_STALE_RATE";
pub const RULE_CONTAINER_COUNT_MISMATCH: &str = "RULE_09_CONTAINER_COUNT_MISMATCH";
pub const RULE_MISSING_SUPPORT: &str = "RULE_07_MISSING_SUPPORT";
pub const RULE_DUPLICATE_CHARGE: &str = "RULE_05_DUPLICATE_CHARGE";
pub const RULE_WASTE_STREAM_MISMATCH: &str = "RULE_08_WASTE_STREAM_MISMATCH";
pub const RULE_UNIT_MISMATCH: &str = "RULE_10_UNIT_MISMATCH";

pub fn check_rate_and_fees(invoice: &Invoice, rate: &ContractRate) -> Vec<DiscrepancyLine> {
    let mut out = Vec::new();

    for line in &invoice.line_items {
        if let Some(fee_code) = &line.fee_code {
            let authorized = rate.authorized_fees.iter().find(|f| &f.fee_code == fee_code);

            match authorized {
                None => {
                    out.push(DiscrepancyLine {
                        discrepancy_type: DiscrepancyType::UnauthorizedFee,
                        description: format!(
                            "Fee '{}' ({}) billed but not found in contracted authorized fees",
                            fee_code, line.description
                        ),
                        amount: line.line_total,
                        rule_id: RULE_UNAUTHORIZED_FEE.to_string(),
                        confidence: Confidence::High,
                    });
                }
                Some(auth_fee) if auth_fee.amount != line.line_total => {
                    let diff = line.line_total - auth_fee.amount;
                    out.push(DiscrepancyLine {
                        discrepancy_type: DiscrepancyType::UnauthorizedFee,
                        description: format!(
                            "Fee '{}' billed at {} but authorized amount is {}",
                            fee_code, line.line_total, auth_fee.amount
                        ),
                        amount: diff,
                        rule_id: RULE_UNAUTHORIZED_FEE.to_string(),
                        confidence: Confidence::High,
                    });
                }
                Some(_) => {}
            }

            continue;
        }

        // Units must match before a contract rate can be compared with an invoice line.
        // No conversion is inferred: a quantity in kg cannot be priced using a rate per drum.
        if !same_text(&line.unit, &rate.unit) {
            out.push(DiscrepancyLine {
                discrepancy_type: DiscrepancyType::Other,
                description: format!(
                    "Invoice line '{}' uses unit '{}' but the contract rate is per '{}'; no unit conversion is configured, so the line cannot be priced reliably",
                    line.description, line.unit, rate.unit
                ),
                amount: Money::zero(),
                rule_id: RULE_UNIT_MISMATCH.to_string(),
                confidence: Confidence::Medium,
            });
            continue;
        }

        if line.unit_price != rate.unit_rate {
            let expected_line_total = rate.unit_rate * line.quantity;
            let diff = line.line_total - expected_line_total;

            out.push(DiscrepancyLine {
                discrepancy_type: DiscrepancyType::RateMismatch,
                description: format!(
                    "Invoiced at {}/{} but contracted rate is {}/{} (qty {})",
                    line.unit_price, line.unit, rate.unit_rate, rate.unit, line.quantity
                ),
                amount: diff,
                rule_id: RULE_RATE_MISMATCH.to_string(),
                confidence: Confidence::High,
            });
        }
    }

    out
}

pub fn check_quantity_vs_manifest(
    invoice: &Invoice,
    manifest: &Manifest,
    tol: &Tolerances,
) -> Option<DiscrepancyLine> {
    let primary_lines: Vec<&InvoiceLineItem> = invoice
        .line_items
        .iter()
        .filter(|line| line.fee_code.is_none())
        .collect();

    let matching_lines: Vec<&InvoiceLineItem> = primary_lines
        .iter()
        .copied()
        .filter(|line| same_text(&line.unit, &manifest.unit))
        .collect();

    // If none of the invoice's primary lines use the manifest's unit,
    // comparing the numeric quantities would be misleading.
    if !primary_lines.is_empty() && matching_lines.is_empty() {
        let units = primary_lines
            .iter()
            .map(|line| line.unit.trim())
            .collect::<Vec<_>>()
            .join(", ");

        return Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::Other,
            description: format!(
                "Invoice primary line unit(s) [{}] cannot be compared with manifest #{} unit '{}'; no unit conversion is configured",
                units, manifest.manifest_number, manifest.unit
            ),
            amount: Money::zero(),
            rule_id: RULE_UNIT_MISMATCH.to_string(),
            confidence: Confidence::Medium,
        });
    }

    let invoice_qty: Decimal = matching_lines
        .iter()
        .map(|line| line.quantity)
        .sum();

    let diff = (invoice_qty - manifest.quantity).abs();

    if diff > tol.quantity_tolerance {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::QuantityMismatch,
            description: format!(
                "Invoice quantity {} {} does not match manifest quantity {} {} (manifest #{})",
                invoice_qty,
                manifest.unit,
                manifest.quantity,
                manifest.unit,
                manifest.manifest_number
            ),
            amount: Money::zero(),
            rule_id: RULE_QUANTITY_VS_MANIFEST.to_string(),
            confidence: Confidence::High,
        })
    } else {
        None
    }
}

pub fn check_weight_mismatch(invoice_weight: Decimal, ticket: &WeightTicket, tol: &Tolerances) -> Option<DiscrepancyLine> {
    if ticket.net_weight.is_zero() { return None; }
    let diff_pct = ((invoice_weight - ticket.net_weight).abs() / ticket.net_weight) * Decimal::from(100);
    if diff_pct > tol.weight_tolerance_percent {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::WeightMismatch,
            description: format!(
                "Invoice implies weight {} but weight ticket #{} recorded {} ({}% variance, tolerance {}%)",
                invoice_weight, ticket.ticket_number, ticket.net_weight, diff_pct.round_dp(2), tol.weight_tolerance_percent
            ),
            amount: Money::zero(),
            rule_id: RULE_WEIGHT_MISMATCH.to_string(),
            confidence: Confidence::Medium,
        })
    } else { None }
}

pub fn check_stale_rate(invoice: &Invoice, rate: &ContractRate) -> Option<DiscrepancyLine> {
    if !rate.covers_date(invoice.service_date) {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::ContractTermMismatch,
            description: format!(
                "Service date {} falls outside rate's effective window ({} to {})",
                invoice.service_date, rate.effective_from,
                rate.effective_to.map(|d| d.to_string()).unwrap_or_else(|| "open".into())
            ),
            amount: Money::zero(),
            rule_id: RULE_STALE_RATE.to_string(),
            confidence: Confidence::High,
        })
    } else { None }
}

pub fn check_container_count(invoice_container_count: Option<i32>, manifest: Option<&Manifest>, ticket: Option<&WeightTicket>) -> Option<DiscrepancyLine> {
    let manifest_count = manifest.and_then(|m| m.container_count);
    let ticket_count = ticket.and_then(|t| t.container_count);
    let counts: Vec<i32> = [invoice_container_count, manifest_count, ticket_count].into_iter().flatten().collect();
    if counts.len() >= 2 && counts.iter().any(|c| *c != counts[0]) {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::ContainerCountMismatch,
            description: format!(
                "Container count mismatch across sources: invoice={:?}, manifest={:?}, weight_ticket={:?}",
                invoice_container_count, manifest_count, ticket_count
            ),
            amount: Money::zero(),
            rule_id: RULE_CONTAINER_COUNT_MISMATCH.to_string(),
            confidence: Confidence::Medium,
        })
    } else { None }
}

pub fn check_missing_support(manifest: Option<&Manifest>, ticket: Option<&WeightTicket>) -> Option<DiscrepancyLine> {
    if manifest.is_none() && ticket.is_none() {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::MissingSupport,
            description: "No manifest or weight ticket found to support this invoice; amounts cannot be independently confirmed".into(),
            amount: Money::zero(),
            rule_id: RULE_MISSING_SUPPORT.to_string(),
            confidence: Confidence::Low,
        })
    } else { None }
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Two lines are "the same charge" when every billed attribute is identical.
fn lines_identical(a: &InvoiceLineItem, b: &InvoiceLineItem) -> bool {
    same_text(&a.description, &b.description)
        && a.fee_code.as_deref().map(str::trim) == b.fee_code.as_deref().map(str::trim)
        && a.quantity == b.quantity
        && same_text(&a.unit, &b.unit)
        && a.unit_price == b.unit_price
        && a.line_total == b.line_total
}

/// Dollar amount RULE_03/RULE_04 already attribute to this exact line (signed).
fn already_attributed(line: &InvoiceLineItem, rate: Option<&ContractRate>) -> Money {
    let Some(rate) = rate else { return Money::zero() };
    if same_text(&line.unit, &rate.unit) && line.fee_code.is_none() {
        if line.unit_price != rate.unit_rate { line.line_total - rate.unit_rate * line.quantity } else { Money::zero() }
    } else if let Some(code) = &line.fee_code {
        match rate.authorized_fees.iter().find(|f| &f.fee_code == code) {
            None => line.line_total,
            Some(a) if a.amount != line.line_total => line.line_total - a.amount,
            Some(_) => Money::zero(),
        }
    } else {
        Money::zero()
    }
}

/// Which lines are duplicates: (line index, Some(earlier invoice number) for a cross-invoice duplicate).
/// Shared by the rule (to report) and the engine (to exclude duplicates from the expected amount).
fn find_duplicates(invoice: &Invoice, related: &[RelatedInvoice]) -> Vec<(usize, Option<String>)> {
    let billable = |l: &InvoiceLineItem| l.line_total.as_decimal() > Decimal::ZERO;
    let mut found: Vec<(usize, Option<String>)> = Vec::new();
    for i in 0..invoice.line_items.len() {
        let line = &invoice.line_items[i];
        if !billable(line) { continue; }
        if invoice.line_items[..i].iter().any(|o| billable(o) && lines_identical(o, line)) {
            found.push((i, None));
        }
    }
    for (i, line) in invoice.line_items.iter().enumerate() {
        if !billable(line) || found.iter().any(|(j, _)| *j == i) { continue; }
        if let Some(other) = related.iter().find(|r| r.line_items.iter().any(|o| lines_identical(o, line))) {
            found.push((i, Some(other.invoice_number.clone())));
        }
    }
    found.sort_by_key(|(i, _)| *i);
    found
}

pub fn duplicate_line_indices(invoice: &Invoice, related: &[RelatedInvoice]) -> Vec<usize> {
    find_duplicates(invoice, related).into_iter().map(|(i, _)| i).collect()
}

/// RULE_05 duplicate charge. Flags (a) a line that appears more than once, identically, on the same
/// invoice and (b) a line identical to a line on an earlier invoice from the same vendor for the same
/// service date. The reported dollar amount is the duplicate line's total MINUS whatever RULE_03/04
/// already attribute to that line, so the discrepancy total never double counts; the engine also
/// leaves duplicate lines out of the expected amount, so `invoiced - expected` equals the sum of
/// findings. Credit/zero lines are ignored. Findings go to human review: a repeated identical charge
/// can occasionally be legitimate.
pub fn check_duplicate_charges(invoice: &Invoice, rate: Option<&ContractRate>, related: &[RelatedInvoice]) -> Vec<DiscrepancyLine> {
    find_duplicates(invoice, related)
        .into_iter()
        .map(|(i, other)| {
            let line = &invoice.line_items[i];
            let (description, confidence) = match other {
                None => (format!("Line '{}' ({} x {} = {}) is billed more than once on this invoice", line.description.trim(), line.quantity, line.unit_price, line.line_total), Confidence::Medium),
                Some(num) => (format!("Line '{}' ({}) was already billed on invoice {} for the same service date", line.description.trim(), line.line_total, num), Confidence::High),
            };
            DiscrepancyLine {
                discrepancy_type: DiscrepancyType::DuplicateCharge,
                description,
                amount: line.line_total - already_attributed(line, rate),
                rule_id: RULE_DUPLICATE_CHARGE.to_string(),
                confidence,
            }
        })
        .collect()
}

/// RULE_08 waste stream mismatch: the invoice bills a different waste stream than the manifest
/// documents. No dollar amount can be derived (the correct price depends on the true stream), so the
/// finding is reported at $0 and must be resolved by a reviewer.
pub fn check_waste_stream_mismatch(invoice: &Invoice, manifest: &Manifest) -> Option<DiscrepancyLine> {
    if invoice.waste_stream.trim().is_empty() || manifest.waste_stream.trim().is_empty() || same_text(&invoice.waste_stream, &manifest.waste_stream) {
        return None;
    }
    Some(DiscrepancyLine {
        discrepancy_type: DiscrepancyType::WrongWasteStream,
        description: format!(
            "Invoice bills waste stream '{}' but manifest #{} documents '{}'",
            invoice.waste_stream.trim(), manifest.manifest_number, manifest.waste_stream.trim()
        ),
        amount: Money::zero(),
        rule_id: RULE_WASTE_STREAM_MISMATCH.to_string(),
        confidence: Confidence::High,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::money::Money;
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn sample_rate() -> ContractRate {
        ContractRate {
            id: Uuid::new_v4(), vendor_id: Uuid::new_v4(), waste_stream: "F003".into(), facility_id: None,
            unit_rate: Money::from_str_exact("400.00").unwrap(), unit: "drum".into(),
            authorized_fees: vec![AuthorizedFee { fee_code: "TRANSPORT".into(), description: "Transportation".into(), amount: Money::from_str_exact("500.00").unwrap() }],
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), effective_to: None,
        }
    }

    fn sample_invoice_with_overcharge() -> Invoice {
        Invoice {
            id: Uuid::new_v4(), organization_id: Uuid::new_v4(), vendor_id: Uuid::new_v4(),
            invoice_number: "INV-1001".into(), invoice_date: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            service_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), facility_id: None, waste_stream: "F003".into(),
            line_items: vec![
                InvoiceLineItem { id: Uuid::new_v4(), description: "Drum disposal".into(), fee_code: None, quantity: Decimal::from(10), unit: "drum".into(), unit_price: Money::from_str_exact("450.00").unwrap(), line_total: Money::from_str_exact("4500.00").unwrap() },
                InvoiceLineItem { id: Uuid::new_v4(), description: "Transportation".into(), fee_code: Some("TRANSPORT".into()), quantity: Decimal::from(1), unit: "flat".into(), unit_price: Money::from_str_exact("500.00").unwrap(), line_total: Money::from_str_exact("500.00").unwrap() },
                InvoiceLineItem { id: Uuid::new_v4(), description: "Environmental fee".into(), fee_code: Some("ENV_FEE".into()), quantity: Decimal::from(1), unit: "flat".into(), unit_price: Money::from_str_exact("200.00").unwrap(), line_total: Money::from_str_exact("200.00").unwrap() },
            ],
            total: Money::from_str_exact("5200.00").unwrap(), source_document_id: None,
        }
    }

    #[test]
    fn spec_section_55_worked_example_produces_700_dollar_discrepancy() {
        let rate = sample_rate();
        let invoice = sample_invoice_with_overcharge();
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        assert_eq!(discrepancies.len(), 2, "expected exactly 2 discrepancy lines: {discrepancies:?}");
        let rate_line = discrepancies.iter().find(|d| d.discrepancy_type == DiscrepancyType::RateMismatch).expect("expected a RateMismatch line");
        assert_eq!(rate_line.amount, Money::from_str_exact("500.00").unwrap());
        let fee_line = discrepancies.iter().find(|d| d.discrepancy_type == DiscrepancyType::UnauthorizedFee).expect("expected an UnauthorizedFee line");
        assert_eq!(fee_line.amount, Money::from_str_exact("200.00").unwrap());
        let total_discrepancy = rate_line.amount + fee_line.amount;
        assert_eq!(total_discrepancy, Money::from_str_exact("700.00").unwrap(), "spec section 55 requires the total discrepancy to be exactly $700.00");
    }

    #[test]
    fn exact_match_produces_no_discrepancy() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        invoice.line_items[0].line_total = Money::from_str_exact("4000.00").unwrap();
        invoice.line_items.remove(2);
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        assert!(discrepancies.is_empty(), "expected no discrepancies: {discrepancies:?}");
    }

    #[test]
    fn zero_quantity_line_does_not_panic_or_false_flag() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].quantity = Decimal::ZERO;
        invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        invoice.line_items[0].line_total = Money::zero();
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        assert!(!discrepancies.iter().any(|d| d.discrepancy_type == DiscrepancyType::RateMismatch));
    }

    #[test]
    fn negative_credit_line_reduces_discrepancy_correctly() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].unit_price = Money::from_str_exact("350.00").unwrap();
        invoice.line_items[0].line_total = Money::from_str_exact("3500.00").unwrap();
        invoice.line_items.remove(2);
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        let rate_line = discrepancies.iter().find(|d| d.discrepancy_type == DiscrepancyType::RateMismatch).unwrap();
        assert!(rate_line.amount.is_negative(), "underbilling should produce a negative discrepancy amount");
        assert_eq!(rate_line.amount, Money::from_str_exact("-500.00").unwrap());
    }
    
    #[test]
    fn primary_rate_unit_mismatch_is_flagged_without_inventing_dollar_amount() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].unit = "kg".into();

        let findings = check_rate_and_fees(&invoice, &rate);

        let finding = findings
            .iter()
            .find(|d| d.rule_id == RULE_UNIT_MISMATCH)
            .expect("incompatible rate units must be flagged");

        assert_eq!(finding.amount, Money::zero());
        assert!(finding.description.contains("kg"));
        assert!(finding.description.contains("drum"));
        assert!(
            !findings.iter().any(|d| d.rule_id == RULE_RATE_MISMATCH),
            "the rate comparison must not price incompatible units"
        );
    }

    #[test]
    fn manifest_quantity_with_incompatible_units_is_not_compared_numerically() {
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].unit = "drum".into();

        let manifest = Manifest {
            id: Uuid::new_v4(),
            organization_id: invoice.organization_id,
            manifest_number: "M-UNIT-1".into(),
            waste_stream: "F003".into(),
            container_count: Some(10),
            quantity: Decimal::from(100),
            unit: "kg".into(),
            shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };

        let finding = check_quantity_vs_manifest(
            &invoice,
            &manifest,
            &Tolerances::default(),
        )
        .expect("incompatible units must be flagged");

        assert_eq!(finding.rule_id, RULE_UNIT_MISMATCH);
        assert_eq!(finding.amount, Money::zero());
        assert!(finding.description.contains("drum"));
        assert!(finding.description.contains("kg"));
    }

    #[test]
    fn quantity_matches_manifest_produces_no_flag() {
        let manifest = Manifest { id: Uuid::new_v4(), organization_id: Uuid::new_v4(), manifest_number: "M-1".into(), waste_stream: "F003".into(), container_count: Some(10), quantity: Decimal::from(10), unit: "drum".into(), shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), source_document_id: None };
        let invoice = sample_invoice_with_overcharge();
        let tol = Tolerances::default();
        assert!(check_quantity_vs_manifest(&invoice, &manifest, &tol).is_none());
    }

    #[test]
    fn quantity_mismatch_vs_manifest_is_flagged() {
        let manifest = Manifest { id: Uuid::new_v4(), organization_id: Uuid::new_v4(), manifest_number: "M-1".into(), waste_stream: "F003".into(), container_count: Some(8), quantity: Decimal::from(8), unit: "drum".into(), shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), source_document_id: None };
        let invoice = sample_invoice_with_overcharge();
        let tol = Tolerances::default();
        assert!(check_quantity_vs_manifest(&invoice, &manifest, &tol).is_some());
    }

    #[test]
    fn weight_within_tolerance_not_flagged() {
        let ticket = WeightTicket { id: Uuid::new_v4(), organization_id: Uuid::new_v4(), manifest_id: None, ticket_number: "WT-1".into(), net_weight: Decimal::from(1000), unit: "lb".into(), container_count: None, ticket_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), source_document_id: None };
        let tol = Tolerances::default();
        let invoice_weight = Decimal::from(1015);
        assert!(check_weight_mismatch(invoice_weight, &ticket, &tol).is_none());
    }

    #[test]
    fn weight_beyond_tolerance_is_flagged() {
        let ticket = WeightTicket { id: Uuid::new_v4(), organization_id: Uuid::new_v4(), manifest_id: None, ticket_number: "WT-1".into(), net_weight: Decimal::from(1000), unit: "lb".into(), container_count: None, ticket_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), source_document_id: None };
        let tol = Tolerances::default();
        let invoice_weight = Decimal::from(1100);
        assert!(check_weight_mismatch(invoice_weight, &ticket, &tol).is_some());
    }

    #[test]
    fn stale_rate_outside_effective_window_is_flagged() {
        let mut rate = sample_rate();
        rate.effective_to = Some(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        let invoice = sample_invoice_with_overcharge();
        assert!(check_stale_rate(&invoice, &rate).is_some());
    }

    #[test]
    fn container_count_mismatch_across_documents_is_flagged() {
        let manifest = Manifest { id: Uuid::new_v4(), organization_id: Uuid::new_v4(), manifest_number: "M-1".into(), waste_stream: "F003".into(), container_count: Some(9), quantity: Decimal::from(9), unit: "drum".into(), shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), source_document_id: None };
        assert!(check_container_count(Some(10), Some(&manifest), None).is_some());
    }

    #[test]
    fn missing_support_flagged_when_no_manifest_or_ticket() {
        assert!(check_missing_support(None, None).is_some());
    }

    #[test]
    fn missing_support_not_flagged_when_manifest_present() {
        let manifest = Manifest { id: Uuid::new_v4(), organization_id: Uuid::new_v4(), manifest_number: "M-1".into(), waste_stream: "F003".into(), container_count: Some(10), quantity: Decimal::from(10), unit: "drum".into(), shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(), source_document_id: None };
        assert!(check_missing_support(Some(&manifest), None).is_none());
    }

    // ---- RULE_05 / RULE_08 ----
    fn dup_invoice(lines: Vec<InvoiceLineItem>) -> Invoice {
        let total = lines.iter().fold(Money::zero(), |a, l| a + l.line_total);
        Invoice {
            id: uuid::Uuid::new_v4(), organization_id: uuid::Uuid::new_v4(), vendor_id: uuid::Uuid::new_v4(), invoice_number: "INV-2".into(),
            invoice_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(), service_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            facility_id: None, waste_stream: "F003".into(), line_items: lines, total, source_document_id: None,
        }
    }
    fn dline(desc: &str, fee: Option<&str>, qty: i64, price: i64, total: i64) -> InvoiceLineItem {
        InvoiceLineItem {
            id: uuid::Uuid::new_v4(), description: desc.into(), fee_code: fee.map(|f| f.to_string()), quantity: Decimal::from(qty), unit: "drum".into(),
            unit_price: Money::from_decimal(Decimal::from(price)), line_total: Money::from_decimal(Decimal::from(total)),
        }
    }
    fn drate() -> ContractRate {
        ContractRate {
            id: uuid::Uuid::new_v4(), vendor_id: uuid::Uuid::new_v4(), waste_stream: "F003".into(), facility_id: None,
            unit_rate: Money::from_decimal(Decimal::from(400)), unit: "drum".into(),
            authorized_fees: vec![AuthorizedFee { fee_code: "TRANSPORT".into(), description: "t".into(), amount: Money::from_decimal(Decimal::from(500)) }],
            effective_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), effective_to: None,
        }
    }

    #[test]
    fn identical_line_twice_on_one_invoice_is_a_duplicate_for_its_full_value() {
        let inv = dup_invoice(vec![dline("Drum disposal", None, 10, 400, 4000), dline("Transportation", Some("TRANSPORT"), 1, 500, 500), dline("Transportation", Some("TRANSPORT"), 1, 500, 500)]);
        let found = check_duplicate_charges(&inv, Some(&drate()), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule_id, RULE_DUPLICATE_CHARGE);
        assert_eq!(found[0].amount, Money::from_decimal(Decimal::from(500)));
        assert!(matches!(found[0].discrepancy_type, DiscrepancyType::DuplicateCharge));
    }

    #[test]
    fn duplicate_amount_never_double_counts_what_rate_and_fee_rules_already_claim() {
        // Duplicated disposal line billed at $450 vs contract $400: RULE_03 already claims $500 on the
        // duplicate line; RULE_05 may only add the remaining $4000 so the sum is exactly the $4500 line.
        let l = dline("Drum disposal", None, 10, 450, 4500);
        let inv = dup_invoice(vec![l.clone(), InvoiceLineItem { id: uuid::Uuid::new_v4(), ..l }]);
        let rate = drate();
        let rule3: Money = check_rate_and_fees(&inv, &rate).iter().fold(Money::zero(), |a, d| a + d.amount);
        let rule5 = check_duplicate_charges(&inv, Some(&rate), &[]);
        assert_eq!(rule5.len(), 1);
        assert_eq!(rule5[0].amount, Money::from_decimal(Decimal::from(4000)));
        // Two lines each $500 over the contracted rate (RULE_03 = $1,000) plus the $4,000 duplicate load
        // = $5,000 = invoiced $9,000 minus the $4,000 a single load should cost.
        assert_eq!(rule3 + rule5[0].amount, Money::from_decimal(Decimal::from(5000)));
        // A duplicated UNAUTHORIZED fee is already fully claimed by RULE_04: RULE_05 adds $0, not another $200.
        let fee = dline("Environmental fee", Some("ENV_FEE"), 1, 200, 200);
        let inv2 = dup_invoice(vec![fee.clone(), InvoiceLineItem { id: uuid::Uuid::new_v4(), ..fee }]);
        let r5 = check_duplicate_charges(&inv2, Some(&rate), &[]);
        assert_eq!(r5.len(), 1);
        assert_eq!(r5[0].amount, Money::zero());
    }

    #[test]
    fn line_matching_an_earlier_invoice_is_flagged_and_names_that_invoice() {
        let earlier = RelatedInvoice { id: uuid::Uuid::new_v4(), invoice_number: "INV-1".into(), line_items: vec![dline("drum disposal", None, 10, 400, 4000)] };
        let inv = dup_invoice(vec![dline("Drum disposal ", None, 10, 400, 4000)]);
        let found = check_duplicate_charges(&inv, Some(&drate()), &[earlier]);
        assert_eq!(found.len(), 1);
        assert!(found[0].description.contains("INV-1"));
        assert_eq!(found[0].amount, Money::from_decimal(Decimal::from(4000)));
        assert!(matches!(found[0].confidence, Confidence::High));
    }

    #[test]
    fn different_lines_credits_and_zero_lines_are_not_duplicates() {
        let inv = dup_invoice(vec![dline("Drum disposal", None, 10, 400, 4000), dline("Drum disposal", None, 5, 400, 2000), dline("Credit", None, 1, -100, -100), dline("Credit", None, 1, -100, -100), dline("Note", None, 1, 0, 0), dline("Note", None, 1, 0, 0)]);
        assert!(check_duplicate_charges(&inv, Some(&drate()), &[]).is_empty());
        let other = RelatedInvoice { id: uuid::Uuid::new_v4(), invoice_number: "X".into(), line_items: vec![dline("Drum disposal", None, 11, 400, 4400)] };
        assert!(check_duplicate_charges(&inv, Some(&drate()), &[other]).is_empty());
    }

    #[test]
    fn waste_stream_mismatch_is_flagged_case_insensitively_and_has_no_invented_amount() {
        let mut inv = dup_invoice(vec![dline("Drum disposal", None, 10, 400, 4000)]);
        let m = |stream: &str| Manifest {
            id: uuid::Uuid::new_v4(), organization_id: uuid::Uuid::new_v4(), manifest_number: "M-1".into(), waste_stream: stream.into(), container_count: None,
            quantity: Decimal::from(10), unit: "drum".into(), shipment_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 9).unwrap(), source_document_id: None,
        };
        assert!(check_waste_stream_mismatch(&inv, &m(" f003 ")).is_none());
        let d = check_waste_stream_mismatch(&inv, &m("F005")).expect("mismatch");
        assert_eq!(d.rule_id, RULE_WASTE_STREAM_MISMATCH);
        assert!(matches!(d.discrepancy_type, DiscrepancyType::WrongWasteStream));
        assert_eq!(d.amount, Money::zero());
        assert!(d.description.contains("F003") && d.description.contains("F005") && d.description.contains("M-1"));
        inv.waste_stream = "".into();
        assert!(check_waste_stream_mismatch(&inv, &m("F005")).is_none(), "an unknown stream is not a mismatch");
    }
}
