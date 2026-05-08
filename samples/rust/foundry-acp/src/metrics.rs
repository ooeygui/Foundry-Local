// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Prometheus metrics endpoint and request instrumentation.

use std::sync::OnceLock;
use std::time::Instant;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use metrics::{counter, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Global Prometheus handle for rendering metrics.
static PROM_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Initialise the Prometheus metrics recorder. Call once at startup.
pub fn init_metrics() {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");
    let _ = PROM_HANDLE.set(handle);
}

/// GET /metrics — render Prometheus text format.
pub async fn metrics_handler() -> impl IntoResponse {
    match PROM_HANDLE.get() {
        Some(handle) => handle.render(),
        None => String::from("# Metrics not initialised\n"),
    }
}

/// Axum middleware that records per-request metrics.
pub async fn track_metrics(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    // Normalise path to avoid high-cardinality labels (replace UUIDs)
    let normalised = normalise_path(&path);

    counter!("http_requests_total", "method" => method.to_string(), "path" => normalised.clone(), "status" => status).increment(1);
    histogram!("http_request_duration_seconds", "method" => method.to_string(), "path" => normalised).record(duration);

    response
}

/// Replace UUID segments in paths with `{id}` to reduce label cardinality.
fn normalise_path(path: &str) -> String {
    let uuid_pattern = |s: &str| -> bool {
        s.len() == 36
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-')
    };

    path.split('/')
        .map(|seg| if uuid_pattern(seg) { "{id}" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}
