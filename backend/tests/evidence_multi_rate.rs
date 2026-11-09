//! Evidence JSON and PDF for invoices priced by SEVERAL contract rates.
//!
//! Before this work such an invoice had `contract_rate: null` and the PDF printed no rates and no
//! authorized fees. Evidence now lists every rate the engine considered applicable (one per unit,
//! including a rate that priced no line, because fees are evaluated against all of them), the lines
//! each priced, and per fee line what authorizes it. These tests drive the real API as `xflow_app`.
//!
//! Rules under test: no single parent rate is ever invented; EXACT / CAP / unclassified (`null`) are
//! shown exactly as stored and never inferred; the evidence agrees with the persisted line-rate records
//! and with the reconciliation's own result; stored snapshots and past reconciliations never change;
//! and the PDF says what the JSON says.

mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use common::{test_app, TestApp, TestOrg};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

async fn ok(app: &TestApp, org: &TestOrg, path: &str, body: Value) -> Value {
    let (s, v) = app.post(path, &org.token, body).await;
    assert!(s.is_success(), "POST {path} -> {s}: {v}");
    v
}

/// An authorized fee on a rate. `ctype = None` is a legacy fee with no charge type (SQL only).
#[derive(Clone, Copy)]
struct Fee {
    code: &'static str,
    amount: &'static str,
    ctype: Option<&'static str>,
}

struct Multi {
    contract_id: String,
    drum_rate: String,
    ton_rate: String,
    invoice_id: String,
}

async fn make_rate(
    app: &TestApp,
    org: &TestOrg,
    contract_id: &str,
    unit: &str,
    unit_rate: &str,
    fees: &[Fee],
) -> String {
    let api_fees: Vec<Value> = fees
        .iter()
        .filter_map(|f| {
            f.ctype
                .map(|t| json!({ "fee_code": f.code, "amount": f.amount, "charge_type": t }))
        })
        .collect();
    let v = ok(
        app,
        org,
        &format!("/api/v1/contracts/{contract_id}/rates"),
        json!({ "waste_stream": "F003", "unit": unit, "unit_rate": unit_rate, "effective_from": "2026-01-01", "authorized_fees": api_fees }),
    )
    .await;
    let rate_id = v["rates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["unit"] == unit)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    for f in fees.iter().filter(|f| f.ctype.is_none()) {
        let rate: Uuid = rate_id.parse().unwrap();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, amount) VALUES ($1, $2, $3::numeric)")
            .bind(rate).bind(f.code).bind(f.amount).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    rate_id
}

/// A drum rate ($400) and a ton rate ($1000) on one contract, with the given authorized fees, and an
/// invoice billing 10 drums, optionally 1 ton, and the given fee lines `(code, amount)`.
async fn multi(
    app: &TestApp,
    org: &TestOrg,
    tag: &str,
    drum: &[Fee],
    ton: &[Fee],
    bill_ton: bool,
    fee_lines: &[(&str, &str)],
) -> Multi {
    let vendor = ok(
        app,
        org,
        "/api/v1/vendors",
        json!({ "name": format!("Vendor {tag}") }),
    )
    .await;
    let vendor_id = vendor["id"].as_str().unwrap().to_string();
    let contract = ok(
        app,
        org,
        "/api/v1/contracts",
        json!({ "vendor_id": vendor_id, "name": "Master" }),
    )
    .await;
    let contract_id = contract["id"].as_str().unwrap().to_string();
    let drum_rate = make_rate(app, org, &contract_id, "drum", "400.00", drum).await;
    let ton_rate = make_rate(app, org, &contract_id, "ton", "1000.00", ton).await;

    let mut total = Decimal::from(4000);
    let mut lines = vec![
        json!({ "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" }),
    ];
    if bill_ton {
        total += Decimal::from(1000);
        lines.push(json!({ "description": "Ton disposal", "quantity": "1", "unit": "ton", "unit_price": "1000.00", "line_total": "1000.00" }));
    }
    for (code, amount) in fee_lines {
        total += amount.parse::<Decimal>().unwrap();
        lines.push(json!({ "description": format!("Fee {code}"), "fee_code": code, "quantity": "1", "unit": "flat", "unit_price": amount, "line_total": amount }));
    }
    let invoice = ok(app, org, "/api/v1/invoices", json!({
        "vendor_id": vendor_id, "invoice_number": format!("INV-{tag}"), "invoice_date": "2026-03-15", "service_date": "2026-03-10",
        "waste_stream": "F003", "total": format!("{total:.2}"), "line_items": lines
    })).await;
    Multi {
        contract_id,
        drum_rate,
        ton_rate,
        invoice_id: invoice["id"].as_str().unwrap().to_string(),
    }
}

