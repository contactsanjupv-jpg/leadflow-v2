//! The reconciliation engine orchestrator (spec section 17).

use crate::common::money::Money;
use crate::reconciliation::model::*;
use crate::reconciliation::rules;

/// COMPLETE pricing whose bill-level gap the findings do not explain (beyond entry rounding).
const RULE_UNRECONCILED_TOTAL: &str = "RULE_13_UNRECONCILED_BILL_TOTAL";

pub fn run(input: &ReconciliationInput) -> ReconciliationResult {
    let mut discrepancies: Vec<DiscrepancyLine> = Vec::new();
    let mut trace: Vec<String> = Vec::new();

    trace.push(format!(
        "Reconciling invoice {} (service date {})",
        input.invoice.invoice_number, input.invoice.service_date
    ));

    let service_date = input.invoice.service_date;

    // Only rates that actually cover the service date may price lines.
    // Rates that actually cover the service date may price lines.
    let applicable_rates: Vec<&ContractRate> = input
        .applicable_rates
        .iter()
        .filter(|r| r.covers_date(service_date))
        .collect();

    // Anything supplied that does NOT cover the date is treated as stale for reporting only.
    // This preserves the old behaviour when an expired rate was passed in the applicable slot.
    let mut stale_rates: Vec<&ContractRate> = input
        .stale_rates
        .iter()
        .filter(|r| !r.covers_date(service_date))
        .collect();
    for r in &input.applicable_rates {
        if !r.covers_date(service_date) {
            stale_rates.push(r);
        }
    }

    // Duplicate lines are excluded from expected pricing (RULE_05).
    let dups = rules::duplicate_line_indices(&input.invoice, &input.related_invoices);
    let counted = |i: usize| !dups.contains(&i);

    let mut priced_lines: Vec<PricedInvoiceLine> = Vec::new();
    let mut expected = Money::zero();
    let mut unpriced_primary = 0usize;
    let mut unpriced_fee = 0usize;

    // ---- Primary (non-fee) lines + fees ----
    // Only run when at least one rate actually covers the service date.
    // An expired rate (even if it was supplied in applicable_rates) must never
    // produce rate-mismatch or unauthorized-fee findings.
    if !applicable_rates.is_empty() {
        for (idx, line) in input.invoice.line_items.iter().enumerate() {
            if !counted(idx) || line.fee_code.is_some() {
                continue;
            }

            let matches: Vec<&ContractRate> = applicable_rates
                .iter()
                .copied()
                .filter(|r| r.unit.trim().eq_ignore_ascii_case(line.unit.trim()))
                .collect();

            match matches.as_slice() {
                [rate] => {
                    let expected_line = rate.unit_rate * line.quantity;
                    expected = expected + expected_line;
                    priced_lines.push(PricedInvoiceLine {
                        invoice_line_item_id: line.id,
                        contract_rate_id: rate.id,
                        expected_line_amount: expected_line,
                    });
                    trace.push(format!(
                        "Priced line '{}' ({} {}): {} x {} = {} using rate {}",
                        line.description.trim(),
                        line.quantity,
                        line.unit,
                        line.quantity,
                        rate.unit_rate,
                        expected_line,
                        rate.id
                    ));

                    if line.unit_price != rate.unit_rate {
                        let diff = line.line_total - expected_line;
                        let d = DiscrepancyLine {
                            discrepancy_type: DiscrepancyType::RateMismatch,
                            description: format!(
                                "Invoiced at {}/{} but contracted rate is {}/{} (qty {})",
                                line.unit_price, line.unit, rate.unit_rate, rate.unit, line.quantity
                            ),
                            amount: diff,
                            rule_id: rules::RULE_RATE_MISMATCH.to_string(),
                            confidence: Confidence::High,
                        };
                        trace.push(format!(
                            "RULE {}: {} (amount: {})",
                            d.rule_id, d.description, d.amount
                        ));
                        discrepancies.push(d);
                    }
                }
                [] => {
                    unpriced_primary += 1;
                    trace.push(format!(
                        "Primary line '{}' unit '{}' has no matching applicable rate; no unit conversion is configured",
                        line.description.trim(),
                        line.unit
                    ));
                    discrepancies.push(DiscrepancyLine {
                        discrepancy_type: DiscrepancyType::Other,
                        description: format!(
                            "Invoice line '{}' uses unit '{}' but no applicable contract rate was found for that unit; no unit conversion is configured",
                            line.description, line.unit
                        ),
                        amount: Money::zero(),
                        rule_id: rules::RULE_UNIT_MISMATCH.to_string(),
                        confidence: Confidence::Medium,
                    });
                }
                _ => {
                    unpriced_primary += 1;
                    trace.push(format!(
                        "Primary line '{}' unit '{}' matches {} applicable rates; refusing to choose",
                        line.description.trim(),
                        line.unit,
                        matches.len()
                    ));
                    discrepancies.push(DiscrepancyLine {
                        discrepancy_type: DiscrepancyType::Other,
                        description: format!(
                            "Invoice line '{}' unit '{}' has multiple applicable contract rates; pricing is ambiguous",
                            line.description, line.unit
                        ),
                        amount: Money::zero(),
                        rule_id: "RULE_11_AMBIGUOUS_RATE".to_string(),
                        confidence: Confidence::High,
                    });
                }
            }
        }

        // Fees
        for (idx, line) in input.invoice.line_items.iter().enumerate() {
            if !counted(idx) {
                continue;
            }
            let Some(fee_code) = &line.fee_code else { continue };

            // One shared evaluator decides EXACT / CAP / unclassified / conflicting outcomes.
            let options = rules::authorized_fee_options(&applicable_rates, fee_code);
            let evaluation = rules::evaluate_authorized_fee(&options, line.line_total);

            match evaluation {
                rules::FeeEvaluation::Priced {
                    charge_type,
                    authorized,
                    expected: fee_expected,
                    ..
                } => {
                    expected = expected + fee_expected;
                    if let Some(rate) = applicable_rates.iter().find(|r| {
                        r.authorized_fees.iter().any(|f| {
                            rules::fee_codes_match(&f.fee_code, fee_code)
                                && f.amount == authorized
                                && f.charge_type == Some(charge_type)
                        })
                    }) {
                        priced_lines.push(PricedInvoiceLine {
                            invoice_line_item_id: line.id,
                            contract_rate_id: rate.id,
                            expected_line_amount: fee_expected,
                        });
                    }
                    match charge_type {
                        FeeChargeType::Exact => trace.push(format!(
                            "Expected authorized fee '{}': {}",
                            fee_code, fee_expected
                        )),
                        FeeChargeType::Cap => trace.push(format!(
                            "Expected authorized fee '{}' (cap {}): {}",
                            fee_code, authorized, fee_expected
                        )),
                    }
                }
                rules::FeeEvaluation::NotAuthorized => {}
                rules::FeeEvaluation::Conflicting { amounts_differ } => {
                    unpriced_fee += 1;
                    trace.push(if amounts_differ {
                        format!(
                            "Fee '{}' has conflicting authorized amounts across applicable rates",
                            fee_code
                        )
                    } else {
                        format!(
                            "Fee '{}' has conflicting charge types across applicable rates",
                            fee_code
                        )
                    });
                }
                rules::FeeEvaluation::Unclassified { authorized } => {
                    unpriced_fee += 1;
                    trace.push(format!(
                        "Fee '{}' is authorized at {} but its charge type (exact or cap) is not set; expected amount cannot be determined",
                        fee_code, authorized
                    ));
                }
            }

            if let Some(d) =
                rules::fee_finding(fee_code, &line.description, line.line_total, &evaluation)
            {
                if !evaluation.is_unresolved() {
                    trace.push(format!(
                        "RULE {}: {} (amount: {})",
                        d.rule_id, d.description, d.amount
                    ));
                }
                discrepancies.push(d);
            }
        }
    }

    // ---- Pricing status ----
    let (mut pricing_status, mut expected_amount, mut partial_expected_amount) =
        if applicable_rates.is_empty() {
            // No covering rate at all.
            if let Some(stale) = stale_rates.first() {
                if let Some(finding) = rules::check_stale_rate(&input.invoice, stale) {
                    trace.push(
                        "Contract rate for this stream expired before the service date and was not used for pricing; expected amount and bill-level discrepancy cannot be computed.".into(),
                    );
                    trace.push(format!("RULE {}: {}", finding.rule_id, finding.description));
                    discrepancies.push(finding);
                }
            } else {
                trace.push("No contracted rate found covering this invoice's waste stream, facility and service date; expected amount and bill-level discrepancy cannot be computed - manual review required.".into());
                discrepancies.push(DiscrepancyLine {
                    discrepancy_type: DiscrepancyType::MissingSupport,
                    description: "No contracted rate found covering this invoice's service date"
                        .into(),
                    amount: Money::zero(),
                    rule_id: "RULE_00_NO_RATE".to_string(),
                    confidence: Confidence::High,
                });
            }
            (PricingStatus::Uncomputable, None, None)
        } else if unpriced_primary > 0 || unpriced_fee > 0 {
            trace.push(format!(
                "Pricing is PARTIAL: {} is a diagnostic subtotal only; {} primary line(s) and {} fee(s) unresolved",
                expected, unpriced_primary, unpriced_fee
            ));
            (PricingStatus::Partial, None, Some(expected))
        } else {
            (PricingStatus::Complete, Some(expected), None)
        };

    // ---- Remaining existing rules (unchanged behaviour) ----
    if let Some(manifest) = &input.manifest {
        if let Some(d) = rules::check_waste_stream_mismatch(&input.invoice, manifest) {
            trace.push(format!("RULE {}: {}", d.rule_id, d.description));
            discrepancies.push(d);
        }
        if let Some(d) =
            rules::check_quantity_vs_manifest(&input.invoice, manifest, &input.tolerances)
        {
            trace.push(format!("RULE {}: {}", d.rule_id, d.description));
            discrepancies.push(d);
        }
    }

    if let Some(ticket) = &input.weight_ticket {
        let primary_lines: Vec<_> = input
            .invoice
            .line_items
            .iter()
            .filter(|line| line.fee_code.is_none())
            .collect();

        let incompatible_units: Vec<&str> = primary_lines
            .iter()
            .filter(|line| !line.unit.trim().eq_ignore_ascii_case(ticket.unit.trim()))
            .map(|line| line.unit.trim())
            .collect();

        if !incompatible_units.is_empty() {
            let units = incompatible_units.join(", ");
            let finding = DiscrepancyLine {
                discrepancy_type: DiscrepancyType::Other,
                description: format!(
                    "Invoice primary line unit(s) [{}] cannot be directly compared with weight ticket #{} recorded in '{}'; no unit conversion is configured",
                    units, ticket.ticket_number, ticket.unit
                ),
                amount: Money::zero(),
                rule_id: rules::RULE_UNIT_MISMATCH.to_string(),
                confidence: Confidence::Medium,
            };
            trace.push(format!("RULE {}: {}", finding.rule_id, finding.description));
            discrepancies.push(finding);
        }

        let invoice_weight: rust_decimal::Decimal = primary_lines
            .iter()
            .filter(|line| line.unit.trim().eq_ignore_ascii_case(ticket.unit.trim()))
            .map(|line| line.quantity)
            .sum();

        if !invoice_weight.is_zero() {
            if let Some(finding) =
                rules::check_weight_mismatch(invoice_weight, ticket, &input.tolerances)
            {
                trace.push(format!("RULE {}: {}", finding.rule_id, finding.description));
                discrepancies.push(finding);
            }
        }
    }

    if let Some(d) =
        rules::check_container_count(None, input.manifest.as_ref(), input.weight_ticket.as_ref())
    {
        trace.push(format!("RULE {}: {}", d.rule_id, d.description));
        discrepancies.push(d);
    }

    // Duplicate charges – pass None for rate because multi-rate attribution
    // is already handled above; the helper still needs a rate for the
    // "already_attributed" calculation. Using the first applicable rate (if any)
    // preserves existing double-counting protection for the single-rate case.
    // Pass None so already_attributed returns zero for every line.
    // Rate-mismatch findings are already emitted only for counted primaries;
    // the duplicate must therefore receive its full line_total.
    for d in rules::check_duplicate_charges(&input.invoice, None, &input.related_invoices)
    {
        trace.push(format!(
            "RULE {}: {} (amount: {})",
            d.rule_id, d.description, d.amount
        ));
        discrepancies.push(d);
    }

    if let Some(d) =
        rules::check_missing_support(input.manifest.as_ref(), input.weight_ticket.as_ref())
    {
        trace.push(format!("RULE {}: {}", d.rule_id, d.description));
        discrepancies.push(d);
    }

    let identified_finding_amount: Money = discrepancies
        .iter()
        .fold(Money::zero(), |acc, d| acc + d.amount);
    let invoiced_amount = input.invoice.total;

    // A COMPLETE result must reconcile at bill level: invoiced - expected equals the sum of findings.
    // Invoice entry allows one cent of rounding per line (quantity x unit price vs line total) and one
    // cent between the invoice total and its lines, so that is the most a legitimate invoice can leave
    // unexplained. Anything larger is materially inconsistent and must not be reported as COMPLETE.
    if pricing_status == PricingStatus::Complete {
        if let Some(exp) = expected_amount {
            let allowance = Money::from_decimal(
                rust_decimal::Decimal::new(1, 2)
                    * rust_decimal::Decimal::from(input.invoice.line_items.len() as i64 + 1),
            );
            let unexplained = (invoiced_amount - exp) - identified_finding_amount;
            if unexplained > allowance || unexplained < Money::zero() - allowance {
                trace.push(format!(
                    "Pricing is PARTIAL: invoiced {} less expected {} leaves {} unexplained by the findings ({}), more than the {} rounding allowance",
                    invoiced_amount, exp, unexplained, identified_finding_amount, allowance
                ));
                discrepancies.push(DiscrepancyLine {
                    discrepancy_type: DiscrepancyType::Other,
                    description: format!(
                        "Invoice total {} differs from the priced expected amount {} by an amount the findings do not explain ({}); the bill-level discrepancy cannot be reported as complete",
                        invoiced_amount, exp, unexplained
                    ),
                    amount: Money::zero(),
                    rule_id: RULE_UNRECONCILED_TOTAL.to_string(),
                    confidence: Confidence::High,
                });
                pricing_status = PricingStatus::Partial;
                partial_expected_amount = Some(exp);
                expected_amount = None;
            }
        }
    }

    let expected_display = expected_amount
        .map(|a| a.to_string())
        .unwrap_or_else(|| "not computable".to_string());
    trace.push(format!(
        "Pricing status: {} | Expected amount: {} | Invoiced: {} | Identified finding amount (not necessarily total exposure): {}",
        pricing_status.as_db(), expected_display, invoiced_amount, identified_finding_amount
    ));

    let review_status = if discrepancies.is_empty() {
        ReviewStatus::Resolved
    } else {
        ReviewStatus::New
    };

    let discrepancy_amount = if pricing_status == PricingStatus::Complete {
        Some(identified_finding_amount)
    } else {
        None
    };

    ReconciliationResult {
        invoice_id: input.invoice.id,
        pricing_status,
        expected_amount,
        invoiced_amount,
        discrepancy_amount,
        partial_expected_amount,
        identified_finding_amount,
        discrepancies,
        priced_lines,
        tolerances_applied: input.tolerances,
        review_status,
        calculation_trace: trace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use uuid::Uuid;

    fn build_input() -> ReconciliationInput {
        let rate = ContractRate {
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
        };
        let invoice = Invoice {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            vendor_id: rate.vendor_id,
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
        };
        ReconciliationInput {
            invoice,
            related_invoices: vec![],
            applicable_rates: vec![rate],
            stale_rates: vec![],
            manifest: None,
            weight_ticket: None,
            tolerances: Tolerances::default(),
        }
    }

    #[test]
    fn multi_unit_invoice_prices_each_line_against_its_own_rate() {
        // NES-TEST-1048 style:
        // Disposal 10 ton @ $1,200  → contract $1,100/ton  → expected $11,000
        // Transport 1 trip @ $1,000 → contract $1,000/trip → expected $1,000
        // Handling 1 lot @ $350     → contract $350/lot    → expected $350
        // Invoice total $13,350 ; expected total $12,350 ; discrepancy $1,000
        let vendor = Uuid::new_v4();
        let disposal_rate = ContractRate {
            id: Uuid::new_v4(),
            vendor_id: vendor,
            waste_stream: "F003".into(),
            facility_id: None,
            unit_rate: Money::from_str_exact("1100.00").unwrap(),
            unit: "ton".into(),
            authorized_fees: vec![],
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
        };
        let transport_rate = ContractRate {
            id: Uuid::new_v4(),
            vendor_id: vendor,
            waste_stream: "F003".into(),
            facility_id: None,
            unit_rate: Money::from_str_exact("1000.00").unwrap(),
            unit: "trip".into(),
            authorized_fees: vec![],
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
        };
        let handling_rate = ContractRate {
            id: Uuid::new_v4(),
            vendor_id: vendor,
            waste_stream: "F003".into(),
            facility_id: None,
            unit_rate: Money::from_str_exact("350.00").unwrap(),
            unit: "lot".into(),
            authorized_fees: vec![],
            effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            effective_to: None,
        };

        let disposal_line_id = Uuid::new_v4();
        let transport_line_id = Uuid::new_v4();
        let handling_line_id = Uuid::new_v4();

        let invoice = Invoice {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            vendor_id: vendor,
            invoice_number: "NES-TEST-1048".into(),
            invoice_date: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            service_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            facility_id: None,
            waste_stream: "F003".into(),
            line_items: vec![
                InvoiceLineItem {
                    id: disposal_line_id,
                    description: "Disposal".into(),
                    fee_code: None,
                    quantity: Decimal::from(10),
                    unit: "ton".into(),
                    unit_price: Money::from_str_exact("1200.00").unwrap(),
                    line_total: Money::from_str_exact("12000.00").unwrap(),
                },
                InvoiceLineItem {
                    id: transport_line_id,
                    description: "Transport".into(),
                    fee_code: None,
                    quantity: Decimal::from(1),
                    unit: "trip".into(),
                    unit_price: Money::from_str_exact("1000.00").unwrap(),
                    line_total: Money::from_str_exact("1000.00").unwrap(),
                },
                InvoiceLineItem {
                    id: handling_line_id,
                    description: "Handling".into(),
                    fee_code: None,
                    quantity: Decimal::from(1),
                    unit: "lot".into(),
                    unit_price: Money::from_str_exact("350.00").unwrap(),
                    line_total: Money::from_str_exact("350.00").unwrap(),
                },
            ],
            total: Money::from_str_exact("13350.00").unwrap(),
            source_document_id: None,
        };

        let input = ReconciliationInput {
            invoice,
            related_invoices: vec![],
            applicable_rates: vec![disposal_rate.clone(), transport_rate.clone(), handling_rate.clone()],
            stale_rates: vec![],
            manifest: None,
            weight_ticket: None,
            tolerances: Tolerances::default(),
        };

        let result = run(&input);

        assert_eq!(result.pricing_status, PricingStatus::Complete);
        assert_eq!(
            result.expected_amount,
            Some(Money::from_str_exact("12350.00").unwrap())
        );
        assert_eq!(
            result.invoiced_amount,
            Money::from_str_exact("13350.00").unwrap()
        );
        assert_eq!(
            result.discrepancy_amount,
            Some(Money::from_str_exact("1000.00").unwrap())
        );
        assert_eq!(result.priced_lines.len(), 3);

        let by_line: std::collections::HashMap<_, _> = result
            .priced_lines
            .iter()
            .map(|p| (p.invoice_line_item_id, p))
            .collect();

        assert_eq!(
            by_line[&disposal_line_id].expected_line_amount,
            Money::from_str_exact("11000.00").unwrap()
        );
        assert_eq!(
            by_line[&disposal_line_id].contract_rate_id,
            disposal_rate.id
        );
        assert_eq!(
            by_line[&transport_line_id].expected_line_amount,
            Money::from_str_exact("1000.00").unwrap()
        );
        assert_eq!(
            by_line[&transport_line_id].contract_rate_id,
            transport_rate.id
        );
        assert_eq!(
            by_line[&handling_line_id].expected_line_amount,
            Money::from_str_exact("350.00").unwrap()
        );
        assert_eq!(
            by_line[&handling_line_id].contract_rate_id,
            handling_rate.id
        );

        // Only the disposal overcharge should appear as a rate-mismatch finding.
        let rate_mismatches: Vec<_> = result
            .discrepancies
            .iter()
            .filter(|d| d.rule_id == rules::RULE_RATE_MISMATCH)
            .collect();
        assert_eq!(rate_mismatches.len(), 1);
        assert_eq!(
            rate_mismatches[0].amount,
            Money::from_str_exact("1000.00").unwrap()
        );
    }
    
    #[test]
    fn full_engine_run_matches_spec_worked_example_exactly() {
        let input = build_input();
        let result = run(&input);
        assert_eq!(result.pricing_status, PricingStatus::Complete);
        assert_eq!(
            result.expected_amount,
            Some(Money::from_str_exact("4500.00").unwrap())
        );
        assert_eq!(
            result.invoiced_amount,
            Money::from_str_exact("5200.00").unwrap()
        );
        assert_eq!(
            result.discrepancy_amount,
            Some(Money::from_str_exact("700.00").unwrap())
        );
        assert!(result.has_material_discrepancy());
        assert_eq!(result.review_status, ReviewStatus::New);
        assert!(result
            .discrepancies
            .iter()
            .any(|d| d.amount == Money::from_str_exact("500.00").unwrap()));
        assert!(result
            .discrepancies
            .iter()
            .any(|d| d.amount == Money::from_str_exact("200.00").unwrap()));
    }

    #[test]
    fn perfectly_matching_invoice_resolves_with_zero_discrepancy() {
        let mut input = build_input();
        input.invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        input.invoice.line_items[0].line_total = Money::from_str_exact("4000.00").unwrap();
        input.invoice.line_items.remove(2);
        input.invoice.total = Money::from_str_exact("4500.00").unwrap();
        input.manifest = Some(Manifest {
            id: Uuid::new_v4(),
            organization_id: input.invoice.organization_id,
            manifest_number: "M-1001".into(),
            waste_stream: "F003".into(),
            container_count: Some(10),
            quantity: Decimal::from(10),
            unit: "drum".into(),
            shipment_date: input.invoice.service_date,
            source_document_id: None,
        });
        let result = run(&input);
        assert!(!result.has_material_discrepancy());
        assert_eq!(result.review_status, ReviewStatus::Resolved, "with matching rate/fees AND a supporting manifest, the reconciliation should resolve cleanly");
    }

    #[test]
    fn clean_dollar_match_without_any_supporting_document_still_requires_review() {
        let mut input = build_input();
        input.invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        input.invoice.line_items[0].line_total = Money::from_str_exact("4000.00").unwrap();
        input.invoice.line_items.remove(2);
        input.invoice.total = Money::from_str_exact("4500.00").unwrap();
        let result = run(&input);
        assert_eq!(result.review_status, ReviewStatus::New, "an invoice with no supporting manifest/weight ticket must require review even if the dollar amounts match");
        assert!(result
            .discrepancies
            .iter()
            .any(|d| d.rule_id == rules::RULE_MISSING_SUPPORT));
    }

    #[test]
    fn incompatible_primary_unit_is_not_priced_using_contract_rate() {
        let mut input = build_input();

        // The contract is priced per drum, but this line is entered in kg.
        input.invoice.line_items[0].unit = "kg".into();

        input.weight_ticket = Some(WeightTicket {
            id: Uuid::new_v4(),
            organization_id: input.invoice.organization_id,
            manifest_id: None,
            ticket_number: "WT-UNIT-1".into(),
            net_weight: Decimal::from(1000),
            unit: "lb".into(),
            container_count: None,
            ticket_date: NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
            source_document_id: None,
        });

        let result = run(&input);

        // The fee-only subtotal is diagnostic; it must not be exposed as the full expected amount.
        assert_eq!(result.pricing_status, PricingStatus::Partial);
        assert_eq!(result.expected_amount, None);
        assert_eq!(
            result.partial_expected_amount,
            Some(Money::from_str_exact("500.00").unwrap())
        );
        assert_eq!(result.discrepancy_amount, None);
        let json = serde_json::to_value(&result).unwrap();
        assert!(json["expected_amount"].is_null());
        assert!(json["discrepancy_amount"].is_null());
        assert_eq!(json["pricing_status"], "PARTIAL");

        assert!(
            result
                .discrepancies
                .iter()
                .any(|d| d.rule_id == rules::RULE_UNIT_MISMATCH),
            "incompatible units must create a review finding"
        );

        assert!(
            !result
                .discrepancies
                .iter()
                .any(|d| d.rule_id == rules::RULE_RATE_MISMATCH),
            "the incompatible quantity must not be compared with the drum rate"
        );

        assert!(
            result
                .calculation_trace
                .iter()
                .any(|line| { line.contains("no unit conversion is configured") }),
            "the calculation trace must explain the unit limitation"
        );
    }

    #[test]
    fn missing_rate_does_not_panic_and_flags_for_review() {
        let mut input = build_input();
        input.applicable_rates.clear();
        let result = run(&input);
        assert_eq!(result.review_status, ReviewStatus::New);
        assert_eq!(result.pricing_status, PricingStatus::Uncomputable);
        assert_eq!(result.expected_amount, None);
        assert_eq!(result.discrepancy_amount, None);
        let json = serde_json::to_value(&result).unwrap();
        assert!(json["expected_amount"].is_null());
        assert!(json["discrepancy_amount"].is_null());
        assert!(result
            .discrepancies
            .iter()
            .any(|d| d.rule_id == "RULE_00_NO_RATE"));
    }

    #[test]
    fn result_is_serializable_for_api_and_evidence_export() {
        let input = build_input();
        let result = run(&input);
        let json = serde_json::to_string(&result).expect("ReconciliationResult must serialize");
        assert!(json.contains("discrepancy_amount"));
        let round_trip: ReconciliationResult =
            serde_json::from_str(&json).expect("ReconciliationResult must deserialize");
        assert_eq!(round_trip.discrepancy_amount, result.discrepancy_amount);
    }

    #[test]
    fn serialized_result_uses_one_consistent_money_representation() {
        let result = run(&build_input());
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["expected_amount"], "4500.0000");
        assert_eq!(json["invoiced_amount"], "5200.0000");
        assert_eq!(json["discrepancy_amount"], "700.0000");
        assert_eq!(json["tolerances_applied"]["rounding_tolerance"], "0.0100");
        for d in json["discrepancies"].as_array().unwrap() {
            let amount = d["amount"].as_str().unwrap();
            let decimals = amount.split('.').nth(1).unwrap().len();
            assert_eq!(
                decimals, 4,
                "discrepancy amount {amount} must have exactly 4 decimals"
            );
        }
    }

    #[test]
    fn duplicate_primary_rate_mismatch_reconciles_to_bill_level_discrepancy() {
        let mut input = build_input();

        // Duplicate the $4,500 disposal line billed above the $400 contract rate.
        let primary = input.invoice.line_items[0].clone();
        input.invoice.line_items.push(InvoiceLineItem {
            id: Uuid::new_v4(),
            ..primary
        });
        input.invoice.total =
            input.invoice.total + Money::from_str_exact("4500.00").unwrap();

        let result = run(&input);

        assert_eq!(result.pricing_status, PricingStatus::Complete);
        assert_eq!(
            result.expected_amount,
            Some(Money::from_str_exact("4500.00").unwrap())
        );
        assert_eq!(
            result.invoiced_amount,
            Money::from_str_exact("9700.00").unwrap()
        );
        assert_eq!(
            result.discrepancy_amount,
            Some(Money::from_str_exact("5200.00").unwrap())
        );
        assert_eq!(
            result.invoiced_amount - result.expected_amount.unwrap(),
            result.discrepancy_amount.unwrap(),
            "complete pricing must reconcile invoiced minus expected"
        );
    }

    #[test]
    fn duplicate_and_stream_rules_run_in_the_engine_without_changing_the_worked_example() {
        // The spec worked example is unchanged: no duplicates, no manifest => still exactly $700.
        let base = run(&build_input());
        assert_eq!(
            base.discrepancy_amount,
            Some(Money::from_decimal(Decimal::from(700)))
        );
        assert!(base
            .discrepancies
            .iter()
            .all(|d| d.rule_id != "RULE_05_DUPLICATE_CHARGE"
                && d.rule_id != "RULE_08_WASTE_STREAM_MISMATCH"));

        // Bill the transportation fee twice: the extra $500 is a RULE_05 finding and the total becomes $1,200.
        let mut dup = build_input();
        let fee = dup
            .invoice
            .line_items
            .iter()
            .find(|l| l.fee_code.as_deref() == Some("TRANSPORT"))
            .unwrap()
            .clone();
        dup.invoice.line_items.push(InvoiceLineItem {
            id: Uuid::new_v4(),
            ..fee
        });
        dup.invoice.total = dup.invoice.total + Money::from_decimal(Decimal::from(500));
        let r = run(&dup);
        let d5: Vec<_> = r
            .discrepancies
            .iter()
            .filter(|d| d.rule_id == "RULE_05_DUPLICATE_CHARGE")
            .collect();
        assert_eq!(d5.len(), 1);
        assert_eq!(
            r.discrepancy_amount,
            Some(Money::from_decimal(Decimal::from(1200)))
        );
        assert_eq!(
            r.invoiced_amount - r.expected_amount.unwrap(),
            r.discrepancy_amount.unwrap(),
            "complete-priced findings must reconcile exactly to invoiced - expected"
        );

        // Manifest with a different stream => RULE_08.
        let mut mism = build_input();
        mism.manifest = Some(Manifest {
            id: Uuid::new_v4(),
            organization_id: mism.invoice.organization_id,
            manifest_number: "M-9".into(),
            waste_stream: "D001".into(),
            container_count: None,
            quantity: Decimal::from(10),
            unit: "drum".into(),
            shipment_date: NaiveDate::from_ymd_opt(2026, 3, 9).unwrap(),
            source_document_id: None,
        });
        assert!(run(&mism)
            .discrepancies
            .iter()
            .any(|d| d.rule_id == "RULE_08_WASTE_STREAM_MISMATCH"));
    }

    // ---- Level 1: missing / expired rate handling ----
    #[test]
    fn expired_rate_is_reported_but_never_prices_the_invoice() {
        let mut input = build_input();
        let mut expired = input.applicable_rates.remove(0);
        expired.effective_to = Some(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        input.stale_rates = vec![expired];
        let r = run(&input);
        assert!(r
            .discrepancies
            .iter()
            .any(|d| d.rule_id == rules::RULE_STALE_RATE));
        assert!(!r
            .discrepancies
            .iter()
            .any(|d| d.rule_id == "RULE_00_NO_RATE"));
        assert!(
            !r.discrepancies
                .iter()
                .any(|d| d.rule_id == rules::RULE_RATE_MISMATCH
                    || d.rule_id == rules::RULE_UNAUTHORIZED_FEE),
            "expired rate must not generate priced findings"
        );
        assert_eq!(r.expected_amount, None);
        assert_eq!(r.discrepancy_amount, None);
        assert_eq!(r.pricing_status, PricingStatus::Uncomputable);
        assert_eq!(
            r.review_status,
            ReviewStatus::New,
            "must require human review"
        );
        assert!(r
            .calculation_trace
            .iter()
            .any(|l| l.contains("expired") && l.contains("not used")));
    }

    #[test]
    fn expired_rate_passed_as_applicable_is_still_not_used_for_pricing() {
        let mut input = build_input();
        input.applicable_rates[0].effective_to =
            Some(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        let r = run(&input);
        assert!(r
            .discrepancies
            .iter()
            .any(|d| d.rule_id == rules::RULE_STALE_RATE));
        assert!(!r
            .discrepancies
            .iter()
            .any(|d| d.rule_id == rules::RULE_RATE_MISMATCH));
        assert_eq!(r.discrepancy_amount, None);
    }

    #[test]
    fn missing_rate_states_that_the_expected_amount_is_not_computable() {
        let mut input = build_input();
        input.applicable_rates.clear();
        let r = run(&input);
        let d = r
            .discrepancies
            .iter()
            .find(|d| d.rule_id == "RULE_00_NO_RATE")
            .unwrap();
        // Exact baseline description (review carry-forward matches on it); detail is in the trace.
        assert_eq!(
            d.description,
            "No contracted rate found covering this invoice's service date"
        );
        assert!(r
            .calculation_trace
            .iter()
            .any(|l| l.contains("cannot be computed") && l.contains("manual review")));
        assert_eq!(r.review_status, ReviewStatus::New);
    }

    // ---- authorized-fee semantics through the production engine ----------------------------

    fn usd(n: i64) -> Money {
        Money::from_decimal(Decimal::from(n))
    }

    /// Disposal priced exactly at the contract rate ($4,000) plus one TRANSPORT fee line billed at
    /// `billed`, against a single TRANSPORT authorization of $500 with the given charge type.
    fn fee_case_input(charge_type: Option<FeeChargeType>, billed: i64) -> ReconciliationInput {
        let mut input = build_input();
        input.applicable_rates[0].authorized_fees[0].charge_type = charge_type;
        input.invoice.line_items.truncate(2);
        input.invoice.line_items[0].unit_price = Money::from_str_exact("400.00").unwrap();
        input.invoice.line_items[0].line_total = Money::from_str_exact("4000.00").unwrap();
        input.invoice.line_items[1].unit_price = usd(billed);
        input.invoice.line_items[1].line_total = usd(billed);
        input.invoice.total = usd(4000) + usd(billed);
        input
    }

    fn fee_case(charge_type: Option<FeeChargeType>, billed: i64) -> ReconciliationResult {
        run(&fee_case_input(charge_type, billed))
    }

    /// For a COMPLETE result: invoiced - expected equals the signed sum of findings, and the
    /// reported discrepancy is that sum.
    fn assert_bill_level_invariant(r: &ReconciliationResult) {
        if r.pricing_status == PricingStatus::Complete {
            let expected = r.expected_amount.expect("COMPLETE has an expected amount");
            assert_eq!(r.invoiced_amount - expected, r.identified_finding_amount);
            assert_eq!(r.discrepancy_amount, Some(r.identified_finding_amount));
        }
    }

    #[test]
    fn exact_fee_below_equal_and_above_authorized_amount() {
        for (billed, finding) in [(400, Some(-100)), (500, None), (600, Some(100))] {
            let r = fee_case(Some(FeeChargeType::Exact), billed);
            assert_eq!(r.pricing_status, PricingStatus::Complete, "billed {billed}");
            assert_eq!(r.expected_amount, Some(usd(4500)), "billed {billed}");
            let fee_findings: Vec<_> = r
                .discrepancies
                .iter()
                .filter(|d| d.rule_id == rules::RULE_UNAUTHORIZED_FEE)
                .collect();
            match finding {
                None => assert!(fee_findings.is_empty(), "billed {billed}"),
                Some(amount) => {
                    assert_eq!(fee_findings.len(), 1, "billed {billed}");
                    assert_eq!(fee_findings[0].amount, usd(amount));
                }
            }
            assert_bill_level_invariant(&r);
        }
    }

    #[test]
    fn exact_fee_finding_text_is_unchanged_so_review_decisions_carry_forward() {
        let r = fee_case(Some(FeeChargeType::Exact), 600);
        let d = r
            .discrepancies
            .iter()
            .find(|d| d.rule_id == rules::RULE_UNAUTHORIZED_FEE)
            .unwrap();
        assert_eq!(
            d.description,
            "Fee 'TRANSPORT' billed at $600.00 but authorized amount is $500.00"
        );
        assert_eq!(d.amount, usd(100));
        assert!(r
            .calculation_trace
            .iter()
            .any(|l| l == "Expected authorized fee 'TRANSPORT': $500.00"));
    }

    #[test]
    fn cap_fee_below_equal_and_above_the_cap() {
        // (billed, expected total, finding)
        for (billed, expected, finding) in [
            (400, 4400, None),
            (500, 4500, None),
            (600, 4500, Some(100)),
        ] {
            let r = fee_case(Some(FeeChargeType::Cap), billed);
            assert_eq!(r.pricing_status, PricingStatus::Complete, "billed {billed}");
            assert_eq!(r.expected_amount, Some(usd(expected)), "billed {billed}");
            let fee_findings: Vec<_> = r
                .discrepancies
                .iter()
                .filter(|d| d.rule_id == rules::RULE_UNAUTHORIZED_FEE)
                .collect();
            match finding {
                None => assert!(fee_findings.is_empty(), "billed {billed}"),
                Some(amount) => {
                    assert_eq!(fee_findings.len(), 1, "billed {billed}");
                    assert_eq!(fee_findings[0].amount, usd(amount));
                    assert_eq!(
                        fee_findings[0].description,
                        "Fee 'TRANSPORT' billed at $600.00 exceeds the authorized cap of $500.00"
                    );
                }
            }
            assert_bill_level_invariant(&r);
        }
    }

    #[test]
    fn cap_fee_below_cap_records_the_billed_amount_as_the_priced_line() {
        let input = fee_case_input(Some(FeeChargeType::Cap), 400);
        let fee_line_id = input.invoice.line_items[1].id;
        let r = run(&input);
        let priced = r
            .priced_lines
            .iter()
            .find(|p| p.invoice_line_item_id == fee_line_id)
            .expect("the fee line is priced");
        assert_eq!(priced.expected_line_amount, usd(400));
        assert_eq!(r.priced_lines.len(), 2);
        assert!(r
            .calculation_trace
            .iter()
            .any(|l| l == "Expected authorized fee 'TRANSPORT' (cap $500.00): $400.00"));
    }

    #[test]
    fn credits_follow_one_sign_convention_for_exact_and_cap() {
        let exact = fee_case(Some(FeeChargeType::Exact), -500);
        assert_eq!(exact.pricing_status, PricingStatus::Complete);
        assert_eq!(exact.expected_amount, Some(usd(4500)));
        assert_eq!(exact.identified_finding_amount, usd(-1000));
        assert_bill_level_invariant(&exact);

        let cap = fee_case(Some(FeeChargeType::Cap), -50);
        assert_eq!(cap.pricing_status, PricingStatus::Complete);
        assert_eq!(cap.expected_amount, Some(usd(3950)));
        assert_eq!(cap.identified_finding_amount, Money::zero());
        assert_bill_level_invariant(&cap);
    }

    #[test]
    fn unclassified_fee_is_never_priced_and_makes_pricing_partial() {
        for billed in [400, 500, 600] {
            let r = fee_case(None, billed);
            assert_eq!(r.pricing_status, PricingStatus::Partial, "billed {billed}");
            assert_eq!(r.expected_amount, None);
            assert_eq!(r.discrepancy_amount, None);
            // Only the disposal line is priced; the unclassified fee contributes nothing.
            assert_eq!(r.partial_expected_amount, Some(usd(4000)));
            assert_eq!(r.priced_lines.len(), 1);
            let flagged: Vec<_> = r
                .discrepancies
                .iter()
                .filter(|d| d.rule_id == rules::RULE_UNCLASSIFIED_FEE)
                .collect();
            assert_eq!(flagged.len(), 1);
            assert_eq!(flagged[0].amount, Money::zero());
            assert!(r
                .discrepancies
                .iter()
                .all(|d| d.rule_id != rules::RULE_UNAUTHORIZED_FEE));
        }
    }

    #[test]
    fn conflicting_fee_types_across_rates_are_unresolved() {
        let mut input = fee_case_input(Some(FeeChargeType::Exact), 500);
        let mut second = input.applicable_rates[0].clone();
        second.id = Uuid::new_v4();
        second.unit = "ton".into();
        second.authorized_fees[0].charge_type = Some(FeeChargeType::Cap);
        input.applicable_rates.push(second);
        let r = run(&input);
        assert_eq!(r.pricing_status, PricingStatus::Partial);
        assert_eq!(r.expected_amount, None);
        let flagged: Vec<_> = r
            .discrepancies
            .iter()
            .filter(|d| d.rule_id == rules::RULE_AMBIGUOUS_FEE)
            .collect();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].amount, Money::zero());
    }

    #[test]
    fn identical_authorizations_from_several_rates_are_not_a_conflict() {
        let mut input = fee_case_input(Some(FeeChargeType::Cap), 400);
        let mut second = input.applicable_rates[0].clone();
        second.id = Uuid::new_v4();
        second.unit = "ton".into();
        input.applicable_rates.push(second);
        let r = run(&input);
        assert_eq!(r.pricing_status, PricingStatus::Complete);
        assert_eq!(r.expected_amount, Some(usd(4400)));
        assert_bill_level_invariant(&r);
    }

    #[test]
    fn complete_results_allow_only_entry_rounding_between_total_and_lines() {
        // One cent between the invoice total and its lines is within what invoice entry allows.
        let mut input = fee_case_input(Some(FeeChargeType::Exact), 500);
        input.invoice.total = input.invoice.total + Money::from_str_exact("0.01").unwrap();
        let r = run(&input);
        assert_eq!(r.pricing_status, PricingStatus::Complete);
        assert_eq!(r.discrepancy_amount, Some(Money::zero()));
        let gap = r.invoiced_amount - r.expected_amount.unwrap();
        assert_eq!(gap, Money::from_str_exact("0.01").unwrap());

        // A materially inconsistent total is never reported as COMPLETE.
        let mut input = fee_case_input(Some(FeeChargeType::Exact), 500);
        input.invoice.total = input.invoice.total + usd(5);
        let r = run(&input);
        assert_eq!(r.pricing_status, PricingStatus::Partial);
        assert_eq!(r.expected_amount, None);
        assert_eq!(r.discrepancy_amount, None);
        assert_eq!(r.partial_expected_amount, Some(usd(4500)));
        assert!(r
            .discrepancies
            .iter()
            .any(|d| d.rule_id == RULE_UNRECONCILED_TOTAL && d.amount == Money::zero()));
        assert_eq!(r.identified_finding_amount, Money::zero());
    }
}
