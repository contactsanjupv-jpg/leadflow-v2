//! Level 1 regression tests: waste-stream matching for contract-rate lookup.
//! Real HTTP routes + real Postgres (RLS active). Capitalization/whitespace differences must not
//! make a valid rate disappear; distinct identifiers must not merge; expired and genuinely
//! missing rates must stay visible findings and must never price an invoice.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

async fn ok(app: &TestApp, org: &TestOrg, path: &str, body: Value) -> Value {
    let (s, v) = app.post(path, &org.token, body).await;
    assert!(s.is_success(), "POST {path} -> {s}: {v}");
    v
}

async fn vendor_and_contract(app: &TestApp, org: &TestOrg, tag: &str) -> (String, String) {
    let v = ok(
        app,
        org,
        "/api/v1/vendors",
        json!({ "name": format!("V {tag}") }),
    )
    .await;
    let vid = v["id"].as_str().unwrap().to_string();
    let c = ok(
        app,
        org,
        "/api/v1/contracts",
        json!({ "vendor_id": vid, "name": "Master" }),
    )
    .await;
    (vid, c["id"].as_str().unwrap().to_string())
}

async fn add_rate(
    app: &TestApp,
    org: &TestOrg,
    contract_id: &str,
    stream: &str,
    unit_rate: &str,
    from: &str,
    to: Option<&str>,
    facility: Option<Uuid>,
) -> (axum::http::StatusCode, Value) {
    app.post(
        &format!("/api/v1/contracts/{contract_id}/rates"),
        &org.token,
        json!({
            "waste_stream": stream, "unit": "drum", "unit_rate": unit_rate,
            "effective_from": from, "effective_to": to, "facility_id": facility,
            "authorized_fees": [{ "fee_code": "TRANSPORT", "amount": "500.00" }]
        }),
    )
    .await
}

/// The spec worked example: 10 drums @ $450 (contract $400), contracted $500 transport,
/// unauthorized $200 ENV_FEE => expected $4,500, invoiced $5,200, discrepancy $700.
async fn worked_invoice(
    app: &TestApp,
    org: &TestOrg,
    vendor_id: &str,
    number: &str,
    stream: &str,
    service_date: &str,
    facility: Option<Uuid>,
) -> Value {
    ok(app, org, "/api/v1/invoices", json!({
        "vendor_id": vendor_id, "invoice_number": number, "invoice_date": "2026-03-15",
        "service_date": service_date, "waste_stream": stream, "facility_id": facility, "total": "5200.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "450.00", "line_total": "4500.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" },
            { "description": "Environmental fee", "fee_code": "ENV_FEE", "quantity": "1", "unit": "flat", "unit_price": "200.00", "line_total": "200.00" }]
    })).await
}

async fn run(app: &TestApp, org: &TestOrg, invoice: &Value) -> Value {
    ok(
        app,
        org,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": invoice["id"] }),
    )
    .await
}

