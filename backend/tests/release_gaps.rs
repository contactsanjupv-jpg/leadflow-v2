//! Release-gap tests: RULE_05/RULE_08 through the real workflow, PDF evidence output, Paddle subscription
//! lifecycle simulations (signed events through the real webhook endpoint), checkout context security,
//! and the document extraction / manual-fallback path.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};

async fn ok(app: &TestApp, org: &TestOrg, path: &str, body: Value) -> Value {
    let (s, v) = app.post(path, &org.token, body).await;
    assert!(s.is_success(), "POST {path} -> {s}: {v}");
    v
}

async fn vendor_contract(app: &TestApp, org: &TestOrg, tag: &str) -> String {
    let v = ok(app, org, "/api/v1/vendors", json!({ "name": format!("V {tag}") })).await;
    let vid = v["id"].as_str().unwrap().to_string();
    let c = ok(app, org, "/api/v1/contracts", json!({ "vendor_id": vid, "name": "Master" })).await;
    ok(app, org, &format!("/api/v1/contracts/{}/rates", c["id"].as_str().unwrap()), json!({
        "waste_stream": "F003", "unit": "drum", "unit_rate": "400.00", "effective_from": "2026-01-01",
        "authorized_fees": [{ "fee_code": "TRANSPORT", "amount": "500.00" }] })).await;
    vid
}

fn invoice_body(vendor_id: &str, number: &str, stream: &str, manifest_id: Option<&str>) -> Value {
    json!({
        "vendor_id": vendor_id, "invoice_number": number, "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": stream,
        "manifest_id": manifest_id, "total": "4500.00",
        "line_items": [
            { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }]
    })
}

#[tokio::test]
async fn rule_05_flags_a_charge_repeated_from_an_earlier_invoice_and_totals_reconcile() {
    let app = test_app().await;
    let org = app.signup_org("dup").await;
    let vid = vendor_contract(&app, &org, "dup").await;
    let m = ok(&app, &org, "/api/v1/manifests", json!({ "manifest_number": "M-D", "waste_stream": "F003", "container_count": 10, "quantity": "10", "unit": "drum", "shipment_date": "2026-03-09" })).await;
    let mid = m["id"].as_str().unwrap();
    ok(&app, &org, "/api/v1/weight-tickets", json!({ "ticket_number": "T-D", "manifest_id": mid, "net_weight": "10", "unit": "drum", "ticket_date": "2026-03-10" })).await;

    let first = ok(&app, &org, "/api/v1/invoices", invoice_body(&vid, "INV-ORIG", "F003", Some(mid))).await;
    let run1 = ok(&app, &org, "/api/v1/reconciliations/run", json!({ "invoice_id": first["id"] })).await;
    assert_eq!(run1["discrepancy_amount"], "0.0000", "a correct invoice has no findings: {run1}");
    assert!(run1["discrepancies"].as_array().unwrap().is_empty());

    // The vendor bills the identical charges again on a second invoice number.
    let second = ok(&app, &org, "/api/v1/invoices", invoice_body(&vid, "INV-AGAIN", "F003", Some(mid))).await;
    let run2 = ok(&app, &org, "/api/v1/reconciliations/run", json!({ "invoice_id": second["id"] })).await;
    let dups: Vec<&Value> = run2["discrepancies"].as_array().unwrap().iter().filter(|d| d["rule_id"] == "RULE_05_DUPLICATE_CHARGE").collect();
    assert_eq!(dups.len(), 2, "both repeated lines must be flagged: {run2}");
    assert!(dups.iter().all(|d| d["description"].as_str().unwrap().contains("INV-ORIG")));
    assert_eq!(run2["discrepancy_amount"], "4500.0000");
    assert_eq!(run2["expected_amount"], "0.0000", "duplicate lines are excluded from the expected amount");
    assert_eq!(run2["invoiced_amount"], "4500.0000");
    // The earlier invoice is not accused of duplicating the later one.
    let rerun1 = ok(&app, &org, "/api/v1/reconciliations/run", json!({ "invoice_id": first["id"] })).await;
    assert_eq!(rerun1["discrepancy_amount"], "0.0000");
}

