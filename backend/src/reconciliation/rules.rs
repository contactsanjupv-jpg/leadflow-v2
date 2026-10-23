//! Deterministic reconciliation rules (spec section 18).

use crate::common::money::Money;
use crate::common::waste_stream;
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
/// A fee several applicable rates authorize inconsistently (differing amounts or charge types).
pub const RULE_AMBIGUOUS_FEE: &str = "RULE_11_AMBIGUOUS_FEE";
/// A fee whose authorized amount has no explicit EXACT/CAP type. The type is never inferred.
pub const RULE_UNCLASSIFIED_FEE: &str = "RULE_12_UNCLASSIFIED_FEE_TYPE";

// ---- Authorized fees: the single authoritative evaluation ----------------------------------
//
// Every production and legacy path decides fee outcomes here. Finding amounts are signed
// `billed - expected`: positive = overbilled, negative = underbilled. For a fee that is priced,
// `billed - expected` equals the finding amount (or zero when there is no finding), so bill-level
// expected totals and findings cannot drift apart.

/// One authorization of a fee code by an applicable contract rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizedFeeOption {
    pub amount: Money,
    pub charge_type: Option<FeeChargeType>,
}

/// Outcome of evaluating one billed fee line against the applicable authorizations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeEvaluation {
    /// No applicable rate authorizes this fee code.
    NotAuthorized,
    /// Applicable rates authorize the code inconsistently; the expected amount is ambiguous.
    Conflicting { amounts_differ: bool },
    /// Exactly one authorization exists but its type is unset. It is never inferred as EXACT or CAP,
    /// so no expected amount is produced.
    Unclassified { authorized: Money },
    /// The fee is priced. `expected` is what the bill-level expected total includes for this line;
    /// `finding_amount` is the signed amount to report, or `None` when nothing is flagged.
    Priced {
        charge_type: FeeChargeType,
        authorized: Money,
        expected: Money,
        finding_amount: Option<Money>,
    },
}

impl FeeEvaluation {
    /// True when no expected amount can be determined, so pricing must not be reported as complete.
    pub fn is_unresolved(&self) -> bool {
        matches!(
            self,
            FeeEvaluation::Conflicting { .. } | FeeEvaluation::Unclassified { .. }
        )
    }
}

/// Fee codes are compared without regard to surrounding whitespace or ASCII case.
pub fn fee_codes_match(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// All authorizations of `fee_code` across the applicable rates.
pub fn authorized_fee_options(rates: &[&ContractRate], fee_code: &str) -> Vec<AuthorizedFeeOption> {
    rates
        .iter()
        .flat_map(|rate| rate.authorized_fees.iter())
        .filter(|fee| fee_codes_match(&fee.fee_code, fee_code))
        .map(|fee| AuthorizedFeeOption {
            amount: fee.amount,
            charge_type: fee.charge_type,
        })
        .collect()
}

/// Evaluate one billed fee line. Identical (amount, type) authorizations from several rates count
/// once; any difference in amount or type is a conflict.
///
/// * EXACT: expected = authorized amount; any difference is flagged as `billed - authorized`.
/// * CAP: expected = `min(billed, cap)`; only the amount above the cap is flagged.
/// * Unset type: unresolved; never treated as EXACT or CAP.
pub fn evaluate_authorized_fee(options: &[AuthorizedFeeOption], billed: Money) -> FeeEvaluation {
    let mut distinct: Vec<AuthorizedFeeOption> = Vec::new();
    for option in options {
        if !distinct.contains(option) {
            distinct.push(*option);
        }
    }

    match distinct.as_slice() {
        [] => FeeEvaluation::NotAuthorized,
        [only] => match only.charge_type {
            None => FeeEvaluation::Unclassified {
                authorized: only.amount,
            },
            Some(FeeChargeType::Exact) => FeeEvaluation::Priced {
                charge_type: FeeChargeType::Exact,
                authorized: only.amount,
                expected: only.amount,
                finding_amount: if billed != only.amount {
                    Some(billed - only.amount)
                } else {
                    None
                },
            },
            Some(FeeChargeType::Cap) => FeeEvaluation::Priced {
                charge_type: FeeChargeType::Cap,
                authorized: only.amount,
                expected: std::cmp::min(billed, only.amount),
                finding_amount: if billed > only.amount {
                    Some(billed - only.amount)
                } else {
                    None
                },
            },
        },
        many => FeeEvaluation::Conflicting {
            amounts_differ: many.iter().any(|o| o.amount != many[0].amount),
        },
    }
}

/// The finding (if any) for a fee line, built from the shared evaluation so every caller reports
/// identical text. EXACT, not-authorized and conflicting-amount wording is unchanged from earlier
/// releases so carried-forward review decisions still match.
pub fn fee_finding(
    fee_code: &str,
    line_description: &str,
    billed: Money,
    evaluation: &FeeEvaluation,
) -> Option<DiscrepancyLine> {
    match evaluation {
        FeeEvaluation::NotAuthorized => Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::UnauthorizedFee,
            description: format!(
                "Fee '{}' ({}) billed but not found in contracted authorized fees",
                fee_code, line_description
            ),
            amount: billed,
            rule_id: RULE_UNAUTHORIZED_FEE.to_string(),
            confidence: Confidence::High,
        }),
        FeeEvaluation::Conflicting { amounts_differ } => Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::Other,
            description: if *amounts_differ {
                format!(
                    "Fee '{}' is authorized at conflicting amounts by multiple rates; expected amount is ambiguous",
                    fee_code
                )
            } else {
                format!(
                    "Fee '{}' is authorized with conflicting charge types by multiple rates; expected amount is ambiguous",
                    fee_code
                )
            },
            amount: Money::zero(),
            rule_id: RULE_AMBIGUOUS_FEE.to_string(),
            confidence: Confidence::High,
        }),
        FeeEvaluation::Unclassified { authorized } => Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::Other,
            description: format!(
                "Fee '{}' is authorized at {} but its charge type (exact or cap) has not been set; the expected amount cannot be determined until the fee is classified",
                fee_code, authorized
            ),
            amount: Money::zero(),
            rule_id: RULE_UNCLASSIFIED_FEE.to_string(),
            confidence: Confidence::High,
        }),
        FeeEvaluation::Priced {
            charge_type,
            authorized,
            finding_amount: Some(amount),
            ..
        } => Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::UnauthorizedFee,
            description: match charge_type {
                FeeChargeType::Exact => format!(
                    "Fee '{}' billed at {} but authorized amount is {}",
                    fee_code, billed, authorized
                ),
                FeeChargeType::Cap => format!(
                    "Fee '{}' billed at {} exceeds the authorized cap of {}",
                    fee_code, billed, authorized
                ),
            },
            amount: *amount,
            rule_id: RULE_UNAUTHORIZED_FEE.to_string(),
            confidence: Confidence::High,
        }),
        FeeEvaluation::Priced {
            finding_amount: None,
            ..
        } => None,
    }
}