/// The rate recorded on a reconciliation (read from the DB: the run response does not carry it).
async fn applied_rate_id(app: &TestApp, org: &TestOrg, run: &Value) -> Option<Uuid> {
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let id: Option<Uuid> =
        sqlx::query_scalar("SELECT contract_rate_id FROM reconciliations WHERE id = $1")
            .bind(
                run["reconciliation_id"]
                    .as_str()
                    .expect("reconciliation_id in run response")
                    .parse::<Uuid>()
                    .unwrap(),
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    id
}

fn rules_of(run: &Value) -> Vec<String> {
    run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn capitalization_and_whitespace_do_not_hide_a_valid_rate() {
    let app = test_app().await;
    let org = app.signup_org("ws-case").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "case").await;
    let (s, b) = add_rate(&app, &org, &cid, "F003", "400.00", "2026-01-01", None, None).await;
    assert!(s.is_success(), "{b}");
    for (n, spelling) in ["f003", " F003 ", "f003  ", "F003"].iter().enumerate() {
        // Distinct service date per invoice so RULE_05 (same vendor + same date duplicate check) stays out of this test.
        let inv = worked_invoice(
            &app,
            &org,
            &vid,
            &format!("INV-C{n}"),
            spelling,
            &format!("2026-03-{:02}", 10 + n),
            None,
        )
        .await;
        let r = run(&app, &org, &inv).await;
        let rules = rules_of(&r);
        assert!(
            !rules.contains(&"RULE_00_NO_RATE".to_string()),
            "{spelling:?} lost its rate: {r}"
        );
        assert_eq!(r["pricing_status"], "COMPLETE", "{spelling:?}: {r}");
        assert_eq!(r["expected_amount"], "4500.0000", "{spelling:?}: {r}");
        assert_eq!(r["invoiced_amount"], "5200.0000");
        assert_eq!(r["discrepancy_amount"], "700.0000", "{spelling:?}: {r}");
        assert!(rules.contains(&"RULE_03_RATE_MISMATCH".to_string()));
        assert!(rules.contains(&"RULE_04_UNAUTHORIZED_FEE".to_string()));
    }
}

#[tokio::test]
async fn rate_stored_in_lowercase_matches_an_uppercase_invoice() {
    let app = test_app().await;
    let org = app.signup_org("ws-rev").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "rev").await;
    let (s, b) = add_rate(
        &app,
        &org,
        &cid,
        " f003 ",
        "400.00",
        "2026-01-01",
        None,
        None,
    )
    .await;
    assert!(s.is_success(), "{b}");
    let inv = worked_invoice(&app, &org, &vid, "INV-R1", "F003", "2026-03-10", None).await;
    let r = run(&app, &org, &inv).await;
    assert_eq!(r["pricing_status"], "COMPLETE", "{r}");
    assert_eq!(r["expected_amount"], "4500.0000", "{r}");
    assert_eq!(r["discrepancy_amount"], "700.0000");
}

#[tokio::test]
async fn distinct_identifiers_are_not_merged() {
    let app = test_app().await;
    let org = app.signup_org("ws-distinct").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "distinct").await;
    add_rate(&app, &org, &cid, "F003", "400.00", "2026-01-01", None, None).await;
    for (n, other) in ["F03", "F0030", "F 003", "D001", "F004"].iter().enumerate() {
        let inv = worked_invoice(
            &app,
            &org,
            &vid,
            &format!("INV-D{n}"),
            other,
            "2026-03-10",
            None,
        )
        .await;
        let r = run(&app, &org, &inv).await;
        assert!(
            rules_of(&r).contains(&"RULE_00_NO_RATE".to_string()),
            "{other:?} must NOT match F003: {r}"
        );
        assert!(
            r["discrepancies"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["rule_id"] != "RULE_03_RATE_MISMATCH"),
            "an unmatched stream must not be priced: {r}"
        );
    }
}

#[tokio::test]
async fn genuinely_missing_rate_is_flagged_and_not_priced() {
    let app = test_app().await;
    let org = app.signup_org("ws-missing").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "missing").await;
    // Rate starts AFTER the service date: not expired, simply not in force yet => missing.
    add_rate(&app, &org, &cid, "F003", "400.00", "2026-06-01", None, None).await;
    let inv = worked_invoice(&app, &org, &vid, "INV-M1", "F003", "2026-03-10", None).await;
    let r = run(&app, &org, &inv).await;
    assert_eq!(
        rules_of(&r)
            .iter()
            .filter(|x| *x == "RULE_00_NO_RATE")
            .count(),
        1,
        "{r}"
    );
    assert!(r["expected_amount"].is_null(), "no rate means expected amount is uncomputable: {r}");
    assert!(r["discrepancy_amount"].is_null(), "no bill-level discrepancy may be asserted: {r}");
    assert_eq!(r["pricing_status"], "UNCOMPUTABLE", "{r}");
    assert_ne!(r["review_status"], "RESOLVED", "must require review: {r}");
    assert_eq!(
        applied_rate_id(&app, &org, &r).await,
        None,
        "no rate may be recorded as applied"
    );
}