async fn run(app: &TestApp, org: &TestOrg, invoice_id: &str) -> Value {
    ok(
        app,
        org,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": invoice_id }),
    )
    .await
}

fn discrepancy_id(run: &Value, rule: &str) -> String {
    let i = run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .position(|d| d["rule_id"] == rule)
        .unwrap_or_else(|| panic!("no {rule} finding in {run}"));
    run["discrepancy_ids"][i].as_str().unwrap().to_string()
}

async fn evidence(app: &TestApp, org: &TestOrg, discrepancy_id: &str) -> Value {
    ok(
        app,
        org,
        &format!("/api/v1/discrepancies/{discrepancy_id}/evidence"),
        json!({}),
    )
    .await
}

async fn pdf_text(app: &TestApp, org: &TestOrg, discrepancy_id: &str) -> String {
    let (st, _h, bytes) = app
        .call_raw(
            Method::GET,
            &format!("/api/v1/discrepancies/{discrepancy_id}/evidence/pdf"),
            Some(&org.token),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(bytes.starts_with(b"%PDF"));
    String::from_utf8_lossy(&bytes).to_string()
}

async fn scalar_i64(app: &TestApp, org: &TestOrg, sql: &str, id: &str) -> i64 {
    let id: Uuid = id.parse().unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

fn money2(v: &str) -> String {
    let d: Decimal = v.parse().unwrap();
    let s = format!("{:.2}", d.abs());
    let (int, frac) = s.split_once('.').unwrap();
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!(
        "{}${out}.{frac}",
        if d.is_sign_negative() { "-" } else { "" }
    )
}

fn label(charge_type: &Value) -> &'static str {
    match charge_type {
        Value::Null => "unclassified: type not set",
        Value::String(t) if t == "EXACT" => "exact charge",
        Value::String(t) if t == "CAP" => "cap: maximum allowed",
        other => panic!("unexpected charge type {other}"),
    }
}

fn rate_by_unit<'a>(content: &'a Value, unit: &str) -> &'a Value {
    content["applicable_rates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["unit"] == unit)
        .unwrap_or_else(|| panic!("no {unit} rate in {}", content["applicable_rates"]))
}

