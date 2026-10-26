//! Regression tests for the classification/reconciliation race.
//!
//! The race: `charge_type_set_at` is `now()`, which PostgreSQL fixes at *transaction start*. A
//! classification that starts before a reconciliation but commits after that reconciliation read the
//! fee has `charge_type_set_at < reconciliation.created_at`, so a worker that only compares those two
//! timestamps believes the reconciliation is current and the invoice silently stays PARTIAL.
//!
//! The interleaving is made deterministic without sleeps by holding a row lock on the fee:
//!   1. the test locks the fee row (`SELECT ... FOR UPDATE`) in its own transaction;
//!   2. the real classify endpoint is called; its transaction starts (so its `now()` is fixed) and then
//!      blocks on that row lock; the test waits until PostgreSQL reports it blocked;
//!   3. a reconciliation starts later and reads the fee while it is still unclassified;
//!   4. the lock is released, the classification commits, and the reconciliation is committed.
//! Every assertion below is then about what the system does with that state.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{test_app, TestApp, TestOrg};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;
use xflow_backend::reconciliation::service;

/// The interleaving tests wait for "a classify request is blocked on a fee row lock"; running them
/// one at a time keeps that observation unambiguous.
static SERIAL: Mutex<()> = Mutex::const_new(());

// ---------------------------------------------------------------------------------------------
// helpers (kept local so this file is self-contained)
// ---------------------------------------------------------------------------------------------

async fn ok(app: &TestApp, org: &TestOrg, path: &str, body: Value) -> Value {
    let (s, v) = app.post(path, &org.token, body).await;
    assert!(s.is_success(), "POST {path} -> {s}: {v}");
    v
}

struct Scn {
    rate_id: String,
    invoice_id: String,
}

/// One drum rate ($400, window from 2026-01-01) whose TRANSPORT fee ($500) is a *legacy* fee with no
/// charge type (only reachable through SQL), plus a clean manifest/ticket and an invoice billing
/// 10 drums and TRANSPORT $500. Optionally billing/authorizing a second fee `ENV_FEE`.
async fn legacy_scenario(app: &TestApp, org: &TestOrg, tag: &str) -> Scn {
    legacy_scenario_with(app, org, tag, false).await
}

/// `with_env_fee` also authorizes (as a legacy unclassified fee) and bills ENV_FEE $200.
async fn legacy_scenario_with(app: &TestApp, org: &TestOrg, tag: &str, with_env_fee: bool) -> Scn {
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
    let rate = ok(
        app,
        org,
        &format!("/api/v1/contracts/{contract_id}/rates"),
        json!({ "waste_stream": "F003", "unit": "drum", "unit_rate": "400.00", "effective_from": "2026-01-01", "authorized_fees": [] }),
    )
    .await;
    let rate_id = rate["rates"][0]["id"].as_str().unwrap().to_string();
    add_legacy_fee(app, org, &rate_id, "TRANSPORT", "500.00").await;
    if with_env_fee {
        add_legacy_fee(app, org, &rate_id, "ENV_FEE", "200.00").await;
    }
    let manifest = ok(app, org, "/api/v1/manifests", json!({ "manifest_number": format!("M-{tag}"), "waste_stream": "F003", "container_count": 10, "quantity": "10", "unit": "drum", "shipment_date": "2026-03-09" })).await;
    let mid = manifest["id"].as_str().unwrap().to_string();
    ok(app, org, "/api/v1/weight-tickets", json!({ "ticket_number": format!("T-{tag}"), "manifest_id": mid, "net_weight": "10", "unit": "drum", "ticket_date": "2026-03-10" })).await;
    let invoice = ok(app, org, "/api/v1/invoices", json!({
        "vendor_id": vendor_id, "invoice_number": format!("INV-{tag}"), "invoice_date": "2026-03-15", "service_date": "2026-03-10",
        "waste_stream": "F003", "manifest_id": mid, "total": if with_env_fee { "4700.00" } else { "4500.00" },
        "line_items": invoice_lines(with_env_fee)
    })).await;
    Scn {
        rate_id,
        invoice_id: invoice["id"].as_str().unwrap().to_string(),
    }
}

