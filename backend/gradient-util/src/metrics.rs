/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Prometheus plumbing shared by every Gradient `/metrics` endpoint.

use prometheus::{Encoder as _, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder};

pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// Render `registry` in the Prometheus text exposition format.
pub fn encode_text(registry: &Registry) -> String {
    let mut buf = Vec::new();
    let _ = TextEncoder::new().encode(&registry.gather(), &mut buf);
    String::from_utf8(buf).unwrap_or_default()
}

/// Process RSS, open fds and CPU on Linux; a no-op elsewhere.
pub fn register_process_collector(registry: &Registry) {
    #[cfg(target_os = "linux")]
    {
        let pc = prometheus::process_collector::ProcessCollector::for_self();
        let _ = registry.register(Box::new(pc));
    }

    #[cfg(not(target_os = "linux"))]
    let _ = registry;
}

pub fn register_labelled_counter(
    registry: &Registry,
    name: &str,
    help: &str,
    label: &str,
    values: &[(String, i64)],
) -> prometheus::Result<()> {
    let cv = IntCounterVec::new(Opts::new(name, help), &[label])?;
    for (value_label, value) in values {
        cv.with_label_values(&[value_label])
            .inc_by((*value).max(0) as u64);
    }

    registry.register(Box::new(cv))
}

pub fn register_labelled_gauge(
    registry: &Registry,
    name: &str,
    help: &str,
    label: &str,
    values: &[(String, i64)],
) -> prometheus::Result<()> {
    let gv = IntGaugeVec::new(Opts::new(name, help), &[label])?;
    for (value_label, value) in values {
        gv.with_label_values(&[value_label]).set(*value);
    }

    registry.register(Box::new(gv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_labelled_counter_renders_one_series_per_value() {
        let registry = Registry::new();
        register_labelled_counter(
            &registry,
            "jobs_total",
            "Jobs.",
            "kind",
            &[("build".into(), 3), ("eval".into(), 1)],
        )
        .expect("register");
        let text = encode_text(&registry);
        assert!(text.contains("jobs_total{kind=\"build\"} 3"), "{text}");
        assert!(text.contains("jobs_total{kind=\"eval\"} 1"), "{text}");
    }

    #[test]
    fn a_labelled_gauge_keeps_negative_values() {
        let registry = Registry::new();
        register_labelled_gauge(&registry, "delta", "Delta.", "side", &[("a".into(), -2)])
            .expect("register");
        assert!(encode_text(&registry).contains("delta{side=\"a\"} -2"));
    }
}