#[tokio::test]
async fn expired_rate_is_reported_and_never_prices_the_invoice() {
    let app = test_app().await;
    let org = app.signup_org("ws-expired").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "expired").await;
    add_rate(
        &app,
        &org,
        &cid,
        "F003",
        "400.00",
        "2026-01-01",
        Some("2026-02-01"),
        None,
    )
    .await;
    let inv = worked_invoice(&app, &org, &vid, "INV-E1", "f003", "2026-03-10", None).await;
    let r = run(&app, &org, &inv).await;
    let rules = rules_of(&r);
    assert!(rules.contains(&"RULE_06_STALE_RATE".to_string()), "{r}");
    assert!(
        !rules.contains(&"RULE_03_RATE_MISMATCH".to_string()),
        "expired rate priced the invoice: {r}"
    );
    assert!(
        !rules.contains(&"RULE_04_UNAUTHORIZED_FEE".to_string()),
        "expired rate priced the fees: {r}"
    );
    assert!(r["expected_amount"].is_null(), "expired rate must not produce an expected amount: {r}");
    assert!(r["discrepancy_amount"].is_null(), "expired rate must not produce a bill-level discrepancy: {r}");
    assert_eq!(r["pricing_status"], "UNCOMPUTABLE", "{r}");
    assert_ne!(r["review_status"], "RESOLVED");
    assert_eq!(
        applied_rate_id(&app, &org, &r).await,
        None,
        "an expired rate must not be recorded as the applied rate: {r}"
    );

    // A covering rate that starts after the expired one wins and prices normally.
    add_rate(&app, &org, &cid, "F003", "400.00", "2026-02-02", None, None).await;
    let inv2 = worked_invoice(&app, &org, &vid, "INV-E2", "f003", "2026-03-11", None).await;
    let r2 = run(&app, &org, &inv2).await;
    assert_eq!(r2["expected_amount"], "4500.0000", "{r2}");
    assert_eq!(r2["discrepancy_amount"], "700.0000");
    assert!(!rules_of(&r2).contains(&"RULE_06_STALE_RATE".to_string()));
}

