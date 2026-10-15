//! The reconciliation engine orchestrator (spec section 17).

use crate::common::money::Money;
use crate::reconciliation::model::*;
use crate::reconciliation::rules;

pub fn run(input: &ReconciliationInput) -> ReconciliationResult {
    let mut discrepancies: Vec<DiscrepancyLine> = Vec::new();
    let mut trace: Vec<String> = Vec::new();

    trace.push(format!(
        "Reconciling invoice {} (service date {})",
        input.invoice.invoice_number, input.invoice.service_date
    ));

    let expected_amount = match &input.applicable_rate {
        Some(rate) => {
            trace.push(format!(
                "Matched contract rate {}/{} effective {} - {}",
                rate.unit_rate,
                rate.unit,
                rate.effective_from,
                rate.effective_to
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "open".into())
            ));

            if let Some(stale) = rules::check_stale_rate(&input.invoice, rate) {
                trace.push(format!("RULE {}: {}", stale.rule_id, stale.description));
                discrepancies.push(stale);
            }

            let fee_and_rate_discrepancies = rules::check_rate_and_fees(&input.invoice, rate);
            for d in &fee_and_rate_discrepancies {
                trace.push(format!(
                    "RULE {}: {} (amount: {})",
                    d.rule_id, d.description, d.amount
                ));
            }
            discrepancies.extend(fee_and_rate_discrepancies);

            // Duplicate lines (RULE_05) are not "expected": they are reported in full as findings.
            let dups = rules::duplicate_line_indices(&input.invoice, &input.related_invoices);
            let counted = |i: usize| !dups.contains(&i);
            let primary_qty: rust_decimal::Decimal = input
                .invoice
                .line_items
                .iter()
                .enumerate()
                .filter(|(i, line)| {
                    counted(*i)
                        && line.fee_code.is_none()
                        && line.unit.trim().eq_ignore_ascii_case(rate.unit.trim())
                })
                .map(|(_, line)| line.quantity)
                .sum();

            let incompatible_primary_count = input
                .invoice
                .line_items
                .iter()
                .enumerate()
                .filter(|(i, line)| {
                    counted(*i)
                        && line.fee_code.is_none()
                        && !line.unit.trim().eq_ignore_ascii_case(rate.unit.trim())
                })
                .count();

            if incompatible_primary_count > 0 {
                trace.push(format!(
                    "Excluded {} primary invoice line(s) from rate-based pricing because their units do not match the contracted unit '{}'; no unit conversion is configured",
                    incompatible_primary_count, rate.unit
                ));
            }

            let mut expected = rate.unit_rate * primary_qty;
            trace.push(format!(
                "Expected primary line: {} {} x {} / {} = {}",
                primary_qty, rate.unit, rate.unit_rate, rate.unit, expected
            ));

            for line in input
                .invoice
                .line_items
                .iter()
                .enumerate()
                .filter(|(i, l)| counted(*i) && l.fee_code.is_some())
                .map(|(_, l)| l)
            {
                if let Some(fee_code) = &line.fee_code {
                    if let Some(auth) = rate
                        .authorized_fees
                        .iter()
                        .find(|f| &f.fee_code == fee_code)
                    {
                        expected = expected + auth.amount;
                        trace.push(format!(
                            "Expected authorized fee '{}': {}",
                            fee_code, auth.amount
                        ));
                    }
                }
            }
            expected
        }
        None => {
            trace.push("No applicable contract rate found for this service date - expected amount cannot be computed.".into());
            discrepancies.push(DiscrepancyLine {
                discrepancy_type: DiscrepancyType::MissingSupport,
                description: "No contracted rate found covering this invoice's service date".into(),
                amount: Money::zero(),
                rule_id: "RULE_00_NO_RATE".to_string(),
                confidence: Confidence::High,
            });
            input.invoice.total
        }
    };

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

    for d in rules::check_duplicate_charges(
        &input.invoice,
        input.applicable_rate.as_ref(),
        &input.related_invoices,
    ) {
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

    let dollar_discrepancy: Money = discrepancies
        .iter()
        .fold(Money::zero(), |acc, d| acc + d.amount);
    let invoiced_amount = input.invoice.total;
    trace.push(format!(
        "Expected: {} | Invoiced: {} | Sum of dollar-quantified discrepancy lines: {}",
        expected_amount, invoiced_amount, dollar_discrepancy
    ));

    let review_status = if discrepancies.is_empty() {
        ReviewStatus::Resolved
    } else {
        ReviewStatus::New
    };

    ReconciliationResult {
        invoice_id: input.invoice.id,
        expected_amount,
        invoiced_amount,
        discrepancy_amount: dollar_discrepancy,
        discrepancies,
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
            applicable_rate: Some(rate),
            manifest: None,
            weight_ticket: None,
            tolerances: Tolerances::default(),
        }
    }

    #[test]
    fn full_engine_run_matches_spec_worked_example_exactly() {
        let input = build_input();
        let result = run(&input);
        assert_eq!(
            result.expected_amount,
            Money::from_str_exact("4500.00").unwrap()
        );
        assert_eq!(
            result.invoiced_amount,
            Money::from_str_exact("5200.00").unwrap()
        );
        assert_eq!(
            result.discrepancy_amount,
            Money::from_str_exact("700.00").unwrap()
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

        // Only the authorized $500 transport fee remains in this fixture's
        // calculable expected amount; the kg quantity is not priced per drum.
        assert_eq!(
            result.expected_amount,
            Money::from_str_exact("500.00").unwrap()
        );

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
        input.applicable_rate = None;
        let result = run(&input);
        assert_eq!(result.review_status, ReviewStatus::New);
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
    fn duplicate_and_stream_rules_run_in_the_engine_without_changing_the_worked_example() {
        // The spec worked example is unchanged: no duplicates, no manifest => still exactly $700.
        let base = run(&build_input());
        assert_eq!(
            base.discrepancy_amount,
            Money::from_decimal(Decimal::from(700))
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
            Money::from_decimal(Decimal::from(1200))
        );
        assert_eq!(
            r.invoiced_amount - r.expected_amount,
            r.discrepancy_amount,
            "findings must reconcile exactly to invoiced - expected"
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
}
