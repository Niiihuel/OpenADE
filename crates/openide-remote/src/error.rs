use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// An answer the phone can show: a stable code and a message in Spanish.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn bad(message: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, code: "invalid_request", message: message.into() }
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self { status: StatusCode::CONFLICT, code: "conflict", message: message.into() }
    }
    pub fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "Esa tarea o proyecto no existe en este PC".into(),
        }
    }
    pub fn unavailable() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "host_error",
            message: "El PC no pudo completar la operación".into(),
        }
    }
    /// The window did not take the command in time: it may still act on it.
    pub fn busy() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "window_busy",
            message: "openADE no respondió a tiempo; revisá la tarea antes de reintentar".into(),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": {"code": self.code, "message": self.message}}))).into_response()
    }
}
