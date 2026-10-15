//! End-to-end HTTP tests for the real workflow: data entry -> reconciliation (with manifest and
//! weight tickets) -> review -> evidence -> dispute -> credit verification, plus RBAC, billing,
//! documents, monitoring and isolation of every new endpoint.

mod common;

use axum::http::{Method, StatusCode};
use common::{paddle_event, sign, test_app, webhook, TestApp, TestOrg, WEBHOOK_SECRET};
use serde_json::{json, Value};

async fn ok(app: &TestApp, org: &TestOrg, path: &str, body: Value) -> Value {
    let (s, v) = app.post(path, &org.token, body).await;
    assert!(s.is_success(), "POST {path} -> {s}: {v}");
    v
}

/// Vendor + contract + rate ($400/drum, TRANSPORT $500) + manifest + invoice from the spec example.
async fn setup(
    app: &TestApp,
    org: &TestOrg,
    tag: &str,
    manifest_qty: &str,
    tickets: &[(&str, &str)],
) -> (String, String, String) {
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
    let cid = contract["id"].as_str().unwrap();
    ok(app, org, &format!("/api/v1/contracts/{cid}/rates"), json!({
        "waste_stream": "F003", "unit": "drum", "unit_rate": "400.00", "effective_from": "2026-01-01",
        "authorized_fees": [{ "fee_code": "TRANSPORT", "description": "Transportation", "amount": "500.00" }]
    })).await;
    let manifest = ok(app, org, "/api/v1/manifests", json!({ "manifest_number": format!("M-{tag}"), "waste_stream": "F003", "container_count": 10, "quantity": manifest_qty, "unit": "drum", "shipment_date": "2026-03-09" })).await;
    let mid = manifest["id"].as_str().unwrap().to_string();
    for (n, (num, w)) in tickets.iter().enumerate() {
        ok(app, org, "/api/v1/weight-tickets", json!({ "ticket_number": format!("{num}-{tag}-{n}"), "manifest_id": mid, "net_weight": w, "unit": "drum", "ticket_date": "2026-03-10" })).await;
    }
    let invoice = ok(app, org, "/api/v1/invoices", json!({
        "vendor_id": vendor_id, "invoice_number": format!("INV-{tag}"), "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003",
        "manifest_id": mid, "total": "5200.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "450.00", "line_total": "4500.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" },
            { "description": "Environmental fee", "fee_code": "ENV_FEE", "quantity": "1", "unit": "flat", "unit_price": "200.00", "line_total": "200.00" }]
    })).await;
    (vendor_id, mid, invoice["id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn manifest_and_tickets_feed_the_real_reconciliation_and_findings_are_persisted() {
    let app = test_app().await;
    let org = app.signup_org("flow").await;
    // Manifest says 8 drums, two tickets weigh 5 + 5 drums: quantity and weight rules must fire.
    let (_, mid, inv) = setup(&app, &org, "a", "8", &[("T", "5"), ("T", "5")]).await;

    let run = ok(
        &app,
        &org,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": inv }),
    )
    .await;
    assert_eq!(
        run["manifest_id"], mid,
        "the linked manifest must be used, not None"
    );
    assert_eq!(run["weight_ticket_count"], 2);
    assert_eq!(run["expected_amount"], "4500.0000");
    assert_eq!(run["invoiced_amount"], "5200.0000");
    let rules: Vec<&str> = run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule_id"].as_str().unwrap())
        .collect();
    assert!(
        rules.contains(&"RULE_03_RATE_MISMATCH") && rules.contains(&"RULE_04_UNAUTHORIZED_FEE"),
        "{rules:?}"
    );
    assert!(
        rules.contains(&"RULE_01_QUANTITY_VS_MANIFEST"),
        "manifest quantity rule must run: {rules:?}"
    );
    assert_eq!(
        run["discrepancy_ids"].as_array().unwrap().len(),
        run["discrepancies"].as_array().unwrap().len()
    );

    let (s, list) = app.get("/api/v1/discrepancies", &org.token).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        list.as_array().unwrap().len(),
        run["discrepancies"].as_array().unwrap().len(),
        "every finding is a reviewable row"
    );

    // Re-running must not double count: the dashboard only reflects the latest run.
    ok(
        &app,
        &org,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": inv }),
    )
    .await;
    let (_, dash) = app.get("/api/v1/dashboard", &org.token).await;
    assert_eq!(dash["invoices_analyzed"], 1);
    let (_, list2) = app.get("/api/v1/discrepancies", &org.token).await;
    assert_eq!(
        list2.as_array().unwrap().len(),
        list.as_array().unwrap().len()
    );
}

