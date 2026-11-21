//! Step 15b-2: the shared invoice-creation path (manual `POST /invoices`) after the refactor.
//! Needs migration 0015 applied to the test database (the source-document index). NOT COMPILED OR RUN by the
//! author. Documents and invoices are SYNTHETIC.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

async fn upload_doc(app: &TestApp, token: &str) -> String {
    let (s, _, body) = app
        .call_raw(
            Method::POST,
            "/api/v1/documents?document_type=INVOICE&filename=inv.txt",
            Some(token),
            Some("text/plain"),
            format!("Invoice text {}", Uuid::new_v4()).into_bytes(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "REVIEW_REQUIRED");
    v["id"].as_str().unwrap().to_string()
}

async fn vendor(app: &TestApp, token: &str) -> String {
    let (s, v) = app.post("/api/v1/vendors", token, json!({ "name": "Evergreen Environmental" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["id"].as_str().unwrap().to_string()
}

fn body(vendor: &str, number: &str, source: Option<&str>) -> Value {
    let mut b = json!({
        "vendor_id": vendor, "invoice_number": number, "invoice_date": "2026-03-15", "service_date": "2026-03-20",
        "waste_stream": "F003", "total": "4500.00",
        "line_items": [
            { "description": "Drum disposal", "fee_code": "DISP", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
            { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }
        ]
    });
    if let Some(d) = source {
        b["source_document_id"] = json!(d);
    }
    b
}

async fn invoice_count(app: &TestApp, token: &str) -> usize {
    app.get("/api/v1/invoices", token).await.1.as_array().unwrap().len()
}

async fn doc_status(app: &TestApp, token: &str, doc: &str) -> String {
    app.get(&format!("/api/v1/documents/{doc}"), token).await.1["status"].as_str().unwrap().to_string()
}

async fn audit_creates(app: &TestApp, org: &TestOrg) -> i64 {
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)").bind(org.organization_id.to_string()).execute(&mut *tx).await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = 'invoice.create'").fetch_one(&mut *tx).await.unwrap()
}

#[tokio::test]
async fn manual_creation_keeps_its_response_shape_status_and_audit() {
    let app = test_app().await;
    let org = app.signup_org("sl-shape").await;
    let v = vendor(&app, &org.token).await;
    let doc = upload_doc(&app, &org.token).await;
    let before = audit_creates(&app, &org).await;
    let (s, inv) = app.post("/api/v1/invoices", &org.token, body(&v, "SL-1", Some(&doc))).await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    let mut keys: Vec<&str> = inv.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["created_at", "facility_id", "id", "invoice_date", "invoice_number", "latest_reconciliation", "lines", "manifest_id", "service_date", "source_document_id", "total", "updated_at", "vendor_id", "waste_stream"]);
    assert_eq!(inv["source_document_id"], doc.as_str());
    assert_eq!(inv["total"], "4500.0000");
    assert_eq!(inv["lines"].as_array().unwrap().len(), 2);
    assert_eq!(doc_status(&app, &org.token, &doc).await, "PROCESSED");
    assert_eq!(audit_creates(&app, &org).await, before + 1, "exactly one invoice.create audit row");
    // an invoice with no source document is unaffected by the new index
    for n in ["SL-2", "SL-3"] {
        assert_eq!(app.post("/api/v1/invoices", &org.token, body(&v, n, None)).await.0, StatusCode::CREATED);
    }
}

#[tokio::test]
async fn a_second_invoice_for_one_source_document_is_a_409_with_its_own_message() {
    let app = test_app().await;
    let org = app.signup_org("sl-second").await;
    let v = vendor(&app, &org.token).await;
    let doc = upload_doc(&app, &org.token).await;
    assert_eq!(app.post("/api/v1/invoices", &org.token, body(&v, "SL-10", Some(&doc))).await.0, StatusCode::CREATED);
    let (s, e) = app.post("/api/v1/invoices", &org.token, body(&v, "SL-11", Some(&doc))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    let msg = e["error"]["message"].as_str().unwrap();
    assert!(msg.contains("source document"), "{msg}");
    assert!(!msg.contains("already has an invoice with that number"), "{msg}");
    assert_eq!(invoice_count(&app, &org.token).await, 1);
}

#[tokio::test]
async fn the_duplicate_vendor_and_number_conflict_is_unchanged_and_rolls_the_document_back() {
    let app = test_app().await;
    let org = app.signup_org("sl-dupnum").await;
    let v = vendor(&app, &org.token).await;
    let (d1, d2) = (upload_doc(&app, &org.token).await, upload_doc(&app, &org.token).await);
    assert_eq!(app.post("/api/v1/invoices", &org.token, body(&v, "SL-20", Some(&d1))).await.0, StatusCode::CREATED);
    let (s, e) = app.post("/api/v1/invoices", &org.token, body(&v, "SL-20", Some(&d2))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("this vendor already has an invoice with that number"), "{e}");
    assert_eq!(doc_status(&app, &org.token, &d2).await, "REVIEW_REQUIRED", "the failed create left d2 untouched");
    assert_eq!(app.post("/api/v1/invoices", &org.token, body(&v, "SL-20", None)).await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn unknown_and_foreign_source_documents_are_still_404() {
    let app = test_app().await;
    let a = app.signup_org("sl-404-a").await;
    let b = app.signup_org("sl-404-b").await;
    let va = vendor(&app, &a.token).await;
    let foreign = upload_doc(&app, &b.token).await;
    assert_eq!(app.post("/api/v1/invoices", &a.token, body(&va, "SL-30", Some(&foreign))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(app.post("/api/v1/invoices", &a.token, body(&va, "SL-30", Some(&Uuid::new_v4().to_string()))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(invoice_count(&app, &a.token).await, 0);
    assert_eq!(doc_status(&app, &b.token, &foreign).await, "REVIEW_REQUIRED");
}

#[tokio::test]
async fn concurrent_manual_creations_for_one_document_produce_one_invoice_and_no_server_error() {
    let app = test_app().await;
    let org = app.signup_org("sl-conc").await;
    let v = vendor(&app, &org.token).await;
    for round in 0..6 {
        let doc = upload_doc(&app, &org.token).await;
        let (a, b, c) = tokio::join!(
            app.post("/api/v1/invoices", &org.token, body(&v, &format!("C{round}-A"), Some(&doc))),
            app.post("/api/v1/invoices", &org.token, body(&v, &format!("C{round}-B"), Some(&doc))),
            app.post("/api/v1/invoices", &org.token, body(&v, &format!("C{round}-C"), Some(&doc))),
        );
        let st = [a.0, b.0, c.0];
        assert!(!st.contains(&StatusCode::INTERNAL_SERVER_ERROR), "round {round}: {st:?}");
        assert_eq!(st.iter().filter(|s| **s == StatusCode::CREATED).count(), 1, "round {round}: {st:?}");
        assert_eq!(st.iter().filter(|s| **s == StatusCode::CONFLICT).count(), 2, "round {round}: {st:?}");
    }
    assert_eq!(invoice_count(&app, &org.token).await, 6);
}

/// The lock-order contract. A second transaction, written the way the confirmation path will work (lock the
/// document row FIRST, then insert the invoice), runs against a manual create of the same document.
/// Without the document lock in the manual path, the manual create inserts its invoice and then waits on the
/// document row, while the other transaction waits on the manual create's unique-index entry: a deadlock that
/// surfaces after about a second as a 40P01 / 500. With the lock, the manual create simply waits, the other
/// transaction commits, and the manual create gets a clean 409.
#[tokio::test]
async fn a_manual_create_racing_a_document_first_transaction_waits_instead_of_deadlocking() {
    let app = test_app().await;
    let org = app.signup_org("sl-lock").await;
    let v = vendor(&app, &org.token).await;
    let doc = upload_doc(&app, &org.token).await;
    let (doc_id, vendor_id) = (doc.parse::<Uuid>().unwrap(), v.parse::<Uuid>().unwrap());

    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)").bind(org.organization_id.to_string()).execute(&mut *tx).await.unwrap();
    sqlx::query("SELECT id FROM documents WHERE id = $1 FOR UPDATE").bind(doc_id).fetch_one(&mut *tx).await.unwrap();

    let manual = app.post("/api/v1/invoices", &org.token, body(&v, "SL-LOCK-MANUAL", Some(&doc)));
    let org_id = org.organization_id;
    let other = async move {
        tokio::time::sleep(Duration::from_millis(500)).await; // give the manual request time to reach its insert
        let ins = sqlx::query("INSERT INTO invoices (organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total, source_document_id)
                               VALUES ($1, $2, 'SL-LOCK-OTHER', '2026-03-15', '2026-03-20', 'F003', 1, $3)")
            .bind(org_id).bind(vendor_id).bind(doc_id).execute(&mut *tx).await;
        if ins.is_err() {
            return ins.map(|_| ());
        }
        sqlx::query("UPDATE documents SET status = 'PROCESSED' WHERE id = $1").bind(doc_id).execute(&mut *tx).await?;
        tx.commit().await
    };
    let (m, o) = tokio::join!(manual, other);
    assert!(o.is_ok(), "the document-first transaction was a deadlock victim or failed: {o:?}");
    assert_eq!(m.0, StatusCode::CONFLICT, "the manual create must lose cleanly, not 500: {}", m.1);
    assert!(m.1["error"]["message"].as_str().unwrap().contains("source document"), "{}", m.1);
    assert_eq!(invoice_count(&app, &org.token).await, 1);
    assert_eq!(doc_status(&app, &org.token, &doc).await, "PROCESSED");
}