fn invoice_lines(with_env_fee: bool) -> Value {
    let mut lines = vec![
        json!({ "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" }),
        json!({ "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }),
    ];
    if with_env_fee {
        lines.push(json!({ "description": "Environmental fee", "fee_code": "ENV_FEE", "quantity": "1", "unit": "flat", "unit_price": "200.00", "line_total": "200.00" }));
    }
    Value::Array(lines)
}

async fn add_legacy_fee(app: &TestApp, org: &TestOrg, rate_id: &str, code: &str, amount: &str) {
    let rate: Uuid = rate_id.parse().unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query("INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, amount) VALUES ($1, $2, $3::numeric)")
        .bind(rate).bind(code).bind(amount).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

async fn count(app: &TestApp, org: &TestOrg, sql: &str, id: &str) -> i64 {
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

async fn recon_count(app: &TestApp, org: &TestOrg, invoice_id: &str) -> i64 {
    count(
        app,
        org,
        "SELECT count(*) FROM reconciliations WHERE invoice_id = $1",
        invoice_id,
    )
    .await
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

async fn latest(app: &TestApp, org: &TestOrg, invoice_id: &str) -> Value {
    let (_, inv) = app
        .get(&format!("/api/v1/invoices/{invoice_id}"), &org.token)
        .await;
    inv["latest_reconciliation"].clone()
}

/// Run the monitoring worker for `org` and return only once none of THAT organization's jobs is still
/// pending or running.
///
/// The job queue is global: `run_one_job` claims the oldest job of ANY organization, and every test in
/// this target (and any other process using the database) drains it concurrently. Another test's drain
/// can therefore claim, and be part-way through, THIS test's job. "Nothing left to claim" then does not
/// mean "my job is done", and returning at that point lets the caller assert before the work has
/// finished. Waiting for the organization's own jobs is correct whoever ends up executing them.
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
        tokio::time::sleep(Duration::from_millis(10)).await;
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

/// Block until PostgreSQL reports a session waiting on a fee-row lock taken by the classify handler.
async fn wait_until_classify_is_blocked(pool: &PgPool) {
    for _ in 0..1000 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE '%FOR UPDATE OF f%'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if n >= 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the classify request never blocked on the fee row lock");
}

#[derive(Clone, Copy, Debug)]
enum Reconciliation {
    /// A reconciliation (the manual-run endpoint) starts after the classification's transaction has
    /// started, reads the unclassified fee, and commits before the classification commits.
    CommitsBeforeClassification,
    /// A reconciliation transaction is still OPEN (read done, not yet committed) when the
    /// classification commits, and commits afterwards.
    StillOpenWhenClassificationCommits,
}

struct Interleaved {
    first_reconciliation_id: String,
    first_status: String,
    first_has_rule12: bool,
}

/// Drive the interleaving described in the module docs against the REAL classify endpoint.
async fn interleave(
    app: &Arc<TestApp>,
    org: &TestOrg,
    s: &Scn,
    mode: Reconciliation,
) -> Interleaved {
    // 1. hold the fee row so the classify request blocks right after its transaction starts
    let mut hold = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let rate: Uuid = s.rate_id.parse().unwrap();
    sqlx::query("SELECT id FROM contract_authorized_fees WHERE contract_rate_id = $1 AND fee_code = 'TRANSPORT' FOR UPDATE")
        .bind(rate).fetch_one(&mut *hold).await.unwrap();

    // 2. the real endpoint starts, fixes its now(), and blocks
    let classify = {
        let app = Arc::clone(app);
        let token = org.token.clone();
        let path = format!("/api/v1/rates/{}/fees/TRANSPORT/type", s.rate_id);
        tokio::spawn(async move {
            app.call(
                Method::PUT,
                &path,
                Some(&token),
                Some(json!({ "charge_type": "EXACT" })),
            )
            .await
        })
    };
    wait_until_classify_is_blocked(&app.pool).await;

    // 3. a LATER reconciliation reads the fee while it is still unclassified
    let invoice: Uuid = s.invoice_id.parse().unwrap();
    let (first_id, status, rule12, open_tx) = match mode {
        Reconciliation::CommitsBeforeClassification => {
            let r = run(app, org, &s.invoice_id).await;
            let has12 = r["discrepancies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["rule_id"] == "RULE_12_UNCLASSIFIED_FEE_TYPE");
            (
                r["reconciliation_id"].as_str().unwrap().to_string(),
                r["pricing_status"].as_str().unwrap().to_string(),
                has12,
                None,
            )
        }
        Reconciliation::StillOpenWhenClassificationCommits => {
            let mut rtx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
                .await
                .unwrap();
            let outcome = service::run_for_invoice(&mut rtx, org.organization_id, invoice)
                .await
                .unwrap();
            let has12 = outcome
                .result
                .discrepancies
                .iter()
                .any(|d| d.rule_id == "RULE_12_UNCLASSIFIED_FEE_TYPE");
            (
                outcome.reconciliation_id.to_string(),
                format!("{:?}", outcome.result.pricing_status).to_uppercase(),
                has12,
                Some(rtx),
            )
        }
    };

    // 4. release the lock: the classification now commits (while the reconciliation tx may still be open)
    hold.rollback().await.unwrap();
    let (status_code, body) = classify.await.unwrap();
    assert_eq!(
        status_code,
        StatusCode::OK,
        "classification must succeed: {body}"
    );
    if let Some(rtx) = open_tx {
        rtx.commit().await.unwrap();
    }
    Interleaved {
        first_reconciliation_id: first_id,
        first_status: status,
        first_has_rule12: rule12,
    }
}

// ---------------------------------------------------------------------------------------------
// the regression tests
// ---------------------------------------------------------------------------------------------

async fn assert_classification_during_reconciliation_is_not_missed(mode: Reconciliation) {
    let _one_at_a_time = SERIAL.lock().await;
    let app = Arc::new(test_app().await);
    let org = app
        .signup_org(&format!("race-{mode:?}").to_lowercase())
        .await;
    let other = app
        .signup_org(&format!("race-other-{mode:?}").to_lowercase())
        .await;

    // Target invoice, plus an UNRELATED invoice (same tenant, other vendor, its own legacy fee) and an
    // invoice of ANOTHER tenant with the identical setup. All three start reconciled once, PARTIAL.
    let target = legacy_scenario(&app, &org, "target").await;
    let unrelated = legacy_scenario(&app, &org, "unrelated").await;
    let foreign = legacy_scenario(&app, &other, "foreign").await;
    for (o, s) in [(&org, &unrelated), (&other, &foreign)] {
        let r = run(&app, o, &s.invoice_id).await;
        assert_eq!(r["pricing_status"], "PARTIAL", "{r}");
    }

    // Historical evidence that must survive: a snapshot for the target's RULE_12 finding taken before.
    let before = run(&app, &org, &target.invoice_id).await;
    let d_idx = before["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .position(|d| d["rule_id"] == "RULE_12_UNCLASSIFIED_FEE_TYPE")
        .expect("RULE_12 finding");
    let discrepancy_id = before["discrepancy_ids"][d_idx]
        .as_str()
        .unwrap()
        .to_string();
    let evidence_before = ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{discrepancy_id}/evidence"),
        json!({}),
    )
    .await;
    let reconciliations_before_race = recon_count(&app, &org, &target.invoice_id).await;
    assert_eq!(reconciliations_before_race, 1);

    // ---- the interleaving ----
    let it = interleave(&app, &org, &target, mode).await;

    // The reconciliation really did read the fee BEFORE it was classified ...
    assert_eq!(it.first_status, "PARTIAL", "{mode:?}");
    assert!(
        it.first_has_rule12,
        "{mode:?}: the in-flight reconciliation must have seen the fee unclassified"
    );
    // ... and the timestamps are in the order that fools a timestamp-only comparison.
    let set_at_before_recon: bool = {
        let fee_rate: Uuid = target.rate_id.parse().unwrap();
        let rid: Uuid = it.first_reconciliation_id.parse().unwrap();
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
            .await
            .unwrap();
        sqlx::query_scalar(
            "SELECT (SELECT charge_type_set_at FROM contract_authorized_fees WHERE contract_rate_id = $1 AND fee_code = 'TRANSPORT') < (SELECT created_at FROM reconciliations WHERE id = $2)",
        )
        .bind(fee_rate).bind(rid).fetch_one(&mut *tx).await.unwrap()
    };
    assert!(
        set_at_before_recon,
        "{mode:?}: the interleaving must produce charge_type_set_at < reconciliation.created_at"
    );
    assert_eq!(count(&app, &org, "SELECT count(*) FROM contract_authorized_fees WHERE contract_rate_id = $1 AND charge_type = 'EXACT'", &target.rate_id).await, 1);

    // ---- the system must still notice ----
    let recs_after_race = recon_count(&app, &org, &target.invoice_id).await;
    drain_worker(&app, &org).await;
    let recs_after_worker = recon_count(&app, &org, &target.invoice_id).await;
    assert_eq!(
        recs_after_worker - recs_after_race, 1,
        "{mode:?}: exactly ONE re-reconciliation must follow a classification that raced a reconciliation (was {recs_after_race}, now {recs_after_worker})"
    );
    let now = latest(&app, &org, &target.invoice_id).await;
    assert_eq!(
        now["pricing_status"], "COMPLETE",
        "{mode:?}: the invoice must reach its correct status: {now}"
    );
    assert_ne!(now["id"].as_str().unwrap(), it.first_reconciliation_id);
    assert_eq!(
        now["expected_amount"]
            .as_str()
            .map(|s| s.parse::<f64>().unwrap()),
        Some(4500.0),
        "{now}"
    );

    // A second worker pass creates nothing.
    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &target.invoice_id).await,
        recs_after_worker,
        "{mode:?}: no churn on the second pass"
    );

    // Unrelated invoice (same tenant) and the other tenant's invoice were not touched.
    assert_eq!(
        recon_count(&app, &org, &unrelated.invoice_id).await,
        1,
        "{mode:?}: unrelated invoice must not be re-reconciled"
    );
    assert_eq!(
        recon_count(&app, &other, &foreign.invoice_id).await,
        1,
        "{mode:?}: other tenant must not be touched"
    );

    // Audit: exactly one classification record, before NULL, after EXACT.
    let (_, audit) = app.get("/api/v1/audit", &org.token).await;
    let rows: Vec<&Value> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["action"] == "contract_authorized_fee.classify")
        .collect();
    assert_eq!(rows.len(), 1, "{audit}");
    assert_eq!(rows[0]["before_state"]["charge_type"], Value::Null);
    assert_eq!(rows[0]["after_state"]["charge_type"], "EXACT");

    // History: the first reconciliation and the earlier evidence snapshot are exactly as they were.
    let (_, first) = app
        .get(
            &format!("/api/v1/reconciliations/{}", it.first_reconciliation_id),
            &org.token,
        )
        .await;
    assert_eq!(first["pricing_status"], "PARTIAL");
    let (st, evidence_after) = app
        .get(
            &format!("/api/v1/discrepancies/{discrepancy_id}/evidence"),
            &org.token,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(evidence_after["id"], evidence_before["id"]);
    assert_eq!(
        evidence_after["content"], evidence_before["content"],
        "stored evidence snapshots are immutable"
    );
}

#[tokio::test]
async fn classification_committed_while_a_reconciliation_is_open_is_not_missed() {
    assert_classification_during_reconciliation_is_not_missed(
        Reconciliation::StillOpenWhenClassificationCommits,
    )
    .await;
}

#[tokio::test]
async fn classification_that_started_before_a_reconciliation_is_not_missed() {
    assert_classification_during_reconciliation_is_not_missed(
        Reconciliation::CommitsBeforeClassification,
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// scope, atomicity, concurrency and rollback of the durable re-reconciliation request
// ---------------------------------------------------------------------------------------------

async fn requests_for(app: &TestApp, org: &TestOrg, invoice_id: &str) -> i64 {
    count(
        app,
        org,
        "SELECT count(*) FROM invoice_reconciliation_requests WHERE invoice_id = $1",
        invoice_id,
    )
    .await
}

async fn classify(
    app: &TestApp,
    org: &TestOrg,
    rate_id: &str,
    code: &str,
    ctype: &str,
) -> (StatusCode, Value) {
    app.call(
        Method::PUT,
        &format!("/api/v1/rates/{rate_id}/fees/{code}/type"),
        Some(&org.token),
        Some(json!({ "charge_type": ctype })),
    )
    .await
}

#[tokio::test]
async fn requests_are_scoped_to_the_invoices_the_fee_can_price() {
    let app = test_app().await;
    let org = app.signup_org("race-scope").await;
    let other = app.signup_org("race-scope-other").await;

    let target = legacy_scenario(&app, &org, "scope-target").await; // vendor A, bills TRANSPORT
    let unrelated = legacy_scenario(&app, &org, "scope-unrelated").await; // vendor B, bills TRANSPORT
    let foreign = legacy_scenario(&app, &other, "scope-foreign").await; // another tenant, bills TRANSPORT

    // (a) a legacy fee the target invoice does NOT bill, and (b) a rate whose window does NOT cover
    // the invoice's service date but which also authorizes a TRANSPORT fee the invoice bills.
    add_legacy_fee(&app, &org, &target.rate_id, "FUEL", "75.00").await;
    let contract_id = {
        let (_, inv) = app
            .get(
                &format!("/api/v1/invoices/{}", target.invoice_id),
                &org.token,
            )
            .await;
        let vendor_id = inv["vendor_id"].as_str().unwrap().to_string();
        let (_, contracts) = app.get("/api/v1/contracts?limit=200", &org.token).await;
        contracts
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["vendor_id"] == vendor_id)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let expired = ok(&app, &org, &format!("/api/v1/contracts/{contract_id}/rates"), json!({
        "waste_stream": "F003", "unit": "ton", "unit_rate": "900.00", "effective_from": "2025-01-01", "effective_to": "2025-12-31", "authorized_fees": []
    })).await;
    let expired_rate = expired["rates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["unit"] == "ton")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    add_legacy_fee(&app, &org, &expired_rate, "TRANSPORT", "500.00").await;

    // Everything is reconciled AFTER the setup, so any later re-run is attributable to the classification.
    for (o, s) in [(&org, &target), (&org, &unrelated), (&other, &foreign)] {
        run(&app, o, &s.invoice_id).await;
    }
    let before = (
        recon_count(&app, &org, &target.invoice_id).await,
        recon_count(&app, &org, &unrelated.invoice_id).await,
        recon_count(&app, &other, &foreign.invoice_id).await,
    );

    // Classifying either of those fees must not ask for anything.
    assert_eq!(
        classify(&app, &org, &target.rate_id, "FUEL", "CAP").await.0,
        StatusCode::OK
    );
    assert_eq!(
        classify(&app, &org, &expired_rate, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );
    for (o, id) in [
        (&org, &target.invoice_id),
        (&org, &unrelated.invoice_id),
        (&other, &foreign.invoice_id),
    ] {
        assert_eq!(
            requests_for(&app, o, id).await,
            0,
            "no request may be created for an invoice the fee cannot price"
        );
    }
    drain_worker(&app, &org).await;
    drain_worker(&app, &other).await;
    let after = (
        recon_count(&app, &org, &target.invoice_id).await,
        recon_count(&app, &org, &unrelated.invoice_id).await,
        recon_count(&app, &other, &foreign.invoice_id).await,
    );
    assert_eq!(
        after, before,
        "classifying an unbilled fee, or a fee of an out-of-window rate, re-reconciles nothing"
    );

    // Classifying the fee that DOES price the target invoice requests exactly that invoice, in its tenant only.
    assert_eq!(
        classify(&app, &org, &target.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(requests_for(&app, &org, &target.invoice_id).await, 1);
    assert_eq!(
        requests_for(&app, &org, &unrelated.invoice_id).await,
        0,
        "other vendor, same tenant"
    );
    assert_eq!(
        requests_for(&app, &other, &foreign.invoice_id).await,
        0,
        "other tenant"
    );
    // RLS: the other tenant cannot even see this tenant's request.
    assert_eq!(requests_for(&app, &other, &target.invoice_id).await, 0);
    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &target.invoice_id).await,
        before.0 + 1
    );
    assert_eq!(
        recon_count(&app, &org, &unrelated.invoice_id).await,
        before.1
    );
    assert_eq!(
        requests_for(&app, &org, &target.invoice_id).await,
        0,
        "the request was consumed"
    );
}

#[tokio::test]
async fn rejected_classifications_create_no_requests_and_two_fees_cost_one_rerun() {
    let app = test_app().await;
    let org = app.signup_org("race-two-fees").await;
    let s = legacy_scenario_with(&app, &org, "two", true).await; // bills TRANSPORT and ENV_FEE, both unclassified
    run(&app, &org, &s.invoice_id).await;
    let base = recon_count(&app, &org, &s.invoice_id).await;

    // Refused attempts (unknown fee, bad type, unknown rate) leave nothing behind.
    assert_eq!(
        classify(&app, &org, &s.rate_id, "NOPE", "EXACT").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "MAX").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        classify(
            &app,
            &org,
            &Uuid::new_v4().to_string(),
            "TRANSPORT",
            "EXACT"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);

    // Two classifications -> two requests (append-only) -> still ONE re-reconciliation.
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 1);
    assert_eq!(
        classify(&app, &org, &s.rate_id, "ENV_FEE", "CAP").await.0,
        StatusCode::OK
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 2);
    // A repeated classification of an already-classified fee is a 409 and adds nothing.
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 2);

    drain_worker(&app, &org).await;
    assert_eq!(recon_count(&app, &org, &s.invoice_id).await, base + 1);
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);
    let now = latest(&app, &org, &s.invoice_id).await;
    assert_eq!(now["pricing_status"], "COMPLETE", "{now}");
    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &s.invoice_id).await,
        base + 1,
        "a second pass is a no-op"
    );
}

#[tokio::test]
async fn classification_and_its_request_are_atomic_when_the_request_cannot_be_written() {
    // Crash window between "fee updated" and "request written": the handler has already UPDATEd the fee
    // (uncommitted) and is blocked inserting the request (its foreign-key check on the invoice waits for
    // a lock the test holds). Terminating that backend must leave NO classification and NO request;
    // a retry then succeeds with exactly one of each.
    let _one_at_a_time = SERIAL.lock().await;
    let app = Arc::new(test_app().await);
    let org = app.signup_org("race-atomic").await;
    let s = legacy_scenario(&app, &org, "atomic").await;
    run(&app, &org, &s.invoice_id).await;
    let base = recon_count(&app, &org, &s.invoice_id).await;

    let invoice: Uuid = s.invoice_id.parse().unwrap();
    let mut hold = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM invoices WHERE id = $1 FOR UPDATE")
        .bind(invoice)
        .fetch_one(&mut *hold)
        .await
        .unwrap();

    let classify_task = {
        let app = Arc::clone(&app);
        let token = org.token.clone();
        let path = format!("/api/v1/rates/{}/fees/TRANSPORT/type", s.rate_id);
        tokio::spawn(async move {
            app.call(
                Method::PUT,
                &path,
                Some(&token),
                Some(json!({ "charge_type": "EXACT" })),
            )
            .await
        })
    };
    // wait for the handler to be stuck on the request insert, i.e. AFTER its fee UPDATE
    let mut terminated = 0i64;
    for _ in 0..1000 {
        terminated = sqlx::query_scalar(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE '%INSERT INTO invoice_reconciliation_requests%'",
        )
        .fetch_one(&app.pool)
        .await
        .unwrap();
        if terminated >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        terminated, 1,
        "the handler must have been blocked on the request insert and been terminated"
    );
    let (status, _) = classify_task.await.unwrap();
    assert!(status.is_server_error(), "a killed request fails: {status}");
    hold.rollback().await.unwrap();

    assert_eq!(count(&app, &org, "SELECT count(*) FROM contract_authorized_fees WHERE contract_rate_id = $1 AND charge_type IS NOT NULL", &s.rate_id).await, 0, "no half-applied classification");
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);
    assert_eq!(count(&app, &org, "SELECT count(*) FROM audit_logs WHERE resource_id IN (SELECT id FROM contract_authorized_fees WHERE contract_rate_id = $1)", &s.rate_id).await, 0);
    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &s.invoice_id).await,
        base,
        "nothing happened, so nothing is re-reconciled"
    );

    // The retry completes both halves together.
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 1);
    drain_worker(&app, &org).await;
    assert_eq!(recon_count(&app, &org, &s.invoice_id).await, base + 1);
    assert_eq!(
        latest(&app, &org, &s.invoice_id).await["pricing_status"],
        "COMPLETE"
    );
}