#[tokio::test]
async fn rule_08_flags_an_invoice_stream_that_differs_from_the_manifest() {
    let app = test_app().await;
    let org = app.signup_org("stream").await;
    let vid = vendor_contract(&app, &org, "stream").await;
    // Rate exists for F003; the manifest documents D001 while the invoice bills F003.
    let m = ok(&app, &org, "/api/v1/manifests", json!({ "manifest_number": "M-S", "waste_stream": "D001", "container_count": 10, "quantity": "10", "unit": "drum", "shipment_date": "2026-03-09" })).await;
    let inv = ok(&app, &org, "/api/v1/invoices", invoice_body(&vid, "INV-S", "F003", Some(m["id"].as_str().unwrap()))).await;
    let run = ok(&app, &org, "/api/v1/reconciliations/run", json!({ "invoice_id": inv["id"] })).await;
    let d = run["discrepancies"].as_array().unwrap().iter().find(|d| d["rule_id"] == "RULE_08_WASTE_STREAM_MISMATCH").expect("RULE_08 must fire");
    assert_eq!(d["amount"], "0.0000");
    assert!(d["description"].as_str().unwrap().contains("D001") && d["description"].as_str().unwrap().contains("F003"));
    // It is a normal reviewable row.
    let (_, list) = app.get("/api/v1/discrepancies", &org.token).await;
    assert!(list.as_array().unwrap().iter().any(|x| x["rule_id"] == "RULE_08_WASTE_STREAM_MISMATCH"));
}

