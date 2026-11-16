/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_scheduler::Scheduler;
use gradient_storage::admission::UploadAdmission;
use gradient_types::cli::MetricsArgs;
use gradient_util::logging::AdditionalTracingLayer;
use gradient_util::supervision::{ChildSpec, Pass, PassError};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, SpanExporter};
use tracing::{info, warn};
use tracing_subscriber::Layer;

pub struct OtlpTracerProviderShutdownOnDrop(SdkTracerProvider);

impl Drop for OtlpTracerProviderShutdownOnDrop {
    fn drop(&mut self) {
        if let Err(error) = self.0.shutdown() {
            warn!(%error, "Failed to flush final OTLP spans at exit");
        }
    }
}

pub fn build_otlp_http_span_export_layer_and_guard(
    metrics: &MetricsArgs,
) -> Result<
    Option<(AdditionalTracingLayer, OtlpTracerProviderShutdownOnDrop)>,
    opentelemetry_otlp::ExporterBuildError,
> {
    let Some(endpoint) = metrics.otlp_traces_endpoint.clone() else {
        return Ok(None);
    };
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()?;
    Ok(Some(build_span_export_layer_and_guard(
        exporter,
        metrics.otlp_service_name.clone(),
        metrics.otlp_traces_sample_ratio,
    )))
}

fn build_span_export_layer_and_guard(
    exporter: impl SpanExporter + 'static,
    service_name: String,
    sample_ratio: f64,
) -> (AdditionalTracingLayer, OtlpTracerProviderShutdownOnDrop) {
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            sample_ratio,
        ))))
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(service_name)
                .build(),
        )
        .build();
    let layer = tracing_opentelemetry::layer()
        .with_tracer(provider.tracer("gradient"))
        .with_filter(gradient_util::trace_file::filter())
        .boxed();
    (layer, OtlpTracerProviderShutdownOnDrop(provider))
}

#[derive(Default, Clone, Copy)]
struct Snapshot {
    workers: i64,
    pending: i64,
    active: i64,
    cache_bytes: i64,
    cache_nar_bytes: i64,
    cache_packages: i64,
}

struct OtlpSnapshot {
    web_db: WebDb,
    started_at: DateTime<Utc>,
    upload_admission: Arc<UploadAdmission>,
    scheduler: Arc<Scheduler>,
    snapshot: Arc<Mutex<Snapshot>>,
    period: Duration,
}

impl Pass for OtlpSnapshot {
    const NAME: &'static str = "otlp-snapshot";
    const BUDGET: Duration = Duration::from_secs(60);

    fn period(&self) -> Duration {
        self.period
    }

    async fn run(&self) -> Result<(), PassError> {
        if let Ok(obs) = crate::endpoints::metrics::collect(
            &self.web_db,
            self.started_at,
            &self.upload_admission,
            &self.scheduler,
        )
        .await
            && let Ok(mut s) = self.snapshot.lock()
        {
            s.workers = obs.workers_connected;
            s.pending = obs.jobs_pending;
            s.active = obs.jobs_active;
            s.cache_bytes = obs.cache_bytes;
            s.cache_nar_bytes = obs.cache_nar_bytes;
            s.cache_packages = obs.cache_packages;
        }
        Ok(())
    }
}

pub fn build_otlp_exporter_child_spec(
    state: &ServerState,
    scheduler: Arc<Scheduler>,
) -> Result<Option<ChildSpec>, opentelemetry_otlp::ExporterBuildError> {
    let Some(endpoint) = state.config.metrics_args.otlp_endpoint.clone() else {
        return Ok(None);
    };
    let interval = Duration::from_secs(state.config.metrics_args.otlp_push_interval_secs.get());

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()?;

    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
        .with_interval(interval)
        .build();

    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(state.config.metrics_args.otlp_service_name.clone())
                .build(),
        )
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

    // The provider must outlive the process for the periodic reader to keep exporting.
    Box::leak(Box::new(provider));
    info!("OTLP metric push enabled");
    Ok(Some(ChildSpec::pass(OtlpSnapshot {
        web_db: state.web_db.clone(),
        started_at: state.started_at,
        upload_admission: Arc::clone(&state.memory.upload_admission),
        scheduler,
        snapshot: snap,
        period: interval,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::Key;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Debug, Clone, Default)]
    struct ResourceRecordingInMemorySpanExporter {
        spans: InMemorySpanExporter,
        resource: Arc<Mutex<Option<Resource>>>,
    }

    impl SpanExporter for ResourceRecordingInMemorySpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.spans.export(batch).await
        }

        fn set_resource(&mut self, resource: &Resource) {
            *self.resource.lock().unwrap() = Some(resource.clone());
        }
    }

    fn record_spans_with_sample_ratio_and_return_exporter(
        sample_ratio: f64,
        emit_spans: impl FnOnce(),
    ) -> ResourceRecordingInMemorySpanExporter {
        let exporter = ResourceRecordingInMemorySpanExporter::default();
        let (layer, guard) = build_span_export_layer_and_guard(
            exporter.clone(),
            "gradient-test".to_owned(),
            sample_ratio,
        );
        tracing::subscriber::with_default(Registry::default().with(layer), emit_spans);
        drop(guard);
        exporter
    }

    fn span_names_received_by_recording_exporter(
        exporter: &ResourceRecordingInMemorySpanExporter,
    ) -> Vec<String> {
        exporter
            .spans
            .get_finished_spans()
            .unwrap()
            .into_iter()
            .map(|span| span.name.into_owned())
            .collect()
    }

    #[test]
    fn debug_span_with_gradient_target_is_exported_with_configured_service_name() {
        let exporter = record_spans_with_sample_ratio_and_return_exporter(1.0, || {
            tracing::debug_span!(target: "gradient_graph::writer", "transact_once").in_scope(|| {});
        });

        assert_eq!(
            span_names_received_by_recording_exporter(&exporter),
            ["transact_once"]
        );
        let resource = exporter.resource.lock().unwrap().clone().unwrap();
        assert_eq!(
            resource.get(&Key::new("service.name")),
            Some("gradient-test".into())
        );
    }

    #[test]
    fn hyper_span_is_not_exported_but_gradient_span_is() {
        let exporter = record_spans_with_sample_ratio_and_return_exporter(1.0, || {
            tracing::debug_span!(target: "hyper::proto", "parse_headers").in_scope(|| {});
            tracing::debug_span!(target: "gradient_web::otlp", "handle_request").in_scope(|| {});
        });

        assert_eq!(
            span_names_received_by_recording_exporter(&exporter),
            ["handle_request"]
        );
    }

    #[test]
    fn no_spans_exported_when_trace_sample_ratio_is_zero() {
        let exporter = record_spans_with_sample_ratio_and_return_exporter(0.0, || {
            tracing::debug_span!(target: "gradient_graph::writer", "transact_once").in_scope(|| {});
        });

        assert!(span_names_received_by_recording_exporter(&exporter).is_empty());
    }
}
