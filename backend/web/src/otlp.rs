/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Optional OpenTelemetry (OTLP) metric push export (#212).
//!
//! Enabled by `GRADIENT_OTLP_ENDPOINT`. A background task refreshes a cached
//! snapshot of the same values the Prometheus endpoint computes; OTLP observable
//! gauges (whose callbacks are synchronous and so can't query the DB) read that
//! cache and are pushed on `GRADIENT_OTLP_PUSH_INTERVAL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gradient_core::types::ServerState;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use scheduler::Scheduler;
use tracing::{error, info};

#[derive(Default, Clone, Copy)]
struct Snapshot {
    workers: i64,
    pending: i64,
    active: i64,
    cache_bytes: i64,
    cache_nar_bytes: i64,
    cache_packages: i64,
}

pub fn start_otlp(state: Arc<ServerState>, scheduler: Arc<Scheduler>) {
    let Some(endpoint) = state.config.metrics_args.otlp_endpoint.clone() else {
        return;
    };
    let interval = Duration::from_secs(state.config.metrics_args.otlp_push_interval_secs.max(1));

    let exporter = match opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "OTLP metric exporter build failed; OTLP disabled");
            return;
        }
    };

    let reader =
        opentelemetry_sdk::metrics::PeriodicReader::builder(exporter, opentelemetry_sdk::runtime::Tokio)
            .with_interval(interval)
            .build();

    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(opentelemetry_sdk::Resource::new(vec![KeyValue::new(
            "service.name",
            "gradient",
        )]))
        .build();

    let meter = provider.meter("gradient");
    let snap = Arc::new(Mutex::new(Snapshot::default()));

    macro_rules! gauge {
        ($name:expr, $field:ident) => {{
            let s = Arc::clone(&snap);
            let _ = meter
                .i64_observable_gauge($name)
                .with_callback(move |obs| {
                    obs.observe(s.lock().map(|v| v.$field).unwrap_or(0), &[]);
                })
                .build();
        }};
    }
    gauge!("gradient_workers_connected", workers);
    gauge!("gradient_jobs_pending", pending);
    gauge!("gradient_jobs_active", active);
    gauge!("gradient_cache_bytes", cache_bytes);
    gauge!("gradient_cache_nar_bytes", cache_nar_bytes);
    gauge!("gradient_cache_packages", cache_packages);

    let shutdown = state.shutdown.clone();
    let refresh = Arc::clone(&snap);
    shutdown.spawn(async move {
        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            if let Ok(obs) = crate::endpoints::metrics::collect(&state, &scheduler).await
                && let Ok(mut s) = refresh.lock()
            {
                s.workers = obs.workers_connected;
                s.pending = obs.jobs_pending;
                s.active = obs.jobs_active;
                s.cache_bytes = obs.cache_bytes;
                s.cache_nar_bytes = obs.cache_nar_bytes;
                s.cache_packages = obs.cache_packages;
            }
        }
    });

    // The provider must outlive the process so the periodic reader keeps exporting.
    Box::leak(Box::new(provider));
    info!("OTLP metric push enabled");
}
