use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("validation error: {0}")]
    Validation(String),
    /// The request was well-formed but semantically unacceptable (HTTP 422), e.g. a new
    /// authorized fee with a missing or unrecognised `charge_type`. Everything else keeps using
    /// `Validation` (HTTP 400); only use this where a caller must tell the two apart.
    #[error("unprocessable: {0}")]
    Unprocessable(String),
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
            AppError::Unprocessable(m) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "UNPROCESSABLE_ENTITY",
                m.clone(),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    async fn render(e: AppError) -> (StatusCode, Value) {
        let resp = e.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn unprocessable_is_422_in_the_standard_error_envelope() {
        let (status, body) = render(AppError::Unprocessable(
            "charge_type must be EXACT or CAP".into(),
        ))
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "UNPROCESSABLE_ENTITY");
        assert_eq!(body["error"]["message"], "charge_type must be EXACT or CAP");
    }

    #[tokio::test]
    async fn validation_remains_400() {
        let (status, body) = render(AppError::Validation("bad input".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
        assert_eq!(body["error"]["message"], "bad input");
    }

    #[test]
    fn unprocessable_display_names_the_reason() {
        assert_eq!(
            AppError::Unprocessable("x".into()).to_string(),
            "unprocessable: x"
        );
    }
}