#[tokio::test]
async fn facility_specific_rate_selection_survives_case_differences() {
    let app = test_app().await;
    let org = app.signup_org("ws-fac").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "fac").await;
    let (fa, fb) = (Uuid::new_v4(), Uuid::new_v4());
    assert!(
        add_rate(&app, &org, &cid, "F003", "400.00", "2026-01-01", None, None)
            .await
            .0
            .is_success()
    );
    assert!(add_rate(
        &app,
        &org,
        &cid,
        "f003",
        "450.00",
        "2026-01-01",
        None,
        Some(fa)
    )
    .await
    .0
    .is_success());

    // Distinct service dates keep RULE_05 (same vendor + same date duplicate check) out of this test.
    let body = |num: &str, day: &str, facility: Option<Uuid>| {
        json!({
            "vendor_id": vid, "invoice_number": num, "invoice_date": "2026-03-15", "service_date": day,
            "waste_stream": "F003", "facility_id": facility, "total": "4500.00",
            "line_items": [{ "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "450.00", "line_total": "4500.00" }]
        })
    };
    // Facility A: its own $450 rate applies (stored as 'f003'), so $4,500 is correct.
    let a = ok(
        &app,
        &org,
        "/api/v1/invoices",
        body("INV-FA", "2026-03-10", Some(fa)),
    )
    .await;
    let ra = run(&app, &org, &a).await;
    assert_eq!(ra["expected_amount"], "4500.0000", "{ra}");
    assert!(
        !rules_of(&ra).contains(&"RULE_03_RATE_MISMATCH".to_string()),
        "{ra}"
    );
    // Facility B has no rate of its own: the vendor-wide $400 applies; A's rate is never a fallback.
    let b = ok(
        &app,
        &org,
        "/api/v1/invoices",
        body("INV-FB", "2026-03-11", Some(fb)),
    )
    .await;
    let rb = run(&app, &org, &b).await;
    assert_eq!(rb["expected_amount"], "4000.0000", "{rb}");
    assert_eq!(rb["discrepancy_amount"], "500.0000");
    // No facility on the invoice: vendor-wide only.
    let n = ok(
        &app,
        &org,
        "/api/v1/invoices",
        body("INV-FN", "2026-03-12", None),
    )
    .await;
    let rn = run(&app, &org, &n).await;
    assert_eq!(rn["expected_amount"], "4000.0000", "{rn}");
}

#[tokio::test]
async fn overlapping_rates_that_differ_only_by_case_are_rejected() {
    let app = test_app().await;
    let org = app.signup_org("ws-overlap").await;
    let (_vid, cid) = vendor_and_contract(&app, &org, "overlap").await;
    assert!(
        add_rate(&app, &org, &cid, "F003", "400.00", "2026-01-01", None, None)
            .await
            .0
            .is_success()
    );
    let (s, _) = add_rate(
        &app,
        &org,
        &cid,
        "f003 ",
        "420.00",
        "2026-03-01",
        None,
        None,
    )
    .await;
    assert_eq!(
        s,
        axum::http::StatusCode::CONFLICT,
        "case/whitespace variants must not create ambiguous overlapping rates"
    );
    // A different identifier is unaffected.
    assert!(
        add_rate(&app, &org, &cid, "F004", "420.00", "2026-03-01", None, None)
            .await
            .0
            .is_success()
    );
}

#[tokio::test]
async fn monitoring_requeues_an_invoice_when_a_matching_rate_arrives_in_different_case() {
    let app = test_app().await;
    let org = app.signup_org("ws-worker").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "worker").await;
    let inv = worked_invoice(&app, &org, &vid, "INV-W1", "f003", "2026-03-10", None).await;
    let first = run(&app, &org, &inv).await;
    assert!(rules_of(&first).contains(&"RULE_00_NO_RATE".to_string()));

    assert!(
        add_rate(&app, &org, &cid, "F003", "400.00", "2026-01-01", None, None)
            .await
            .0
            .is_success()
    );
    let n = xflow_backend::workers::run_reconcile_pending(&app.pool, org.organization_id, false)
        .await
        .unwrap();
    assert!(
        n >= 1,
        "the late-arriving rate must trigger re-reconciliation, processed {n}"
    );

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let rules: Vec<String> = sqlx::query_scalar(
        "SELECT d.rule_id FROM discrepancies d WHERE d.reconciliation_id =
           (SELECT id FROM reconciliations WHERE invoice_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1)",
    )
    .bind(inv["id"].as_str().unwrap().parse::<Uuid>().unwrap())
    .fetch_all(&mut *tx).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(!rules.contains(&"RULE_00_NO_RATE".to_string()), "{rules:?}");
    assert!(
        rules.contains(&"RULE_03_RATE_MISMATCH".to_string()),
        "{rules:?}"
    );
}

#[tokio::test]
async fn rust_and_sql_keys_agree() {
    // Evaluates the SAME expression migration 0006 uses for the generated columns, directly,
    // and compares it with the Rust canonicalization (no table or generated column involved).
    let app = test_app().await;
    let samples = [
        "F003",
        "f003",
        "  F003  ",
        "f 003",
        "F  003",
        "F\t003",
        "F 003",
        "hazardous   waste",
        "d001 ",
        "F0030",
        "Ab  cD",
        "",
        "   ",
    ];
    for raw in samples {
        let sql_key: Option<String> = sqlx::query_scalar(
            "SELECT NULLIF(upper(btrim(regexp_replace($1::text, '[[:space:]]+', ' ', 'g'))), '')",
        )
        .bind(raw)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(
            sql_key,
            xflow_backend::common::waste_stream::key(raw),
            "{raw:?}"
        );
    }
}

