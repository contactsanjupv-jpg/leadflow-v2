//! Document upload + extraction. Upload is a raw-body POST (`Content-Type` = the file's MIME
//! type) with `document_type` and `filename` query parameters, so no multipart dependency is needed.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::audit;
use crate::extraction::ExtractionOutcome;
use crate::http::authed_user::AuthedUser;
use crate::http::errors::AppError;
use crate::http::state::AppState;
use crate::http::util::*;
use crate::rbac::Permission;

const ALLOWED_TYPES: [&str; 6] = [
    "INVOICE",
    "CONTRACT",
    "RATE_SHEET",
    "MANIFEST",
    "WEIGHT_TICKET",
    "OTHER",
];
const ALLOWED_MIME: [&str; 6] = [
    "application/pdf",
    "image/png",
    "image/jpeg",
    "text/plain",
    "text/csv",
    "image/tiff",
];

#[derive(Deserialize)]
pub struct UploadQuery {
    pub document_type: String,
    pub filename: String,
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    cleaned.trim().chars().take(200).collect()
}

const DOC_SQL: &str = "SELECT to_jsonb(d) FROM (SELECT id, document_type::text AS document_type, original_filename, mime_type, file_size_bytes, sha256, status::text AS status, status_detail, created_at FROM documents WHERE id = $1) d";

pub async fn upload_document(
    user: AuthedUser,
    State(state): State<AppState>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), AppError> {
    user.require(Permission::ManageData)?;
    let document_type = q.document_type.to_ascii_uppercase();
    if !ALLOWED_TYPES.contains(&document_type.as_str()) {
        return Err(AppError::Validation("document_type must be one of INVOICE, CONTRACT, RATE_SHEET, MANIFEST, WEIGHT_TICKET, OTHER".into()));
    }
    let filename = safe_filename(&q.filename);
    if filename.is_empty() {
        return Err(AppError::Validation("filename is required".into()));
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !ALLOWED_MIME.contains(&mime.as_str()) {
        return Err(AppError::Validation(
            "unsupported content type; allowed: pdf, png, jpeg, tiff, plain text, csv".into(),
        ));
    }
    if body.is_empty() {
        return Err(AppError::Validation("file is empty".into()));
    }
    if body.len() > state.max_upload_bytes {
        return Err(AppError::PayloadTooLarge);
    }
    // Content sniffing for the binary types: the declared type must match the magic bytes.
    let magic_ok = match mime.as_str() {
        "application/pdf" => body.starts_with(b"%PDF-"),
        "image/png" => body.starts_with(&[0x89, b'P', b'N', b'G']),
        "image/jpeg" => body.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/tiff" => {
            body.starts_with(&[b'I', b'I', 0x2A, 0x00])
                || body.starts_with(&[b'M', b'M', 0x00, 0x2A])
        }
        _ => true,
    };
    if !magic_ok {
        return Err(AppError::Validation(
            "file content does not match its declared content type".into(),
        ));
    }
    let digest: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let id = Uuid::new_v4();
    let key = format!("{}/{}", user.organization_id, id);
    let mut tx = scoped_tx(&state, &user).await?;
    let dup: Option<Uuid> = sqlx::query_scalar("SELECT id FROM documents WHERE sha256 = $1")
        .bind(&digest)
        .fetch_optional(&mut *tx)
        .await?;
    if dup.is_some() {
        return Err(AppError::Conflict(
            "this exact file has already been uploaded".into(),
        ));
    }
    state
        .storage
        .put(&key, &body)
        .await
        .map_err(AppError::Internal)?;
    sqlx::query("INSERT INTO documents (id, organization_id, uploaded_by, document_type, source, object_storage_key, original_filename, mime_type, file_size_bytes, sha256, status)
                 VALUES ($1, $2, $3, $4::document_type, 'UPLOAD', $5, $6, $7, $8, $9, 'PROCESSING')")
        .bind(id).bind(user.organization_id).bind(user.user_id).bind(&document_type).bind(&key).bind(&filename).bind(&mime).bind(body.len() as i64).bind(&digest)
        .execute(&mut *tx).await?;

    // Extraction. Results are stored for human review only and never create financial records.
    let (status, detail) = match state.extractor.extract(&mime, &body).await {
        Ok(ExtractionOutcome::Extracted { provider, raw, confidence }) => {
            sqlx::query("INSERT INTO document_extractions (document_id, organization_id, extractor_provider, raw_extraction, confidence) VALUES ($1, $2, $3, $4, $5)")
                .bind(id).bind(user.organization_id).bind(provider).bind(raw).bind(confidence).execute(&mut *tx).await?;
            ("REVIEW_REQUIRED", Some("text extracted; a person must enter and confirm the structured data".to_string()))
        }
        Ok(ExtractionOutcome::Unsupported { reason }) => ("REVIEW_REQUIRED", Some(reason)),
        Ok(ExtractionOutcome::NotApplicable) => ("REVIEW_REQUIRED", Some("no extraction provider handles this file type; enter the data manually and link this document as its source".to_string())),
        Err(e) => {
            tracing::error!(error = %e, "document extraction failed");
            ("FAILED", Some("extraction failed".to_string()))
        }
    };
    sqlx::query("UPDATE documents SET status = $2::document_status, status_detail = $3, updated_at = now() WHERE id = $1").bind(id).bind(status).bind(detail).execute(&mut *tx).await?;
    audit::record(&mut tx, user.organization_id, Some(user.user_id), "document.upload", "document", Some(id), None,
        Some(json!({ "document_type": document_type, "filename": filename, "size": body.len(), "sha256": digest }))).await?;
    let out = json_one(&mut tx, DOC_SQL, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)))
}