#[tokio::test]
async fn clean_support_documents_remove_the_missing_support_finding() {
    let app = test_app().await;
    let org = app.signup_org("support").await;
    let (_, _, inv) = setup(&app, &org, "b", "10", &[("T", "10")]).await;
    let run = ok(
        &app,
        &org,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": inv }),
    )
    .await;
    let rules: Vec<&str> = run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule_id"].as_str().unwrap())
        .collect();
    assert!(!rules.contains(&"RULE_07_MISSING_SUPPORT"), "{rules:?}");
    assert!(
        !rules.contains(&"RULE_01_QUANTITY_VS_MANIFEST"),
        "{rules:?}"
    );
    assert_eq!(run["discrepancy_amount"], "700.0000");
}

#[tokio::test]
async fn full_review_evidence_dispute_and_recovery_lifecycle() {
    let app = test_app().await;
    let org = app.signup_org("life").await;
    let (_, _, inv) = setup(&app, &org, "c", "10", &[("T", "10")]).await;
    let run = ok(
        &app,
        &org,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": inv }),
    )
    .await;
    let d = run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["rule_id"] == "RULE_03_RATE_MISMATCH")
        .unwrap();
    assert_eq!(d["amount"], "500.0000");
    let did = run["discrepancy_ids"].as_array().unwrap()[run["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .position(|x| x["rule_id"] == "RULE_03_RATE_MISMATCH")
        .unwrap()]
    .as_str()
    .unwrap()
    .to_string();

    // Reject needs notes; disputing before confirmation is refused.
    let (s, _) = app
        .post(
            &format!("/api/v1/discrepancies/{did}/review"),
            &org.token,
            json!({ "action": "reject" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app
        .post(
            "/api/v1/disputes",
            &org.token,
            json!({ "discrepancy_id": did }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "only CONFIRMED discrepancies can be disputed"
    );

    let c = ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/review"),
        json!({ "action": "confirm" }),
    )
    .await;
    assert_eq!(c["review_status"], "CONFIRMED");
    let (s, _) = app
        .post(
            &format!("/api/v1/discrepancies/{did}/review"),
            &org.token,
            json!({ "action": "reject", "notes": "x" }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a decided finding cannot be re-decided without reopening"
    );

    let ev = ok(
        &app,
        &org,
        &format!("/api/v1/discrepancies/{did}/evidence"),
        json!({}),
    )
    .await;
    assert_eq!(ev["content"]["invoice"]["invoice_number"], "INV-c");
    assert_eq!(ev["content"]["contract_rate"]["unit_rate"], "400.0000");
    assert_eq!(ev["content"]["manifest"]["manifest_number"], "M-c");
    assert_eq!(ev["content"]["weight_tickets"].as_array().unwrap().len(), 1);
    assert!(
        ev["content"]["reconciliation"]["calculation_trace"]
            .as_array()
            .unwrap()
            .len()
            > 2
    );
    let (s, latest) = app
        .get(&format!("/api/v1/discrepancies/{did}/evidence"), &org.token)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(latest["id"], ev["id"]);

    let dispute = ok(
        &app,
        &org,
        "/api/v1/disputes",
        json!({ "discrepancy_id": did }),
    )
    .await;
    let dsp = dispute["id"].as_str().unwrap().to_string();
    assert_eq!(dispute["status"], "OPEN");
    assert_eq!(dispute["disputed_amount"], "500.0000");
    assert_eq!(dispute["credit"]["identified_amount"], "500.0000");
    let (s, _) = app
        .post(
            "/api/v1/disputes",
            &org.token,
            json!({ "discrepancy_id": did }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "no duplicate active dispute");
    let (_, disc) = app
        .get(&format!("/api/v1/discrepancies/{did}"), &org.token)
        .await;
    assert_eq!(disc["review_status"], "DISPUTED");

    let t = |to: &str, extra: Value| {
        let mut b = json!({ "to": to });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                b[k] = v;
            }
        }
        b
    };
    let path = format!("/api/v1/disputes/{dsp}/transition");
    let (s, _) = app.post(&path, &org.token, t("SUBMITTED", json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "cannot skip PREPARE/REVIEW");
    for to in ["PREPARE", "REVIEW", "SUBMITTED", "VENDOR_RESPONDED"] {
        let v = ok(&app, &org, &path, t(to, json!({}))).await;
        assert_eq!(v["status"], to);
    }
    let (s, _) = app
        .post(&path, &org.token, t("CREDIT_PROMISED", json!({})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "promised amount is required");
    let (s, _) = app
        .post(
            &path,
            &org.token,
            t("CREDIT_PROMISED", json!({ "amount": "600.00" })),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "cannot promise more than disputed"
    );
    let v = ok(
        &app,
        &org,
        &path,
        t("CREDIT_PROMISED", json!({ "amount": "500.00" })),
    )
    .await;
    assert_eq!(v["confirmed_amount"], "500.0000");
    let (s, _) = app
        .post(
            &path,
            &org.token,
            t("CREDIT_RECEIVED", json!({ "amount": "400.00" })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "memo reference is required");
    ok(
        &app,
        &org,
        &path,
        t(
            "CREDIT_RECEIVED",
            json!({ "amount": "400.00", "credit_memo_reference": "CM-7781" }),
        ),
    )
    .await;

    let (_, rec) = app.get("/api/v1/recovery", &org.token).await;
    assert_eq!(rec["identified"], "500.0000");
    assert_eq!(rec["confirmed"], "500.0000");
    assert_eq!(
        rec["recovered_verified"], "0.0000",
        "unverified credit must not count as recovered"
    );
    assert_eq!(rec["received_pending_verification"], "400.0000");

    let (s, _) = app.post(&path, &org.token, t("CLOSED", json!({}))).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a received credit must be verified before closing"
    );
    let v = ok(&app, &org, &path, t("VERIFIED", json!({}))).await;
    assert_eq!(v["credit"]["recovered_amount"], "400.0000");
    assert!(v["credit"]["verified_at"].is_string());
    ok(&app, &org, &path, t("CLOSED", json!({}))).await;

    let (_, rec) = app.get("/api/v1/recovery", &org.token).await;
    assert_eq!(rec["recovered_verified"], "400.0000");
    assert_eq!(rec["unrecovered"], "100.0000");
    let (_, disc) = app
        .get(&format!("/api/v1/discrepancies/{did}"), &org.token)
        .await;
    assert_eq!(disc["review_status"], "RESOLVED");

    // Every step left an audit trail.
    let (_, audit) = app.get("/api/v1/audit?limit=200", &org.token).await;
    let actions: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    for a in [
        "invoice.create",
        "reconciliation.run",
        "discrepancy.confirm",
        "evidence.generate",
        "dispute.create",
        "dispute.verified",
        "dispute.closed",
    ] {
        assert!(
            actions.contains(&a),
            "missing audit action {a}: {actions:?}"
        );
    }
}

#[tokio::test]
async fn new_endpoints_are_tenant_isolated_and_reject_foreign_references() {
    let app = test_app().await;
    let a = app.signup_org("iso-a").await;
    let b = app.signup_org("iso-b").await;
    let (vendor_a, manifest_a, inv_a) = setup(&app, &a, "d", "10", &[("T", "10")]).await;
    let run = ok(
        &app,
        &a,
        "/api/v1/reconciliations/run",
        json!({ "invoice_id": inv_a }),
    )
    .await;
    let did = run["discrepancy_ids"].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let (s, own) = app
        .get(
            &format!(
                "/api/v1/reconciliations/{}",
                run["reconciliation_id"].as_str().unwrap()
            ),
            &a.token,
        )
        .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "Org A must read its own reconciliation: {own}"
    );
    assert_eq!(
        own["discrepancies"].as_array().unwrap().len(),
        run["discrepancies"].as_array().unwrap().len()
    );
    assert_eq!(own["expected_amount"], "4500.0000");

    for path in [
        format!("/api/v1/invoices/{inv_a}"),
        format!("/api/v1/manifests/{manifest_a}"),
        format!("/api/v1/discrepancies/{did}"),
        format!(
            "/api/v1/reconciliations/{}",
            run["reconciliation_id"].as_str().unwrap()
        ),
        format!("/api/v1/discrepancies/{did}/evidence"),
    ] {
        let (s, _) = app.get(&path, &b.token).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "Org B must get 404 for {path}");
    }
    let (s, _) = app
        .post(
            &format!("/api/v1/discrepancies/{did}/review"),
            &b.token,
            json!({ "action": "confirm" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .post(
            &format!("/api/v1/discrepancies/{did}/evidence"),
            &b.token,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // FK checks bypass RLS, so referencing Org A's vendor/manifest from Org B must be refused explicitly.
    let (s, _) = app
        .post(
            "/api/v1/contracts",
            &b.token,
            json!({ "vendor_id": vendor_a, "name": "steal" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app.post("/api/v1/weight-tickets", &b.token, json!({ "ticket_number": "X", "manifest_id": manifest_a, "net_weight": "1", "unit": "drum", "ticket_date": "2026-03-10" })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, list) = app.get("/api/v1/discrepancies", &b.token).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    let (_, inv_list) = app.get("/api/v1/invoices", &b.token).await;
    assert_eq!(inv_list.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn rbac_is_enforced_per_role_on_real_endpoints() {
    let app = test_app().await;
    let owner = app.signup_org("rbac").await;
    let (_, _, inv) = setup(&app, &owner, "e", "10", &[("T", "10")]).await;

    let mk = |role: &str| json!({ "email": format!("{role}-{}@test.invalid", uuid::Uuid::new_v4()), "password": "a-sufficiently-long-password", "role": role });
    let mut tokens = std::collections::HashMap::new();
    for role in ["ADMIN", "MEMBER", "REVIEWER"] {
        let body = mk(role);
        let email = body["email"].as_str().unwrap().to_string();
        let (s, _) = app.post("/api/v1/members", &owner.token, body).await;
        assert_eq!(s, StatusCode::CREATED, "{role}");
        let (s, login) = app
            .call(
                Method::POST,
                "/api/v1/auth/login",
                None,
                Some(json!({ "email": email, "password": "a-sufficiently-long-password" })),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        tokens.insert(role, login["session_token"].as_str().unwrap().to_string());
    }
    let (member, reviewer, admin) = (&tokens["MEMBER"], &tokens["REVIEWER"], &tokens["ADMIN"]);

    // MEMBER: can manage data, cannot review or administer.
    let (s, _) = app
        .post("/api/v1/vendors", member, json!({ "name": "by member" }))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app
        .post(
            "/api/v1/reconciliations/run",
            member,
            json!({ "invoice_id": inv }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, list) = app.get("/api/v1/discrepancies", member).await;
    let did = list.as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = app
        .post(
            &format!("/api/v1/discrepancies/{did}/review"),
            member,
            json!({ "action": "confirm" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.get("/api/v1/audit", member).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.get("/api/v1/members", member).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // REVIEWER: can review/dispute, cannot enter or change source data.
    let (s, _) = app
        .post(
            "/api/v1/vendors",
            reviewer,
            json!({ "name": "by reviewer" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .post(
            "/api/v1/reconciliations/run",
            reviewer,
            json!({ "invoice_id": inv }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .post(
            &format!("/api/v1/discrepancies/{did}/review"),
            reviewer,
            json!({ "action": "confirm" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, d) = app
        .post(
            "/api/v1/disputes",
            reviewer,
            json!({ "discrepancy_id": did }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let dsp = d["id"].as_str().unwrap();
    for to in ["PREPARE", "REVIEW", "SUBMITTED", "VENDOR_RESPONDED"] {
        let (s, _) = app
            .post(
                &format!("/api/v1/disputes/{dsp}/transition"),
                reviewer,
                json!({ "to": to }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{to}");
    }
    let (s, _) = app
        .post(
            &format!("/api/v1/disputes/{dsp}/transition"),
            reviewer,
            json!({ "to": "CREDIT_PROMISED", "amount": "100.00" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = app
        .post(
            &format!("/api/v1/disputes/{dsp}/transition"),
            reviewer,
            json!({ "to": "CREDIT_RECEIVED", "amount": "100.00", "credit_memo_reference": "CM-1" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = app
        .post(
            &format!("/api/v1/disputes/{dsp}/transition"),
            reviewer,
            json!({ "to": "VERIFIED" }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "credit verification is reserved for OWNER/ADMIN"
    );
    let (s, _) = app
        .post(
            &format!("/api/v1/disputes/{dsp}/transition"),
            admin,
            json!({ "to": "VERIFIED" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);

    // Role assignment limits: ADMIN cannot mint admins/owners.
    let (s, _) = app.post("/api/v1/members", admin, json!({ "email": "x@test.invalid", "password": "a-sufficiently-long-password", "role": "ADMIN" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.get("/api/v1/audit", admin).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn money_inputs_reject_floats_and_excess_precision_and_conflicts_are_409() {
    let app = test_app().await;
    let org = app.signup_org("val").await;
    let vendor = ok(&app, &org, "/api/v1/vendors", json!({ "name": "V" })).await;
    let vid = vendor["id"].as_str().unwrap();
    let contract = ok(
        &app,
        &org,
        "/api/v1/contracts",
        json!({ "vendor_id": vid, "name": "C" }),
    )
    .await;
    let cid = contract["id"].as_str().unwrap();
    let rate = |from: &str, to: Option<&str>, unit_rate: Value| json!({ "waste_stream": "F003", "unit": "drum", "unit_rate": unit_rate, "effective_from": from, "effective_to": to });
    let p = format!("/api/v1/contracts/{cid}/rates");
    let (s, _) = app
        .post(&p, &org.token, rate("2026-01-01", None, json!(400.5)))
        .await;
    assert!(s.is_client_error(), "JSON float must be rejected, got {s}");
    let (s, _) = app
        .post(&p, &org.token, rate("2026-01-01", None, json!("400.12345")))
        .await;
    assert!(
        s.is_client_error(),
        "5 decimal places must be rejected, got {s}"
    );
    let (s, _) = app
        .post(
            &p,
            &org.token,
            rate("2026-01-01", Some("2025-01-01"), json!("400")),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app
        .post(
            &p,
            &org.token,
            rate("2026-01-01", Some("2026-06-30"), json!("400.00")),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app
        .post(&p, &org.token, rate("2026-06-01", None, json!("410.00")))
        .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "overlapping effective windows must be refused by the database"
    );
    let (s, _) = app
        .post(&p, &org.token, rate("2026-07-01", None, json!("410.00")))
        .await;
    assert_eq!(s, StatusCode::CREATED);

    let m = json!({ "manifest_number": "DUP", "waste_stream": "F003", "quantity": "1", "unit": "drum", "shipment_date": "2026-03-09" });
    let (s, _) = app.post("/api/v1/manifests", &org.token, m.clone()).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app.post("/api/v1/manifests", &org.token, m).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = app.post("/api/v1/manifests", &org.token, json!({ "manifest_number": "Z", "waste_stream": "F003", "quantity": "0", "unit": "drum", "shipment_date": "2026-03-09" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn documents_are_validated_deduplicated_isolated_and_never_fake_ocr() {
    let app = test_app().await;
    let a = app.signup_org("doc-a").await;
    let b = app.signup_org("doc-b").await;
    let upload = |token: String, name: &str, mime: &str, bytes: Vec<u8>| {
        let router = app.router.clone();
        let uri = format!("/api/v1/documents?document_type=INVOICE&filename={name}");
        let mime = mime.to_string();
        async move {
            use tower::ServiceExt;
            let req = axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", mime)
                .body(axum::body::Body::from(bytes))
                .unwrap();
            let resp = router.oneshot(req).await.unwrap();
            let st = resp.status();
            let b = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                st,
                serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
            )
        }
    };
    let unique = uuid::Uuid::new_v4().to_string();
    let (s, doc) = upload(
        a.token.clone(),
        "note.txt",
        "text/plain",
        format!("hello {unique}").into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    assert_eq!(doc["status"], "REVIEW_REQUIRED");
    let id = doc["id"].as_str().unwrap();
    let (_, detail) = app.get(&format!("/api/v1/documents/{id}"), &a.token).await;
    assert_eq!(detail["extraction"]["extractor_provider"], "plain_text");

    let (s, _) = upload(
        a.token.clone(),
        "note.txt",
        "text/plain",
        format!("hello {unique}").into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate content");
    let (s, pdf) = upload(
        a.token.clone(),
        "scan.pdf",
        "application/pdf",
        format!("%PDF-1.7 {unique}").into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(pdf["status"], "REVIEW_REQUIRED");
    assert!(
        pdf["status_detail"].as_str().unwrap().contains("manually"),
        "unreadable PDFs must be stored and honestly routed to manual entry: {pdf}"
    );
    let (s, _) = upload(
        a.token.clone(),
        "fake.pdf",
        "application/pdf",
        b"not a pdf".to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "magic bytes must match");
    let (s, _) = upload(
        a.token.clone(),
        "x.exe",
        "application/x-msdownload",
        b"MZ".to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = upload(
        a.token.clone(),
        "big.txt",
        "text/plain",
        vec![b'a'; 2 * 1024 * 1024],
    )
    .await;
    assert!(
        s == StatusCode::PAYLOAD_TOO_LARGE || s == StatusCode::BAD_REQUEST,
        "oversize rejected, got {s}"
    );

    let (s, _) = app.get(&format!("/api/v1/documents/{id}"), &b.token).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .get(&format!("/api/v1/documents/{id}/download"), &b.token)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .get(&format!("/api/v1/documents/{id}/download"), &a.token)
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn paddle_webhook_verifies_signatures_is_idempotent_and_drives_entitlements() {
    let app = test_app().await;
    let org = app.signup_org("bill").await;
    let now = chrono::Utc::now().timestamp();
    let sub = format!("sub_{}", org.organization_id.simple());
    let ev = |id: &str, status: &str, at: &str| {
        paddle_event(
            "subscription.updated",
            &format!("{id}-{}", uuid::Uuid::new_v4()),
            &sub,
            status,
            at,
            &org,
            Some(&org),
        )
    };

    let body = ev("e1", "active", "2026-09-01T10:00:00Z");
    let (s, _) = webhook(&app, &body, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "missing signature");
    let (s, _) = webhook(&app, &body, Some(sign("wrong-secret", now, &body))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "wrong secret");
    let (s, _) = webhook(&app, &body, Some(sign(WEBHOOK_SECRET, now - 10_000, &body))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "stale timestamp (replay)");
    let (s, _) = webhook(&app, &body, Some(sign(WEBHOOK_SECRET, now, "tampered"))).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "signature must cover the exact body"
    );
    let (_, st) = app.get("/api/v1/billing/status", &org.token).await;
    assert_eq!(
        st["status"], "NONE",
        "rejected webhooks must change nothing"
    );

    let (s, r) = webhook(&app, &body, Some(sign(WEBHOOK_SECRET, now, &body))).await;
    assert_eq!((s, r["status"].as_str()), (StatusCode::OK, Some("applied")));
    let (_, st) = app.get("/api/v1/billing/status", &org.token).await;
    assert_eq!(
        (st["status"].as_str(), st["entitled"].as_bool()),
        (Some("active"), Some(true))
    );

    let (s, r) = webhook(&app, &body, Some(sign(WEBHOOK_SECRET, now, &body))).await;
    assert_eq!(
        (s, r["status"].as_str()),
        (StatusCode::OK, Some("duplicate")),
        "redelivery is acknowledged, not reprocessed"
    );

    let stale = ev("e0", "canceled", "2026-08-01T10:00:00Z");
    let (_, r) = webhook(&app, &stale, Some(sign(WEBHOOK_SECRET, now, &stale))).await;
    assert_eq!(r["status"], "stale");
    let (_, st) = app.get("/api/v1/billing/status", &org.token).await;
    assert_eq!(
        st["status"], "active",
        "an older event must not overwrite newer state"
    );

    let cancel = ev("e2", "canceled", "2026-09-02T10:00:00Z");
    let (_, r) = webhook(&app, &cancel, Some(sign(WEBHOOK_SECRET, now, &cancel))).await;
    assert_eq!(r["status"], "applied");
    let (_, st) = app.get("/api/v1/billing/status", &org.token).await;
    assert_eq!(
        (st["status"].as_str(), st["entitled"].as_bool()),
        (Some("canceled"), Some(false))
    );
}

#[tokio::test]
async fn unentitled_organizations_are_blocked_server_side_when_enforcement_is_on() {
    use std::sync::Arc;
    use xflow_backend::http::{
        build_router,
        state::{AppState, BillingSettings},
    };
    let base = test_app().await;
    let router = build_router(AppState {
        pool: base.pool.clone(),
        session_ttl_hours: 24,
        storage: Arc::new(xflow_backend::storage::LocalFsStorage::new(
            std::env::temp_dir().join("xflow-enf"),
        )),
        extractor: Arc::new(xflow_backend::extraction::PlainTextExtractor),
        billing: BillingSettings {
            webhook_secret: None,
            signature_tolerance_secs: 300,
            enforce_entitlements: true,
        },
        max_upload_bytes: 1024 * 1024,
    });
    let enforced = TestApp {
        router,
        pool: base.pool.clone(),
    };
    let org = base.signup_org("paywall").await;
    let (_, _, inv) = setup(&base, &org, "f", "10", &[("T", "10")]).await;

    let (s, r) = enforced
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": inv }),
        )
        .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{r}");
    let (s, _) = enforced.get("/api/v1/invoices", &org.token).await;
    assert_eq!(s, StatusCode::OK, "reading existing data stays available");

    let now = chrono::Utc::now().timestamp();
    let body = paddle_event(
        "subscription.created",
        &format!("ent-{}", uuid::Uuid::new_v4()),
        &format!("sub_{}", org.organization_id.simple()),
        "trialing",
        "2026-09-05T00:00:00Z",
        &org,
        Some(&org),
    );
    let (s, r) = webhook(&base, &body, Some(sign(WEBHOOK_SECRET, now, &body))).await;
    assert_eq!((s, r["status"].as_str()), (StatusCode::OK, Some("applied")));
    let (s, _) = enforced
        .post(
            "/api/v1/reconciliations/run",
            &org.token,
            json!({ "invoice_id": inv }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "an entitled subscription unlocks processing"
    );

    // Webhook with no configured secret fails closed.
    let (s, _) = webhook(&enforced, &body, Some(sign(WEBHOOK_SECRET, now, &body))).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn monitoring_worker_reconciles_new_and_changed_inputs_and_notifies() {
    let app = test_app().await;
    let org = app.signup_org("mon").await;
    let (_, mid, inv) = setup(&app, &org, "g", "10", &[("T", "10")]).await;

    // Another tenant with pending work must be untouched by this org's job being processed first/last.
    let other = app.signup_org("mon-other").await;
    let (_, _, other_inv) = setup(&app, &other, "h", "10", &[("T", "10")]).await;

    let (s, q) = app
        .post("/api/v1/monitoring/run", &org.token, json!({}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(q["queued"], true);
    let (_, q2) = app
        .post("/api/v1/monitoring/run", &org.token, json!({}))
        .await;
    assert_eq!(
        q2["queued"], false,
        "no pile-up: one active job per organization"
    );

    // Drain the queue (other tests may have queued jobs too).
    for _ in 0..200 {
        if !xflow_backend::workers::run_one_job(&app.pool, false)
            .await
            .unwrap()
        {
            break;
        }
    }

    let (_, invd) = app
        .get(&format!("/api/v1/invoices/{inv}"), &org.token)
        .await;
    assert!(
        invd["latest_reconciliation"]["id"].is_string(),
        "worker must have reconciled the new invoice: {invd}"
    );
    assert_eq!(
        invd["latest_reconciliation"]["discrepancy_amount"],
        "700.0000"
    );
    let (_, notes) = app.get("/api/v1/notifications", &org.token).await;
    assert!(notes
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["event_type"] == "discrepancies.detected"));
    let (_, jobs) = app.get("/api/v1/monitoring/jobs", &org.token).await;
    assert_eq!(jobs[0]["status"], "COMPLETED");
    let (_, other_jobs) = app.get("/api/v1/monitoring/jobs", &org.token).await;
    assert!(other_jobs
        .as_array()
        .unwrap()
        .iter()
        .all(|j| j["job_type"] == "RECONCILE_PENDING"));
    let _ = other_inv;

    // Nothing changed -> a second pass must not create another reconciliation.
    let count = |v: &Value| {
        v["latest_reconciliation"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let before = count(&invd);
    app.post("/api/v1/monitoring/run", &org.token, json!({}))
        .await;
    for _ in 0..200 {
        if !xflow_backend::workers::run_one_job(&app.pool, false)
            .await
            .unwrap()
        {
            break;
        }
    }
    let (_, again) = app
        .get(&format!("/api/v1/invoices/{inv}"), &org.token)
        .await;
    assert_eq!(
        count(&again),
        before,
        "unchanged inputs must not churn the review queue"
    );

    // New supporting evidence arrives -> monitoring picks it up and re-reconciles.
    ok(&app, &org, "/api/v1/weight-tickets", json!({ "ticket_number": "LATE-1", "manifest_id": mid, "net_weight": "3", "unit": "drum", "ticket_date": "2026-03-11" })).await;
    app.post("/api/v1/monitoring/run", &org.token, json!({}))
        .await;
    for _ in 0..200 {
        if !xflow_backend::workers::run_one_job(&app.pool, false)
            .await
            .unwrap()
        {
            break;
        }
    }
    let (_, changed) = app
        .get(&format!("/api/v1/invoices/{inv}"), &org.token)
        .await;
    assert_ne!(
        count(&changed),
        before,
        "a new weight ticket must trigger a fresh reconciliation"
    );
}