#[tokio::test]
async fn internal_whitespace_is_collapsed_not_removed() {
    let app = test_app().await;
    let org = app.signup_org("ws-inner").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "inner").await;
    // Rate for the identifier "F 003" (with a space) -- a different identifier from "F003".
    assert!(add_rate(
        &app,
        &org,
        &cid,
        "F 003",
        "400.00",
        "2026-01-01",
        None,
        None
    )
    .await
    .0
    .is_success());
    // "f  003" (double space, lowercase) collapses to "F 003": matches and prices.
    let hit = worked_invoice(&app, &org, &vid, "INV-I1", "f  003", "2026-03-10", None).await;
    let r = run(&app, &org, &hit).await;
    assert!(
        !rules_of(&r).contains(&"RULE_00_NO_RATE".to_string()),
        "{r}"
    );
    assert_eq!(r["expected_amount"], "4500.0000", "{r}");
    // "F003" (no space) is a different identifier: it must NOT match the "F 003" rate.
    let miss = worked_invoice(&app, &org, &vid, "INV-I2", "F003", "2026-03-11", None).await;
    let r2 = run(&app, &org, &miss).await;
    assert!(
        rules_of(&r2).contains(&"RULE_00_NO_RATE".to_string()),
        "F003 must not match 'F 003': {r2}"
    );
}

#[tokio::test]
async fn stale_rate_review_decision_survives_re_reconciliation() {
    let app = test_app().await;
    let org = app.signup_org("ws-carry").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "carry").await;
    add_rate(
        &app,
        &org,
        &cid,
        "F003",
        "400.00",
        "2026-01-01",
        Some("2026-02-01"),
        None,
    )
    .await;
    let inv = worked_invoice(&app, &org, &vid, "INV-S1", "F003", "2026-03-10", None).await;
    let first = run(&app, &org, &inv).await;
    let stale: Vec<&Value> = first["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["rule_id"] == "RULE_06_STALE_RATE")
        .collect();
    assert_eq!(stale.len(), 1, "{first}");
    let desc = stale[0]["description"].as_str().unwrap().to_string();
    // Exact baseline text (commit 33a32b5). Findings already stored by the baseline build carry this
    // description, and carry-forward matches on it, so it must never change.
    assert_eq!(
        desc,
        "Service date 2026-03-10 falls outside rate's effective window (2026-01-01 to 2026-02-01)"
    );
    let did = first["discrepancy_ids"]
        .as_array()
        .unwrap()
        .iter()
        .zip(first["discrepancies"].as_array().unwrap())
        .find(|(_, d)| d["rule_id"] == "RULE_06_STALE_RATE")
        .unwrap()
        .0
        .as_str()
        .unwrap()
        .to_string();

    // A reviewer decides the stale-rate finding.
    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "start_review" }),
    )
    .await;
    let note = "Vendor rate lapsed; extension letter on file";
    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "confirm", "notes": note }),
    )
    .await;

    // Same inputs re-reconciled: the identical finding recurs and must keep the decision.
    let second = run(&app, &org, &inv).await;
    assert_ne!(second["reconciliation_id"], first["reconciliation_id"]);
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let row: (String, Option<Uuid>, Option<String>, String) = sqlx::query_as(
        "SELECT review_status::text, reviewed_by, review_notes, description FROM discrepancies
          WHERE reconciliation_id = $1 AND rule_id = 'RULE_06_STALE_RATE'",
    )
    .bind(
        second["reconciliation_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap(),
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        row.3, desc,
        "description must be stable across runs (carry-forward matches on it)"
    );
    assert_eq!(row.0, "CONFIRMED", "review status was not carried forward");
    assert_eq!(row.1, Some(org.user_id), "reviewer was not carried forward");
    assert_eq!(
        row.2.as_deref(),
        Some(note),
        "review notes were not carried forward"
    );
}

