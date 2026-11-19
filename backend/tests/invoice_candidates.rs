//! Step 15a: `GET /api/v1/documents/:id/invoice-candidates`.
//!
//! The endpoint is read-only and advisory. These tests drive the real router and database (RLS active) and
//! prove that reading candidates creates no financial record, never changes the document, respects tenancy and
//! RBAC, and reports scans/non-invoices as manual-entry cases. The invoices used are SYNTHETIC.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};

fn row(d: &str, f: &str, q: &str, u: &str, p: &str, a: &str) -> String {
    format!("{d:<28}{f:<13}{q:>6}  {u:<9}{p:>13}{a:>13}")
}

/// A synthetic born-digital invoice laid out like `pdftotext -layout` output. `tag` keeps the bytes (and so the
/// SHA-256 the upload de-duplicates on) unique per test run.
fn invoice_text(tag: &str, vendor_line: &str) -> String {
    [
        format!("{vendor_line:<62}INVOICE"),
        "1200 Industrial Way, Houston, TX 77001".to_string(),
        format!("Invoice Number: INV-{tag}              Invoice Date: 03/15/2026"),
        "Service Date:   03/20/2026                 Terms: Net 30".to_string(),
        "Waste Stream:   F003 Spent Solvents".to_string(),
        String::new(),
        row(
            "Description",
            "Fee Code",
            "Qty",
            "Unit",
            "Unit Price",
            "Amount",
        ),
        row("Drum disposal", "", "10", "drum", "400.00", "4,000.00"),
        row(
            "Transportation",
            "TRANSPORT",
            "1",
            "flat",
            "500.00",
            "500.00",
        ),
        String::new(),
        "                                          Subtotal                        4,500.00"
            .to_string(),
        "                                          Total Due                       4,500.00"
            .to_string(),
    ]
    .join("\n")
}

