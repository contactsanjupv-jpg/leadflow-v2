//! Persistence, reload, tenant isolation, and multi-unit audit for reconciliation_line_rates.
//! Requires a migrated Postgres (through 0010) and the same DATABASE_URL as other integration tests.

mod common;

use axum::http::StatusCode;
use common::{test_app, TestApp, TestOrg};
use serde_json::{json, Value};
use uuid::Uuid;

struct MultiUnitSeed {
    invoice_id: Uuid,
    disposal_rate_id: Uuid,
    transport_rate_id: Uuid,
    handling_rate_id: Uuid,
    disposal_line_id: Uuid,
    transport_line_id: Uuid,
    handling_line_id: Uuid,
}

/// NES-TEST-1048 style invoice: 10 ton + 1 trip + 1 lot; invoice total $13,350.
async fn seed_multi_unit(app: &TestApp, org: &TestOrg) -> MultiUnitSeed {
    let org_id = org.organization_id;
    let vendor_id = Uuid::new_v4();
    let contract_id = Uuid::new_v4();
    let disposal_rate_id = Uuid::new_v4();
    let transport_rate_id = Uuid::new_v4();
    let handling_rate_id = Uuid::new_v4();
    let invoice_id = Uuid::new_v4();
    let disposal_line_id = Uuid::new_v4();
    let transport_line_id = Uuid::new_v4();
    let handling_line_id = Uuid::new_v4();

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();

    sqlx::query("INSERT INTO vendors (id, organization_id, name) VALUES ($1, $2, 'Multi-Unit Vendor')")
        .bind(vendor_id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO contracts (id, organization_id, vendor_id, name) VALUES ($1, $2, $3, 'Multi-Unit Contract')",
    )
    .bind(contract_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();

    for (id, unit, rate) in [
        (disposal_rate_id, "ton", "1100.0000"),
        (transport_rate_id, "trip", "1000.0000"),
        (handling_rate_id, "lot", "350.0000"),
    ] {
        sqlx::query(
            r#"
            INSERT INTO contract_rates
              (id, organization_id, contract_id, vendor_id, waste_stream, unit, unit_rate, effective_from)
            VALUES ($1, $2, $3, $4, 'F003', $5, $6::numeric, '2026-01-01')
            "#,
        )
        .bind(id)
        .bind(org_id)
        .bind(contract_id)
        .bind(vendor_id)
        .bind(unit)
        .bind(rate)
        .execute(&mut *tx)
        .await
        .unwrap();
    }

    sqlx::query(
        r#"
        INSERT INTO invoices
          (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total)
        VALUES ($1, $2, $3, 'NES-TEST-1048', '2026-03-15', '2026-03-10', 'F003', 13350.0000)
        "#,
    )
    .bind(invoice_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();

    for (line_id, desc, qty, unit, price, total) in [
        (disposal_line_id, "Disposal", "10", "ton", "1200.0000", "12000.0000"),
        (transport_line_id, "Transport", "1", "trip", "1000.0000", "1000.0000"),
        (handling_line_id, "Handling", "1", "lot", "350.0000", "350.0000"),
    ] {
        sqlx::query(
            r#"
            INSERT INTO invoice_line_items
              (id, invoice_id, organization_id, description, fee_code, quantity, unit, unit_price, line_total)
            VALUES ($1, $2, $3, $4, NULL, $5::numeric, $6, $7::numeric, $8::numeric)
            "#,
        )
        .bind(line_id)
        .bind(invoice_id)
        .bind(org_id)
        .bind(desc)
        .bind(qty)
        .bind(unit)
        .bind(price)
        .bind(total)
        .execute(&mut *tx)
        .await
        .unwrap();
    }

    tx.commit().await.unwrap();

    MultiUnitSeed {
        invoice_id,
        disposal_rate_id,
        transport_rate_id,
        handling_rate_id,
        disposal_line_id,
        transport_line_id,
        handling_line_id,
    }
}

fn money_eq(v: &Value, expected: &str) {
    let s = v.as_str().unwrap_or("");
    let left: rust_decimal::Decimal = s.parse().unwrap_or_default();
    let right: rust_decimal::Decimal = expected.parse().unwrap();
    assert_eq!(left, right, "money mismatch: {s} vs {expected}");
}

fn db_error_parts(e: &sqlx::Error) -> (Option<String>, Option<String>) {
    match e.as_database_error() {
        Some(d) => (
            d.code().map(|c| c.to_string()),
            d.constraint().map(|c| c.to_string()),
        ),
        None => (None, None),
    }
}

#[tokio::test]
async fn multi_unit_run_persists_three_line_rates_and_detail_reload_matches() {
    let app = test_app().await;
    let org = app.signup_org("mu-persist").await;
    let seed = seed_multi_unit(&app, &org).await;

    let (status, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "run failed: {run}");
    assert_eq!(run["pricing_status"], "COMPLETE");
    money_eq(&run["expected_amount"], "12350.0000");
    money_eq(&run["invoiced_amount"], "13350.0000");
    money_eq(&run["discrepancy_amount"], "1000.0000");

    let priced = run["priced_lines"].as_array().expect("run priced_lines");
    assert_eq!(priced.len(), 3, "run response priced_lines: {priced:?}");

    let rec_id = run["reconciliation_id"].as_str().unwrap();

    // DB rows in the same TX as the reconciliation.
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let rows: Vec<(Uuid, Uuid, rust_decimal::Decimal)> = sqlx::query_as(
        r#"
        SELECT invoice_line_item_id, contract_rate_id, expected_line_amount
        FROM reconciliation_line_rates
        WHERE reconciliation_id = $1
        ORDER BY invoice_line_item_id
        "#,
    )
    .bind(Uuid::parse_str(rec_id).unwrap())
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(rows.len(), 3, "expected 3 persisted line rates");

    let by_line: std::collections::HashMap<_, _> = rows
        .into_iter()
        .map(|(line, rate, amt)| (line, (rate, amt)))
        .collect();
    assert_eq!(by_line[&seed.disposal_line_id].0, seed.disposal_rate_id);
    assert_eq!(
        by_line[&seed.disposal_line_id].1,
        "11000.0000".parse().unwrap()
    );
    assert_eq!(by_line[&seed.transport_line_id].0, seed.transport_rate_id);
    assert_eq!(
        by_line[&seed.transport_line_id].1,
        "1000.0000".parse().unwrap()
    );
    assert_eq!(by_line[&seed.handling_line_id].0, seed.handling_rate_id);
    assert_eq!(
        by_line[&seed.handling_line_id].1,
        "350.0000".parse().unwrap()
    );

    // GET detail must return the same persisted evidence (not only the run response).
    let (status, detail) = app
        .get(&format!("/api/v1/reconciliations/{rec_id}"), &org.token)
        .await;
    assert_eq!(status, StatusCode::OK, "detail failed: {detail}");
    let detail_lines = detail["priced_lines"].as_array().expect("detail priced_lines");
    assert_eq!(detail_lines.len(), 3, "detail priced_lines: {detail_lines:?}");
    money_eq(&detail["expected_amount"], "12350.0000");
    money_eq(&detail["discrepancy_amount"], "1000.0000");

    let detail_by_line: std::collections::HashMap<String, &Value> = detail_lines
        .iter()
        .map(|p| {
            (
                p["invoice_line_item_id"].as_str().unwrap().to_string(),
                p,
            )
        })
        .collect();
    money_eq(
        &detail_by_line[&seed.disposal_line_id.to_string()]["expected_line_amount"],
        "11000.0000",
    );
    assert_eq!(
        detail_by_line[&seed.disposal_line_id.to_string()]["contract_rate_id"],
        seed.disposal_rate_id.to_string()
    );
    money_eq(
        &detail_by_line[&seed.transport_line_id.to_string()]["expected_line_amount"],
        "1000.0000",
    );
    money_eq(
        &detail_by_line[&seed.handling_line_id.to_string()]["expected_line_amount"],
        "350.0000",
    );
}

#[tokio::test]
async fn second_run_keeps_historical_line_rates_and_creates_new_rows() {
    let app = test_app().await;
    let org = app.signup_org("mu-history").await;
    let seed = seed_multi_unit(&app, &org).await;

    let (_, run1) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    let rec1 = Uuid::parse_str(run1["reconciliation_id"].as_str().unwrap()).unwrap();

    let (_, run2) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    let rec2 = Uuid::parse_str(run2["reconciliation_id"].as_str().unwrap()).unwrap();
    assert_ne!(rec1, rec2);

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org.organization_id)
        .await
        .unwrap();
    let count1: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reconciliation_line_rates WHERE reconciliation_id = $1",
    )
    .bind(rec1)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let count2: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reconciliation_line_rates WHERE reconciliation_id = $1",
    )
    .bind(rec2)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(count1, 3, "historical run must keep its line rates");
    assert_eq!(count2, 3, "new run must have its own line rates");
}

