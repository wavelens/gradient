/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::{BoxFuture, FutureExt as _, Shared, join_all};
use gradient_derivation::{Derivation, parse_drv};
use gradient_sources::ImportBuilder;
use gradient_util::store_path::strip_store_prefix;
use gradient_util::sync::Mutex;
use gradient_wire::types::ImportOutcome;

use super::AbortSignal;
use super::eval::{Root, walk_and_publish};
use crate::traits::{DrvReader, JobReporter};

type ImportFuture<'a> = Shared<BoxFuture<'a, Result<(), String>>>;

pub(super) struct EvalImportBuilder<'a> {
    reporter: &'a dyn JobReporter,
    drv_reader: &'a dyn DrvReader,
    abort: AbortSignal,
    names: Arc<Mutex<HashMap<String, String>>>,
    in_flight: Mutex<HashMap<String, ImportFuture<'a>>>,
}

impl<'a> EvalImportBuilder<'a> {
    pub(super) fn new(
        reporter: &'a dyn JobReporter,
        drv_reader: &'a dyn DrvReader,
        abort: AbortSignal,
    ) -> Self {
        Self {
            reporter,
            drv_reader,
            abort,
            names: Arc::default(),
            in_flight: Mutex::default(),
        }
    }

    pub(super) fn import_attr(
        names: &mut HashMap<String, String>,
        system: &str,
        drv_path: &str,
    ) -> String {
        let (hash, name) = hash_and_name(drv_path);
        let attr = format!("other.{system}.{name}");
        let attr = match names.get(&attr) {
            Some(known) if known != drv_path => {
                format!("{attr}-{}", hash.get(..8).unwrap_or(hash))
            }
            _ => attr,
        };
        names.insert(attr.clone(), drv_path.to_owned());
        attr
    }

    fn import(&self, drv_path: String) -> ImportFuture<'a> {
        let mut in_flight = self.in_flight.lock();
        in_flight
            .entry(drv_path.clone())
            .or_insert_with(|| {
                import_derivation(
                    self.reporter,
                    self.drv_reader,
                    self.abort.clone(),
                    Arc::clone(&self.names),
                    drv_path,
                )
                .boxed()
                .shared()
            })
            .clone()
    }
}

#[async_trait]
impl ImportBuilder for EvalImportBuilder<'_> {
    async fn build_imports(&self, derived_paths: Vec<String>) -> Result<(), String> {
        let mut drv_paths: Vec<String> = derived_paths
            .iter()
            .map(|p| {
                p.split_once('^')
                    .map_or(p.as_str(), |(drv, _)| drv)
                    .to_owned()
            })
            .collect();
        drv_paths.sort_unstable();
        drv_paths.dedup();

        let imports: Vec<_> = drv_paths.into_iter().map(|drv| self.import(drv)).collect();
        join_all(imports).await.into_iter().collect()
    }
}

async fn import_derivation(
    reporter: &dyn JobReporter,
    drv_reader: &dyn DrvReader,
    mut abort: AbortSignal,
    names: Arc<Mutex<HashMap<String, String>>>,
    drv_path: String,
) -> Result<(), String> {
    let drv = read_drv(drv_reader, &drv_path).await?;
    let attr = EvalImportBuilder::import_attr(&mut names.lock(), &drv.system, &drv_path);
    let root = Root {
        drv_path: drv_path.clone(),
        attr,
        ifd: true,
    };
    walk_and_publish(drv_reader, reporter, vec![root], &mut abort)
        .await
        .map_err(|e| format!("recording import '{drv_path}' failed: {e:#}"))?;

    let outcome = reporter
        .request_import(vec![drv_path.clone()])
        .await
        .map_err(|e| format!("import request for '{drv_path}' failed: {e:#}"))?;
    match outcome {
        ImportOutcome::Completed => reporter
            .pull_paths(output_paths(&drv))
            .await
            .map_err(|e| format!("pulling the outputs of import '{drv_path}' failed: {e:#}")),
        ImportOutcome::Failed {
            drv_path,
            build_id,
            status,
        } => Err(format!(
            "import from derivation '{}' failed: build {build_id} {status}",
            hash_and_name(&drv_path).1
        )),
        ImportOutcome::Unknown { drv_path } => {
            Err(format!("the server does not know import '{drv_path}'"))
        }
    }
}

async fn read_drv(drv_reader: &dyn DrvReader, drv_path: &str) -> Result<Derivation, String> {
    let bytes = drv_reader
        .read_drv(drv_path)
        .await
        .map_err(|e| format!("cannot read import '{drv_path}': {e:#}"))?;
    parse_drv(&bytes).map_err(|e| format!("cannot parse import '{drv_path}': {e:#}"))
}

fn output_paths(drv: &Derivation) -> Vec<String> {
    drv.outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| o.path.clone())
        .collect()
}

