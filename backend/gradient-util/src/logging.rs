/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One tracing setup for every Gradient binary: a base level, dependency
//! noise pinned to `warn`, per-target overrides and an optional `RUST_LOG`.

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Dependency targets pinned to `warn` so a plain `info` log stays readable.
pub const NOISY_DEPS: &[&str] = &[
    "hyper", "h2", "sqlx", "sea_orm", "tower", "reqwest", "rustls",
];

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

pub fn init(setup: &LogSetup<'_>) {
    let rust_log = std::env::var("RUST_LOG").ok();
    let filter = EnvFilter::new(effective_directive(setup, rust_log.as_deref()));
    let layer = fmt::layer().with_target(true).with_thread_ids(true);
    let registry = tracing_subscriber::registry().with(filter);
    match setup.writer {
        LogWriter::Stdout => registry.with(layer).init(),
        LogWriter::Stderr => registry.with(layer.with_writer(std::io::stderr)).init(),
    }
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
}
