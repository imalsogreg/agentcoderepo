//! Content negotiation: respond as JSON or minimalist ASCII text.
//!
//! Agents can request `Accept: text/plain` to get a compact, token-efficient
//! text representation. JSON remains the default.

use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

/// Output format determined by the `Accept` header.
#[derive(Debug, Clone, Copy, Default)]
pub enum Format {
    #[default]
    Json,
    Text,
}

/// Extractor that reads the `Accept` header to determine response format.
///
/// - `Accept: text/plain` → plain text
/// - Everything else (including no header) → JSON
#[derive(Debug, Clone, Copy)]
pub struct ContentNeg(pub Format);

impl<S: Send + Sync> FromRequestParts<S> for ContentNeg {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let format = parts
            .headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|accept| {
                if accept.contains("text/plain") {
                    Format::Text
                } else {
                    Format::Json
                }
            })
            .unwrap_or(Format::Json);
        Ok(ContentNeg(format))
    }
}

impl ContentNeg {
    pub fn ok<T>(self, data: T) -> Negotiated<T> {
        Negotiated {
            format: self.0,
            status: StatusCode::OK,
            data,
        }
    }

    pub fn created<T>(self, data: T) -> Negotiated<T> {
        Negotiated {
            format: self.0,
            status: StatusCode::CREATED,
            data,
        }
    }
}

/// Types that can render as plain text for agent consumption.
pub trait TextFormat {
    fn to_text(&self) -> String;
}

/// A response that serializes as JSON or plain text depending on content negotiation.
pub struct Negotiated<T> {
    format: Format,
    status: StatusCode,
    data: T,
}

impl<T: serde::Serialize + TextFormat> IntoResponse for Negotiated<T> {
    fn into_response(self) -> Response {
        match self.format {
            Format::Json => (self.status, Json(self.data)).into_response(),
            Format::Text => (
                self.status,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                self.data.to_text(),
            )
                .into_response(),
        }
    }
}
