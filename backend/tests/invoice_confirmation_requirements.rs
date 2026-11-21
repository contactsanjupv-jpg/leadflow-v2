//! Step 15b-3: `GET /documents/:id/invoice-candidates` gains an additive `confirmation_requirements` field.
//! The endpoint stays read-only and its existing fields keep their meaning. SYNTHETIC invoices.
//! NOT COMPILED OR RUN by the author.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

fn row(d: &str, f: &str, q: &str, u: &str, p: &str, a: &str) -> String {
    format!("{d:<28}{f:<13}{q:>6}  {u:<9}{p:>13}{a:>13}")
}

fn invoice_text(extra: &str, with_totals: bool) -> String {
    let tag = Uuid::new_v4().simple().to_string();
    let mut v = vec![
        format!("{:<62}INVOICE", "Vendor: Evergreen Environmental"),
        format!("Invoice Number: INV-{tag}              Invoice Date: 03/15/2026"),
        "Service Date:   03/20/2026".to_string(),
        "Waste Stream:   F003 Spent Solvents".to_string(),
        String::new(),
        row("Description", "Fee Code", "Qty", "Unit", "Unit Price", "Amount"),
        row("Drum disposal", "", "10", "drum", "400.00", "4,000.00"),
        row("Transportation", "TRANSPORT", "1", "flat", "500.00", "500.00"),
        String::new(),
    ];
    if with_totals {
        v.push("                                          Subtotal                        4,500.00".to_string());
        v.push("                                          Total Due                       4,500.00".to_string());
    }
    if !extra.is_empty() {
        v.push(String::new());
        v.push(String::new());
        v.push(extra.to_string());
    }
    v.join("\n")
}