/// Dollar amount RULE_04 attributes to a fee line (signed); zero when nothing is flagged or the
/// outcome is unresolved (those report at $0).
fn fee_attributed(options: &[AuthorizedFeeOption], billed: Money) -> Money {
    match evaluate_authorized_fee(options, billed) {
        FeeEvaluation::NotAuthorized => billed,
        FeeEvaluation::Priced { finding_amount, .. } => finding_amount.unwrap_or_else(Money::zero),
        FeeEvaluation::Conflicting { .. } | FeeEvaluation::Unclassified { .. } => Money::zero(),
    }
}

pub fn check_rate_and_fees(invoice: &Invoice, rate: &ContractRate) -> Vec<DiscrepancyLine> {
    let mut out = Vec::new();

    for line in &invoice.line_items {
        if let Some(fee_code) = &line.fee_code {
            // The shared evaluator decides EXACT / CAP / unclassified / conflicting outcomes.
            let options = authorized_fee_options(&[rate], fee_code);
            let evaluation = evaluate_authorized_fee(&options, line.line_total);
            out.extend(fee_finding(
                fee_code,
                &line.description,
                line.line_total,
                &evaluation,
            ));

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

/// Check primary invoice lines against their own unit-matched rates.
/// Existing single-rate callers continue to use `check_rate_and_fees`.
pub fn check_rate_and_fees_multi(
    invoice: &Invoice,
    rates: &[&ContractRate],
) -> Vec<DiscrepancyLine> {
    let mut out = Vec::new();

    for line in &invoice.line_items {
        if let Some(fee_code) = &line.fee_code {
            let options = authorized_fee_options(rates, fee_code);
            let evaluation = evaluate_authorized_fee(&options, line.line_total);
            out.extend(fee_finding(
                fee_code,
                &line.description,
                line.line_total,
                &evaluation,
            ));

            continue;
        }

        let matching: Vec<&ContractRate> = rates
            .iter()
            .copied()
            .filter(|rate| same_text(&line.unit, &rate.unit))
            .collect();

        // Missing or ambiguous rates are reported by the engine; do not invent
        // a price or duplicate those findings here.
        let [rate] = matching.as_slice() else {
            continue;
        };

        if line.unit_price != rate.unit_rate {
            let expected_line_total = rate.unit_rate * line.quantity;

            out.push(DiscrepancyLine {
                discrepancy_type: DiscrepancyType::RateMismatch,
                description: format!(
                    "Invoiced at {}/{} but contracted rate is {}/{} (qty {})",
                    line.unit_price,
                    line.unit,
                    rate.unit_rate,
                    rate.unit,
                    line.quantity
                ),
                amount: line.line_total - expected_line_total,
                rule_id: RULE_RATE_MISMATCH.to_string(),
                confidence: Confidence::High,
            });
        }
    }

    out
}

pub fn select_rate_for_line<'a>(
    invoice: &Invoice,
    line: &InvoiceLineItem,
    rates: &'a [ContractRate],
) -> Option<&'a ContractRate> {
    let stream_key = waste_stream::key(&invoice.waste_stream)?;

    let mut candidates: Vec<&ContractRate> = rates
        .iter()
        .filter(|rate| {
            rate.vendor_id == invoice.vendor_id
                && rate.covers_date(invoice.service_date)
                && waste_stream::key(&rate.waste_stream).as_deref()
                    == Some(stream_key.as_str())
                && (rate.facility_id.is_none()
                    || rate.facility_id == invoice.facility_id)
                && same_text(&rate.unit, &line.unit)
        })
        .collect();

    candidates.sort_by_key(|rate| {
        (
            if rate.facility_id == invoice.facility_id
                && invoice.facility_id.is_some()
            {
                0
            } else {
                1
            },
            std::cmp::Reverse(rate.effective_from),
        )
    });

    let best = *candidates.first()?;

    // Never silently choose between equally specific, equally recent rates.
    let ambiguous = candidates.iter().skip(1).any(|candidate| {
        candidate.facility_id == best.facility_id
            && candidate.effective_from == best.effective_from
    });

    if ambiguous {
        None
    } else {
        Some(best)
    }
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

    let invoice_qty: Decimal = matching_lines.iter().map(|line| line.quantity).sum();

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

pub fn check_weight_mismatch(
    invoice_weight: Decimal,
    ticket: &WeightTicket,
    tol: &Tolerances,
) -> Option<DiscrepancyLine> {
    if ticket.net_weight.is_zero() {
        return None;
    }
    let diff_pct =
        ((invoice_weight - ticket.net_weight).abs() / ticket.net_weight) * Decimal::from(100);
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
    } else {
        None
    }
}

pub fn check_stale_rate(invoice: &Invoice, rate: &ContractRate) -> Option<DiscrepancyLine> {
    if !rate.covers_date(invoice.service_date) {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::ContractTermMismatch,
            description: format!(
                "Service date {} falls outside rate's effective window ({} to {})",
                invoice.service_date,
                rate.effective_from,
                rate.effective_to
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "open".into())
            ),
            amount: Money::zero(),
            rule_id: RULE_STALE_RATE.to_string(),
            confidence: Confidence::High,
        })
    } else {
        None
    }
}