#[tokio::test]
async fn a_failed_reconciliation_does_not_consume_the_request() {
    let app = test_app().await;
    let org = app.signup_org("race-rollback").await;
    let s = legacy_scenario(&app, &org, "rollback").await;
    run(&app, &org, &s.invoice_id).await;
    let base = recon_count(&app, &org, &s.invoice_id).await;
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 1);

    // A reconciliation that consumes the request and then rolls back (crash, error) must restore it.
    let invoice: Uuid = s.invoice_id.parse().unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    service::run_for_invoice(&mut tx, org.organization_id, invoice)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        requests_for(&app, &org, &s.invoice_id).await,
        1,
        "the request survives a rolled-back reconciliation"
    );
    assert_eq!(recon_count(&app, &org, &s.invoice_id).await, base);

    drain_worker(&app, &org).await;
    assert_eq!(recon_count(&app, &org, &s.invoice_id).await, base + 1);
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);
}

#[tokio::test]
async fn a_manual_run_consumes_the_request_so_the_worker_does_not_repeat_it() {
    let app = test_app().await;
    let org = app.signup_org("race-manual").await;
    let s = legacy_scenario(&app, &org, "manual").await;
    run(&app, &org, &s.invoice_id).await;
    let base = recon_count(&app, &org, &s.invoice_id).await;
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );
    let r = run(&app, &org, &s.invoice_id).await;
    assert_eq!(r["pricing_status"], "COMPLETE", "{r}");
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);
    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &s.invoice_id).await,
        base + 1,
        "manual run + worker pass = one re-reconciliation in total"
    );
}