#[tokio::test]
async fn reviewed_no_rate_finding_survives_re_reconciliation() {
    let app = test_app().await;
    let org = app.signup_org("ws-carry0").await;
    let (vid, _cid) = vendor_and_contract(&app, &org, "carry0").await;
    // No rate at all => RULE_00_NO_RATE.
    let inv = worked_invoice(&app, &org, &vid, "INV-N1", "F003", "2026-03-10", None).await;
    let first = run(&app, &org, &inv).await;
    let (did, desc) = first["discrepancy_ids"]
        .as_array()
        .unwrap()
        .iter()
        .zip(first["discrepancies"].as_array().unwrap())
        .find(|(_, d)| d["rule_id"] == "RULE_00_NO_RATE")
        .map(|(id, d)| {
            (
                id.as_str().unwrap().to_string(),
                d["description"].as_str().unwrap().to_string(),
            )
        })
        .expect("RULE_00 finding");
    // Exact baseline text (commit 33a32b5). Findings already stored by the baseline build carry this
    // description, and carry-forward matches on it, so it must never change.
    assert_eq!(
        desc,
        "No contracted rate found covering this invoice's service date"
    );

    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "start_review" }),
    )
    .await;
    let note = "Rate sheet pending from vendor; accepted for now";
    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "confirm", "notes": note }),
    )
    .await;

    let second = run(&app, &org, &inv).await;
    assert_ne!(second["reconciliation_id"], first["reconciliation_id"]);
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let row: (String, Option<Uuid>, Option<String>, String) = sqlx::query_as(
        "SELECT review_status::text, reviewed_by, review_notes, description FROM discrepancies
          WHERE reconciliation_id = $1 AND rule_id = 'RULE_00_NO_RATE'",
    )
    .bind(
        second["reconciliation_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap(),
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(row.3, desc, "description must be stable across runs");
    assert_eq!(row.0, "CONFIRMED", "review status was not carried forward");
    assert_eq!(row.1, Some(org.user_id), "reviewer was not carried forward");
    assert_eq!(
        row.2.as_deref(),
        Some(note),
        "review notes were not carried forward"
    );
}

#[tokio::test]
async fn padded_legacy_rate_inserted_directly_matches_an_uppercase_invoice() {
    let app = test_app().await;
    let org = app.signup_org("ws-legacy").await;
    let (vid, cid) = vendor_and_contract(&app, &org, "legacy").await;
    let (vendor_id, contract_id): (Uuid, Uuid) = (vid.parse().unwrap(), cid.parse().unwrap());

    // The API trims before storage, so write the legacy shape directly: padded, lowercase, tab.
    let padded = "  \tf003  ";
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let rate_id: Uuid = sqlx::query_scalar(
        "INSERT INTO contract_rates (organization_id, contract_id, vendor_id, waste_stream, unit, unit_rate, effective_from)
         VALUES ($1, $2, $3, $4, 'drum', 400.00, '2026-01-01') RETURNING id",
    )
    .bind(org.organization_id).bind(contract_id).bind(vendor_id).bind(padded)
    .fetch_one(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, amount) VALUES ($1, 'TRANSPORT', 500.00)")
        .bind(rate_id).execute(&mut *tx).await.unwrap();
    let (stored, key): (String, Option<String>) =
        sqlx::query_as("SELECT waste_stream, waste_stream_key FROM contract_rates WHERE id = $1")
            .bind(rate_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        stored, padded,
        "the legacy text must be stored exactly as written"
    );
    assert_eq!(key.as_deref(), Some("F003"));

    let inv = worked_invoice(&app, &org, &vid, "INV-L1", "F003", "2026-03-10", None).await;
    let r = run(&app, &org, &inv).await;
    assert!(
        !rules_of(&r).contains(&"RULE_00_NO_RATE".to_string()),
        "padded legacy rate was not found: {r}"
    );
    assert_eq!(r["expected_amount"], "4500.0000", "{r}");
    assert_eq!(r["discrepancy_amount"], "700.0000", "{r}");
    assert_eq!(
        applied_rate_id(&app, &org, &r).await,
        Some(rate_id),
        "the padded legacy rate must be the applied rate"
    );
}
