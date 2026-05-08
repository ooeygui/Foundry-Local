// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Serves a bundled ACP OpenAPI specification at GET /openapi.json.

use axum::response::{IntoResponse, Response};
use axum::http::{header, StatusCode};

/// The ACP v0.2.3 OpenAPI specification (minimal subset relevant to this server).
/// This is embedded at compile time.
const OPENAPI_SPEC: &str = include_str!("openapi.json");

/// GET /openapi.json — serve the ACP OpenAPI specification.
pub async fn openapi_json() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        OPENAPI_SPEC,
    )
        .into_response()
}