async fn upload(
    app: &TestApp,
    token: &str,
    doc_type: &str,
    name: &str,
    mime: &str,
    bytes: Vec<u8>,
) -> (StatusCode, Value) {
    let (s, _, body) = app
        .call_raw(
            Method::POST,
            &format!("/api/v1/documents?document_type={doc_type}&filename={name}"),
            Some(token),
            Some(mime),
            bytes,
        )
        .await;
    (s, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

async fn upload_text_invoice(app: &TestApp, org: &TestOrg, vendor_line: &str) -> String {
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let (s, doc) = upload(
        app,
        &org.token,
        "INVOICE",
        "inv.txt",
        "text/plain",
        invoice_text(&tag, vendor_line).into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    assert_eq!(doc["status"], "REVIEW_REQUIRED");
    doc["id"].as_str().unwrap().to_string()
}

fn candidates_path(id: &str) -> String {
    format!("/api/v1/documents/{id}/invoice-candidates")
}

#[tokio::test]
async fn candidates_are_returned_with_status_evidence_and_a_notice_and_create_nothing() {
    let app = test_app().await;
    let org = app.signup_org("cand-basic").await;
    let id = upload_text_invoice(&app, &org, "Evergreen Environmental Services, Inc.").await;

    let (s, v) = app.get(&candidates_path(&id), &org.token).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["parser_version"], "invoice-rules-3");
    assert_eq!(v["candidates"]["parser_version"], "invoice-rules-3");
    assert_eq!(v["document_id"], id.as_str());
    assert_eq!(v["document_status"], "REVIEW_REQUIRED");
    assert_eq!(v["reviewable"], true);
    assert!(v["linked_invoice_id"].is_null());
    assert_eq!(v["extraction"]["provider"], "plain_text");
    assert_eq!(v["extraction"]["text_truncated"], false);
    assert!(v["notice"]
        .as_str()
        .unwrap()
        .contains("Nothing has been created"));
    assert!(
        v.get("confidence").is_none() && v["candidates"].get("confidence").is_none(),
        "no confidence score"
    );

    let c = &v["candidates"];
    assert_eq!(c["invoice_date"]["value"], "2026-03-15");
    assert_eq!(c["invoice_date"]["status"], "extracted");
    assert_eq!(c["service_date"]["value"], "2026-03-20");
    assert_eq!(c["waste_stream"]["value"], "F003 Spent Solvents");
    assert_eq!(c["invoice_total"]["value"], "4500.00");
    assert_eq!(c["invoice_total"]["evidence"]["line"], 12);
    assert_eq!(c["vendor_name"]["status"], "uncertain");
    assert_eq!(c["lines"].as_array().unwrap().len(), 2);
    assert_eq!(c["lines"][0]["blank_fee_code"], true);
    assert_eq!(c["lines"][1]["fee_code"]["value"], "TRANSPORT");
    assert_eq!(c["checks"]["total_matches_lines"], true);
    assert_eq!(c["checks"]["line_sum_complete"], true);
    assert_eq!(
        c["checks"]["completeness_basis"],
        "the invoice total and a subtotal both equal the sum of the parsed line totals"
    );
    assert_eq!(c["checks"]["incompleteness_reasons"], json!([]));
    assert_eq!(c["unparsed_lines"], json!([]));
    assert_eq!(
        c["acknowledgments_required"],
        json!(["vendor_name", "lines[0].fee_code"])
    );

    // Reading candidates is deterministic and has no side effects.
    let (_, again) = app.get(&candidates_path(&id), &org.token).await;
    assert_eq!(
        v, again,
        "the same document gives byte-identical candidates"
    );
    let (_, invoices) = app.get("/api/v1/invoices", &org.token).await;
    assert_eq!(
        invoices.as_array().unwrap().len(),
        0,
        "no invoice was created"
    );
    let (_, doc) = app
        .get(&format!("/api/v1/documents/{id}"), &org.token)
        .await;
    assert_eq!(
        doc["status"], "REVIEW_REQUIRED",
        "the document is untouched"
    );
}

#[tokio::test]
async fn a_malformed_extraction_is_reported_not_repaired_through_the_endpoint() {
    let app = test_app().await;
    let org = app.signup_org("cand-bad").await;
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let text = invoice_text(&tag, "Acme Hazmat LLC")
        .replace("03/15/2026", "03/09/2026")
        .replace("4,000.00", "4,00.00");
    let (s, doc) = upload(
        &app,
        &org.token,
        "INVOICE",
        "bad.txt",
        "text/plain",
        text.into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    let (s, v) = app
        .get(&candidates_path(doc["id"].as_str().unwrap()), &org.token)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = &v["candidates"];
    assert_eq!(c["invoice_date"]["status"], "uncertain");
    assert_eq!(
        c["invoice_date"]["alternatives"],
        json!(["2026-03-09", "2026-09-03"])
    );
    assert_eq!(c["lines"][0]["line_total"]["status"], "uncertain");
    assert!(c["lines"][0]["line_total"]["value"].is_null());
    let acks = c["acknowledgments_required"].as_array().unwrap();
    assert!(acks.contains(&json!("invoice_date")) && acks.contains(&json!("lines[0].line_total")));
}

#[tokio::test]
async fn scans_images_and_non_invoice_documents_have_no_candidates() {
    let app = test_app().await;
    let org = app.signup_org("cand-none").await;

    // a PDF with no text layer: stored, REVIEW_REQUIRED, no extraction -> manual entry
    let mut pdf = xflow_backend::evidence_pdf::PdfBuilder::new("blank").finish();
    pdf.extend_from_slice(format!("\n% {}\n", uuid::Uuid::new_v4()).as_bytes());
    // an image is never extracted
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let (s, img) = upload(&app, &org.token, "INVOICE", "scan.png", "image/png", png).await;
    assert_eq!(s, StatusCode::CREATED, "{img}");
    let (s, v) = app
        .get(&candidates_path(img["id"].as_str().unwrap()), &org.token)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.to_string().contains("no extracted text"), "{v}");

    // a text-bearing document that is not an invoice
    let (s, man) = upload(
        &app,
        &org.token,
        "MANIFEST",
        "m.txt",
        "text/plain",
        format!("Manifest {}", uuid::Uuid::new_v4()).into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{man}");
    let (s, v) = app
        .get(&candidates_path(man["id"].as_str().unwrap()), &org.token)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.to_string().contains("INVOICE"), "{v}");

    // unknown id
    let (s, _) = app
        .get(
            &candidates_path(&uuid::Uuid::new_v4().to_string()),
            &org.token,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // not a uuid
    let (s, _) = app
        .get(
            "/api/v1/documents/not-a-uuid/invoice-candidates",
            &org.token,
        )
        .await;
    assert!(
        s == StatusCode::BAD_REQUEST
            || s == StatusCode::UNPROCESSABLE_ENTITY
            || s == StatusCode::NOT_FOUND,
        "{s}"
    );
}

#[tokio::test]
async fn candidates_are_tenant_isolated_authenticated_and_readable_by_a_reviewer() {
    let app = test_app().await;
    let a = app.signup_org("cand-a").await;
    let b = app.signup_org("cand-b").await;
    let id = upload_text_invoice(&app, &a, "Evergreen Environmental Services, Inc.").await;

    let (s, v) = app.get(&candidates_path(&id), &b.token).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "another tenant must not see the document or its text: {v}"
    );
    assert!(!v.to_string().contains("INV-"));

    let (s, _, _) = app
        .call_raw(Method::GET, &candidates_path(&id), None, None, vec![])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let email = format!("reviewer-{}@test.invalid", uuid::Uuid::new_v4());
    let (st, _) = app
        .post("/api/v1/members", &a.token, json!({ "email": email, "password": "a-sufficiently-long-password", "role": "REVIEWER" }))
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
    let (s, v) = app.get(&candidates_path(&id), &reviewer).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "a reviewer may view documents and so their candidates: {v}"
    );
}

#[tokio::test]
async fn a_document_already_linked_to_an_invoice_reports_it_and_is_not_reviewable() {
    let app = test_app().await;
    let org = app.signup_org("cand-linked").await;
    let id = upload_text_invoice(&app, &org, "Evergreen Environmental Services, Inc.").await;
    let (s, vendor) = app
        .post(
            "/api/v1/vendors",
            &org.token,
            json!({ "name": "Evergreen Environmental Services, Inc." }),
        )
        .await;
    assert!(s.is_success(), "{vendor}");
    let (s, inv) = app
        .post(
            "/api/v1/invoices",
            &org.token,
            json!({
                "vendor_id": vendor["id"], "invoice_number": "MANUAL-1", "invoice_date": "2026-03-15", "service_date": "2026-03-20",
                "waste_stream": "F003", "total": "4500.00", "source_document_id": id,
                "line_items": [
                    { "description": "Drum disposal", "quantity": "10", "unit": "drum", "unit_price": "400.00", "line_total": "4000.00" },
                    { "description": "Transportation", "fee_code": "TRANSPORT", "quantity": "1", "unit": "flat", "unit_price": "500.00", "line_total": "500.00" }
                ]
            }),
        )
        .await;
    assert!(s.is_success(), "{inv}");

    let (s, v) = app.get(&candidates_path(&id), &org.token).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["document_status"], "PROCESSED");
    assert_eq!(v["linked_invoice_id"], inv["id"]);
    assert_eq!(v["reviewable"], false);
    // defence in depth: even if the status were somehow REVIEW_REQUIRED again, a linked document is not reviewable
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)")
        .bind(org.organization_id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE documents SET status = 'REVIEW_REQUIRED' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&id).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (_, w) = app.get(&candidates_path(&id), &org.token).await;
    assert_eq!(w["document_status"], "REVIEW_REQUIRED");
    assert_eq!(w["linked_invoice_id"], inv["id"]);
    assert_eq!(
        w["reviewable"], false,
        "a document that already backs an invoice must not be offered for review"
    );
    // and the existing manual path is unchanged: one invoice, created only by the explicit POST above
    let (_, invoices) = app.get("/api/v1/invoices", &org.token).await;
    assert_eq!(invoices.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_vendor_is_suggested_only_on_an_exact_name_match_and_never_created() {
    let app = test_app().await;
    let org = app.signup_org("cand-vendor").await;
    let id = upload_text_invoice(&app, &org, "Vendor: Evergreen Environmental").await;
    let (_, none) = app.get(&candidates_path(&id), &org.token).await;
    assert!(none["vendor_suggestion"].is_null(), "no vendors exist yet");
    let (_, vendors) = app.get("/api/v1/vendors", &org.token).await;
    assert_eq!(
        vendors.as_array().unwrap().len(),
        0,
        "reading candidates never creates a vendor"
    );

    let (s, vendor) = app
        .post(
            "/api/v1/vendors",
            &org.token,
            json!({ "name": "  EVERGREEN environmental " }),
        )
        .await;
    assert!(s.is_success(), "{vendor}");
    let (_, v) = app.get(&candidates_path(&id), &org.token).await;
    assert_eq!(v["vendor_suggestion"]["vendor_id"], vendor["id"]);
    assert_eq!(v["vendor_suggestion"]["match"], "exact_name");
    assert_eq!(
        v["candidates"]["vendor_name"]["status"], "extracted",
        "an explicit Vendor: label is extracted"
    );

    // a near-miss is not suggested, even though another vendor's name merely STARTS with the candidate
    let (s, west) = app
        .post(
            "/api/v1/vendors",
            &org.token,
            json!({ "name": "Evergreen Environmental Services West" }),
        )
        .await;
    assert!(s.is_success(), "{west}");
    let id2 = upload_text_invoice(&app, &org, "Vendor: Evergreen Environmental Services").await;
    let (_, w) = app.get(&candidates_path(&id2), &org.token).await;
    assert!(w["vendor_suggestion"].is_null());
}

#[tokio::test]
async fn a_born_digital_pdf_goes_through_pdftotext_into_candidates() {
    if !have_pdftotext() {
        eprintln!("pdftotext not installed; skipping");
        return;
    }
    let app = test_app().await;
    let org = app.signup_org("cand-pdf").await;
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let mut b = xflow_backend::evidence_pdf::PdfBuilder::new("invoice");
    for line in invoice_text(&tag, "Evergreen Environmental Services, Inc.").lines() {
        b.row(line);
    }
    let (s, doc) = upload(
        &app,
        &org.token,
        "INVOICE",
        "inv.pdf",
        "application/pdf",
        b.finish(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    let id = doc["id"].as_str().unwrap();
    let (_, full) = app
        .get(&format!("/api/v1/documents/{id}"), &org.token)
        .await;
    assert_eq!(
        full["extraction"]["extractor_provider"], "pdftotext",
        "{full}"
    );
    let (s, v) = app.get(&candidates_path(id), &org.token).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = &v["candidates"];
    assert_eq!(c["invoice_number"]["value"], format!("INV-{tag}"), "{c}");
    assert_eq!(c["invoice_total"]["value"], "4500.00", "{c}");
    assert_eq!(c["invoice_date"]["value"], "2026-03-15", "{c}");
    // the table survives pdftotext's column layout: both rows, exact amounts, the blank fee-code cell kept blank
    assert_eq!(c["lines"].as_array().unwrap().len(), 2, "{c}");
    assert_eq!(
        c["lines"][0]["description"]["value"], "Drum disposal",
        "{c}"
    );
    assert_eq!(c["lines"][0]["blank_fee_code"], true, "{c}");
    assert_eq!(c["lines"][0]["line_total"]["value"], "4000.00", "{c}");
    assert_eq!(c["lines"][1]["fee_code"]["value"], "TRANSPORT", "{c}");
    assert_eq!(c["checks"]["total_matches_lines"], true, "{c}");
    assert_eq!(c["unparsed_lines"].as_array().unwrap().len(), 0, "{c}");
}

#[tokio::test]
async fn tax_and_late_charges_stay_visible_and_the_sum_is_not_called_complete_through_the_endpoint()
{
    let app = test_app().await;
    let org = app.signup_org("cand-tax").await;
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let text = invoice_text(&tag, "Acme Hazmat LLC")
        .replace(
            "                                          Subtotal                        4,500.00",
            "                                          Subtotal                        4,500.00\n                                          Sales Tax (8.25%)                 371.25",
        )
        .replace("Total Due                       4,500.00", "Total Due                       4,871.25")
        + "\n\nEmergency response fee                         150\nLate fee  $25.00\n";
    let (s, doc) = upload(
        &app,
        &org.token,
        "INVOICE",
        "tax.txt",
        "text/plain",
        text.into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    let (s, v) = app
        .get(&candidates_path(doc["id"].as_str().unwrap()), &org.token)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = &v["candidates"];
    let kinds: Vec<&str> = c["unparsed_lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"tax"), "{kinds:?} {c}");
    assert!(kinds.contains(&"possible_charge"), "{kinds:?}");
    assert!(kinds.contains(&"charge_like"), "{kinds:?}");
    assert_eq!(c["checks"]["line_sum_complete"], false);
    assert!(c["checks"]["completeness_basis"].is_null());
    let why = c["checks"]["incompleteness_reasons"].to_string();
    assert!(
        why.contains("tax row(s)") && why.contains("may be charges"),
        "{why}"
    );
    assert_eq!(
        c["invoice_total"]["status"], "uncertain",
        "the total no longer equals the parsed lines"
    );
}

#[tokio::test]
async fn a_malformed_charge_amount_stays_visible_through_the_endpoint_and_blocks_completeness() {
    let app = test_app().await;
    let org = app.signup_org("cand-malformed").await;
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let text = invoice_text(&tag, "Acme Hazmat LLC") + "\n\nEmergency response fee                 $4,50.00\nSales Tax                                          $36,0.00\n";
    let (s, doc) = upload(
        &app,
        &org.token,
        "INVOICE",
        "mal.txt",
        "text/plain",
        text.into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    let (s, v) = app
        .get(&candidates_path(doc["id"].as_str().unwrap()), &org.token)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = &v["candidates"];
    assert_eq!(
        c["checks"]["total_matches_lines"], true,
        "lines and total match"
    );
    assert_eq!(c["checks"]["line_sum_complete"], false);
    let rows = c["unparsed_lines"].as_array().unwrap();
    let charge = rows
        .iter()
        .find(|u| u["text"].as_str().unwrap().contains("Emergency response"))
        .expect("charge row kept");
    assert_eq!(charge["kind"], "unreadable_amount");
    assert!(charge["text"].as_str().unwrap().contains("$4,50.00"));
    let tax = rows
        .iter()
        .find(|u| u["text"].as_str().unwrap().contains("Sales Tax"))
        .expect("tax row kept");
    assert_eq!(tax["kind"], "tax");
    assert!(tax["reason"]
        .as_str()
        .unwrap()
        .contains("could not be read"));
}

#[tokio::test]
async fn single_spaced_rate_only_tax_and_subtotal_rows_stay_visible_through_the_endpoint() {
    let app = test_app().await;
    let org = app.signup_org("cand-rate").await;
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let text = invoice_text(&tag, "Acme Hazmat LLC")
        + "\n\nSales Tax 8.25%\nSubtotal 8.25%\nDiscount 10%\n";
    let (s, doc) = upload(
        &app,
        &org.token,
        "INVOICE",
        "rate.txt",
        "text/plain",
        text.into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{doc}");
    let (s, v) = app
        .get(&candidates_path(doc["id"].as_str().unwrap()), &org.token)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["parser_version"], "invoice-rules-3");
    let c = &v["candidates"];
    let rows = c["unparsed_lines"].as_array().unwrap();
    let find = |needle: &str| rows.iter().find(|u| u["text"].as_str().unwrap() == needle);
    let tax = find("Sales Tax 8.25%").expect("tax row kept");
    assert_eq!(tax["kind"], "tax");
    assert!(tax["reason"].as_str().unwrap().contains("NOT included"));
    assert_eq!(
        find("Subtotal 8.25%").expect("subtotal row kept")["kind"],
        "subtotal"
    );
    assert!(
        find("Discount 10%").is_none(),
        "an ordinary percentage line is not a tax row"
    );
    assert_eq!(c["checks"]["line_sum_complete"], false);
}