#[tokio::test]
async fn concurrent_worker_passes_neither_lose_nor_duplicate_the_reconciliation() {
    let app = test_app().await;
    let org = app.signup_org("race-concurrent").await;
    let s = legacy_scenario(&app, &org, "concurrent").await;
    run(&app, &org, &s.invoice_id).await;
    let base = recon_count(&app, &org, &s.invoice_id).await;
    assert_eq!(
        classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT")
            .await
            .0,
        StatusCode::OK
    );

    // Three passes for the same tenant at the same time (several replicas, manual enqueue, etc.).
    let pool = app.pool.clone();
    let org_id = org.organization_id;
    let (a, b, c) = tokio::join!(
        xflow_backend::workers::run_reconcile_pending(&pool, org_id, false),
        xflow_backend::workers::run_reconcile_pending(&pool, org_id, false),
        xflow_backend::workers::run_reconcile_pending(&pool, org_id, false),
    );
    let processed = a.unwrap() + b.unwrap() + c.unwrap();
    assert_eq!(
        processed, 1,
        "exactly one of the concurrent passes does the work"
    );
    assert_eq!(
        recon_count(&app, &org, &s.invoice_id).await,
        base + 1,
        "exactly one re-reconciliation, none lost, none duplicated"
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);
    assert_eq!(
        latest(&app, &org, &s.invoice_id).await["pricing_status"],
        "COMPLETE"
    );
    drain_worker(&app, &org).await;
    assert_eq!(recon_count(&app, &org, &s.invoice_id).await, base + 1);
}

