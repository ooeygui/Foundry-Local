// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! ACP error response type and conversions.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// ACP error — the spec defines ErrorResponse as a plain string.
#[derive(Debug)]
pub struct AcpError {
    pub status: StatusCode,
    pub message: String,
}

impl AcpError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for AcpError {
    fn into_response(self) -> Response {
        (self.status, Json(self.message)).into_response()
    }
}

impl From<foundry_local_sdk::FoundryLocalError> for AcpError {
    fn from(e: foundry_local_sdk::FoundryLocalError) -> Self {
        AcpError::internal(format!("{e}"))
    }
}
