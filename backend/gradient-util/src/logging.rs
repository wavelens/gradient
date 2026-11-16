/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use tracing::Subscriber;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{
    EnvFilter, Layer, Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::trace_file;

pub const NOISY_DEPS: &[&str] = &[
    "hyper", "h2", "sqlx", "sea_orm", "tower", "reqwest", "rustls",
];

pub type AdditionalTracingLayer = Box<dyn Layer<Registry> + Send + Sync>;

pub enum LogWriter {
    Stdout,
    Stderr,
}

pub struct LogSetup<'a> {
    pub level: &'a str,
    pub overrides: &'a [(&'a str, Option<&'a str>)],
    pub quiet: &'a [&'a str],
    pub honor_rust_log: bool,
    pub writer: LogWriter,
    pub trace: Option<TraceSetup<'a>>,
}

pub struct TraceSetup<'a> {
    pub dir: &'a Path,
    pub process: &'a str,
}

pub fn directive(setup: &LogSetup<'_>) -> String {
    let quiet = setup.quiet.iter().map(|dep| format!("{dep}=warn"));
    let overrides = setup
        .overrides
        .iter()
        .filter_map(|(target, level)| level.map(|l| format!("{target}={l}")));

    std::iter::once(setup.level.to_owned())
        .chain(quiet)
        .chain(overrides)
        .collect::<Vec<_>>()
        .join(",")
}

pub fn effective_directive(setup: &LogSetup<'_>, rust_log: Option<&str>) -> String {
    rust_log
        .filter(|_| setup.honor_rust_log)
        .filter(|rust_log| {
            EnvFilter::try_new(rust_log)
                .inspect_err(|e| {
                    eprintln!("invalid RUST_LOG {rust_log:?}: {e}; using configured levels")
                })
                .is_ok()
        })
        .map_or_else(|| directive(setup), str::to_owned)
}

pub fn init(setup: &LogSetup<'_>, extra_tracing_layer: Option<AdditionalTracingLayer>) {
    #[expect(
        clippy::disallowed_methods,
        reason = "RUST_LOG is the conventional override of the tracing filter, not an option of Gradient"
    )]
    let rust_log = std::env::var("RUST_LOG").ok();
    let filter = EnvFilter::new(effective_directive(setup, rust_log.as_deref()));
    #[expect(
        clippy::disallowed_methods,
        reason = "JOURNAL_STREAM is the systemd variable for the identity of the connected journal stream. This variable isn't an option of Gradient."
    )]
    let journal_stream = std::env::var("JOURNAL_STREAM").ok();
    let console_log_output_stream_factory = match setup.writer {
        LogWriter::Stdout => BoxMakeWriter::new(std::io::stdout),
        LogWriter::Stderr => BoxMakeWriter::new(std::io::stderr),
    };
    let console = fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_writer(console_log_output_stream_factory);
    let console = if is_log_writer_journal_stream(
        journal_stream.as_deref(),
        log_writer_file_device_and_inode(&setup.writer),
    ) {
        console.without_time().boxed()
    } else {
        console.boxed()
    };

    let registry = tracing_subscriber::registry()
        .with(extra_tracing_layer)
        .with(console.with_filter(filter));
    match setup.trace.as_ref().and_then(trace_layer) {
        Some(trace) => registry.with(trace).init(),
        None => registry.init(),
    }
}

fn parse_journal_stream_device_and_inode(value: &str) -> Option<(u64, u64)> {
    let (device, inode) = value.split_once(':')?;
    Some((device.parse().ok()?, inode.parse().ok()?))
}

fn is_log_writer_journal_stream(
    journal_stream: Option<&str>,
    writer_device_and_inode: Option<(u64, u64)>,
) -> bool {
    journal_stream
        .and_then(parse_journal_stream_device_and_inode)
        .is_some_and(|journal| Some(journal) == writer_device_and_inode)
}

fn log_writer_file_device_and_inode(writer: &LogWriter) -> Option<(u64, u64)> {
    let fd = match writer {
        LogWriter::Stdout => std::io::stdout().as_fd().try_clone_to_owned(),
        LogWriter::Stderr => std::io::stderr().as_fd().try_clone_to_owned(),
    }
    .ok()?;
    let metadata = File::from(fd).metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

fn trace_layer<S>(setup: &TraceSetup<'_>) -> Option<impl Layer<S> + use<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    trace_file::layer(setup.dir, setup.process)
        .inspect_err(|e| {
            eprintln!(
                "trace dir {}: {e}; spans are not traced to a file",
                setup.dir.display()
            )
        })
        .ok()
        .map(|layer| layer.with_filter(trace_file::filter()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup<'a>(
        overrides: &'a [(&'a str, Option<&'a str>)],
        quiet: &'a [&'a str],
    ) -> LogSetup<'a> {
        LogSetup {
            level: "info",
            overrides,
            quiet,
            honor_rust_log: false,
            writer: LogWriter::Stderr,
            trace: None,
        }
    }

    #[test]
    fn the_base_level_comes_first_then_quiet_deps_then_overrides() {
        let d = directive(&setup(
            &[
                ("gradient_worker_client", Some("debug")),
                ("gradient_web", None),
            ],
            &["hyper"],
        ));
        assert_eq!(d, "info,hyper=warn,gradient_worker_client=debug");
    }

    #[test]
    fn an_unset_override_adds_nothing() {
        assert_eq!(directive(&setup(&[("gradient_web", None)], &[])), "info");
    }

    #[test]
    fn rust_log_wins_only_when_honoured_and_valid() {
        let ignored = setup(&[], &[]);
        assert_eq!(effective_directive(&ignored, Some("debug")), "info");

        let honoured = LogSetup {
            honor_rust_log: true,
            ..setup(&[], &[])
        };
        assert_eq!(effective_directive(&honoured, Some("debug")), "debug");
        assert_eq!(effective_directive(&honoured, Some("=[bad")), "info");
        assert_eq!(effective_directive(&honoured, None), "info");
    }

    #[test]
    fn an_unopenable_trace_dir_leaves_logging_without_a_trace_layer() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let setup = TraceSetup {
            dir: file.path(),
            process: "server",
        };
        assert!(trace_layer::<tracing_subscriber::Registry>(&setup).is_none());
    }

    #[test]
    fn writer_counts_as_journal_stream_when_device_and_inode_match_journal_stream() {
        assert!(is_log_writer_journal_stream(
            Some("25:4242"),
            Some((25, 4242))
        ));
    }

    #[test]
    fn a_set_journal_stream_variable_with_a_different_writer_inode_is_not_the_journal_stream() {
        assert!(!is_log_writer_journal_stream(
            Some("25:4242"),
            Some((25, 9001))
        ));
        assert!(!is_log_writer_journal_stream(Some("25:4242"), None));
    }

    #[test]
    fn journal_stream_unset_or_malformed_means_writer_is_not_journal_stream() {
        let writer = Some((25, 4242));
        assert!(!is_log_writer_journal_stream(None, writer));
        assert!(!is_log_writer_journal_stream(Some(""), writer));
        assert!(!is_log_writer_journal_stream(Some("25"), writer));
        assert!(!is_log_writer_journal_stream(Some("25:"), writer));
        assert!(!is_log_writer_journal_stream(Some("x:4242"), writer));
        assert!(!is_log_writer_journal_stream(Some("25:4242:1"), writer));
    }
}