pub fn check_container_count(
    invoice_container_count: Option<i32>,
    manifest: Option<&Manifest>,
    ticket: Option<&WeightTicket>,
) -> Option<DiscrepancyLine> {
    let manifest_count = manifest.and_then(|m| m.container_count);
    let ticket_count = ticket.and_then(|t| t.container_count);
    let counts: Vec<i32> = [invoice_container_count, manifest_count, ticket_count]
        .into_iter()
        .flatten()
        .collect();
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
    } else {
        None
    }
}

pub fn check_missing_support(
    manifest: Option<&Manifest>,
    ticket: Option<&WeightTicket>,
) -> Option<DiscrepancyLine> {
    if manifest.is_none() && ticket.is_none() {
        Some(DiscrepancyLine {
            discrepancy_type: DiscrepancyType::MissingSupport,
            description: "No manifest or weight ticket found to support this invoice; amounts cannot be independently confirmed".into(),
            amount: Money::zero(),
            rule_id: RULE_MISSING_SUPPORT.to_string(),
            confidence: Confidence::Low,
        })
    } else {
        None
    }
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
    let Some(rate) = rate else {
        return Money::zero();
    };
    if same_text(&line.unit, &rate.unit) && line.fee_code.is_none() {
        if line.unit_price != rate.unit_rate {
            line.line_total - rate.unit_rate * line.quantity
        } else {
            Money::zero()
        }
    } else if let Some(code) = &line.fee_code {
        fee_attributed(&authorized_fee_options(&[rate], code), line.line_total)
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
        if !billable(line) {
            continue;
        }
        if invoice.line_items[..i]
            .iter()
            .any(|o| billable(o) && lines_identical(o, line))
        {
            found.push((i, None));
        }
    }
    for (i, line) in invoice.line_items.iter().enumerate() {
        if !billable(line) || found.iter().any(|(j, _)| *j == i) {
            continue;
        }
        if let Some(other) = related
            .iter()
            .find(|r| r.line_items.iter().any(|o| lines_identical(o, line)))
        {
            found.push((i, Some(other.invoice_number.clone())));
        }
    }
    found.sort_by_key(|(i, _)| *i);
    found
}

pub fn duplicate_line_indices(invoice: &Invoice, related: &[RelatedInvoice]) -> Vec<usize> {
    find_duplicates(invoice, related)
        .into_iter()
        .map(|(i, _)| i)
        .collect()
}