/// The evidence must agree with the persisted line-rate records: for every applicable rate, the lines it
/// lists as priced are exactly the `priced_lines` rows that carry that rate, and every priced line
/// belongs to a listed rate.
fn assert_matches_line_rate_records(content: &Value) {
    let priced = content["priced_lines"].as_array().unwrap();
    for rate in content["applicable_rates"].as_array().unwrap() {
        let listed: BTreeSet<&str> = rate["priced_line_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let persisted: BTreeSet<&str> = priced
            .iter()
            .filter(|p| p["contract_rate_id"] == rate["contract_rate_id"])
            .map(|p| p["invoice_line_item_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            listed, persisted,
            "rate {} priced-line list must equal the persisted records",
            rate["unit"]
        );
    }
    let known: BTreeSet<&str> = content["applicable_rates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["contract_rate_id"].as_str().unwrap())
        .collect();
    for p in priced {
        assert!(
            known.contains(p["contract_rate_id"].as_str().unwrap()),
            "priced line references a rate missing from applicable_rates"
        );
    }
}

/// Everything the JSON says about rates, fees and fee lines must appear in the PDF.
fn assert_pdf_says_what_json_says(pdf: &str, content: &Value) {
    let rates = content["applicable_rates"].as_array().unwrap();
    for (i, rate) in rates.iter().enumerate() {
        let header = format!(
            "Rate {}: Master / F003 - {} per {}",
            i + 1,
            money2(rate["unit_rate"].as_str().unwrap()),
            rate["unit"].as_str().unwrap()
        );
        assert!(pdf.contains(&header), "PDF is missing {header:?}");
        for f in rate["authorized_fees"].as_array().unwrap() {
            let line = format!(
                "Authorized fee: {} - {} [{}]",
                f["fee_code"].as_str().unwrap(),
                money2(f["amount"].as_str().unwrap()),
                label(&f["charge_type"])
            );
            assert!(pdf.contains(&line), "PDF is missing {line:?}");
        }
        if rate["priced_line_ids"].as_array().unwrap().is_empty() {
            assert!(pdf.contains("none, this rate applied but priced no invoice line"));
        }
    }
    let label_of = |id: &str| {
        let i = rates
            .iter()
            .position(|r| r["contract_rate_id"] == id)
            .unwrap();
        format!("Rate {}, {}", i + 1, rates[i]["unit"].as_str().unwrap())
    };
    for fl in content["fee_lines"].as_array().unwrap() {
        let head = format!(
            "Fee line: {} billed {}",
            fl["fee_code"].as_str().unwrap(),
            money2(fl["billed_amount"].as_str().unwrap())
        );
        assert!(pdf.contains(&head), "PDF is missing {head:?}");
        for a in fl["authorizations"].as_array().unwrap() {
            let line = format!(
                "Authorized by: {}: {} [{}]",
                label_of(a["contract_rate_id"].as_str().unwrap()),
                money2(a["amount"].as_str().unwrap()),
                label(&a["charge_type"])
            );
            assert!(pdf.contains(&line), "PDF is missing {line:?}");
        }
        match fl["priced_by_contract_rate_id"].as_str() {
            Some(id) => {
                let line = format!(
                    "Priced against: {}, expected {}",
                    label_of(id),
                    money2(fl["expected_line_amount"].as_str().unwrap())
                );
                assert!(pdf.contains(&line), "PDF is missing {line:?}");
            }
            None => {
                assert!(pdf.contains("Priced against: no rate, no expected amount was recorded"))
            }
        }
    }
    assert!(
        !pdf.contains("No contract rate covered"),
        "a multi-rate invoice must not claim that no rate applied"
    );
}

// ---------------------------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn multi_rate_evidence_lists_each_rate_with_its_exact_and_cap_fees_and_agrees_with_the_pdf() {
    let app = test_app().await;
    let org = app.signup_org("mr-complete").await;
    // drum rate authorizes TRANSPORT $500 EXACT; ton rate authorizes HANDLING $150 CAP.
    let m = multi(
        &app,
        &org,
        "complete",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "HANDLING",
            amount: "150.00",
            ctype: Some("CAP"),
        }],
        true,
        &[("TRANSPORT", "600.00"), ("HANDLING", "100.00")], // EXACT overbilled by $100; CAP billed below its cap
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    assert_eq!(r["pricing_status"], "COMPLETE", "{r}");
    assert_eq!(
        r["expected_amount"]
            .as_str()
            .map(|s| s.parse::<Decimal>().unwrap()),
        Some(Decimal::from(5600)),
        "{r}"
    );
    let did = discrepancy_id(&r, "RULE_04_UNAUTHORIZED_FEE");

    let ev = evidence(&app, &org, &did).await;
    let c = &ev["content"];

    // No parent rate is invented: the reconciliation stored none, and neither does the evidence.
    assert!(c["reconciliation"]["contract_rate_id"].is_null());
    assert!(c["contract_rate"].is_null());
    assert_eq!(c["applicable_rates_basis"], "recorded");
    let rates = c["applicable_rates"].as_array().unwrap();
    assert_eq!(rates.len(), 2);
    assert_eq!(rates[0]["unit"], "drum");
    assert_eq!(rates[1]["unit"], "ton");

    // Each rate carries ITS OWN fees with their stored types.
    let drum = rate_by_unit(c, "drum");
    assert_eq!(drum["contract_rate_id"], m.drum_rate);
    assert_eq!(
        drum["authorized_fees"],
        json!([{ "fee_code": "TRANSPORT", "description": null, "amount": "500.0000", "charge_type": "EXACT" }])
    );
    let ton = rate_by_unit(c, "ton");
    assert_eq!(ton["contract_rate_id"], m.ton_rate);
    assert_eq!(
        ton["authorized_fees"],
        json!([{ "fee_code": "HANDLING", "description": null, "amount": "150.0000", "charge_type": "CAP" }])
    );

    // Per-line attribution: each fee line points at the rate that authorized and priced it.
    let fl = |code: &str| {
        c["fee_lines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["fee_code"] == code)
            .unwrap()
            .clone()
    };
    let transport = fl("TRANSPORT");
    assert_eq!(transport["priced_by_contract_rate_id"], m.drum_rate);
    assert_eq!(transport["expected_line_amount"], "500.0000"); // EXACT: the authorized amount
    assert_eq!(transport["billed_amount"], "600.0000");
    assert_eq!(transport["authorizations"].as_array().unwrap().len(), 1);
    assert_eq!(transport["authorizations"][0]["charge_type"], "EXACT");
    let handling = fl("HANDLING");
    assert_eq!(handling["priced_by_contract_rate_id"], m.ton_rate);
    assert_eq!(handling["expected_line_amount"], "100.0000"); // CAP: min(billed, cap) = billed
    assert_eq!(handling["authorizations"][0]["charge_type"], "CAP");

    assert_matches_line_rate_records(c);
    assert_eq!(
        drum["priced_line_ids"].as_array().unwrap().len(),
        2,
        "the drum line and the TRANSPORT fee line"
    );
    assert_eq!(
        ton["priced_line_ids"].as_array().unwrap().len(),
        2,
        "the ton line and the HANDLING fee line"
    );

    let pdf = pdf_text(&app, &org, &did).await;
    assert_pdf_says_what_json_says(&pdf, c);
    assert!(pdf.contains("Authorized fee: TRANSPORT - $500.00 [exact charge]"));
    assert!(pdf.contains("Authorized fee: HANDLING - $150.00 [cap: maximum allowed]"));

    // The stored snapshot is what later reads return.
    let (st, latest) = app
        .get(&format!("/api/v1/discrepancies/{did}/evidence"), &org.token)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(latest["content"], ev["content"]);
}

#[tokio::test]
async fn conflicting_authorizations_are_shown_side_by_side_and_pricing_stays_partial() {
    let app = test_app().await;
    let org = app.signup_org("mr-conflict").await;
    let m = multi(
        &app,
        &org,
        "conflict",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "TRANSPORT",
            amount: "600.00",
            ctype: Some("EXACT"),
        }], // same code, different amount
        true,
        &[("TRANSPORT", "500.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    assert_eq!(r["pricing_status"], "PARTIAL", "{r}");
    assert!(r["expected_amount"].is_null());
    let did = discrepancy_id(&r, "RULE_11_AMBIGUOUS_FEE");
    let c = evidence(&app, &org, &did).await["content"].clone();

    let fl = &c["fee_lines"][0];
    assert_eq!(fl["fee_code"], "TRANSPORT");
    assert!(
        fl["priced_by_contract_rate_id"].is_null(),
        "a conflicting fee is not priced against any rate"
    );
    assert!(fl["expected_line_amount"].is_null());
    let auths = fl["authorizations"].as_array().unwrap();
    assert_eq!(auths.len(), 2, "both conflicting authorizations are listed");
    let by_rate = |id: &str| auths.iter().find(|a| a["contract_rate_id"] == id).unwrap();
    assert_eq!(by_rate(&m.drum_rate)["amount"], "500.0000");
    assert_eq!(by_rate(&m.ton_rate)["amount"], "600.0000");
    assert!(c["contract_rate"].is_null());
    assert!(
        c["reconciliation"]["expected_amount"].is_null(),
        "evidence agrees with the reconciliation: no complete expected amount"
    );
    assert_matches_line_rate_records(&c);

    let pdf = pdf_text(&app, &org, &did).await;
    assert_pdf_says_what_json_says(&pdf, &c);
    assert!(pdf.contains("Authorized by: Rate 1, drum: $500.00 [exact charge]"));
    assert!(pdf.contains("Authorized by: Rate 2, ton: $600.00 [exact charge]"));
}

#[tokio::test]
async fn conflicting_charge_types_and_unclassified_fees_are_never_inferred() {
    let app = test_app().await;
    let org = app.signup_org("mr-types").await;
    // TRANSPORT: EXACT on one rate, CAP on the other (same amount) -> conflicting types.
    // ENV_FEE: a legacy fee with NO charge type on the ton rate -> unclassified.
    let m = multi(
        &app,
        &org,
        "types",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[
            Fee {
                code: "TRANSPORT",
                amount: "500.00",
                ctype: Some("CAP"),
            },
            Fee {
                code: "ENV_FEE",
                amount: "200.00",
                ctype: None,
            },
        ],
        true,
        &[("TRANSPORT", "500.00"), ("ENV_FEE", "200.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    assert_eq!(r["pricing_status"], "PARTIAL", "{r}");
    let d11 = discrepancy_id(&r, "RULE_11_AMBIGUOUS_FEE");
    let d12 = discrepancy_id(&r, "RULE_12_UNCLASSIFIED_FEE_TYPE");

    let c = evidence(&app, &org, &d12).await["content"].clone();
    let env = c["fee_lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["fee_code"] == "ENV_FEE")
        .unwrap();
    assert!(env["priced_by_contract_rate_id"].is_null());
    assert_eq!(env["authorizations"].as_array().unwrap().len(), 1);
    assert!(
        env["authorizations"][0]["charge_type"].is_null(),
        "an unclassified fee stays null, never EXACT or CAP"
    );
    assert!(rate_by_unit(&c, "ton")["authorized_fees"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["fee_code"] == "ENV_FEE" && f["charge_type"].is_null()));
    let transport = c["fee_lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["fee_code"] == "TRANSPORT")
        .unwrap();
    let types: BTreeSet<&str> = transport["authorizations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["charge_type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        BTreeSet::from(["CAP", "EXACT"]),
        "both conflicting types are shown as stored"
    );

    let pdf = pdf_text(&app, &org, &d12).await;
    assert_pdf_says_what_json_says(&pdf, &c);
    assert!(pdf.contains("Authorized fee: ENV_FEE - $200.00 [unclassified: type not set]"));
    assert!(pdf.contains("Authorized by: Rate 2, ton: $200.00 [unclassified: type not set]"));
    // Same package from the other finding.
    let c11 = evidence(&app, &org, &d11).await["content"].clone();
    assert_eq!(c11["applicable_rates"], c["applicable_rates"]);
    assert_eq!(c11["fee_lines"], c["fee_lines"]);
}

#[tokio::test]
async fn a_rate_that_priced_no_line_but_made_the_fee_conflict_is_still_shown() {
    let app = test_app().await;
    let org = app.signup_org("mr-unused").await;
    // The invoice bills only drums, yet the ton rate (also covering) authorizes TRANSPORT differently:
    // the engine evaluates fees against EVERY applicable rate, so the result is "conflicting".
    let m = multi(
        &app,
        &org,
        "unused",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "TRANSPORT",
            amount: "600.00",
            ctype: Some("EXACT"),
        }],
        false,
        &[("TRANSPORT", "500.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    assert_eq!(r["pricing_status"], "PARTIAL", "{r}");
    let did = discrepancy_id(&r, "RULE_11_AMBIGUOUS_FEE");
    let c = evidence(&app, &org, &did).await["content"].clone();

    assert_eq!(c["applicable_rates_basis"], "recorded");
    assert_eq!(
        c["applicable_rates"].as_array().unwrap().len(),
        2,
        "both applicable rates are recorded"
    );
    assert!(
        rate_by_unit(&c, "ton")["priced_line_ids"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the ton rate priced nothing"
    );
    assert_eq!(
        c["fee_lines"][0]["authorizations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(c["contract_rate"].is_null());
    assert_matches_line_rate_records(&c);
    // The persisted line-rate records alone mention only the drum rate: without the recorded set the
    // evidence could not explain the conflict.
    let line_rate_rates: BTreeSet<&str> = c["priced_lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["contract_rate_id"].as_str().unwrap())
        .collect();
    assert_eq!(line_rate_rates, BTreeSet::from([m.drum_rate.as_str()]));

    let pdf = pdf_text(&app, &org, &did).await;
    assert_pdf_says_what_json_says(&pdf, &c);
}

#[tokio::test]
async fn a_reconciliation_made_before_the_rate_set_was_recorded_says_so_and_is_not_rewritten() {
    let app = test_app().await;
    let org = app.signup_org("mr-legacy").await;
    let m = multi(
        &app,
        &org,
        "legacy",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "TRANSPORT",
            amount: "600.00",
            ctype: Some("EXACT"),
        }],
        false,
        &[("TRANSPORT", "500.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    let rec_id = r["reconciliation_id"].as_str().unwrap().to_string();
    let did = discrepancy_id(&r, "RULE_11_AMBIGUOUS_FEE");
    let (_, before) = app
        .get(&format!("/api/v1/reconciliations/{rec_id}"), &org.token)
        .await;

    // Emulate a reconciliation that predates migration 0013: it has no recorded rate set.
    {
        let rid: Uuid = rec_id.parse().unwrap();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        let n =
            sqlx::query("DELETE FROM reconciliation_applicable_rates WHERE reconciliation_id = $1")
                .bind(rid)
                .execute(&mut *tx)
                .await
                .unwrap()
                .rows_affected();
        assert_eq!(n, 2);
        // ... and, as such a row would, says so: the explicit marker is false.
        sqlx::query("UPDATE reconciliations SET applicable_rates_recorded = false WHERE id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert!(!marker(&app, &org, &rec_id).await, "emulated legacy row");
    let ev = evidence(&app, &org, &did).await;
    let c = &ev["content"];
    assert_eq!(
        c["applicable_rates_basis"], "line_rates_only",
        "evidence says what it is built from"
    );
    let rates = c["applicable_rates"].as_array().unwrap();
    assert_eq!(
        rates.len(),
        1,
        "only the rate found in the persisted line-rate records"
    );
    assert_eq!(rates[0]["unit"], "drum");
    assert!(
        c["contract_rate"].is_null(),
        "no parent rate is invented for a reconciliation that stored none"
    );
    assert_matches_line_rate_records(c);
    let pdf = pdf_text(&app, &org, &did).await;
    assert!(pdf.contains("predates recording the full set of applicable rates"));
    assert!(pdf.contains("Rate 1: Master / F003 - $400.00 per drum"));
    assert!(!pdf.contains("Rate 2:"));

    // The reconciliation itself was not touched by any of this.
    let (_, after) = app
        .get(&format!("/api/v1/reconciliations/{rec_id}"), &org.token)
        .await;
    assert_eq!(after, before);
}

#[tokio::test]
async fn single_rate_evidence_and_pdf_are_unchanged() {
    let app = test_app().await;
    let org = app.signup_org("mr-single").await;
    let vendor = ok(
        &app,
        &org,
        "/api/v1/vendors",
        json!({ "name": "Vendor single" }),
    )
    .await;
    let contract = ok(
        &app,
        &org,
        "/api/v1/contracts",
        json!({ "vendor_id": vendor["id"], "name": "Master" }),
    )
    .await;
    let rate_id = make_rate(
        &app,
        &org,
        contract["id"].as_str().unwrap(),
        "drum",
        "400.00",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("CAP"),
        }],
    )
    .await;
    let invoice = ok(&app, &org, "/api/v1/invoices", json!({
        "vendor_id": vendor["id"], "invoice_number": "INV-single", "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003", "total": "5000.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "1000.00", "line_total": "1000.00" }]
    })).await;
    let r = run(&app, &org, invoice["id"].as_str().unwrap()).await;
    let did = discrepancy_id(&r, "RULE_04_UNAUTHORIZED_FEE"); // billed above the CAP
    let c = evidence(&app, &org, &did).await["content"].clone();

    assert_eq!(
        c["contract_rate"]["id"], rate_id,
        "a single applicable rate is still the parent"
    );
    assert_eq!(c["applicable_rates"].as_array().unwrap().len(), 1);
    assert_eq!(c["applicable_rates"][0]["contract_rate_id"], rate_id);
    assert_eq!(
        c["applicable_rates"][0]["authorized_fees"],
        c["contract_rate"]["authorized_fees"]
    );
    assert_matches_line_rate_records(&c);

    let pdf = pdf_text(&app, &org, &did).await;
    assert!(pdf.contains("Authorized fee: TRANSPORT - $500.00 [cap: maximum allowed]"));
    assert!(
        !pdf.contains("More than one contract rate applied"),
        "single-rate layout is unchanged"
    );
}

#[tokio::test]
async fn stored_snapshots_and_past_reconciliations_survive_a_later_classification() {
    let app = test_app().await;
    let org = app.signup_org("mr-history").await;
    let m = multi(
        &app,
        &org,
        "history",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "ENV_FEE",
            amount: "200.00",
            ctype: None,
        }],
        true,
        &[("TRANSPORT", "500.00"), ("ENV_FEE", "200.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    assert_eq!(r["pricing_status"], "PARTIAL");
    let rec1 = r["reconciliation_id"].as_str().unwrap().to_string();
    let did = discrepancy_id(&r, "RULE_12_UNCLASSIFIED_FEE_TYPE");
    let (_, rec1_before) = app
        .get(&format!("/api/v1/reconciliations/{rec1}"), &org.token)
        .await;
    let snapshot = evidence(&app, &org, &did).await;
    assert!(snapshot["content"]["fee_lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["fee_code"] == "ENV_FEE" && l["authorizations"][0]["charge_type"].is_null()));
    let pdf_before = pdf_text(&app, &org, &did).await;

    // The fee is classified and the invoice is reconciled again.
    let (st, _) = app
        .call(
            Method::PUT,
            &format!("/api/v1/rates/{}/fees/ENV_FEE/type", m.ton_rate),
            Some(&org.token),
            Some(json!({ "charge_type": "EXACT" })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let r2 = run(&app, &org, &m.invoice_id).await;
    assert_eq!(r2["pricing_status"], "COMPLETE", "{r2}");
    // The new reconciliation recorded its own rate set; the old one is untouched.
    assert_eq!(
        scalar_i64(
            &app,
            &org,
            "SELECT count(*) FROM reconciliation_applicable_rates WHERE reconciliation_id = $1",
            r2["reconciliation_id"].as_str().unwrap()
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &app,
            &org,
            "SELECT count(*) FROM reconciliation_applicable_rates WHERE reconciliation_id = $1",
            &rec1
        )
        .await,
        2
    );
    let (_, rec1_after) = app
        .get(&format!("/api/v1/reconciliations/{rec1}"), &org.token)
        .await;
    assert_eq!(
        rec1_after, rec1_before,
        "the earlier reconciliation is never rewritten"
    );

    // The stored snapshot and its PDF are exactly what they were.
    let (st, latest) = app
        .get(&format!("/api/v1/discrepancies/{did}/evidence"), &org.token)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(latest["id"], snapshot["id"]);
    assert_eq!(latest["content"], snapshot["content"]);
    assert_eq!(pdf_text(&app, &org, &did).await, pdf_before);
}

// ---------------------------------------------------------------------------------------------
// provenance of the applicable-rate set: "recorded" is an explicit fact, never inferred from rows
// ---------------------------------------------------------------------------------------------

/// `reconciliations.applicable_rates_recorded` for a reconciliation.
async fn marker(app: &TestApp, org: &TestOrg, reconciliation_id: &str) -> bool {
    let id: Uuid = reconciliation_id.parse().unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT applicable_rates_recorded FROM reconciliations WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

const RATE_ROWS: &str =
    "SELECT count(*) FROM reconciliation_applicable_rates WHERE reconciliation_id = $1";

/// A vendor and contract with NO rate at all, and an invoice billing a drum line and a fee.
async fn invoice_with_no_rate(app: &TestApp, org: &TestOrg, tag: &str) -> String {
    let vendor = ok(
        app,
        org,
        "/api/v1/vendors",
        json!({ "name": format!("Vendor {tag}") }),
    )
    .await;
    ok(
        app,
        org,
        "/api/v1/contracts",
        json!({ "vendor_id": vendor["id"], "name": "Master" }),
    )
    .await;
    let invoice = ok(app, org, "/api/v1/invoices", json!({
        "vendor_id": vendor["id"], "invoice_number": format!("INV-{tag}"), "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003", "total": "4500.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }]
    })).await;
    invoice["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_new_reconciliation_with_no_applicable_rate_is_recorded_not_legacy() {
    let app = test_app().await;
    let org = app.signup_org("mr-empty").await;
    let invoice_id = invoice_with_no_rate(&app, &org, "empty").await;
    let r = run(&app, &org, &invoice_id).await;
    let rec_id = r["reconciliation_id"].as_str().unwrap().to_string();

    // No rate covered the service date: an empty set, but a RECORDED empty set.
    assert!(
        marker(&app, &org, &rec_id).await,
        "every new reconciliation is marked recorded"
    );
    assert_eq!(scalar_i64(&app, &org, RATE_ROWS, &rec_id).await, 0);
    assert!(
        r["discrepancies"]
            .as_array()
            .map_or(false, |d| !d.is_empty()),
        "the unpriced invoice has findings to build evidence from: {r}"
    );
    let did = r["discrepancy_ids"][0].as_str().unwrap().to_string();

    let c = evidence(&app, &org, &did).await["content"].clone();
    assert_eq!(
        c["applicable_rates_basis"], "recorded",
        "an empty recorded set is not a legacy reconciliation"
    );
    assert_eq!(c["applicable_rates"], json!([]));
    assert!(c["contract_rate"].is_null());
    assert!(c["priced_lines"].as_array().unwrap().is_empty());
    for fl in c["fee_lines"].as_array().unwrap() {
        assert_eq!(
            fl["authorizations"],
            json!([]),
            "no rate authorizes anything"
        );
        assert!(fl["priced_by_contract_rate_id"].is_null());
    }
    let pdf = pdf_text(&app, &org, &did).await;
    assert!(pdf.contains("No contract rate covered"));
    assert!(
        !pdf.contains("predates recording"),
        "the legacy note must not appear for a recorded reconciliation"
    );
}

#[tokio::test]
async fn a_historical_reconciliation_with_no_recorded_rows_keeps_the_legacy_state() {
    let app = test_app().await;
    let org = app.signup_org("mr-historical").await;
    let m = multi(
        &app,
        &org,
        "historical",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "TRANSPORT",
            amount: "600.00",
            ctype: Some("EXACT"),
        }],
        false,
        &[("TRANSPORT", "500.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    let rec_id = r["reconciliation_id"].as_str().unwrap().to_string();
    let did = discrepancy_id(&r, "RULE_11_AMBIGUOUS_FEE");
    // A row as it exists after the migration for a reconciliation made before 0013: the column's
    // DEFAULT (false) and no rate rows.
    {
        let rid: Uuid = rec_id.parse().unwrap();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        sqlx::query("DELETE FROM reconciliation_applicable_rates WHERE reconciliation_id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE reconciliations SET applicable_rates_recorded = false WHERE id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let c = evidence(&app, &org, &did).await["content"].clone();
    assert_eq!(c["applicable_rates_basis"], "line_rates_only");
    assert_eq!(
        c["applicable_rates"].as_array().unwrap().len(),
        1,
        "the existing fallback: rates found in line-rate rows"
    );
    assert!(pdf_text(&app, &org, &did)
        .await
        .contains("predates recording the full set of applicable rates"));
}

#[tokio::test]
async fn a_reconciliation_from_between_0013_and_0014_is_recognised_by_its_rows() {
    let app = test_app().await;
    let org = app.signup_org("mr-between").await;
    let m = multi(
        &app,
        &org,
        "between",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "TRANSPORT",
            amount: "600.00",
            ctype: Some("EXACT"),
        }],
        false,
        &[("TRANSPORT", "500.00")],
    )
    .await;
    let r = run(&app, &org, &m.invoice_id).await;
    let rec_id = r["reconciliation_id"].as_str().unwrap().to_string();
    let did = discrepancy_id(&r, "RULE_11_AMBIGUOUS_FEE");
    let before = evidence(&app, &org, &did).await["content"]["applicable_rates"].clone();
    {
        // Created by 0013 code: rate rows exist, the (later) marker column holds its default, false.
        let rid: Uuid = rec_id.parse().unwrap();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        sqlx::query("UPDATE reconciliations SET applicable_rates_recorded = false WHERE id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(scalar_i64(&app, &org, RATE_ROWS, &rec_id).await, 2);
    let c = evidence(&app, &org, &did).await["content"].clone();
    assert_eq!(
        c["applicable_rates_basis"], "recorded",
        "recorded rate rows prove the snapshot was recorded"
    );
    assert_eq!(
        c["applicable_rates"], before,
        "same rates as when the marker was set"
    );
}

#[tokio::test]
async fn every_new_reconciliation_is_marked_and_the_marker_commits_with_its_rows() {
    let app = test_app().await;
    let org = app.signup_org("mr-marker").await;

    // zero rates, one rate, several rates
    let none = invoice_with_no_rate(&app, &org, "marker-none").await;
    let r0 = run(&app, &org, &none).await;
    let vendor = ok(
        &app,
        &org,
        "/api/v1/vendors",
        json!({ "name": "Vendor marker-one" }),
    )
    .await;
    let contract = ok(
        &app,
        &org,
        "/api/v1/contracts",
        json!({ "vendor_id": vendor["id"], "name": "Master" }),
    )
    .await;
    make_rate(
        &app,
        &org,
        contract["id"].as_str().unwrap(),
        "drum",
        "400.00",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
    )
    .await;
    let one = ok(&app, &org, "/api/v1/invoices", json!({
        "vendor_id": vendor["id"], "invoice_number": "INV-marker-one", "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003", "total": "4500.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }]
    })).await;
    let r1 = run(&app, &org, one["id"].as_str().unwrap()).await;
    let m = multi(
        &app,
        &org,
        "marker-many",
        &[Fee {
            code: "TRANSPORT",
            amount: "500.00",
            ctype: Some("EXACT"),
        }],
        &[Fee {
            code: "HANDLING",
            amount: "150.00",
            ctype: Some("CAP"),
        }],
        true,
        &[("TRANSPORT", "500.00")],
    )
    .await;
    let r2 = run(&app, &org, &m.invoice_id).await;
    // a re-run is a new reconciliation and is marked too
    let r2b = run(&app, &org, &m.invoice_id).await;

    for (label, r, rows) in [
        ("zero rates", &r0, 0),
        ("one rate", &r1, 1),
        ("two rates", &r2, 2),
        ("two rates, re-run", &r2b, 2),
    ] {
        let id = r["reconciliation_id"].as_str().unwrap();
        assert!(marker(&app, &org, id).await, "{label}: marked recorded");
        assert_eq!(
            scalar_i64(&app, &org, RATE_ROWS, id).await,
            rows,
            "{label}: rate rows"
        );
    }

    // Atomicity: a reconciliation that is rolled back leaves neither the marker nor any rate row.
    let before = scalar_i64(
        &app,
        &org,
        "SELECT count(*) FROM reconciliations WHERE invoice_id = $1",
        &m.invoice_id,
    )
    .await;
    let rows_before = scalar_i64(
        &app,
        &org,
        "SELECT count(*) FROM reconciliation_applicable_rates WHERE invoice_id = $1",
        &m.invoice_id,
    )
    .await;
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let outcome = xflow_backend::reconciliation::service::run_for_invoice(
        &mut tx,
        org.organization_id,
        m.invoice_id.parse().unwrap(),
    )
    .await
    .unwrap();
    let inside: bool =
        sqlx::query_scalar("SELECT applicable_rates_recorded FROM reconciliations WHERE id = $1")
            .bind(outcome.reconciliation_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(
        inside,
        "the marker is part of the reconciliation row itself"
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        scalar_i64(
            &app,
            &org,
            "SELECT count(*) FROM reconciliations WHERE invoice_id = $1",
            &m.invoice_id
        )
        .await,
        before
    );
    assert_eq!(
        scalar_i64(
            &app,
            &org,
            "SELECT count(*) FROM reconciliation_applicable_rates WHERE invoice_id = $1",
            &m.invoice_id
        )
        .await,
        rows_before
    );
}
