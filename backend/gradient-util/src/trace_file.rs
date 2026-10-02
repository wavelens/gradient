/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Spans are timed on the wall clock. Files of several hosts can then merge into one timeline.

use std::fmt::Debug;
use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Level, Subscriber};
use tracing_subscriber::filter::{FilterExt as _, Targets, filter_fn};
use tracing_subscriber::layer::{Context, Filter, Layer};
use tracing_subscriber::registry::LookupSpan;

static ACTIVE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub struct TraceFileLayer {
    out: Mutex<LineWriter<File>>,
    process: String,
    pid: u32,
}

struct Timing {
    wall_us: u64,
    start: Instant,
    fields: Map<String, Value>,
}

struct Fields<'a>(&'a mut Map<String, Value>);

impl Visit for Fields<'_> {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0
            .insert(field.name().to_owned(), format!("{value:?}").into());
    }
}

pub fn layer(dir: &Path, process: &str) -> std::io::Result<TraceFileLayer> {
    std::fs::create_dir_all(dir)?;
    let pid = std::process::id();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{process}-{pid}.jsonl")))?;

    let _ = ACTIVE_DIR.set(dir.to_owned());
    Ok(TraceFileLayer {
        out: Mutex::new(LineWriter::new(file)),
        process: process.to_owned(),
        pid,
    })
}

pub fn active_dir() -> Option<&'static Path> {
    ACTIVE_DIR.get().map(PathBuf::as_path)
}

pub fn filter<S: Subscriber>() -> impl Filter<S> + use<S> {
    Targets::new()
        .with_target("gradient", Level::DEBUG)
        .and(filter_fn(|meta| meta.is_span()))
}

fn wall_clock_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

impl<S> Layer<S> for TraceFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };

        let mut fields = Map::new();
        attrs.record(&mut Fields(&mut fields));
        span.extensions_mut().insert(Timing {
            wall_us: wall_clock_us(),
            start: Instant::now(),
            fields,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(timing) = span.extensions_mut().get_mut::<Timing>()
        {
            values.record(&mut Fields(&mut timing.fields));
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };

        let Some(timing) = span.extensions_mut().remove::<Timing>() else {
            return;
        };

        let line = json!({
            "name": span.name(),
            "target": span.metadata().target(),
            "ts_us": timing.wall_us,
            "dur_us": timing.start.elapsed().as_micros() as u64,
            "pid": self.pid,
            "process": self.process,
            "fields": timing.fields,
        });
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Duration;
    use tracing_subscriber::layer::SubscriberExt;

    fn traced(dir: &Path, body: impl FnOnce()) -> Vec<Value> {
        let layer = layer(dir, "server").expect("open trace file");
        let subscriber = tracing_subscriber::registry().with(layer.with_filter(filter()));
        tracing::subscriber::with_default(subscriber, body);
        let path = dir.join(format!("server-{}.jsonl", std::process::id()));
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect()
    }

    #[test]
    fn a_closed_span_is_one_line_with_its_duration_and_fields() {
        let dir = tempfile::tempdir().unwrap();
        let lines = traced(dir.path(), || {
            let span = tracing::debug_span!("flush", batches = 3_u64, rows = tracing::field::Empty);
            let _entered = span.enter();
            span.record("rows", 150_u64);
            std::thread::sleep(Duration::from_millis(5));
        });

        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert_eq!(line["name"], "flush");
        assert_eq!(line["process"], "server");
        assert_eq!(line["pid"], std::process::id());
        assert_eq!(line["fields"]["batches"], 3);
        assert_eq!(line["fields"]["rows"], 150);
        assert!(line["dur_us"].as_u64().unwrap() >= 5_000, "{line}");
        assert!(
            line["ts_us"].as_u64().unwrap() > 1_700_000_000_000_000,
            "{line}"
        );
    }

    #[test]
    fn events_and_foreign_targets_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let lines = traced(dir.path(), || {
            tracing::info!("an event is not a span");
            let _hyper = tracing::debug_span!(target: "hyper", "request").entered();
        });

        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn an_opened_trace_dir_is_remembered_for_child_processes() {
        let dir = tempfile::tempdir().unwrap();
        layer(dir.path(), "worker").expect("open trace file");
        assert!(active_dir().is_some());
    }

    #[test]
    fn events_are_not_enabled_by_the_span_filter() {
        let dir = tempfile::tempdir().unwrap();
        let subscriber = tracing_subscriber::registry()
            .with(layer(dir.path(), "server").unwrap().with_filter(filter()));
        tracing::subscriber::with_default(subscriber, || {
            assert!(!tracing::event_enabled!(tracing::Level::DEBUG));
            assert!(tracing::span_enabled!(tracing::Level::DEBUG));
        });
    }

    #[test]
    fn a_trace_dir_that_is_a_file_cannot_be_opened() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(layer(file.path(), "worker").is_err());
    }
}
