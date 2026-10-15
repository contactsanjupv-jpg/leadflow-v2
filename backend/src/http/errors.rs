use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("authentication required")]
    Authentication,
    #[error("not authorized")]
    Authorization,
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("subscription required")]
    PaymentRequired,
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            AppError::Validation(m) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", m.clone()),
            AppError::Authentication => (
                StatusCode::UNAUTHORIZED,
                "AUTHENTICATION_ERROR",
                "authentication required".into(),
            ),
            AppError::Authorization => (
                StatusCode::FORBIDDEN,
                "AUTHORIZATION_ERROR",
                "not authorized".into(),
            ),
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "resource not found".into(),
            ),
            AppError::Conflict(m) => (StatusCode::CONFLICT, "CONFLICT", m.clone()),
            AppError::PaymentRequired => (
                StatusCode::PAYMENT_REQUIRED,
                "SUBSCRIPTION_REQUIRED",
                "an active subscription is required for this action".into(),
            ),
            AppError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "payload too large".into(),
            ),
            AppError::ServiceUnavailable(m) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                m.clone(),
            ),
            AppError::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "an internal error occurred".into(),
                )
            }
            AppError::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "an internal error occurred".into(),
                )
            }
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}
