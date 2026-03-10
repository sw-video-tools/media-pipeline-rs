//! Custom JSON extractor that returns ApiError on deserialization failure.

use axum::extract::{FromRequest, Request};
use axum::response::IntoResponse;
use serde::de::DeserializeOwned;

use crate::AppState;
use crate::errors::ApiError;

/// Like `axum::Json<T>`, but returns an `ApiError` (400 Bad Request)
/// when the request body cannot be parsed.
pub struct JsonBody<T>(pub T);

impl<T> FromRequest<AppState> for JsonBody<T>
where
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
        Ok(JsonBody(value))
    }
}

impl<T> IntoResponse for JsonBody<T>
where
    T: serde::Serialize,
{
    fn into_response(self) -> axum::response::Response {
        axum::Json(self.0).into_response()
    }
}