/// RULE_05 duplicate charge. Flags (a) a line that appears more than once, identically, on the same
/// invoice and (b) a line identical to a line on an earlier invoice from the same vendor for the same
/// service date. The reported dollar amount is the duplicate line's total MINUS whatever RULE_03/04
/// already attribute to that line, so the discrepancy total never double counts; the engine also
/// leaves duplicate lines out of the expected amount, so `invoiced - expected` equals the sum of
/// findings. Credit/zero lines are ignored. Findings go to human review: a repeated identical charge
/// can occasionally be legitimate.
pub fn check_duplicate_charges(
    invoice: &Invoice,
    rate: Option<&ContractRate>,
    related: &[RelatedInvoice],
) -> Vec<DiscrepancyLine> {
    find_duplicates(invoice, related)
        .into_iter()
        .map(|(i, other)| {
            let line = &invoice.line_items[i];
            let (description, confidence) = match other {
                None => (
                    format!(
                        "Line '{}' ({} x {} = {}) is billed more than once on this invoice",
                        line.description.trim(),
                        line.quantity,
                        line.unit_price,
                        line.line_total
                    ),
                    Confidence::Medium,
                ),
                Some(num) => (
                    format!(
                        "Line '{}' ({}) was already billed on invoice {} for the same service date",
                        line.description.trim(),
                        line.line_total,
                        num
                    ),
                    Confidence::High,
                ),
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

/// Multi-rate version of RULE_05. Existing single-rate callers remain supported.
pub fn check_duplicate_charges_multi(
    invoice: &Invoice,
    rates: &[&ContractRate],
    related: &[RelatedInvoice],
) -> Vec<DiscrepancyLine> {
    find_duplicates(invoice, related)
        .into_iter()
        .map(|(i, other)| {
            let line = &invoice.line_items[i];

            let (description, confidence) = match other {
                None => (
                    format!(
                        "Line '{}' ({} x {} = {}) is billed more than once on this invoice",
                        line.description.trim(),
                        line.quantity,
                        line.unit_price,
                        line.line_total
                    ),
                    Confidence::Medium,
                ),
                Some(num) => (
                    format!(
                        "Line '{}' ({}) was already billed on invoice {} for the same service date",
                        line.description.trim(),
                        line.line_total,
                        num
                    ),
                    Confidence::High,
                ),
            };

            DiscrepancyLine {
                discrepancy_type: DiscrepancyType::DuplicateCharge,
                description,
                amount: line.line_total - already_attributed_multi(line, rates),
                rule_id: RULE_DUPLICATE_CHARGE.to_string(),
                confidence,
            }
        })
        .collect()
}

fn already_attributed_multi(
    line: &InvoiceLineItem,
    rates: &[&ContractRate],
) -> Money {
    if line.fee_code.is_none() {
        let matching: Vec<&ContractRate> = rates
            .iter()
            .copied()
            .filter(|rate| same_text(&line.unit, &rate.unit))
            .collect();

        if let [rate] = matching.as_slice() {
            if line.unit_price != rate.unit_rate {
                return line.line_total - rate.unit_rate * line.quantity;
            }
        }

        return Money::zero();
    }

    let Some(code) = line.fee_code.as_deref() else {
        return Money::zero();
    };

    fee_attributed(&authorized_fee_options(rates, code), line.line_total)
}

/// RULE_08 waste stream mismatch: the invoice bills a different waste stream than the manifest
/// documents. No dollar amount can be derived (the correct price depends on the true stream), so the
/// finding is reported at $0 and must be resolved by a reviewer.
pub fn check_waste_stream_mismatch(
    invoice: &Invoice,
    manifest: &Manifest,
) -> Option<DiscrepancyLine> {
    // Same canonical key as rate lookup (case and whitespace folded). A blank stream on either
    // side is unknown, not a mismatch.
    if waste_stream::key(&invoice.waste_stream).is_none()
        || waste_stream::key(&manifest.waste_stream).is_none()
        || waste_stream::same(&invoice.waste_stream, &manifest.waste_stream)
    {
        return None;
    }
    Some(DiscrepancyLine {
        discrepancy_type: DiscrepancyType::WrongWasteStream,
        description: format!(
            "Invoice bills waste stream '{}' but manifest #{} documents '{}'",
            invoice.waste_stream.trim(),
            manifest.manifest_number,
            manifest.waste_stream.trim()
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
            id: Uuid::new_v4(),
            vendor_id: Uuid::new_v4(),
            waste_stream: "F003".into(),
            facility_id: None,
            unit_rate: Money::from_str_exact("400.00").unwrap(),
            unit: "drum".into(),
            authorized_fees: vec![AuthorizedFee {
                fee_code: "TRANSPORT".into(),
                description: "Transportation".into(),
                amount: Money::from_str_exact("500.00").unwrap(),
                charge_type: Some(FeeChargeType::Exact),
            }],
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
        }
    }

    fn sample_invoice_with_overcharge() -> Invoice {
        Invoice {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            vendor_id: Uuid::new_v4(),
            invoice_number: "INV-1001".into(),
            invoice_date: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            service_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            facility_id: None,
            waste_stream: "F003".into(),
            line_items: vec![
                InvoiceLineItem {
                    id: Uuid::new_v4(),
                    description: "Drum disposal".into(),
                    fee_code: None,
                    quantity: Decimal::from(10),
                    unit: "drum".into(),
                    unit_price: Money::from_str_exact("450.00").unwrap(),
                    line_total: Money::from_str_exact("4500.00").unwrap(),
                },
                InvoiceLineItem {
                    id: Uuid::new_v4(),
                    description: "Transportation".into(),
                    fee_code: Some("TRANSPORT".into()),
                    quantity: Decimal::from(1),
                    unit: "flat".into(),
                    unit_price: Money::from_str_exact("500.00").unwrap(),
                    line_total: Money::from_str_exact("500.00").unwrap(),
                },
                InvoiceLineItem {
                    id: Uuid::new_v4(),
                    description: "Environmental fee".into(),
                    fee_code: Some("ENV_FEE".into()),
                    quantity: Decimal::from(1),
                    unit: "flat".into(),
                    unit_price: Money::from_str_exact("200.00").unwrap(),
                    line_total: Money::from_str_exact("200.00").unwrap(),
                },
            ],
            total: Money::from_str_exact("5200.00").unwrap(),
            source_document_id: None,
        }
    }

    #[test]
    fn spec_section_55_worked_example_produces_700_dollar_discrepancy() {
        let rate = sample_rate();
        let invoice = sample_invoice_with_overcharge();
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        assert_eq!(
            discrepancies.len(),
            2,
            "expected exactly 2 discrepancy lines: {discrepancies:?}"
        );
        let rate_line = discrepancies
            .iter()
            .find(|d| d.discrepancy_type == DiscrepancyType::RateMismatch)
            .expect("expected a RateMismatch line");
        assert_eq!(rate_line.amount, Money::from_str_exact("500.00").unwrap());
        let fee_line = discrepancies
            .iter()
            .find(|d| d.discrepancy_type == DiscrepancyType::UnauthorizedFee)
            .expect("expected an UnauthorizedFee line");
        assert_eq!(fee_line.amount, Money::from_str_exact("200.00").unwrap());
        let total_discrepancy = rate_line.amount + fee_line.amount;
        assert_eq!(
            total_discrepancy,
            Money::from_str_exact("700.00").unwrap(),
            "spec section 55 requires the total discrepancy to be exactly $700.00"
        );
    }

    #[test]
    fn exact_match_produces_no_discrepancy() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        invoice.line_items[0].line_total = Money::from_str_exact("4000.00").unwrap();
        invoice.line_items.remove(2);
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        assert!(
            discrepancies.is_empty(),
            "expected no discrepancies: {discrepancies:?}"
        );
    }

    #[test]
    fn zero_quantity_line_does_not_panic_or_false_flag() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].quantity = Decimal::ZERO;
        invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        invoice.line_items[0].line_total = Money::zero();
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        assert!(!discrepancies
            .iter()
            .any(|d| d.discrepancy_type == DiscrepancyType::RateMismatch));
    }

    #[test]
    fn negative_credit_line_reduces_discrepancy_correctly() {
        let rate = sample_rate();
        let mut invoice = sample_invoice_with_overcharge();
        invoice.line_items[0].unit_price = Money::from_str_exact("350.00").unwrap();
        invoice.line_items[0].line_total = Money::from_str_exact("3500.00").unwrap();
        invoice.line_items.remove(2);
        let discrepancies = check_rate_and_fees(&invoice, &rate);
        let rate_line = discrepancies
            .iter()
            .find(|d| d.discrepancy_type == DiscrepancyType::RateMismatch)
            .unwrap();
        assert!(
            rate_line.amount.is_negative(),
            "underbilling should produce a negative discrepancy amount"
        );
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

        let finding = check_quantity_vs_manifest(&invoice, &manifest, &Tolerances::default())
            .expect("incompatible units must be flagged");

        assert_eq!(finding.rule_id, RULE_UNIT_MISMATCH);
        assert_eq!(finding.amount, Money::zero());
        assert!(finding.description.contains("drum"));
        assert!(finding.description.contains("kg"));
    }

    #[test]
    fn quantity_matches_manifest_produces_no_flag() {
        let manifest = Manifest {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            manifest_number: "M-1".into(),
            waste_stream: "F003".into(),
            container_count: Some(10),
            quantity: Decimal::from(10),
            unit: "drum".into(),
            shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };
        let invoice = sample_invoice_with_overcharge();
        let tol = Tolerances::default();
        assert!(check_quantity_vs_manifest(&invoice, &manifest, &tol).is_none());
    }

    #[test]
    fn quantity_mismatch_vs_manifest_is_flagged() {
        let manifest = Manifest {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            manifest_number: "M-1".into(),
            waste_stream: "F003".into(),
            container_count: Some(8),
            quantity: Decimal::from(8),
            unit: "drum".into(),
            shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };
        let invoice = sample_invoice_with_overcharge();
        let tol = Tolerances::default();
        assert!(check_quantity_vs_manifest(&invoice, &manifest, &tol).is_some());
    }

    #[test]
    fn weight_within_tolerance_not_flagged() {
        let ticket = WeightTicket {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            manifest_id: None,
            ticket_number: "WT-1".into(),
            net_weight: Decimal::from(1000),
            unit: "lb".into(),
            container_count: None,
            ticket_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };
        let tol = Tolerances::default();
        let invoice_weight = Decimal::from(1015);
        assert!(check_weight_mismatch(invoice_weight, &ticket, &tol).is_none());
    }

    #[test]
    fn weight_beyond_tolerance_is_flagged() {
        let ticket = WeightTicket {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            manifest_id: None,
            ticket_number: "WT-1".into(),
            net_weight: Decimal::from(1000),
            unit: "lb".into(),
            container_count: None,
            ticket_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };
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
        let manifest = Manifest {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            manifest_number: "M-1".into(),
            waste_stream: "F003".into(),
            container_count: Some(9),
            quantity: Decimal::from(9),
            unit: "drum".into(),
            shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };
        assert!(check_container_count(Some(10), Some(&manifest), None).is_some());
    }

    #[test]
    fn missing_support_flagged_when_no_manifest_or_ticket() {
        assert!(check_missing_support(None, None).is_some());
    }

    #[test]
    fn missing_support_not_flagged_when_manifest_present() {
        let manifest = Manifest {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            manifest_number: "M-1".into(),
            waste_stream: "F003".into(),
            container_count: Some(10),
            quantity: Decimal::from(10),
            unit: "drum".into(),
            shipment_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        };
        assert!(check_missing_support(Some(&manifest), None).is_none());
    }

    // ---- RULE_05 / RULE_08 ----
    fn dup_invoice(lines: Vec<InvoiceLineItem>) -> Invoice {
        let total = lines.iter().fold(Money::zero(), |a, l| a + l.line_total);
        Invoice {
            id: uuid::Uuid::new_v4(),
            organization_id: uuid::Uuid::new_v4(),
            vendor_id: uuid::Uuid::new_v4(),
            invoice_number: "INV-2".into(),
            invoice_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            service_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            facility_id: None,
            waste_stream: "F003".into(),
            line_items: lines,
            total,
            source_document_id: None,
        }
    }
    fn dline(desc: &str, fee: Option<&str>, qty: i64, price: i64, total: i64) -> InvoiceLineItem {
        InvoiceLineItem {
            id: uuid::Uuid::new_v4(),
            description: desc.into(),
            fee_code: fee.map(|f| f.to_string()),
            quantity: Decimal::from(qty),
            unit: "drum".into(),
            unit_price: Money::from_decimal(Decimal::from(price)),
            line_total: Money::from_decimal(Decimal::from(total)),
        }
    }
    fn drate() -> ContractRate {
        ContractRate {
            id: uuid::Uuid::new_v4(),
            vendor_id: uuid::Uuid::new_v4(),
            waste_stream: "F003".into(),
            facility_id: None,
            unit_rate: Money::from_decimal(Decimal::from(400)),
            unit: "drum".into(),
            authorized_fees: vec![AuthorizedFee {
                fee_code: "TRANSPORT".into(),
                description: "t".into(),
                amount: Money::from_decimal(Decimal::from(500)),
                charge_type: Some(FeeChargeType::Exact),
            }],
            effective_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
        }
    }

    #[test]
    fn identical_line_twice_on_one_invoice_is_a_duplicate_for_its_full_value() {
        let inv = dup_invoice(vec![
            dline("Drum disposal", None, 10, 400, 4000),
            dline("Transportation", Some("TRANSPORT"), 1, 500, 500),
            dline("Transportation", Some("TRANSPORT"), 1, 500, 500),
        ]);
        let found = check_duplicate_charges(&inv, Some(&drate()), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule_id, RULE_DUPLICATE_CHARGE);
        assert_eq!(found[0].amount, Money::from_decimal(Decimal::from(500)));
        assert!(matches!(
            found[0].discrepancy_type,
            DiscrepancyType::DuplicateCharge
        ));
    }

    #[test]
    fn duplicate_amount_never_double_counts_what_rate_and_fee_rules_already_claim() {
        // Duplicated disposal line billed at $450 vs contract $400: RULE_03 already claims $500 on the
        // duplicate line; RULE_05 may only add the remaining $4000 so the sum is exactly the $4500 line.
        let l = dline("Drum disposal", None, 10, 450, 4500);
        let inv = dup_invoice(vec![
            l.clone(),
            InvoiceLineItem {
                id: uuid::Uuid::new_v4(),
                ..l
            },
        ]);
        let rate = drate();
        let rule3: Money = check_rate_and_fees(&inv, &rate)
            .iter()
            .fold(Money::zero(), |a, d| a + d.amount);
        let rule5 = check_duplicate_charges(&inv, Some(&rate), &[]);
        assert_eq!(rule5.len(), 1);
        assert_eq!(rule5[0].amount, Money::from_decimal(Decimal::from(4000)));
        // Two lines each $500 over the contracted rate (RULE_03 = $1,000) plus the $4,000 duplicate load
        // = $5,000 = invoiced $9,000 minus the $4,000 a single load should cost.
        assert_eq!(
            rule3 + rule5[0].amount,
            Money::from_decimal(Decimal::from(5000))
        );
        // A duplicated UNAUTHORIZED fee is already fully claimed by RULE_04: RULE_05 adds $0, not another $200.
        let fee = dline("Environmental fee", Some("ENV_FEE"), 1, 200, 200);
        let inv2 = dup_invoice(vec![
            fee.clone(),
            InvoiceLineItem {
                id: uuid::Uuid::new_v4(),
                ..fee
            },
        ]);
        let r5 = check_duplicate_charges(&inv2, Some(&rate), &[]);
        assert_eq!(r5.len(), 1);
        assert_eq!(r5[0].amount, Money::zero());
    }

    #[test]
    fn line_matching_an_earlier_invoice_is_flagged_and_names_that_invoice() {
        let earlier = RelatedInvoice {
            id: uuid::Uuid::new_v4(),
            invoice_number: "INV-1".into(),
            line_items: vec![dline("drum disposal", None, 10, 400, 4000)],
        };
        let inv = dup_invoice(vec![dline("Drum disposal ", None, 10, 400, 4000)]);
        let found = check_duplicate_charges(&inv, Some(&drate()), &[earlier]);
        assert_eq!(found.len(), 1);
        assert!(found[0].description.contains("INV-1"));
        assert_eq!(found[0].amount, Money::from_decimal(Decimal::from(4000)));
        assert!(matches!(found[0].confidence, Confidence::High));
    }

    #[test]
    fn different_lines_credits_and_zero_lines_are_not_duplicates() {
        let inv = dup_invoice(vec![
            dline("Drum disposal", None, 10, 400, 4000),
            dline("Drum disposal", None, 5, 400, 2000),
            dline("Credit", None, 1, -100, -100),
            dline("Credit", None, 1, -100, -100),
            dline("Note", None, 1, 0, 0),
            dline("Note", None, 1, 0, 0),
        ]);
        assert!(check_duplicate_charges(&inv, Some(&drate()), &[]).is_empty());
        let other = RelatedInvoice {
            id: uuid::Uuid::new_v4(),
            invoice_number: "X".into(),
            line_items: vec![dline("Drum disposal", None, 11, 400, 4400)],
        };
        assert!(check_duplicate_charges(&inv, Some(&drate()), &[other]).is_empty());
    }

    #[test]
    fn waste_stream_mismatch_is_flagged_case_insensitively_and_has_no_invented_amount() {
        let mut inv = dup_invoice(vec![dline("Drum disposal", None, 10, 400, 4000)]);
        let m = |stream: &str| Manifest {
            id: uuid::Uuid::new_v4(),
            organization_id: uuid::Uuid::new_v4(),
            manifest_number: "M-1".into(),
            waste_stream: stream.into(),
            container_count: None,
            quantity: Decimal::from(10),
            unit: "drum".into(),
            shipment_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 9).unwrap(),
            source_document_id: None,
        };
        assert!(check_waste_stream_mismatch(&inv, &m(" f003 ")).is_none());
        let d = check_waste_stream_mismatch(&inv, &m("F005")).expect("mismatch");
        assert_eq!(d.rule_id, RULE_WASTE_STREAM_MISMATCH);
        assert!(matches!(
            d.discrepancy_type,
            DiscrepancyType::WrongWasteStream
        ));
        assert_eq!(d.amount, Money::zero());
        assert!(
            d.description.contains("F003")
                && d.description.contains("F005")
                && d.description.contains("M-1")
        );
        inv.waste_stream = "".into();
        assert!(
            check_waste_stream_mismatch(&inv, &m("F005")).is_none(),
            "an unknown stream is not a mismatch"
        );
        // Whitespace runs collapse to one space exactly like rate lookup; distinct identifiers still mismatch.
        inv.waste_stream = "F  003".into();
        assert!(check_waste_stream_mismatch(&inv, &m("f 003")).is_none());
        inv.waste_stream = "F003".into();
        assert!(check_waste_stream_mismatch(&inv, &m("F0030")).is_some());
        // Internal whitespace is collapsed, NOT removed: "F 003" is a different identifier from "F003".
        assert!(check_waste_stream_mismatch(&inv, &m("F 003")).is_some());
    }

    // ---- shared authorized-fee evaluator (EXACT / CAP / unclassified / conflicting) ----

    fn usd(n: i64) -> Money {
        Money::from_decimal(Decimal::from(n))
    }

    fn opt(n: i64, charge_type: Option<FeeChargeType>) -> AuthorizedFeeOption {
        AuthorizedFeeOption {
            amount: usd(n),
            charge_type,
        }
    }

    /// (expected contribution, finding amount) of a priced fee; panics for any other outcome.
    fn priced(evaluation: FeeEvaluation) -> (Money, Option<Money>) {
        match evaluation {
            FeeEvaluation::Priced {
                expected,
                finding_amount,
                ..
            } => (expected, finding_amount),
            other => panic!("expected a priced fee, got {other:?}"),
        }
    }

    fn fee_rate(charge_type: Option<FeeChargeType>) -> ContractRate {
        let mut rate = drate();
        rate.authorized_fees[0].charge_type = charge_type;
        rate
    }

    #[test]
    fn exact_fee_flags_any_difference_with_a_signed_amount() {
        let exact = [opt(500, Some(FeeChargeType::Exact))];
        assert_eq!(
            priced(evaluate_authorized_fee(&exact, usd(400))),
            (usd(500), Some(usd(-100)))
        );
        assert_eq!(
            priced(evaluate_authorized_fee(&exact, usd(500))),
            (usd(500), None)
        );
        assert_eq!(
            priced(evaluate_authorized_fee(&exact, usd(600))),
            (usd(500), Some(usd(100)))
        );
    }

    #[test]
    fn cap_fee_prices_at_the_lower_of_billed_and_cap_and_flags_only_the_excess() {
        let cap = [opt(500, Some(FeeChargeType::Cap))];
        assert_eq!(
            priced(evaluate_authorized_fee(&cap, usd(400))),
            (usd(400), None),
            "a bill below the cap is not a discrepancy and is expected at the billed amount"
        );
        assert_eq!(
            priced(evaluate_authorized_fee(&cap, usd(500))),
            (usd(500), None)
        );
        assert_eq!(
            priced(evaluate_authorized_fee(&cap, usd(600))),
            (usd(500), Some(usd(100))),
            "only the amount above the cap is flagged"
        );
    }

    #[test]
    fn credits_follow_the_same_sign_convention_for_exact_and_cap() {
        // EXACT keeps its existing behavior: a credit differs from the authorized amount.
        assert_eq!(
            priced(evaluate_authorized_fee(
                &[opt(500, Some(FeeChargeType::Exact))],
                usd(-500)
            )),
            (usd(500), Some(usd(-1000)))
        );
        // A credit is below any cap: priced at the credit itself and never flagged.
        assert_eq!(
            priced(evaluate_authorized_fee(
                &[opt(500, Some(FeeChargeType::Cap))],
                usd(-50)
            )),
            (usd(-50), None)
        );
    }

    #[test]
    fn priced_fees_always_satisfy_billed_minus_expected_equals_finding() {
        for charge_type in [FeeChargeType::Exact, FeeChargeType::Cap] {
            for billed in [-600, -500, -1, 0, 1, 400, 499, 500, 501, 600, 100_000] {
                let (expected, finding) = priced(evaluate_authorized_fee(
                    &[opt(500, Some(charge_type))],
                    usd(billed),
                ));
                assert_eq!(
                    usd(billed) - expected,
                    finding.unwrap_or_else(Money::zero),
                    "{charge_type:?} billed {billed}"
                );
            }
        }
    }

    #[test]
    fn unclassified_fee_is_never_inferred_as_exact_or_cap() {
        for billed in [400, 500, 600] {
            let evaluation = evaluate_authorized_fee(&[opt(500, None)], usd(billed));
            assert_eq!(
                evaluation,
                FeeEvaluation::Unclassified {
                    authorized: usd(500)
                },
                "billed {billed}"
            );
            assert!(evaluation.is_unresolved());
            let finding = fee_finding("TRANSPORT", "Transportation", usd(billed), &evaluation)
                .expect("an unclassified fee must be reported for review");
            assert_eq!(finding.rule_id, RULE_UNCLASSIFIED_FEE);
            assert_eq!(finding.amount, Money::zero());
            assert!(matches!(finding.discrepancy_type, DiscrepancyType::Other));
        }
    }

    #[test]
    fn missing_authorization_is_not_authorized() {
        let evaluation = evaluate_authorized_fee(&[], usd(200));
        assert_eq!(evaluation, FeeEvaluation::NotAuthorized);
        assert!(!evaluation.is_unresolved());
    }

    #[test]
    fn conflicting_authorizations_are_unresolved_and_never_pick_a_candidate() {
        // Different amounts.
        let amounts = evaluate_authorized_fee(
            &[
                opt(500, Some(FeeChargeType::Exact)),
                opt(450, Some(FeeChargeType::Exact)),
            ],
            usd(500),
        );
        assert_eq!(
            amounts,
            FeeEvaluation::Conflicting {
                amounts_differ: true
            }
        );
        // Same amount, different types.
        let types = evaluate_authorized_fee(
            &[
                opt(500, Some(FeeChargeType::Exact)),
                opt(500, Some(FeeChargeType::Cap)),
            ],
            usd(500),
        );
        assert_eq!(
            types,
            FeeEvaluation::Conflicting {
                amounts_differ: false
            }
        );
        // A classified and an unclassified authorization of the same amount are also ambiguous.
        let mixed = evaluate_authorized_fee(
            &[opt(500, Some(FeeChargeType::Exact)), opt(500, None)],
            usd(500),
        );
        assert!(mixed.is_unresolved());
        // Identical authorizations from several rates are one authorization, not a conflict.
        let same = evaluate_authorized_fee(
            &[
                opt(500, Some(FeeChargeType::Cap)),
                opt(500, Some(FeeChargeType::Cap)),
            ],
            usd(400),
        );
        assert_eq!(priced(same), (usd(400), None));
    }

    #[test]
    fn fee_findings_keep_exact_wording_and_describe_the_cap_explicitly() {
        let exact = evaluate_authorized_fee(&[opt(500, Some(FeeChargeType::Exact))], usd(600));
        let f = fee_finding("TRANSPORT", "Transportation", usd(600), &exact).unwrap();
        // This text is matched when review decisions are carried forward; it must not change.
        assert_eq!(
            f.description,
            "Fee 'TRANSPORT' billed at $600.00 but authorized amount is $500.00"
        );
        assert_eq!(f.amount, usd(100));
        assert_eq!(f.rule_id, RULE_UNAUTHORIZED_FEE);
        assert!(matches!(f.discrepancy_type, DiscrepancyType::UnauthorizedFee));

        let cap = evaluate_authorized_fee(&[opt(500, Some(FeeChargeType::Cap))], usd(600));
        let f = fee_finding("TRANSPORT", "Transportation", usd(600), &cap).unwrap();
        assert_eq!(
            f.description,
            "Fee 'TRANSPORT' billed at $600.00 exceeds the authorized cap of $500.00"
        );
        assert_eq!(f.amount, usd(100));
        assert_eq!(f.rule_id, RULE_UNAUTHORIZED_FEE);

        let below_cap = evaluate_authorized_fee(&[opt(500, Some(FeeChargeType::Cap))], usd(400));
        assert!(fee_finding("TRANSPORT", "Transportation", usd(400), &below_cap).is_none());

        let none = evaluate_authorized_fee(&[], usd(200));
        let f = fee_finding("ENV_FEE", "Environmental fee", usd(200), &none).unwrap();
        assert_eq!(
            f.description,
            "Fee 'ENV_FEE' (Environmental fee) billed but not found in contracted authorized fees"
        );
        assert_eq!(f.amount, usd(200));

        let conflict = evaluate_authorized_fee(
            &[
                opt(500, Some(FeeChargeType::Exact)),
                opt(450, Some(FeeChargeType::Exact)),
            ],
            usd(500),
        );
        let f = fee_finding("TRANSPORT", "Transportation", usd(500), &conflict).unwrap();
        assert_eq!(
            f.description,
            "Fee 'TRANSPORT' is authorized at conflicting amounts by multiple rates; expected amount is ambiguous"
        );
        assert_eq!(f.rule_id, RULE_AMBIGUOUS_FEE);
        assert_eq!(f.amount, Money::zero());
    }

    #[test]
    fn fee_codes_match_ignoring_case_and_surrounding_whitespace() {
        assert!(fee_codes_match(" transport ", "TRANSPORT"));
        assert!(!fee_codes_match("TRANSPORT", "TRANSPORTS"));
        let rate = drate();
        let options = authorized_fee_options(&[&rate], "  transport");
        assert_eq!(options, vec![opt(500, Some(FeeChargeType::Exact))]);
    }

    #[test]
    fn legacy_single_rate_helper_delegates_cap_exact_and_unclassified_semantics() {
        let bill = |amount: i64| {
            dup_invoice(vec![dline(
                "Transportation",
                Some("TRANSPORT"),
                1,
                amount,
                amount,
            )])
        };
        let fee_findings = |findings: Vec<DiscrepancyLine>| -> Vec<DiscrepancyLine> {
            findings
                .into_iter()
                .filter(|d| d.rule_id != RULE_UNIT_MISMATCH)
                .collect()
        };

        let cap = fee_rate(Some(FeeChargeType::Cap));
        assert!(fee_findings(check_rate_and_fees(&bill(400), &cap)).is_empty());
        let above = fee_findings(check_rate_and_fees(&bill(600), &cap));
        assert_eq!(above.len(), 1);
        assert_eq!(above[0].amount, usd(100));

        let exact = fee_rate(Some(FeeChargeType::Exact));
        let below = fee_findings(check_rate_and_fees(&bill(400), &exact));
        assert_eq!(below.len(), 1);
        assert_eq!(below[0].amount, usd(-100));

        let unclassified = fee_rate(None);
        let findings = fee_findings(check_rate_and_fees(&bill(500), &unclassified));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, RULE_UNCLASSIFIED_FEE);
        assert_eq!(findings[0].amount, Money::zero());
    }

    #[test]
    fn legacy_multi_rate_helper_reports_conflicting_fee_types_without_choosing() {
        let cap = fee_rate(Some(FeeChargeType::Cap));
        let exact = fee_rate(Some(FeeChargeType::Exact));
        let inv = dup_invoice(vec![dline("Transportation", Some("TRANSPORT"), 1, 400, 400)]);
        let findings = check_rate_and_fees_multi(&inv, &[&cap, &exact]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, RULE_AMBIGUOUS_FEE);
        assert_eq!(findings[0].amount, Money::zero());
    }

    #[test]
    fn duplicate_fee_attribution_follows_exact_and_cap_semantics() {
        let inv = dup_invoice(vec![
            dline("Transportation", Some("TRANSPORT"), 1, 400, 400),
            dline("Transportation", Some("TRANSPORT"), 1, 400, 400),
        ]);
        // EXACT $500 billed $400: RULE_04 already claims -$100 on the duplicate, so RULE_05 adds the
        // remaining $500 and the two together equal the duplicate line's $400.
        let exact = fee_rate(Some(FeeChargeType::Exact));
        let r5 = check_duplicate_charges(&inv, Some(&exact), &[]);
        assert_eq!(r5.len(), 1);
        assert_eq!(r5[0].amount, usd(500));
        // CAP $500 billed $400: RULE_04 claims nothing, so the whole duplicate line is RULE_05's.
        let cap = fee_rate(Some(FeeChargeType::Cap));
        let r5 = check_duplicate_charges(&inv, Some(&cap), &[]);
        assert_eq!(r5.len(), 1);
        assert_eq!(r5[0].amount, usd(400));
        // The multi-rate variant agrees.
        let r5m = check_duplicate_charges_multi(&inv, &[&cap], &[]);
        assert_eq!(r5m[0].amount, usd(400));
    }
}