#[tokio::test]
async fn facility_specific_rate_is_preferred_over_global_for_same_unit() {
    let app = test_app().await;
    let org = app.signup_org("mu-facility").await;
    let org_id = org.organization_id;
    let vendor_id = Uuid::new_v4();
    let contract_id = Uuid::new_v4();
    let facility_id = Uuid::new_v4();
    let global_rate_id = Uuid::new_v4();
    let facility_rate_id = Uuid::new_v4();
    let invoice_id = Uuid::new_v4();
    let line_id = Uuid::new_v4();

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vendors (id, organization_id, name) VALUES ($1, $2, 'Fac Vendor')")
        .bind(vendor_id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO contracts (id, organization_id, vendor_id, name) VALUES ($1, $2, $3, 'Fac Contract')",
    )
    .bind(contract_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO contract_rates
          (id, organization_id, contract_id, vendor_id, facility_id, waste_stream, unit, unit_rate, effective_from)
        VALUES
          ($1, $3, $4, $5, NULL, 'F003', 'ton', 1100.0000, '2026-01-01'),
          ($2, $3, $4, $5, $6, 'F003', 'ton', 900.0000, '2026-01-01')
        "#,
    )
    .bind(global_rate_id)
    .bind(facility_rate_id)
    .bind(org_id)
    .bind(contract_id)
    .bind(vendor_id)
    .bind(facility_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoices
          (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, facility_id, waste_stream, total)
        VALUES ($1, $2, $3, 'FAC-1', '2026-03-15', '2026-03-10', $4, 'F003', 9000.0000)
        "#,
    )
    .bind(invoice_id)
    .bind(org_id)
    .bind(vendor_id)
    .bind(facility_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoice_line_items
          (id, invoice_id, organization_id, description, quantity, unit, unit_price, line_total)
        VALUES ($1, $2, $3, 'Disposal', 10, 'ton', 900.0000, 9000.0000)
        "#,
    )
    .bind(line_id)
    .bind(invoice_id)
    .bind(org_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["pricing_status"], "COMPLETE");
    money_eq(&run["expected_amount"], "9000.0000");

    let rec_id = Uuid::parse_str(run["reconciliation_id"].as_str().unwrap()).unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    let chosen: Uuid = sqlx::query_scalar(
        "SELECT contract_rate_id FROM reconciliation_line_rates WHERE reconciliation_id = $1 AND invoice_line_item_id = $2",
    )
    .bind(rec_id)
    .bind(line_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        chosen, facility_rate_id,
        "facility-specific rate must win over global"
    );
}