async fn upload(app: &TestApp, token: &str, doc_type: &str, mime: &str, bytes: Vec<u8>) -> Value {
    let (s, _, body) = app
        .call_raw(Method::POST, &format!("/api/v1/documents?document_type={doc_type}&filename=f.bin"), Some(token), Some(mime), bytes)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

async fn doc_with(app: &TestApp, token: &str, text: String) -> String {
    upload(app, token, "INVOICE", "text/plain", text.into_bytes()).await["id"].as_str().unwrap().to_string()
}

fn path(id: &str) -> String {
    format!("/api/v1/documents/{id}/invoice-candidates")
}

async fn get_ok(app: &TestApp, token: &str, id: &str) -> Value {
    let (s, v) = app.get(&path(id), token).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn keys(v: &Value) -> Vec<String> {
    v["confirmation_requirements"]["acknowledgments_required"].as_array().unwrap().iter().map(|k| k.as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn existing_response_fields_are_unchanged_and_the_new_field_is_additive() {
    let app = test_app().await;
    let org = app.signup_org("cr-compat").await;
    let id = doc_with(&app, &org.token, invoice_text("", true)).await;
    let v = get_ok(&app, &org.token, &id).await;
    let mut top: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    top.sort();
    assert_eq!(top, ["candidates", "confirmation_requirements", "document_id", "document_status", "extraction", "linked_invoice_id", "notice", "parser_version", "reviewable", "vendor_suggestion"]);
    assert_eq!(v["parser_version"], "invoice-rules-3", "the parser is untouched");
    assert_eq!(v["candidates"]["acknowledgments_required"], json!(["lines[0].fee_code"]), "the parser's own list is unchanged");
    assert_eq!(v["candidates"]["checks"]["line_sum_complete"], true);
    assert_eq!(v["confirmation_requirements"], json!({ "acknowledgments_required": ["lines[0].fee_code"], "original_document_check_required": false }));
    assert_eq!(v, get_ok(&app, &org.token, &id).await, "still deterministic");
}

#[tokio::test]
async fn unparsed_unreadable_tax_and_uncorroborated_cases_are_derived_by_the_server() {
    let app = test_app().await;
    let org = app.signup_org("cr-derive").await;
    for (label, extra, totals) in [
        ("charge-like", "Emergency response fee                         150.00", true),
        ("unreadable", "Emergency response fee                 $4,50.00", true),
        ("tax", "Sales Tax 8.25%", true),
    ] {
        let text = invoice_text(extra, totals);
        let n = text.lines().position(|l| l.contains(extra)).unwrap() + 1;
        let id = doc_with(&app, &org.token, text).await;
        let v = get_ok(&app, &org.token, &id).await;
        let k = keys(&v);
        assert!(k.contains(&format!("unparsed_lines[{n}]")), "{label}: {k:?}");
        assert!(k.contains(&"line_sum_incomplete".to_string()), "{label}: {k:?}");
        assert_eq!(v["confirmation_requirements"]["original_document_check_required"], true, "{label}");
    }
    let id = doc_with(&app, &org.token, invoice_text("", false)).await;
    let v = get_ok(&app, &org.token, &id).await;
    assert!(keys(&v).contains(&"line_sum_incomplete".to_string()));
    assert!(keys(&v).iter().all(|k| !k.starts_with("unparsed_lines[")));
}

#[tokio::test]
async fn a_truncated_extraction_adds_text_truncated() {
    let app = test_app().await;
    let org = app.signup_org("cr-trunc").await;
    let id = doc_with(&app, &org.token, invoice_text("", true)).await;
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)").bind(org.organization_id.to_string()).execute(&mut *tx).await.unwrap();
    let n = sqlx::query("UPDATE document_extractions SET raw_extraction = jsonb_set(raw_extraction, '{truncated}', 'true') WHERE document_id = $1")
        .bind(id.parse::<Uuid>().unwrap()).execute(&mut *tx).await.unwrap().rows_affected();
    assert_eq!(n, 1, "the test needs UPDATE on document_extractions for xflow_app");
    tx.commit().await.unwrap();
    let v = get_ok(&app, &org.token, &id).await;
    assert_eq!(v["extraction"]["text_truncated"], true);
    let k = keys(&v);
    assert!(k.contains(&"text_truncated".to_string()) && k.contains(&"line_sum_incomplete".to_string()), "{k:?}");
    assert_eq!(v["confirmation_requirements"]["original_document_check_required"], true);
}

#[tokio::test]
async fn more_unparsed_lines_than_the_cap_add_not_listed() {
    let app = test_app().await;
    let org = app.signup_org("cr-cap").await;
    let text: String = (0..620).map(|i| format!("Item {i}      1      {i}.50\n")).collect();
    let id = doc_with(&app, &org.token, text).await;
    let v = get_ok(&app, &org.token, &id).await;
    let k = keys(&v);
    assert!(k.contains(&"unparsed_not_listed".to_string()));
    assert_eq!(k.iter().filter(|x| x.starts_with("unparsed_lines[")).count(), 500);
    assert_eq!(v["candidates"]["unparsed_not_listed"], 120);
    assert_eq!(v["confirmation_requirements"]["original_document_check_required"], true);
}

#[tokio::test]
async fn the_endpoint_stays_read_only_tenant_isolated_and_readable_by_a_reviewer() {
    let app = test_app().await;
    let a = app.signup_org("cr-ro-a").await;
    let b = app.signup_org("cr-ro-b").await;
    let id = doc_with(&app, &a.token, invoice_text("Emergency response fee                         150.00", true)).await;
    get_ok(&app, &a.token, &id).await;
    // nothing was created or changed
    assert_eq!(app.get("/api/v1/invoices", &a.token).await.1.as_array().unwrap().len(), 0);
    assert_eq!(app.get(&format!("/api/v1/documents/{id}"), &a.token).await.1["status"], "REVIEW_REQUIRED");
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)").bind(a.organization_id.to_string()).execute(&mut *tx).await.unwrap();
    let reviews: i64 = sqlx::query_scalar("SELECT count(*) FROM invoice_extraction_reviews").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(reviews, 0, "no provenance row is written by a GET (needs migration 0015)");
    drop(tx);
    // another tenant, unauthenticated, reviewer
    let (s, v) = app.get(&path(&id), &b.token).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert!(!v.to_string().contains("confirmation_requirements"));
    let (s, _, _) = app.call_raw(Method::GET, &path(&id), None, None, vec![]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let email = format!("reviewer-{}@test.invalid", Uuid::new_v4());
    let (st, _) = app.post("/api/v1/members", &a.token, json!({ "email": email, "password": "a-sufficiently-long-password", "role": "REVIEWER" })).await;
    assert_eq!(st, StatusCode::CREATED);
    let (_, login) = app.call(Method::POST, "/api/v1/auth/login", None, Some(json!({ "email": email, "password": "a-sufficiently-long-password" }))).await;
    let reviewer = login["session_token"].as_str().unwrap().to_string();
    let v = get_ok(&app, &reviewer, &id).await;
    assert!(v["confirmation_requirements"]["acknowledgments_required"].is_array());
}
