//! Authorized-fee semantics end to end: EXACT vs CAP pricing, unclassified and conflicting fees,
//! the 422/404/409 contract of fee creation and classification, audit, RBAC and tenant isolation,
//! re-reconciliation by the monitoring worker, evidence JSON / stored snapshots / PDF, and the
//! bill-level RULE_13 guard.
//!
//! These drive the real axum router against a real PostgreSQL database as the least-privilege
//! `xflow_app` role (RLS active). `DATABASE_URL` must be set and migrations 0001-0011 applied.
//!
//! Sign convention under test: a finding amount is `billed - expected`. Positive is overbilled,
//! negative is underbilled; dashboard and recovery totals count only positive findings and a
//! non-positive finding cannot be disputed.

mod common;

use axum::http::{Method, StatusCode};
use common::{have_pdftotext, test_app, TestApp, TestOrg};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

const BASE: i64 = 4000; // 10 drums x $400.00, always billed exactly at the contract rate.

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

async fn ok(app: &TestApp, org: &TestOrg, path: &str, body: Value) -> Value {
    let (s, v) = app.post(path, &org.token, body).await;
    assert!(s.is_success(), "POST {path} -> {s}: {v}");
    v
}

fn dec(v: &Value) -> Decimal {
    v.as_str()
        .unwrap_or_else(|| panic!("expected a money string, got {v}"))
        .parse()
        .unwrap()
}

fn money(v: &Value, expected: &str) {
    assert_eq!(
        dec(v),
        expected.parse::<Decimal>().unwrap(),
        "money mismatch: {v} vs {expected}"
    );
}

fn findings<'a>(run: &'a Value, rule: &str) -> Vec<&'a Value> {
    run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["rule_id"] == rule)
        .collect()
}

fn text_of(d: &Value) -> &str {
    d["description"].as_str().unwrap()
}

/// COMPLETE results must reconcile at bill level: invoiced - expected equals the sum of findings
/// within the documented rounding allowance of one cent per invoice line plus one.
fn assert_bill_invariant(run: &Value, line_count: i64) {
    if run["pricing_status"] != "COMPLETE" {
        return;
    }
    let sum: Decimal = run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| dec(&d["amount"]))
        .sum();
    assert_eq!(
        dec(&run["discrepancy_amount"]),
        sum,
        "discrepancy_amount is the sum of findings"
    );
    let unexplained = (dec(&run["invoiced_amount"]) - dec(&run["expected_amount"])) - sum;
    let allowance = Decimal::new(1, 2) * Decimal::from(line_count + 1);
    assert!(
        unexplained.abs() <= allowance,
        "COMPLETE result leaves {unexplained} unexplained (allowance {allowance}): {run}"
    );
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Exact,
    Cap,
    /// A legacy fee that predates charge types (charge_type NULL); only reachable through SQL.
    Unclassified,
}

struct Scn {
    contract_id: String,
    rate_id: String,
    invoice_id: String,
}

async fn vendor_contract(app: &TestApp, org: &TestOrg, tag: &str) -> (String, String) {
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
    (vendor_id, contract["id"].as_str().unwrap().to_string())
}