#[tokio::test]
async fn evidence_pdf_is_a_valid_pdf_with_exact_amounts_and_is_tenant_isolated() {
    let app = test_app().await;
    let a = app.signup_org("pdf-a").await;
    let b = app.signup_org("pdf-b").await;
    let vid = vendor_contract(&app, &a, "pdf").await;
    let mut body = invoice_body(&vid, "INV-PDF-1", "F003", None);
    body["total"] = json!("5200.00");
    body["line_items"][0]["unit_price"] = json!("450.00");
    body["line_items"][0]["line_total"] = json!("4500.00");
    body["line_items"].as_array_mut().unwrap().push(json!({ "description": "Environmental fee", "fee_code": "ENV_FEE", "quantity": "1", "unit": "flat", "unit_price": "200.00", "line_total": "200.00" }));
    let inv = ok(&app, &a, "/api/v1/invoices", body).await;
    let run = ok(&app, &a, "/api/v1/reconciliations/run", json!({ "invoice_id": inv["id"] })).await;
    let did = run["discrepancy_ids"][0].as_str().unwrap().to_string();

    // No package yet -> 404, not an empty PDF.
    let (s, _, _) = app.call_raw(Method::GET, &format!("/api/v1/discrepancies/{did}/evidence/pdf"), Some(&a.token), None, vec![]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    ok(&app, &a, &format!("/api/v1/discrepancies/{did}/evidence"), json!({})).await;

    let (s, headers, pdf) = app.call_raw(Method::GET, &format!("/api/v1/discrepancies/{did}/evidence/pdf"), Some(&a.token), None, vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/pdf");
    assert!(headers["content-disposition"].to_str().unwrap().contains("evidence-INV-PDF-1-"));
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(pdf.starts_with(b"%PDF-1.4"));

    if have_pdftotext() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("xflow-evidence-{}.pdf", uuid::Uuid::new_v4()));
        std::fs::File::create(&path).unwrap().write_all(&pdf).unwrap();
        let out = std::process::Command::new("pdftotext").args(["-layout"]).arg(&path).arg("-").output().unwrap();
        assert!(out.status.success(), "pdftotext must be able to open the generated PDF");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        for needle in ["Evidence Package", "INV-PDF-1", "$4,500.00", "$5,200.00", "$700.00", "RULE_0", "Drum disposal", "Environmental fee", "$400.00", "TRANSPORT"] {
            assert!(text.contains(needle), "extracted PDF text is missing {needle:?}\n{text}");
        }
        if let Ok(q) = std::process::Command::new("qpdf").arg("--check").arg(&path).output() {
            assert!(q.status.success(), "qpdf --check failed: {}", String::from_utf8_lossy(&q.stdout));
        }
        let _ = std::fs::remove_file(path);
    } else {
        eprintln!("SKIP external PDF validation: pdftotext not installed");
    }

    let (s, _, _) = app.call_raw(Method::GET, &format!("/api/v1/discrepancies/{did}/evidence/pdf"), Some(&b.token), None, vec![]).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "another organization must not download this PDF");
    let (s, _, _) = app.call_raw(Method::GET, &format!("/api/v1/discrepancies/{did}/evidence/pdf"), None, None, vec![]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ------------------------------------------------------------- Paddle lifecycle

#[tokio::test]
async fn paddle_subscription_lifecycle_drives_entitlement_through_signed_webhooks() {
    let app = test_app().await;
    let org = app.signup_org("life").await;
    let sub = format!("sub_{}", org.organization_id.simple());
    let ev = |kind: &str, id: &str, status: &str, at: &str| paddle_event(kind, &format!("{id}-{}", uuid::Uuid::new_v4()), &sub, status, at, &org, Some(&org));
    let status = |app: &TestApp, token: &str| { let (app, token) = (app.router.clone(), token.to_string()); async move {
        use tower::ServiceExt;
        let req = axum::http::Request::builder().uri("/api/v1/billing/status").header("authorization", format!("Bearer {token}")).body(axum::body::Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let b = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice::<Value>(&b).unwrap()
    } };

    assert_eq!(status(&app, &org.token).await["entitled"], false, "a new organization has no entitlement");

    let steps: [(&str, &str, &str, bool); 6] = [
        ("subscription.created", "trialing", "2026-10-01T10:00:00Z", true),
        ("subscription.activated", "active", "2026-10-02T10:00:00Z", true),
        ("subscription.past_due", "past_due", "2026-10-20T10:00:00Z", true),   // dunning keeps access
        ("subscription.updated", "active", "2026-10-21T10:00:00Z", true),      // payment recovered
        ("subscription.paused", "paused", "2026-10-25T10:00:00Z", false),
        ("subscription.canceled", "canceled", "2026-10-30T10:00:00Z", false),
    ];
    for (kind, st, at, entitled) in steps {
        let (s, r) = send_signed(&app, &ev(kind, "e", st, at)).await;
        assert_eq!((s, r["status"].as_str()), (StatusCode::OK, Some("applied")), "{kind}: {r}");
        let cur = status(&app, &org.token).await;
        assert_eq!((cur["status"].as_str(), cur["entitled"].as_bool()), (Some(st), Some(entitled)), "after {kind}: {cur}");
    }
    // A late re-activation event that is OLDER than the cancellation must not resurrect access.
    let (_, r) = send_signed(&app, &ev("subscription.activated", "late", "active", "2026-10-03T00:00:00Z")).await;
    assert_eq!(r["status"], "stale");
    assert_eq!(status(&app, &org.token).await["entitled"], false);
    // Non-subscription notifications are stored and ignored.
    let txn = json!({ "event_id": format!("txn-{}", uuid::Uuid::new_v4()), "event_type": "transaction.completed", "occurred_at": "2026-10-31T00:00:00Z", "data": { "id": "txn_1" } }).to_string();
    let (s, r) = send_signed(&app, &txn).await;
    assert_eq!((s, r["status"].as_str()), (StatusCode::OK, Some("ignored")));
    // A returning customer can subscribe again after cancelling (new subscription id, old one not entitled).
    let again = paddle_event("subscription.created", &format!("re-{}", uuid::Uuid::new_v4()), &format!("sub_rebuy_{}", uuid::Uuid::new_v4().simple()), "active", "2026-11-02T00:00:00Z", &org, Some(&org));
    let (_, r) = send_signed(&app, &again).await;
    assert_eq!(r["status"], "applied");
    assert_eq!(status(&app, &org.token).await["entitled"], true);
}

#[tokio::test]
async fn forged_organization_links_cannot_grant_or_revoke_another_organizations_access() {
    let app = test_app().await;
    let victim = app.signup_org("victim").await;
    let attacker = app.signup_org("attacker").await;
    let vsub = format!("sub_v{}", victim.organization_id.simple());

    // Victim legitimately subscribes.
    let (_, r) = send_signed(&app, &paddle_event("subscription.activated", &format!("v-{}", uuid::Uuid::new_v4()), &vsub, "active", "2026-10-01T00:00:00Z", &victim, Some(&victim))).await;
    assert_eq!(r["status"], "applied");

    // Attacker starts a checkout claiming the victim's organization id but cannot produce its signature
    // (their own signature is for THEIR organization, and an absent signature is rejected too).
    for sig_for in [Some(&attacker), None] {
        let forged = paddle_event("subscription.canceled", &format!("f-{}", uuid::Uuid::new_v4()), &format!("sub_attacker_{}", uuid::Uuid::new_v4().simple()), "canceled", "2026-10-05T00:00:00Z", &victim, sig_for);
        let (s, r) = send_signed(&app, &forged).await;
        assert_eq!((s, r["status"].as_str()), (StatusCode::OK, Some("unmapped")), "forged link must not be applied: {r}");
    }
    let (_, st) = app.get("/api/v1/billing/status", &victim.token).await;
    assert_eq!((st["status"].as_str(), st["entitled"].as_bool()), (Some("active"), Some(true)), "victim access is untouched");

    // Even a correctly SIGNED event for the victim org from a DIFFERENT subscription id cannot replace an
    // active subscription (protects against a second checkout hijacking the row).
    let other = paddle_event("subscription.canceled", &format!("o-{}", uuid::Uuid::new_v4()), &format!("sub_other_{}", uuid::Uuid::new_v4().simple()), "canceled", "2026-10-06T00:00:00Z", &victim, Some(&victim));
    let (_, r) = send_signed(&app, &other).await;
    assert_eq!(r["status"], "conflict");
    let (_, st) = app.get("/api/v1/billing/status", &victim.token).await;
    assert_eq!(st["entitled"], true);

    // The attacker's own org was never granted anything.
    let (_, st) = app.get("/api/v1/billing/status", &attacker.token).await;
    assert_eq!(st["entitled"], false);
}

#[tokio::test]
async fn checkout_context_is_owner_admin_only_bound_to_the_callers_org_and_verifiable() {
    let app = test_app().await;
    let owner = app.signup_org("ctx").await;
    let (s, ctx) = app.get("/api/v1/billing/checkout-context", &owner.token).await;
    assert_eq!(s, StatusCode::OK, "{ctx}");
    assert_eq!(ctx["organization_id"], json!(owner.organization_id));
    assert!(ctx["email"].as_str().unwrap().contains('@'));
    assert!(xflow_backend::billing::verify_checkout_signature(WEBHOOK_SECRET, owner.organization_id, ctx["organization_sig"].as_str().unwrap()));
    assert!(!xflow_backend::billing::verify_checkout_signature(WEBHOOK_SECRET, uuid::Uuid::new_v4(), ctx["organization_sig"].as_str().unwrap()));

    let email = format!("m-{}@test.invalid", uuid::Uuid::new_v4());
    ok(&app, &owner, "/api/v1/members", json!({ "email": email, "password": "a-sufficiently-long-password", "role": "MEMBER" })).await;
    let (_, login) = app.call(Method::POST, "/api/v1/auth/login", None, Some(json!({ "email": email, "password": "a-sufficiently-long-password" }))).await;
    let (s, _) = app.get("/api/v1/billing/checkout-context", login["session_token"].as_str().unwrap()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "members cannot start a subscription");
    let (s, _) = app.call(Method::GET, "/api/v1/billing/checkout-context", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ------------------------------------------------------------------- documents

fn upload_path(doc_type: &str, name: &str) -> String {
    format!("/api/v1/documents?document_type={doc_type}&filename={name}")
}

#[tokio::test]
async fn text_layer_pdfs_are_extracted_and_scans_and_images_fall_back_to_manual_entry() {
    let app = test_app().await;
    let org = app.signup_org("docx").await;
    let unique = uuid::Uuid::new_v4().to_string();

    // 1) A born-digital PDF with a text layer.
    let mut b = xflow_backend::evidence_pdf::PdfBuilder::new("manifest");
    b.title(&format!("Hazardous waste manifest M-77 {unique}"));
    b.row("Quantity 10 drums waste stream F003");
    let digital = b.finish();
    let (s, _, body) = app.call_raw(Method::POST, &upload_path("MANIFEST", "manifest.pdf"), Some(&org.token), Some("application/pdf"), digital).await;
    let doc: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    assert_eq!(doc["status"], "REVIEW_REQUIRED", "extracted text still needs a person to confirm the data");
    if have_pdftotext() {
        let (_, detail) = app.get(&format!("/api/v1/documents/{}", doc["id"].as_str().unwrap()), &org.token).await;
        assert_eq!(detail["extraction"]["extractor_provider"], "pdftotext");
        assert!(detail["extraction"]["raw_extraction"]["text"].as_str().unwrap().contains("M-77"));
    }

    // 2) A PDF with no text layer (scan-like) and a PNG: stored, flagged, with a manual-entry reason; no extraction row.
    let blank_pdf = format!("%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\n% {unique}\ntrailer<</Root 1 0 R/Size 4>>\n%%EOF\n").into_bytes();
    let png = { let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]; v.extend_from_slice(unique.as_bytes()); v };
    let mut scan_ids = Vec::new();
    for (name, mime, bytes) in [("scan.pdf", "application/pdf", blank_pdf), ("photo.png", "image/png", png)] {
        let (s, _, body) = app.call_raw(Method::POST, &upload_path("INVOICE", name), Some(&org.token), Some(mime), bytes).await;
        let d: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(s, StatusCode::CREATED, "{name}: {d}");
        assert_eq!(d["status"], "REVIEW_REQUIRED", "{name}");
        assert!(d["status_detail"].as_str().unwrap().contains("manually"), "{name}: {d}");
        let (_, detail) = app.get(&format!("/api/v1/documents/{}", d["id"].as_str().unwrap()), &org.token).await;
        assert!(detail["extraction"].is_null(), "{name}: nothing may be fabricated when extraction is unsupported");
        scan_ids.push(d["id"].as_str().unwrap().to_string());
    }

    // 3) Manual fallback: enter the invoice linking the scanned document -> the document becomes PROCESSED.
    let vid = vendor_contract(&app, &org, "docx").await;
    let mut inv = invoice_body(&vid, "INV-FROM-SCAN", "F003", None);
    inv["source_document_id"] = json!(scan_ids[0]);
    ok(&app, &org, "/api/v1/invoices", inv).await;
    let (_, d0) = app.get(&format!("/api/v1/documents/{}", scan_ids[0]), &org.token).await;
    assert_eq!(d0["status"], "PROCESSED");
    assert!(d0["status_detail"].as_str().unwrap().contains("linked to an invoice"));

    // 4) Explicit resolve for a document that needs no record; only legal from REVIEW_REQUIRED/FAILED.
    let (s, r) = app.post(&format!("/api/v1/documents/{}/resolve", scan_ids[1]), &org.token, json!({ "note": "photo of unrelated yard" })).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "PROCESSED");
    let (s, _) = app.post(&format!("/api/v1/documents/{}/resolve", scan_ids[1]), &org.token, json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT, "an already-processed document cannot be re-resolved");

    // 5) Isolation + RBAC on the new endpoint.
    let other = app.signup_org("docx-other").await;
    let (s, _) = app.post(&format!("/api/v1/documents/{}/resolve", scan_ids[0]), &other.token, json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app.post(&format!("/api/v1/documents/{}/resolve", scan_ids[0]), "bad", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
