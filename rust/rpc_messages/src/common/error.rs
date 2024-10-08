use serde::{Deserialize, Serialize};
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ErrorDto {
    pub error: String,
}

impl ErrorDto {
    pub fn new(error: String) -> Self {
        Self { error }
    }
}

impl IntoResponse for ErrorDto {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.error }));
        (StatusCode::BAD_REQUEST, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn serialize_error_dto() {
        let error_dto = ErrorDto::new("An error occurred".to_string());
        let serialized = serde_json::to_string(&error_dto).unwrap();
        let expected_json = r#"{"error":"An error occurred"}"#;
        assert_eq!(serialized, expected_json);
    }

    #[test]
    fn deserialize_error_dto() {
        let json_str = r#"{"error":"An error occurred"}"#;
        let deserialized: ErrorDto = serde_json::from_str(json_str).unwrap();
        let expected_error_dto = ErrorDto::new("An error occurred".to_string());
        assert_eq!(deserialized, expected_error_dto);
    }
}