#[tokio::test]
async fn expired_rate_is_not_used_to_price_lines() {
    let app = test_app().await;
    let org = app.signup_org("mu-expired").await;
    let org_id = org.organization_id;
    let vendor_id = Uuid::new_v4();
    let contract_id = Uuid::new_v4();
    let rate_id = Uuid::new_v4();
    let invoice_id = Uuid::new_v4();

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vendors (id, organization_id, name) VALUES ($1, $2, 'Exp Vendor')")
        .bind(vendor_id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO contracts (id, organization_id, vendor_id, name) VALUES ($1, $2, $3, 'Exp Contract')",
    )
    .bind(contract_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO contract_rates
          (id, organization_id, contract_id, vendor_id, waste_stream, unit, unit_rate, effective_from, effective_to)
        VALUES ($1, $2, $3, $4, 'F003', 'ton', 1100.0000, '2025-01-01', '2025-12-31')
        "#,
    )
    .bind(rate_id)
    .bind(org_id)
    .bind(contract_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoices
          (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total)
        VALUES ($1, $2, $3, 'EXP-1', '2026-03-15', '2026-03-10', 'F003', 11000.0000)
        "#,
    )
    .bind(invoice_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoice_line_items
          (invoice_id, organization_id, description, quantity, unit, unit_price, line_total)
        VALUES ($1, $2, 'Disposal', 10, 'ton', 1100.0000, 11000.0000)
        "#,
    )
    .bind(invoice_id)
    .bind(org_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_ne!(run["pricing_status"], "COMPLETE");
    let priced = run["priced_lines"].as_array().cloned().unwrap_or_default();
    assert!(priced.is_empty(), "expired rate must not price lines: {priced:?}");

    if let Some(rec) = run["reconciliation_id"].as_str() {
        let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM reconciliation_line_rates WHERE reconciliation_id = $1",
        )
        .bind(Uuid::parse_str(rec).unwrap())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(n, 0);
    }
}

