//! Automated HTTP-layer cross-tenant isolation tests (spec section 7).
//! Real router + real auth extractor + real PostgreSQL with RLS enforced via the xflow_app role.

mod common;

use axum::http::{Method, StatusCode};
use common::test_app;
use serde_json::json;

#[tokio::test]
async fn org_b_gets_404_for_org_a_invoice_through_the_full_http_path() {
    let app = test_app().await;
    let org_a = app.signup_org("org-a").await;
    let org_b = app.signup_org("org-b").await;
    let seeded = app.seed_worked_example(&org_a).await;
    let body = json!({ "invoice_id": seeded.invoice_id });

    // Control: the owner (Org A) can reconcile its own invoice, so a 404 for Org B is
    // isolation and not a broken fixture.
    let (status, result) = app
        .post("/api/v1/reconciliations/run", &org_a.token, body.clone())
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Org A must reach its own invoice: {result}"
    );
    assert_eq!(result["discrepancy_amount"], "700.0000");

    // Org B authenticates successfully but must not be able to see or act on Org A's invoice.
    let (status, result) = app
        .post("/api/v1/reconciliations/run", &org_b.token, body)
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "cross-tenant access must be 404, got {status}: {result}"
    );
    assert_eq!(result["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn cross_tenant_attempt_does_not_leak_into_org_b_dashboard_or_create_rows() {
    let app = test_app().await;
    let org_a = app.signup_org("dash-a").await;
    let org_b = app.signup_org("dash-b").await;
    let seeded = app.seed_worked_example(&org_a).await;

    let (status, _) = app
        .post(
            "/api/v1/reconciliations/run",
            &org_b.token,
            json!({ "invoice_id": seeded.invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, dash_b) = app.get("/api/v1/dashboard", &org_b.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dash_b["invoices_analyzed"], 0);
    assert_eq!(dash_b["potential_discrepancy_total"], "0.0000");

    // Org A's own run is visible only to Org A.
    let (status, _) = app
        .post(
            "/api/v1/reconciliations/run",
            &org_a.token,
            json!({ "invoice_id": seeded.invoice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, dash_a) = app.get("/api/v1/dashboard", &org_a.token).await;
    assert_eq!(dash_a["invoices_analyzed"], 1);
    assert_eq!(dash_a["potential_discrepancy_total"], "700.0000");
    let (_, dash_b_again) = app.get("/api/v1/dashboard", &org_b.token).await;
    assert_eq!(dash_b_again["invoices_analyzed"], 0);
}

#[tokio::test]
async fn database_rls_hides_rows_even_without_application_filters() {
    let app = test_app().await;
    let org_a = app.signup_org("rls-a").await;
    let org_b = app.signup_org("rls-b").await;
    let seeded = app.seed_worked_example(&org_a).await;

    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_b.organization_id)
        .await
        .unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices WHERE id = $1")
        .bind(seeded.invoice_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        visible, 0,
        "Org B's scoped session must see zero of Org A's invoices"
    );
    tx.rollback().await.unwrap();

    // A session with no tenant context sees nothing at all.
    let unscoped: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices WHERE id = $1")
        .bind(seeded.invoice_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        unscoped, 0,
        "a session without tenant context must see zero rows"
    );

    // WITH CHECK: Org B's scope cannot insert rows claiming to belong to Org A.
    let mut tx = xflow_backend::tenants::begin_scoped_tx(&app.pool, org_b.organization_id)
        .await
        .unwrap();
    let res = sqlx::query("INSERT INTO vendors (organization_id, name) VALUES ($1, 'smuggled')")
        .bind(org_a.organization_id)
        .execute(&mut *tx)
        .await;
    assert!(
        res.is_err(),
        "RLS WITH CHECK must reject cross-tenant inserts"
    );
}

#[tokio::test]
async fn unauthenticated_and_revoked_sessions_are_rejected() {
    let app = test_app().await;
    let org = app.signup_org("authz").await;
    let seeded = app.seed_worked_example(&org).await;
    let body = json!({ "invoice_id": seeded.invoice_id });

    let (status, _) = app
        .call(
            Method::POST,
            "/api/v1/reconciliations/run",
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app
        .post(
            "/api/v1/reconciliations/run",
            "not-a-real-token",
            body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = app
        .post("/api/v1/reconciliations/run", &org.token, body.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app
        .call(Method::POST, "/api/v1/auth/logout", Some(&org.token), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app
        .post("/api/v1/reconciliations/run", &org.token, body)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a revoked session must no longer authenticate"
    );
}