#[tokio::test]
async fn classification_committed_between_a_reconciliations_reads_and_its_commit_is_not_missed() {
    // The classification commits INSIDE the reconciliation: after it has read the (unclassified) fee and
    // before it finishes. The reconciliation is held at its insert by a lock on the rate row (the
    // reconciliation row references it), which the classify endpoint does not need.
    // Here the classification starts AFTER the reconciliation, so a timestamp comparison would not be
    // fooled; this guards the other half of the design: pending requests must be consumed at the START
    // of a reconciliation, never at its end, or a request committed mid-flight would be deleted unseen.
    let _one_at_a_time = SERIAL.lock().await;
    let app = Arc::new(test_app().await);
    let org = app.signup_org("race-inside").await;
    let s = legacy_scenario(&app, &org, "inside").await;
    run(&app, &org, &s.invoice_id).await;
    let base = recon_count(&app, &org, &s.invoice_id).await;

    let rate: Uuid = s.rate_id.parse().unwrap();
    let mut hold = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM contract_rates WHERE id = $1 FOR UPDATE")
        .bind(rate)
        .fetch_one(&mut *hold)
        .await
        .unwrap();

    let reconcile = {
        let pool = app.pool.clone();
        let (org_id, invoice) = (org.organization_id, s.invoice_id.parse::<Uuid>().unwrap());
        tokio::spawn(async move {
            let mut tx = xflow_backend::tenants::begin_scoped_tx(&pool, org_id)
                .await
                .unwrap();
            let outcome = service::run_for_invoice(&mut tx, org_id, invoice)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            outcome
                .result
                .discrepancies
                .iter()
                .any(|d| d.rule_id == "RULE_12_UNCLASSIFIED_FEE_TYPE")
        })
    };
    // wait until the reconciliation has done all its reads and is stuck inserting its row
    for i in 0..1000 {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE '%INSERT INTO reconciliations%'")
            .fetch_one(&app.pool).await.unwrap();
        if n >= 1 {
            break;
        }
        assert!(i < 999, "the reconciliation never blocked at its insert");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // the classification commits now, while the reconciliation is in flight
    let (st, body) = classify(&app, &org, &s.rate_id, "TRANSPORT", "EXACT").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 1);

    hold.rollback().await.unwrap();
    assert!(
        reconcile.await.unwrap(),
        "the in-flight reconciliation read the fee before it was classified"
    );
    assert_eq!(recon_count(&app, &org, &s.invoice_id).await, base + 1);
    assert_eq!(
        latest(&app, &org, &s.invoice_id).await["pricing_status"],
        "PARTIAL"
    );
    assert_eq!(
        requests_for(&app, &org, &s.invoice_id).await,
        1,
        "the request committed mid-flight must survive that reconciliation"
    );

    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &s.invoice_id).await,
        base + 2,
        "exactly one more reconciliation"
    );
    assert_eq!(
        latest(&app, &org, &s.invoice_id).await["pricing_status"],
        "COMPLETE"
    );
    assert_eq!(requests_for(&app, &org, &s.invoice_id).await, 0);
    drain_worker(&app, &org).await;
    assert_eq!(
        recon_count(&app, &org, &s.invoice_id).await,
        base + 2,
        "no churn"
    );
}