fn hash_and_name(drv_path: &str) -> (&str, &str) {
    let base = strip_store_prefix(drv_path);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    base.split_once('-').unwrap_or(("", base))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::prelude::*;

    const SOURCE_A: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source.drv";
    const SOURCE_B: &str = "/nix/store/cccccccccccccccccccccccccccccccc-source.drv";

    fn drv_text(out: &str) -> Vec<u8> {
        format!(
            r#"Derive([("out","{out}","","")],[],[],"x86_64-linux","/bin/sh",[],[("name","source")])"#
        )
        .into_bytes()
    }

    fn reader() -> FakeDrvReader {
        FakeDrvReader::new()
            .with_drv(
                SOURCE_A,
                drv_text("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source"),
            )
            .with_drv(
                SOURCE_B,
                drv_text("/nix/store/dddddddddddddddddddddddddddddddd-source"),
            )
    }

    fn import_requests(reporter: &RecordingJobReporter) -> Vec<Vec<String>> {
        reporter
            .events()
            .into_iter()
            .filter_map(|e| match e {
                ReportedEvent::ImportRequested { drv_paths } => Some(drv_paths),
                _ => None,
            })
            .collect()
    }

    fn failed(drv_path: &str) -> ImportOutcome {
        ImportOutcome::Failed {
            drv_path: drv_path.into(),
            build_id: "b-1".into(),
            status: "FailedPermanent".into(),
        }
    }

    #[tokio::test]
    async fn concurrent_requests_for_a_path_share_the_result() {
        let reporter = RecordingJobReporter::new().with_import_outcome(SOURCE_A, failed(SOURCE_A));
        let reader = reader();
        let builder = EvalImportBuilder::new(&reporter, &reader, AbortSignal::never());

        let (first, second) = tokio::join!(
            builder.build_imports(vec![format!("{SOURCE_A}^out")]),
            builder.build_imports(vec![format!("{SOURCE_A}^*")]),
        );

        assert!(first.is_err(), "{first:?}");
        assert_eq!(first, second);
        assert_eq!(import_requests(&reporter), vec![vec![SOURCE_A.to_string()]]);
    }

    #[tokio::test]
    async fn a_completed_import_pulls_its_outputs() {
        let reporter = RecordingJobReporter::new();
        let reader = reader();
        let builder = EvalImportBuilder::new(&reporter, &reader, AbortSignal::never());

        builder
            .build_imports(vec![format!("{SOURCE_A}^out")])
            .await
            .expect("a completed import");

        let events = reporter.events();
        let recorded = events
            .iter()
            .position(|e| {
                matches!(e, ReportedEvent::EvalResult { derivations, .. }
                if derivations.iter().any(|d| d.drv_path == SOURCE_A
                    && d.attr == "other.x86_64-linux.source" && d.ifd))
            })
            .expect("the import is recorded as an imported entry point");
        let requested = events
            .iter()
            .position(|e| matches!(e, ReportedEvent::ImportRequested { .. }))
            .expect("an import request");
        let pulled = events
            .iter()
            .position(|e| {
                matches!(e, ReportedEvent::PathsPulled { paths }
                if paths == &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source".to_string()])
            })
            .expect("the outputs are pulled");
        assert!(recorded < requested && requested < pulled, "{events:?}");
    }

    #[tokio::test]
    async fn a_failed_import_names_the_build() {
        let reporter = RecordingJobReporter::new().with_import_outcome(SOURCE_A, failed(SOURCE_A));
        let reader = reader();
        let builder = EvalImportBuilder::new(&reporter, &reader, AbortSignal::never());

        let built = builder.build_imports(vec![format!("{SOURCE_A}^out")]).await;

        assert_eq!(
            built,
            Err("import from derivation 'source' failed: build b-1 FailedPermanent".to_string())
        );
        assert!(
            !reporter
                .events()
                .iter()
                .any(|e| matches!(e, ReportedEvent::PathsPulled { .. })),
            "a failed import pulls nothing"
        );
    }

    #[tokio::test]
    async fn a_repeated_name_gets_a_hash_suffix() {
        let reporter = RecordingJobReporter::new();
        let reader = reader();
        let builder = EvalImportBuilder::new(&reporter, &reader, AbortSignal::never());

        builder
            .build_imports(vec![format!("{SOURCE_A}^out"), format!("{SOURCE_B}^out")])
            .await
            .expect("both imports complete");

        let mut attrs: Vec<String> = reporter
            .all_eval_derivations()
            .into_iter()
            .filter(|d| d.ifd)
            .map(|d| d.attr)
            .collect();
        attrs.sort_unstable();
        assert_eq!(
            attrs,
            vec![
                "other.x86_64-linux.source".to_string(),
                "other.x86_64-linux.source-cccccccc".to_string(),
            ]
        );
    }

    #[test]
    fn import_attr_strips_hash_and_drv() {
        let mut names = HashMap::new();
        let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12.drv";

        assert_eq!(
            EvalImportBuilder::import_attr(&mut names, "aarch64-linux", drv),
            "other.aarch64-linux.hello-2.12"
        );
        assert_eq!(
            EvalImportBuilder::import_attr(&mut names, "aarch64-linux", drv),
            "other.aarch64-linux.hello-2.12",
            "the same derivation keeps its attribute"
        );
    }
}