pub async fn list_documents(
    user: AuthedUser,
    State(state): State<AppState>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.created_at DESC, t.id), '[]'::jsonb) FROM (
           SELECT id, document_type::text AS document_type, original_filename, mime_type, file_size_bytes, status::text AS status, status_detail, created_at FROM documents ORDER BY created_at DESC, id LIMIT $1 OFFSET $2) t",
    ).bind(page.limit()).bind(page.offset()).fetch_one(&mut *tx).await?;
    Ok(Json(v))
}

pub async fn get_document(
    user: AuthedUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let mut doc = json_one(&mut tx, DOC_SQL, id).await?;
    let extraction: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(e) FROM (SELECT extractor_provider, raw_extraction, confidence::text AS confidence, created_at FROM document_extractions WHERE document_id = $1 ORDER BY created_at DESC LIMIT 1) e")
        .bind(id).fetch_optional(&mut *tx).await?;
    doc["extraction"] = extraction.unwrap_or(Value::Null);
    Ok(Json(doc))
}

pub async fn download_document(
    user: AuthedUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    user.require(Permission::ViewData)?;
    let mut tx = scoped_tx(&state, &user).await?;
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT object_storage_key, mime_type, original_filename FROM documents WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (key, mime, filename) = row.ok_or(AppError::NotFound)?;
    drop(tx);
    let bytes = state.storage.get(&key).await.map_err(AppError::Internal)?;
    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', ""));
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_DISPOSITION, disposition),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub note: Option<String>,
}

/// Manual fallback: after a person has entered the data from a document that could not be (fully)
/// extracted, mark it PROCESSED. Linking the document as `source_document_id` on an invoice, manifest,
/// weight ticket or contract does this automatically (`mark_used`); this endpoint covers documents
/// that need no record. Allowed only from REVIEW_REQUIRED or FAILED.
pub async fn resolve_document(
    user: AuthedUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ResolveRequest>,
) -> Result<Json<Value>, AppError> {
    user.require(Permission::ManageData)?;
    let note = req
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(500).collect::<String>());
    let mut tx = scoped_tx(&state, &user).await?;
    let cur: Option<String> =
        sqlx::query_scalar("SELECT status::text FROM documents WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let cur = cur.ok_or(AppError::NotFound)?;
    if cur != "REVIEW_REQUIRED" && cur != "FAILED" {
        return Err(AppError::Conflict(format!(
            "a {cur} document does not need manual processing"
        )));
    }
    let detail = format!(
        "processed manually{}",
        note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
    );
    sqlx::query("UPDATE documents SET status = 'PROCESSED', status_detail = $2, updated_at = now() WHERE id = $1").bind(id).bind(&detail).execute(&mut *tx).await?;
    audit::record(
        &mut tx,
        user.organization_id,
        Some(user.user_id),
        "document.resolve",
        "document",
        Some(id),
        Some(json!({ "status": cur })),
        Some(json!({ "status": "PROCESSED", "note": note })),
    )
    .await?;
    let out = json_one(&mut tx, DOC_SQL, id).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Called by record-creating endpoints when a `source_document_id` is linked: a document awaiting manual
/// entry is now accounted for. Does not touch documents in any other state.
pub async fn mark_used(tx: &mut Tx, document_id: Uuid, label: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE documents SET status = 'PROCESSED', status_detail = $2, updated_at = now() WHERE id = $1 AND status = 'REVIEW_REQUIRED'")
        .bind(document_id)
        .bind(format!("data entered manually and linked to {label}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}