#[tokio::test]
async fn incompatible_unit_yields_no_line_rate_row_for_that_line() {
    let app = test_app().await;
    let org = app.signup_org("mu-unit").await;
    let org_id = org.organization_id;
    let vendor_id = Uuid::new_v4();
    let contract_id = Uuid::new_v4();
    let rate_id = Uuid::new_v4();
    let invoice_id = Uuid::new_v4();
    let line_id = Uuid::new_v4();

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vendors (id, organization_id, name) VALUES ($1, $2, 'Unit Vendor')")
        .bind(vendor_id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO contracts (id, organization_id, vendor_id, name) VALUES ($1, $2, $3, 'Unit Contract')",
    )
    .bind(contract_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    // Contract is per drum; invoice line is kg — must not invent a conversion.
    sqlx::query(
        r#"
        INSERT INTO contract_rates
          (id, organization_id, contract_id, vendor_id, waste_stream, unit, unit_rate, effective_from)
        VALUES ($1, $2, $3, $4, 'F003', 'drum', 400.0000, '2026-01-01')
        "#,
    )
    .bind(rate_id)
    .bind(org_id)
    .bind(contract_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoices
          (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total)
        VALUES ($1, $2, $3, 'UNIT-1', '2026-03-15', '2026-03-10', 'F003', 4000.0000)
        "#,
    )
    .bind(invoice_id)
    .bind(org_id)
    .bind(vendor_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoice_line_items
          (id, invoice_id, organization_id, description, quantity, unit, unit_price, line_total)
        VALUES ($1, $2, $3, 'Disposal', 100, 'kg', 40.0000, 4000.0000)
        "#,
    )
    .bind(line_id)
    .bind(invoice_id)
    .bind(org_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["pricing_status"], "PARTIAL");
    let priced = run["priced_lines"].as_array().cloned().unwrap_or_default();
    assert!(priced.is_empty(), "incompatible unit must not be priced: {priced:?}");

    let rec_id = Uuid::parse_str(run["reconciliation_id"].as_str().unwrap()).unwrap();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reconciliation_line_rates WHERE reconciliation_id = $1",
    )
    .bind(rec_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn cross_tenant_cannot_read_or_write_line_rates() {
    let app = test_app().await;
    let org_a = app.signup_org("mu-rls-a").await;
    let org_b = app.signup_org("mu-rls-b").await;
    let seed = seed_multi_unit(&app, &org_a).await;

    let (_, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org_a.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    let rec_id = Uuid::parse_str(run["reconciliation_id"].as_str().unwrap()).unwrap();

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_b.organization_id)
        .await
        .unwrap();
    let visible: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reconciliation_line_rates WHERE reconciliation_id = $1",
    )
    .bind(rec_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(visible, 0, "Org B must not see Org A line rates via RLS");

    // All NOT NULL columns are supplied, so the only reason to fail is RLS WITH CHECK.
    let res = sqlx::query(
        r#"
        INSERT INTO reconciliation_line_rates
          (organization_id, reconciliation_id, invoice_id, invoice_line_item_id, contract_rate_id, expected_line_amount)
        VALUES ($1, $2, $3, $4, $5, 1.0000)
        "#,
    )
    .bind(org_a.organization_id)
    .bind(rec_id)
    .bind(seed.invoice_id)
    .bind(seed.disposal_line_id)
    .bind(seed.disposal_rate_id)
    .execute(&mut *tx)
    .await;
    let err = res.expect_err("RLS WITH CHECK must reject cross-tenant line-rate inserts");
    let (code, _) = db_error_parts(&err);
    assert_eq!(
        code.as_deref(),
        Some("42501"),
        "expected an RLS violation (42501), got {err:?}"
    );
    tx.rollback().await.unwrap();

    let (status, _) = app
        .post(
            "/api/v1/reconciliations/run",
            &org_b.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app
        .get(&format!("/api/v1/reconciliations/{rec_id}"), &org_b.token)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn evidence_package_priced_lines_match_detail_api() {
    let app = test_app().await;
    let org = app.signup_org("mu-evidence").await;
    let seed = seed_multi_unit(&app, &org).await;

    let (status, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "run failed: {run}");
    let rec_id = run["reconciliation_id"].as_str().unwrap().to_string();

    let (status, detail) = app
        .get(&format!("/api/v1/reconciliations/{rec_id}"), &org.token)
        .await;
    assert_eq!(status, StatusCode::OK, "detail failed: {detail}");
    let detail_lines = detail["priced_lines"].as_array().expect("detail priced_lines");
    assert_eq!(detail_lines.len(), 3);

    let discrepancy_id = detail["discrepancies"]
        .as_array()
        .and_then(|d| d.first())
        .and_then(|d| d["id"].as_str())
        .expect("three-unit invoice must produce at least one discrepancy to attach evidence to")
        .to_string();

    let (status, pkg) = app
        .post(
            &format!("/api/v1/discrepancies/{discrepancy_id}/evidence"),
            &org.token,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "evidence failed: {pkg}");

    let ev_lines = pkg["content"]["priced_lines"]
        .as_array()
        .expect("evidence content.priced_lines");
    assert_eq!(ev_lines.len(), 3, "evidence priced_lines: {ev_lines:?}");

    for d in detail_lines {
        let line_id = d["invoice_line_item_id"].as_str().unwrap();
        let e = ev_lines
            .iter()
            .find(|e| e["invoice_line_item_id"].as_str() == Some(line_id))
            .unwrap_or_else(|| panic!("evidence missing line {line_id}"));
        assert_eq!(e["contract_rate_id"], d["contract_rate_id"]);
        money_eq(&e["expected_line_amount"], d["expected_line_amount"].as_str().unwrap());
    }

    // Each line is tied to its own rate, not to one parent rate.
    let rate_ids: std::collections::HashSet<&str> = ev_lines
        .iter()
        .map(|e| e["contract_rate_id"].as_str().unwrap())
        .collect();
    assert_eq!(rate_ids.len(), 3, "three lines must reference three distinct rates");
}

#[tokio::test]
async fn composite_fk_rejects_cross_org_rate_on_line_rate_row() {
    let app = test_app().await;
    let org_a = app.signup_org("mu-fk-a").await;
    let org_b = app.signup_org("mu-fk-b").await;
    let seed_a = seed_multi_unit(&app, &org_a).await;
    let seed_b = seed_multi_unit(&app, &org_b).await;

    let (_, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org_a.token,
            json!({ "invoice_id": seed_a.invoice_id }),
        )
        .await;
    let rec_a = Uuid::parse_str(run["reconciliation_id"].as_str().unwrap()).unwrap();

    // Org A's own session, so RLS passes; only Org B's rate id is foreign.
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_a.organization_id)
        .await
        .unwrap();
    // The run already stored a row for this line; remove it inside this rolled-back
    // transaction so the primary key does not fire before the foreign key is checked.
    sqlx::query(
        "DELETE FROM reconciliation_line_rates WHERE reconciliation_id = $1 AND invoice_line_item_id = $2",
    )
    .bind(rec_a)
    .bind(seed_a.disposal_line_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let res = sqlx::query(
        r#"
        INSERT INTO reconciliation_line_rates
          (organization_id, reconciliation_id, invoice_id, invoice_line_item_id, contract_rate_id, expected_line_amount)
        VALUES ($1, $2, $3, $4, $5, 99.0000)
        "#,
    )
    .bind(org_a.organization_id)
    .bind(rec_a)
    .bind(seed_a.invoice_id)
    .bind(seed_a.disposal_line_id)
    .bind(seed_b.disposal_rate_id)
    .execute(&mut *tx)
    .await;
    let err = res.expect_err("composite FK must reject another organization's rate");
    let (code, constraint) = db_error_parts(&err);
    assert_eq!(code.as_deref(), Some("23503"), "{err:?}");
    assert_eq!(
        constraint.as_deref(),
        Some("reconciliation_line_rates_rate_same_org"),
        "{err:?}"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn composite_fk_rejects_cross_invoice_line_and_reconciliation() {
    let app = test_app().await;
    let org = app.signup_org("mu-xinv").await;
    let seed = seed_multi_unit(&app, &org).await;
    let org_id = org.organization_id;

    let (_, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    let rec_id = Uuid::parse_str(run["reconciliation_id"].as_str().unwrap()).unwrap();

    // A second invoice with one line in the SAME organization.
    let vendor2 = Uuid::new_v4();
    let invoice2 = Uuid::new_v4();
    let line2 = Uuid::new_v4();
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vendors (id, organization_id, name) VALUES ($1, $2, 'Other Vendor')")
        .bind(vendor2)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoices
          (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total)
        VALUES ($1, $2, $3, 'XINV-2', '2026-03-15', '2026-03-10', 'F003', 100.0000)
        "#,
    )
    .bind(invoice2)
    .bind(org_id)
    .bind(vendor2)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO invoice_line_items
          (id, invoice_id, organization_id, description, quantity, unit, unit_price, line_total)
        VALUES ($1, $2, $3, 'Other line', 1, 'ton', 100.0000, 100.0000)
        "#,
    )
    .bind(line2)
    .bind(invoice2)
    .bind(org_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let insert_sql = r#"
        INSERT INTO reconciliation_line_rates
          (organization_id, reconciliation_id, invoice_id, invoice_line_item_id, contract_rate_id, expected_line_amount)
        VALUES ($1, $2, $3, $4, $5, 1.0000)
        "#;

    // Case 1: reconciliation of invoice 1, but the line belongs to invoice 2.
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    let err = sqlx::query(insert_sql)
        .bind(org_id)
        .bind(rec_id)
        .bind(seed.invoice_id)
        .bind(line2)
        .bind(seed.disposal_rate_id)
        .execute(&mut *tx)
        .await
        .expect_err("a line from another invoice must be rejected");
    let (code, constraint) = db_error_parts(&err);
    assert_eq!(code.as_deref(), Some("23503"), "{err:?}");
    assert_eq!(
        constraint.as_deref(),
        Some("reconciliation_line_rates_line_same_invoice"),
        "{err:?}"
    );
    tx.rollback().await.unwrap();

    // Case 2: the line matches invoice 2, but the reconciliation belongs to invoice 1.
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    let err = sqlx::query(insert_sql)
        .bind(org_id)
        .bind(rec_id)
        .bind(invoice2)
        .bind(line2)
        .bind(seed.disposal_rate_id)
        .execute(&mut *tx)
        .await
        .expect_err("a reconciliation of another invoice must be rejected");
    let (code, constraint) = db_error_parts(&err);
    assert_eq!(code.as_deref(), Some("23503"), "{err:?}");
    assert_eq!(
        constraint.as_deref(),
        Some("reconciliation_line_rates_reconciliation_same_scope"),
        "{err:?}"
    );
    tx.rollback().await.unwrap();
}
#[tokio::test]
#[ignore = "needs MIGRATION_DATABASE_URL (superuser) to install a temporary failure trigger"]
async fn failed_line_insert_rolls_back_reconciliation_and_discrepancies() {
    let admin_url = std::env::var("MIGRATION_DATABASE_URL")
        .expect("set MIGRATION_DATABASE_URL to a superuser URL to run this test");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .expect("connect as superuser");

    let app = test_app().await;
    let org = app.signup_org("mu-rollback").await;
    let seed = seed_multi_unit(&app, &org).await;
    let org_id = org.organization_id;

    // Temporary failure, scoped to this invoice only so parallel tests are unaffected.
    let name = format!("fail_lr_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $body$ BEGIN RAISE EXCEPTION 'forced line-rate failure'; END $body$"
    ))
    .execute(&admin)
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER {name} BEFORE INSERT ON reconciliation_line_rates FOR EACH ROW WHEN (NEW.invoice_id = '{}') EXECUTE FUNCTION {name}()",
        seed.invoice_id
    ))
    .execute(&admin)
    .await
    .unwrap();

    let (status, body) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;

    // Inspect state while the failure is still installed.
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_id)
        .await
        .unwrap();
    let recs: i64 = sqlx::query_scalar("SELECT count(*) FROM reconciliations WHERE invoice_id = $1")
        .bind(seed.invoice_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let discs: i64 = sqlx::query_scalar("SELECT count(*) FROM discrepancies")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM reconciliation_line_rates")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    // Always clean up the temporary trigger before asserting.
    sqlx::query(&format!("DROP TRIGGER {name} ON reconciliation_line_rates"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION {name}()"))
        .execute(&admin)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(recs, 0, "failed run must not leave a reconciliation row");
    assert_eq!(discs, 0, "failed run must not leave discrepancy rows");
    assert_eq!(lines, 0, "failed run must not leave line-rate rows");

    // With the failure removed, the same invoice reconciles normally.
    let (status, run) = app
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": seed.invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["priced_lines"].as_array().map(|a| a.len()), Some(3));
}