async fn make_rate(
    app: &TestApp,
    org: &TestOrg,
    contract_id: &str,
    unit: &str,
    unit_rate: &str,
    fees: Value,
) -> String {
    let v = ok(
        app,
        org,
        &format!("/api/v1/contracts/{contract_id}/rates"),
        json!({ "waste_stream": "F003", "unit": unit, "unit_rate": unit_rate, "effective_from": "2026-01-01", "authorized_fees": fees }),
    )
    .await;
    v["rates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["unit"] == unit)
        .unwrap_or_else(|| panic!("rate for unit {unit} missing in {v}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Insert an authorized fee with SQL as the app role (RLS WITH CHECK applies). `ctype = None`
/// creates a legacy unclassified fee, which the API refuses to create.
async fn add_fee_sql(
    app: &TestApp,
    org: &TestOrg,
    rate_id: &str,
    code: &str,
    amount: &str,
    ctype: Option<&str>,
) {
    let rate: Uuid = rate_id.parse().unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    match ctype {
        Some(t) => {
            sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, amount, charge_type, charge_type_set_at) VALUES ($1, $2, $3::numeric, $4, now())")
                .bind(rate).bind(code).bind(amount).bind(t).execute(&mut *tx).await.unwrap();
        }
        None => {
            sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, amount) VALUES ($1, $2, $3::numeric)")
                .bind(rate).bind(code).bind(amount).execute(&mut *tx).await.unwrap();
        }
    }
    tx.commit().await.unwrap();
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

async fn fee_type_in_db(app: &TestApp, org: &TestOrg, rate_id: &str, code: &str) -> Option<String> {
    let id: Uuid = rate_id.parse().unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query_scalar(
        "SELECT charge_type FROM contract_authorized_fees WHERE contract_rate_id = $1 AND fee_code = $2",
    )
    .bind(id)
    .bind(code)
    .fetch_one(&mut *tx)
    .await
    .unwrap()
}

fn invoice_body(vendor_id: &str, number: &str, manifest_id: Option<&str>, billed: &str) -> Value {
    let total = format!(
        "{:.2}",
        Decimal::from(BASE) + billed.parse::<Decimal>().unwrap()
    );
    let mut b = json!({
        "vendor_id": vendor_id, "invoice_number": number, "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003",
        "total": total,
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": billed, "line_total": billed }]
    });
    if let Some(m) = manifest_id {
        b["manifest_id"] = json!(m);
    }
    b
}

/// One rate ($400/drum) with TRANSPORT authorized at $500 as `kind`, a clean manifest + ticket
/// (10 drums), and an invoice of 10 drums at $400 plus a TRANSPORT line billed at `billed`.
async fn single(app: &TestApp, org: &TestOrg, tag: &str, kind: Kind, billed: &str) -> Scn {
    let (vendor_id, contract_id) = vendor_contract(app, org, tag).await;
    let fees = match kind {
        Kind::Exact => {
            json!([{ "fee_code": "TRANSPORT", "description": "Transportation", "amount": "500.00", "charge_type": "EXACT" }])
        }
        Kind::Cap => {
            json!([{ "fee_code": "TRANSPORT", "description": "Transportation", "amount": "500.00", "charge_type": "CAP" }])
        }
        Kind::Unclassified => json!([]),
    };
    let rate_id = make_rate(app, org, &contract_id, "drum", "400.00", fees).await;
    if let Kind::Unclassified = kind {
        add_fee_sql(app, org, &rate_id, "TRANSPORT", "500.00", None).await;
    }
    let manifest = ok(app, org, "/api/v1/manifests", json!({ "manifest_number": format!("M-{tag}"), "waste_stream": "F003", "container_count": 10, "quantity": "10", "unit": "drum", "shipment_date": "2026-03-09" })).await;
    let mid = manifest["id"].as_str().unwrap().to_string();
    ok(app, org, "/api/v1/weight-tickets", json!({ "ticket_number": format!("T-{tag}"), "manifest_id": mid, "net_weight": "10", "unit": "drum", "ticket_date": "2026-03-10" })).await;
    let invoice = ok(
        app,
        org,
        "/api/v1/invoices",
        invoice_body(&vendor_id, &format!("INV-{tag}"), Some(&mid), billed),
    )
    .await;
    Scn {
        contract_id,
        rate_id,
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

async fn classify(
    app: &TestApp,
    token: &str,
    rate_id: &str,
    code: &str,
    body: Value,
) -> (StatusCode, Value) {
    app.call(
        Method::PUT,
        &format!("/api/v1/rates/{rate_id}/fees/{code}/type"),
        Some(token),
        Some(body),
    )
    .await
}

/// Run the monitoring worker for `org` and return only once none of THAT organization's jobs is still
/// pending or running.
///
/// The job queue is global: `run_one_job` claims the oldest job of ANY organization, and anything else
/// using the same database (another test process, `cargo nextest`, a running server's worker) can claim,
/// and be part-way through, THIS test's job. "Nothing left to claim" then does not mean "my job is
/// done", and returning at that point lets the caller assert before the work has finished. Waiting for
/// the organization's own jobs is correct whoever ends up executing them.
async fn drain_worker(app: &TestApp, org: &TestOrg) {
    app.post("/api/v1/monitoring/run", &org.token, json!({}))
        .await;
    for _ in 0..3000 {
        for _ in 0..200 {
            if !xflow_backend::workers::run_one_job(&app.pool, false)
                .await
                .unwrap()
            {
                break;
            }
        }
        if active_jobs(app, org).await == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the monitoring jobs of the organization never finished");
}

async fn active_jobs(app: &TestApp, org: &TestOrg) -> i64 {
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query_scalar(
        "SELECT count(*) FROM monitoring_jobs WHERE organization_id = $1 AND status IN ('PENDING', 'RUNNING')",
    )
    .bind(org.organization_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap()
}

fn assert_422_envelope(status: StatusCode, body: &Value, ctx: &str) {
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{ctx}: {body}");
    assert_eq!(
        body["error"]["code"], "UNPROCESSABLE_ENTITY",
        "{ctx}: must be the application error envelope, got {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("EXACT or CAP"),
        "{ctx}: {body}"
    );
}

fn fee_of<'a>(detail: &'a Value, code: &str) -> &'a Value {
    detail["rates"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["authorized_fees"].as_array().unwrap())
        .find(|f| f["fee_code"] == code)
        .unwrap_or_else(|| panic!("fee {code} missing in {detail}"))
}

fn pdf_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

// ---------------------------------------------------------------------------------------------
// EXACT and CAP pricing: below, equal to and above the authorized amount
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn exact_and_cap_fees_price_below_equal_and_above_the_authorized_amount() {
    let app = test_app().await;
    let org = app.signup_org("fee-matrix").await;

    // (kind, billed, expected total, finding amount billed - expected, wording fragment)
    let cases: [(Kind, &str, &str, Option<&str>, &str); 6] = [
        (
            Kind::Exact,
            "400.00",
            "4500",
            Some("-100"),
            "authorized amount is",
        ),
        (Kind::Exact, "500.00", "4500", None, ""),
        (
            Kind::Exact,
            "600.00",
            "4500",
            Some("100"),
            "authorized amount is",
        ),
        (Kind::Cap, "400.00", "4400", None, ""),
        (Kind::Cap, "500.00", "4500", None, ""),
        (
            Kind::Cap,
            "600.00",
            "4500",
            Some("100"),
            "exceeds the authorized cap of",
        ),
    ];
    for (n, (kind, billed, expected, finding, wording)) in cases.into_iter().enumerate() {
        let tag = format!("m{n}");
        let s = single(&app, &org, &tag, kind, billed).await;
        let r = run(&app, &org, &s.invoice_id).await;
        let ctx = format!("{kind:?} billed {billed}: {r}");

        assert_eq!(r["pricing_status"], "COMPLETE", "{ctx}");
        money(&r["expected_amount"], expected);
        let fee_findings = findings(&r, "RULE_04_UNAUTHORIZED_FEE");
        match finding {
            Some(amount) => {
                assert_eq!(fee_findings.len(), 1, "{ctx}");
                money(&fee_findings[0]["amount"], amount);
                assert!(text_of(fee_findings[0]).contains(wording), "{ctx}");
                money(&r["discrepancy_amount"], amount);
            }
            None => {
                assert!(fee_findings.is_empty(), "{ctx}");
                money(&r["discrepancy_amount"], "0");
            }
        }
        assert_eq!(
            r["discrepancies"].as_array().unwrap().len(),
            usize::from(finding.is_some()),
            "no other findings expected: {ctx}"
        );
        assert_bill_invariant(&r, 2);
    }
}

#[tokio::test]
async fn exact_findings_keep_their_original_wording_for_review_carry_forward() {
    let app = test_app().await;
    let org = app.signup_org("fee-wording").await;
    let s = single(&app, &org, "w", Kind::Exact, "600.00").await;
    let r = run(&app, &org, &s.invoice_id).await;
    let f = findings(&r, "RULE_04_UNAUTHORIZED_FEE");
    assert_eq!(f.len(), 1);
    // Byte-identical to the pre-charge-type EXACT text, so earlier review decisions still match.
    assert_eq!(
        text_of(f[0]),
        "Fee 'TRANSPORT' billed at $600.00 but authorized amount is $500.00"
    );

    // A review decision on the finding survives an unchanged re-run (carry-forward).
    let did = discrepancy_id(&r, "RULE_04_UNAUTHORIZED_FEE");
    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "confirm" }),
    )
    .await;
    let again = run(&app, &org, &s.invoice_id).await;
    let (_, list) = app.get("/api/v1/discrepancies", &org.token).await;
    let carried = list
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == again["discrepancy_ids"][0])
        .unwrap_or_else(|| panic!("re-run finding missing from {list}"));
    assert_eq!(carried["review_status"], "CONFIRMED");
}

// ---------------------------------------------------------------------------------------------
// unclassified and conflicting fees are unresolved (PARTIAL), never falsely COMPLETE
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_unclassified_fee_makes_pricing_partial_and_is_never_inferred() {
    let app = test_app().await;
    let org = app.signup_org("fee-unclassified").await;
    // Billed exactly at the authorized amount: even a "matching" fee must not be assumed EXACT.
    let s = single(&app, &org, "u", Kind::Unclassified, "500.00").await;
    let r = run(&app, &org, &s.invoice_id).await;
    assert_eq!(r["pricing_status"], "PARTIAL", "{r}");
    assert!(r["expected_amount"].is_null(), "{r}");
    assert!(r["discrepancy_amount"].is_null(), "{r}");
    money(&r["partial_expected_amount"], "4000");
    let f = findings(&r, "RULE_12_UNCLASSIFIED_FEE_TYPE");
    assert_eq!(f.len(), 1, "{r}");
    money(&f[0]["amount"], "0");
    assert!(text_of(f[0]).contains("has not been set"), "{r}");
    assert!(
        findings(&r, "RULE_04_UNAUTHORIZED_FEE").is_empty(),
        "an unclassified fee must not produce an amount finding: {r}"
    );
}

#[tokio::test]
async fn conflicting_fee_authorizations_across_rates_are_unresolved() {
    let app = test_app().await;
    let org = app.signup_org("fee-conflict").await;

    // (tag, drum-rate fee as API json, ton-rate: (amount, charge type or None=legacy), expected wording or None=resolves)
    #[allow(clippy::type_complexity)]
    let cases: [(&str, (&str, &str), (&str, Option<&str>), Option<&str>); 4] = [
        (
            "c-amount",
            ("500.00", "EXACT"),
            ("600.00", Some("EXACT")),
            Some("conflicting amounts"),
        ),
        (
            "c-type",
            ("500.00", "EXACT"),
            ("500.00", Some("CAP")),
            Some("conflicting charge types"),
        ),
        (
            "c-legacy",
            ("500.00", "EXACT"),
            ("500.00", None),
            Some("conflicting charge types"),
        ),
        (
            "c-same",
            ("500.00", "EXACT"),
            ("500.00", Some("EXACT")),
            None,
        ),
    ];
    for (tag, drum, ton, wording) in cases {
        let (vendor_id, contract_id) = vendor_contract(&app, &org, tag).await;
        make_rate(
            &app,
            &org,
            &contract_id,
            "drum",
            "400.00",
            json!([{ "fee_code": "TRANSPORT", "amount": drum.0, "charge_type": drum.1 }]),
        )
        .await;
        let ton_rate = match ton.1 {
            Some(t) => {
                make_rate(
                    &app,
                    &org,
                    &contract_id,
                    "ton",
                    "1000.00",
                    json!([{ "fee_code": "TRANSPORT", "amount": ton.0, "charge_type": t }]),
                )
                .await
            }
            None => {
                let id = make_rate(&app, &org, &contract_id, "ton", "1000.00", json!([])).await;
                add_fee_sql(&app, &org, &id, "TRANSPORT", ton.0, None).await;
                id
            }
        };
        let _ = ton_rate;
        let inv = ok(&app, &org, "/api/v1/invoices", json!({
            "vendor_id": vendor_id, "invoice_number": format!("INV-{tag}"), "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003",
            "total": "5500.00",
            "line_items": [
                { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
                { "description": "Ton disposal", "quantity": "1", "unit": "ton", "unit_price": "1000.00", "line_total": "1000.00" },
                { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }]
        })).await;
        let r = run(&app, &org, inv["id"].as_str().unwrap()).await;
        let ctx = format!("{tag}: {r}");
        match wording {
            Some(w) => {
                assert_eq!(r["pricing_status"], "PARTIAL", "{ctx}");
                assert!(r["expected_amount"].is_null(), "{ctx}");
                assert!(r["discrepancy_amount"].is_null(), "{ctx}");
                let f = findings(&r, "RULE_11_AMBIGUOUS_FEE");
                assert_eq!(f.len(), 1, "{ctx}");
                money(&f[0]["amount"], "0");
                assert!(text_of(f[0]).contains(w), "{ctx}");
                assert!(findings(&r, "RULE_04_UNAUTHORIZED_FEE").is_empty(), "{ctx}");
            }
            None => {
                // Identical (amount, type) authorizations from several rates count once.
                assert_eq!(r["pricing_status"], "COMPLETE", "{ctx}");
                assert!(findings(&r, "RULE_11_AMBIGUOUS_FEE").is_empty(), "{ctx}");
                money(&r["expected_amount"], "5500");
                assert_bill_invariant(&r, 3);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// request validation: 422 envelope, 404, 409
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn new_fees_require_a_valid_charge_type_with_the_application_422() {
    let app = test_app().await;
    let org = app.signup_org("fee-422").await;
    let (_, contract_id) = vendor_contract(&app, &org, "v").await;
    let path = format!("/api/v1/contracts/{contract_id}/rates");
    let rate = |fee: Value| json!({ "waste_stream": "F003", "unit": "drum", "unit_rate": "400.00", "effective_from": "2026-01-01", "authorized_fees": [fee] });

    for (label, fee) in [
        (
            "missing",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00" }),
        ),
        (
            "null",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": null }),
        ),
        (
            "lowercase",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": "exact" }),
        ),
        (
            "unknown",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": "MAX" }),
        ),
        (
            "empty",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": "" }),
        ),
        (
            "number",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": 5 }),
        ),
        (
            "padded",
            json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": " CAP" }),
        ),
    ] {
        let (s, b) = app
            .call(Method::POST, &path, Some(&org.token), Some(rate(fee)))
            .await;
        assert_422_envelope(s, &b, label);
    }
    // Nothing was created by any rejected request (validation precedes the transaction).
    let (_, detail) = app
        .get(&format!("/api/v1/contracts/{contract_id}"), &org.token)
        .await;
    assert!(detail["rates"].as_array().unwrap().is_empty(), "{detail}");

    // Existing 400 behavior is untouched: a negative amount is still a plain validation error.
    let (s, b) = app
        .call(
            Method::POST,
            &path,
            Some(&org.token),
            Some(rate(
                json!({ "fee_code": "TRANSPORT", "amount": "-1.00", "charge_type": "EXACT" }),
            )),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["error"]["code"], "VALIDATION_ERROR");

    // Valid EXACT and CAP are accepted and echoed with their types.
    let v = ok(
        &app,
        &org,
        &path,
        rate(json!({ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": "CAP" })),
    )
    .await;
    assert_eq!(fee_of(&v, "TRANSPORT")["charge_type"], "CAP");
}

#[tokio::test]
async fn classification_is_one_time_validated_and_audited() {
    let app = test_app().await;
    let org = app.signup_org("fee-classify").await;
    let s = single(&app, &org, "k", Kind::Unclassified, "500.00").await;

    // Missing or invalid body types: application 422, nothing changes.
    for (label, body) in [
        ("empty body object", json!({})),
        ("null", json!({ "charge_type": null })),
        ("lowercase", json!({ "charge_type": "cap" })),
        ("unknown", json!({ "charge_type": "MAX" })),
        ("number", json!({ "charge_type": 1 })),
    ] {
        let (st, b) = classify(&app, &org.token, &s.rate_id, "TRANSPORT", body).await;
        assert_422_envelope(st, &b, label);
    }
    assert_eq!(
        fee_type_in_db(&app, &org, &s.rate_id, "TRANSPORT").await,
        None
    );

    // Unknown fee code and unknown rate are 404.
    let (st, _) = classify(
        &app,
        &org.token,
        &s.rate_id,
        "NOPE",
        json!({ "charge_type": "EXACT" }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = classify(
        &app,
        &org.token,
        &Uuid::new_v4().to_string(),
        "TRANSPORT",
        json!({ "charge_type": "EXACT" }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // First valid classification wins and returns the contract detail with the type.
    let (st, detail) = classify(
        &app,
        &org.token,
        &s.rate_id,
        "TRANSPORT",
        json!({ "charge_type": "CAP" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{detail}");
    assert_eq!(fee_of(&detail, "TRANSPORT")["charge_type"], "CAP");
    assert_eq!(
        fee_type_in_db(&app, &org, &s.rate_id, "TRANSPORT")
            .await
            .as_deref(),
        Some("CAP")
    );

    // A classified fee is immutable: both a different and the same value are 409.
    for (label, t) in [("different", "EXACT"), ("same", "CAP")] {
        let (st, b) = classify(
            &app,
            &org.token,
            &s.rate_id,
            "TRANSPORT",
            json!({ "charge_type": t }),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT, "{label}: {b}");
        assert!(
            b["error"]["message"]
                .as_str()
                .unwrap()
                .contains("already classified as CAP"),
            "{b}"
        );
    }
    assert_eq!(
        fee_type_in_db(&app, &org, &s.rate_id, "TRANSPORT")
            .await
            .as_deref(),
        Some("CAP")
    );

    // The database trigger also refuses direct reclassification, even for the app role.
    {
        let id: Uuid = s.rate_id.parse().unwrap();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        let res = sqlx::query("UPDATE contract_authorized_fees SET charge_type = 'EXACT', charge_type_set_at = now() WHERE contract_rate_id = $1")
            .bind(id).execute(&mut *tx).await;
        assert!(res.is_err(), "trigger must block reclassification");
    }

    // Exactly one audit record, with old and new values, actor and time.
    let (_, audit) = app.get("/api/v1/audit", &org.token).await;
    let rows: Vec<&Value> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["action"] == "contract_authorized_fee.classify")
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "one audit record per classification: {audit}"
    );
    let a = rows[0];
    assert_eq!(a["resource_type"], "contract_authorized_fee");
    assert_eq!(a["before_state"]["charge_type"], Value::Null);
    assert_eq!(a["after_state"]["charge_type"], "CAP");
    assert_eq!(a["after_state"]["fee_code"], "TRANSPORT");
    assert_eq!(a["after_state"]["contract_rate_id"], s.rate_id);
    assert_eq!(a["after_state"]["classified_by"], org.user_id.to_string());
    assert!(a["after_state"]["classified_at"].is_string());
    assert_eq!(a["actor_user_id"], org.user_id.to_string());
    assert!(a["created_at"].is_string());
}

#[tokio::test]
async fn classification_enforces_permission_and_tenant_isolation() {
    let app = test_app().await;
    let org = app.signup_org("fee-rbac").await;
    let other = app.signup_org("fee-rbac-other").await;
    let s = single(&app, &org, "r", Kind::Unclassified, "500.00").await;
    let body = json!({ "charge_type": "EXACT" });

    // Unauthenticated.
    let (st, _) = app
        .call(
            Method::PUT,
            &format!("/api/v1/rates/{}/fees/TRANSPORT/type", s.rate_id),
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A REVIEWER cannot manage data.
    let email = format!("reviewer-{}@test.invalid", Uuid::new_v4());
    let (st, _) = app
        .post("/api/v1/members", &org.token, json!({ "email": email, "password": "a-sufficiently-long-password", "role": "REVIEWER" }))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (_, login) = app
        .call(
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": email, "password": "a-sufficiently-long-password" })),
        )
        .await;
    let reviewer = login["session_token"].as_str().unwrap().to_string();
    let (st, _) = classify(&app, &reviewer, &s.rate_id, "TRANSPORT", body.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Another tenant cannot see or change the fee: indistinguishable from "not found".
    let (st, _) = classify(&app, &other.token, &s.rate_id, "TRANSPORT", body.clone()).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // None of the refused attempts changed anything, and no audit row was written for them.
    assert_eq!(
        fee_type_in_db(&app, &org, &s.rate_id, "TRANSPORT").await,
        None
    );
    let (_, audit) = app.get("/api/v1/audit", &org.token).await;
    assert!(audit
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["action"] != "contract_authorized_fee.classify"));
    let (_, other_audit) = app.get("/api/v1/audit", &other.token).await;
    assert!(other_audit
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["action"] != "contract_authorized_fee.classify"));

    // The owner can, and the other tenant's contract detail never exposes this org's fees.
    let (st, detail) = classify(&app, &org.token, &s.rate_id, "TRANSPORT", body).await;
    assert_eq!(st, StatusCode::OK, "{detail}");
    let (st, _) = app
        .get(
            &format!("/api/v1/contracts/{}", s.contract_id),
            &other.token,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------------
// classification -> worker re-reconciliation, history untouched, evidence timing pinned
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn classifying_a_fee_triggers_exactly_one_rereconciliation_and_keeps_history() {
    let app = test_app().await;
    let org = app.signup_org("fee-worker").await;
    let other = app.signup_org("fee-worker-other").await;
    let s = single(&app, &org, "wk", Kind::Unclassified, "600.00").await;
    // Another tenant with its own unclassified fee must not be touched by this classification.
    let o = single(&app, &other, "wk-o", Kind::Unclassified, "600.00").await;

    let first = run(&app, &org, &s.invoice_id).await;
    let other_first = run(&app, &other, &o.invoice_id).await;
    assert_eq!(first["pricing_status"], "PARTIAL");
    let rec1 = first["reconciliation_id"].as_str().unwrap().to_string();
    let d1 = discrepancy_id(&first, "RULE_12_UNCLASSIFIED_FEE_TYPE");

    // History to preserve: the reconciliation row and an evidence snapshot taken while unclassified.
    let (_, rec1_before) = app
        .get(&format!("/api/v1/reconciliations/{rec1}"), &org.token)
        .await;
    let ev1 = ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{d1}/evidence"),
        json!({}),
    )
    .await;
    assert_eq!(fee_of_evidence(&ev1)["charge_type"], Value::Null);
    let pdf1 = fetch_pdf(&app, &org, &d1).await;
    assert!(
        pdf1.contains("TRANSPORT - $500.00 [unclassified: type not set]"),
        "{pdf1}"
    );

    // Nothing is stale yet: the worker leaves the invoice alone.
    drain_worker(&app, &org).await;
    let count_sql = "SELECT count(*) FROM reconciliations WHERE invoice_id = $1";
    let before = scalar_i64(&app, &org, count_sql, &s.invoice_id).await;
    let before_other = scalar_i64(&app, &other, count_sql, &o.invoice_id).await;
    assert_eq!(before, 1, "an unchanged invoice must not be re-run");

    // Classify, then the worker re-reconciles exactly once.
    let (st, _) = classify(
        &app,
        &org.token,
        &s.rate_id,
        "TRANSPORT",
        json!({ "charge_type": "EXACT" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    drain_worker(&app, &org).await;
    assert_eq!(
        scalar_i64(&app, &org, count_sql, &s.invoice_id).await,
        2,
        "exactly one new reconciliation"
    );
    let (_, inv) = app
        .get(&format!("/api/v1/invoices/{}", s.invoice_id), &org.token)
        .await;
    let latest = &inv["latest_reconciliation"];
    assert_ne!(latest["id"].as_str().unwrap(), rec1);
    assert_eq!(
        latest["pricing_status"], "COMPLETE",
        "classification resolves the fee: {inv}"
    );

    // A second worker pass does not churn the queue.
    drain_worker(&app, &org).await;
    assert_eq!(
        scalar_i64(&app, &org, count_sql, &s.invoice_id).await,
        2,
        "no churn on the second pass"
    );

    // Other tenant untouched by this org's classification and worker runs.
    assert_eq!(
        scalar_i64(&app, &other, count_sql, &o.invoice_id).await,
        before_other
    );
    let _ = other_first;

    // Historical reconciliation and evidence are exactly as they were.
    let (_, rec1_after) = app
        .get(&format!("/api/v1/reconciliations/{rec1}"), &org.token)
        .await;
    assert_eq!(
        rec1_after, rec1_before,
        "the earlier reconciliation is never rewritten"
    );
    assert_eq!(rec1_after["pricing_status"], "PARTIAL");
    let (st, latest_ev) = app
        .get(&format!("/api/v1/discrepancies/{d1}/evidence"), &org.token)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(latest_ev["id"], ev1["id"]);
    assert_eq!(
        latest_ev["content"], ev1["content"],
        "stored snapshot is immutable"
    );
    assert_eq!(fee_of_evidence(&latest_ev)["charge_type"], Value::Null);
    assert_eq!(
        fetch_pdf(&app, &org, &d1).await,
        pdf1,
        "PDF renders from the stored snapshot"
    );

    // PINNED known behavior (timing mismatch): evidence is built from the live fee row, so a NEW
    // snapshot for a finding of the OLD reconciliation shows the type assigned afterwards next to
    // that reconciliation's trace, which still says the type was not set. charge_type_set_at is
    // deliberately not part of evidence. If this changes, it is a deliberate product decision.
    let ev2 = ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{d1}/evidence"),
        json!({}),
    )
    .await;
    assert_ne!(ev2["id"], ev1["id"]);
    assert_eq!(fee_of_evidence(&ev2)["charge_type"], "EXACT");
    assert!(fee_of_evidence(&ev2).get("charge_type_set_at").is_none());
    assert_eq!(
        ev2["content"]["reconciliation"]["pricing_status"],
        "PARTIAL"
    );
    let trace = ev2["content"]["reconciliation"]["calculation_trace"].to_string();
    assert!(
        trace.contains("charge type (exact or cap) is not set"),
        "{trace}"
    );
    assert!(fetch_pdf(&app, &org, &d1)
        .await
        .contains("TRANSPORT - $500.00 [exact charge]"));
    assert_eq!(
        scalar_i64(
            &app,
            &org,
            "SELECT count(*) FROM evidence_packages WHERE discrepancy_id = $1",
            &d1
        )
        .await,
        2,
        "the first snapshot is kept"
    );
}

fn fee_of_evidence(ev: &Value) -> &Value {
    &ev["content"]["contract_rate"]["authorized_fees"][0]
}

async fn fetch_pdf(app: &TestApp, org: &TestOrg, discrepancy_id: &str) -> String {
    let (st, headers, bytes) = app
        .call_raw(
            Method::GET,
            &format!("/api/v1/discrepancies/{discrepancy_id}/evidence/pdf"),
            Some(&org.token),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/pdf");
    assert!(bytes.starts_with(b"%PDF"));
    pdf_text(&bytes)
}

// ---------------------------------------------------------------------------------------------
// evidence JSON, stored snapshot, latest_evidence and PDF agree for EXACT, CAP and null
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn evidence_json_snapshot_and_pdf_agree_for_exact_cap_and_unclassified() {
    let app = test_app().await;
    let org = app.signup_org("fee-evidence").await;
    let _ = have_pdftotext(); // PDF text is asserted from the uncompressed content stream.

    let cases: [(Kind, &str, &str, Value, &str); 3] = [
        (
            Kind::Exact,
            "ex",
            "RULE_04_UNAUTHORIZED_FEE",
            json!("EXACT"),
            "TRANSPORT - $500.00 [exact charge]",
        ),
        (
            Kind::Cap,
            "cp",
            "RULE_04_UNAUTHORIZED_FEE",
            json!("CAP"),
            "TRANSPORT - $500.00 [cap: maximum allowed]",
        ),
        (
            Kind::Unclassified,
            "un",
            "RULE_12_UNCLASSIFIED_FEE_TYPE",
            Value::Null,
            "TRANSPORT - $500.00 [unclassified: type not set]",
        ),
    ];
    for (kind, tag, rule, expected_type, pdf_line) in cases {
        let s = single(&app, &org, tag, kind, "600.00").await;
        let r = run(&app, &org, &s.invoice_id).await;
        let did = discrepancy_id(&r, rule);

        let ev = ok(
            &app,
            &org,
            &format!("/api/v1/discrepancies/{did}/evidence"),
            json!({}),
        )
        .await;
        let fee = fee_of_evidence(&ev);
        assert_eq!(fee["fee_code"], "TRANSPORT");
        assert_eq!(
            fee.get("charge_type"),
            Some(&expected_type),
            "{kind:?}: the key must be present (null when unclassified): {fee}"
        );
        money(&fee["amount"], "500");

        let (st, latest) = app
            .get(&format!("/api/v1/discrepancies/{did}/evidence"), &org.token)
            .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(latest["id"], ev["id"]);
        assert_eq!(
            latest["content"], ev["content"],
            "latest_evidence returns the stored snapshot"
        );

        let pdf = fetch_pdf(&app, &org, &did).await;
        assert!(
            pdf.contains(pdf_line),
            "{kind:?}: PDF must say {pdf_line:?}"
        );
        for other in [
            "[exact charge]",
            "[cap: maximum allowed]",
            "[unclassified: type not set]",
        ] {
            if !pdf_line.contains(other) {
                assert!(!pdf.contains(other), "{kind:?}: PDF must not show {other}");
            }
        }
    }
}

#[tokio::test]
async fn multi_rate_invoices_carry_each_rates_fee_types_into_evidence_without_a_parent_rate() {
    // When several rates apply the reconciliation has no single parent rate and evidence never
    // invents one (`contract_rate` stays null). Evidence and the PDF instead list every applicable
    // rate with its authorized fees and their stored types (see tests/evidence_multi_rate.rs for the
    // full matrix). This test used to pin the OPPOSITE: that the PDF printed no fee types here.
    let app = test_app().await;
    let org = app.signup_org("fee-multi").await;
    let (vendor_id, contract_id) = vendor_contract(&app, &org, "mr").await;
    let fee = json!([{ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": "EXACT" }]);
    make_rate(&app, &org, &contract_id, "drum", "400.00", fee.clone()).await;
    make_rate(&app, &org, &contract_id, "ton", "1000.00", fee).await;
    let inv = ok(&app, &org, "/api/v1/invoices", json!({
        "vendor_id": vendor_id, "invoice_number": "INV-mr", "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003",
        "total": "5600.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Ton disposal", "quantity": "1", "unit": "ton", "unit_price": "1000.00", "line_total": "1000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "600.00", "line_total": "600.00" }]
    })).await;
    let r = run(&app, &org, inv["id"].as_str().unwrap()).await;
    assert_eq!(r["pricing_status"], "COMPLETE", "{r}");
    let f = findings(&r, "RULE_04_UNAUTHORIZED_FEE");
    assert_eq!(f.len(), 1, "{r}");
    money(&f[0]["amount"], "100");
    assert_bill_invariant(&r, 3);

    let did = discrepancy_id(&r, "RULE_04_UNAUTHORIZED_FEE");
    let ev = ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/evidence"),
        json!({}),
    )
    .await;
    assert!(
        ev["content"]["contract_rate"].is_null(),
        "no single parent rate: {}",
        ev["content"]["contract_rate"]
    );
    assert!(ev["content"]["priced_lines"].as_array().unwrap().len() >= 2);
    // Both applicable rates are listed, each with the EXACT fee it authorizes.
    let rates = ev["content"]["applicable_rates"].as_array().unwrap();
    assert_eq!(rates.len(), 2);
    for rate in rates {
        assert_eq!(rate["authorized_fees"][0]["charge_type"], "EXACT", "{rate}");
    }
    let pdf = fetch_pdf(&app, &org, &did).await;
    assert!(pdf.contains("More than one contract rate applied"));
    assert!(pdf.contains("Authorized fee: TRANSPORT - $500.00 [exact charge]"));
    assert!(!pdf.contains("No contract rate covered"));
}

// ---------------------------------------------------------------------------------------------
// bill-level RULE_13 guard
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn complete_results_reconcile_at_bill_level_and_inconsistent_totals_are_partial() {
    let app = test_app().await;
    let org = app.signup_org("fee-rule13").await;
    let (vendor_id, contract_id) = vendor_contract(&app, &org, "r13").await;
    make_rate(
        &app,
        &org,
        &contract_id,
        "drum",
        "400.00",
        json!([{ "fee_code": "TRANSPORT", "amount": "500.00", "charge_type": "EXACT" }]),
    )
    .await;
    let vendor: Uuid = vendor_id.parse().unwrap();

    // The API refuses inconsistent totals, so materially inconsistent invoices are inserted with
    // SQL as the app role. Lines: 10 x $400.00 + TRANSPORT $500.00 = $4,500.00 (2 lines).
    // (header total, expected: stays COMPLETE?)
    for (n, (total, stays_complete)) in [
        ("4500.00", true),
        ("4500.01", true),
        ("4500.03", true),
        ("4500.04", false),
        ("4550.00", false),
        ("4450.00", false),
    ]
    .into_iter()
    .enumerate()
    {
        let inv = Uuid::new_v4();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        sqlx::query("INSERT INTO invoices (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total) VALUES ($1, $2, $3, $4, '2026-03-30', $6::date, 'F003', $5::numeric)")
            .bind(inv).bind(org.organization_id).bind(vendor).bind(format!("INV-R13-{n}")).bind(total)
            // A distinct service date per invoice: identical lines on the same date would
            // (correctly) be flagged as duplicate charges and distort the expected amount.
            .bind(format!("2026-03-{:02}", 10 + n)).execute(&mut *tx).await.unwrap();
        for (desc, fee, qty, unit, price, line_total) in [
            ("Drum disposal", None, "10", "drum", "400.00", "4000.00"),
            (
                "Transportation",
                Some("TRANSPORT"),
                "1",
                "flat",
                "500.00",
                "500.00",
            ),
        ] {
            sqlx::query("INSERT INTO invoice_line_items (invoice_id, organization_id, description, fee_code, quantity, unit, unit_price, line_total) VALUES ($1, $2, $3, $4, $5::numeric, $6, $7::numeric, $8::numeric)")
                .bind(inv).bind(org.organization_id).bind(desc).bind(fee).bind(qty).bind(unit).bind(price).bind(line_total).execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();

        let r = run(&app, &org, &inv.to_string()).await;
        let ctx = format!("header total {total}: {r}");
        let rule13 = findings(&r, "RULE_13_UNRECONCILED_BILL_TOTAL");
        if stays_complete {
            assert_eq!(r["pricing_status"], "COMPLETE", "{ctx}");
            assert!(rule13.is_empty(), "{ctx}");
            assert_eq!(dec(&r["expected_amount"]), Decimal::from(4500), "{ctx}");
            assert_bill_invariant(&r, 2);
        } else {
            assert_eq!(
                r["pricing_status"], "PARTIAL",
                "never silently COMPLETE: {ctx}"
            );
            assert_eq!(rule13.len(), 1, "{ctx}");
            money(&rule13[0]["amount"], "0");
            assert!(
                r["expected_amount"].is_null() && r["discrepancy_amount"].is_null(),
                "{ctx}"
            );
            assert_eq!(
                dec(&r["partial_expected_amount"]),
                Decimal::from(4500),
                "{ctx}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// sign convention: negative findings are informational, never recoverable
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn underbilled_findings_are_excluded_from_totals_and_cannot_be_disputed() {
    let app = test_app().await;
    let org = app.signup_org("fee-sign").await;
    // EXACT authorized $500 but billed $400: finding = billed - expected = -$100.
    let s = single(&app, &org, "neg", Kind::Exact, "400.00").await;
    let r = run(&app, &org, &s.invoice_id).await;
    let f = findings(&r, "RULE_04_UNAUTHORIZED_FEE");
    assert_eq!(f.len(), 1, "{r}");
    money(&f[0]["amount"], "-100");
    let did = discrepancy_id(&r, "RULE_04_UNAUTHORIZED_FEE");

    let (_, dash) = app.get("/api/v1/dashboard", &org.token).await;
    money(&dash["potential_discrepancy_total"], "0");
    money(&dash["confirmed_discrepancy_total"], "0");

    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "confirm" }),
    )
    .await;
    let (_, dash) = app.get("/api/v1/dashboard", &org.token).await;
    money(&dash["confirmed_discrepancy_total"], "0");

    let (st, b) = app
        .post(
            "/api/v1/disputes",
            &org.token,
            json!({ "discrepancy_id": did }),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "a non-positive finding cannot be disputed: {b}"
    );
}

#[tokio::test]
async fn overbilled_findings_count_in_totals_and_can_be_disputed() {
    let app = test_app().await;
    let org = app.signup_org("fee-sign-pos").await;
    let s = single(&app, &org, "pos", Kind::Cap, "600.00").await;
    let r = run(&app, &org, &s.invoice_id).await;
    let did = discrepancy_id(&r, "RULE_04_UNAUTHORIZED_FEE");
    let (_, dash) = app.get("/api/v1/dashboard", &org.token).await;
    money(&dash["potential_discrepancy_total"], "100");
    ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "confirm" }),
    )
    .await;
    let d = ok(
        &app,
        &org,
        "/api/v1/disputes",
        json!({ "discrepancy_id": did }),
    )
    .await;
    money(&d["disputed_amount"], "100");
}
