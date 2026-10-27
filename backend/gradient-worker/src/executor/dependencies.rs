/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::{BoxFuture, FutureExt as _, Shared, join_all};
use gradient_derivation::{Derivation, ProducedDrv, parse_drv};
use gradient_sources::EvalDependencies;
use gradient_util::store_path::strip_store_prefix;
use gradient_util::sync::Mutex;
use gradient_wire::traits::InputProgressSink;
use gradient_wire::types::{ImportOutcome, InputOutcome, Realisation};

use super::AbortSignal;
use super::eval::{Root, walk_and_publish};
use crate::traits::{DrvReader, JobReporter};
use gradient_wire::messages::QueryMode;

type ImportFuture<'a> = Shared<BoxFuture<'a, Result<Vec<Realisation>, String>>>;
type InputFuture<'a> = Shared<BoxFuture<'a, Result<String, String>>>;

#[derive(Clone)]
pub(super) struct Imports<'a> {
    pub(super) reporter: &'a dyn JobReporter,
    pub(super) drv_reader: &'a dyn DrvReader,
    abort: AbortSignal,
    names: Arc<Mutex<HashMap<String, String>>>,
    in_flight: Arc<Mutex<HashMap<String, ImportFuture<'a>>>>,
}

impl<'a> Imports<'a> {
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
            in_flight: Arc::default(),
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
            .or_insert_with(|| import_derivation(self.clone(), drv_path).boxed().shared())
            .clone()
    }

    pub(super) async fn produced_by_inputs(&self, drv: &Derivation) -> Result<Vec<String>, String> {
        let mut found = Vec::new();
        for input in &drv.input_derivations {
            for produced in &input.produced {
                found.extend(self.produced_drv(input.path.clone(), produced).await?);
            }
        }
        Ok(found)
    }

    fn produced_drv<'b>(
        &'b self,
        parent: String,
        produced: &'b ProducedDrv,
    ) -> BoxFuture<'b, Result<Vec<String>, String>> {
        async move {
            let outputs = self.import(parent.clone()).await?;
            let drv_path = output_path(&outputs, &parent, &produced.output)?;
            let mut found = vec![drv_path.clone()];
            for nested in &produced.produced {
                found.extend(self.produced_drv(drv_path.clone(), nested).await?);
            }
            Ok(found)
        }
        .boxed()
    }
}

fn output_path(outputs: &[Realisation], drv_path: &str, output: &str) -> Result<String, String> {
    outputs
        .iter()
        .find(|o| o.drv_path == drv_path && o.output == output)
        .map(|o| o.path.clone())
        .ok_or_else(|| format!("the import '{drv_path}' has no output '{output}'"))
}

pub(super) struct EvalDependencySource<'a> {
    imports: Imports<'a>,
    inputs: InputSource<'a>,
}

impl<'a> EvalDependencySource<'a> {
    pub(super) fn new(imports: Imports<'a>) -> Self {
        Self {
            inputs: InputSource::new(imports.reporter),
            imports,
        }
    }
}

#[async_trait]
impl EvalDependencies for EvalDependencySource<'_> {
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

        let imports: Vec<_> = drv_paths
            .into_iter()
            .map(|drv| self.imports.import(drv))
            .collect();
        join_all(imports)
            .await
            .into_iter()
            .try_for_each(|imported| imported.map(drop))
    }

    async fn fetch_input(&self, request: String) -> Result<String, String> {
        self.inputs.fetch(request, Arc::new(NoProgress)).await
    }
}

struct NoProgress;

impl InputProgressSink for NoProgress {
    fn transfer(&self, _bytes_done: u64, _bytes_total: Option<u64>) {}
}

pub(super) struct InputSource<'a> {
    reporter: &'a dyn JobReporter,
    in_flight: Mutex<HashMap<String, InputFuture<'a>>>,
}

impl<'a> InputSource<'a> {
    pub(super) fn new(reporter: &'a dyn JobReporter) -> Self {
        Self {
            reporter,
            in_flight: Mutex::default(),
        }
    }

    pub(super) async fn fetch(
        &self,
        request: String,
        progress: Arc<dyn InputProgressSink>,
    ) -> Result<String, String> {
        let store_path = store_path_of(&request)?;
        let key = store_path.clone().unwrap_or_else(|| request.clone());
        let fetch = self
            .in_flight
            .lock()
            .entry(key)
            .or_insert_with(|| {
                fetch_input(self.reporter, request, store_path, progress)
                    .boxed()
                    .shared()
            })
            .clone();
        fetch.await
    }
}

async fn fetch_input(
    reporter: &dyn JobReporter,
    request: String,
    store_path: Option<String>,
    progress: Arc<dyn InputProgressSink>,
) -> Result<String, String> {
    if let Some(path) = store_path
        && in_cache(reporter, &path).await
        && reporter.pull_paths(vec![path.clone()]).await.is_ok()
    {
        return Ok(serde_json::json!({ "storePath": path }).to_string());
    }

    let outcome = reporter
        .request_input(request, progress)
        .await
        .map_err(|e| format!("input request failed: {e:#}"))?;
    match outcome {
        InputOutcome::Fetched { response } => {
            let fetched = store_path_of(&response)?
                .ok_or_else(|| "the fetch answer names no store path".to_owned())?;
            reporter
                .pull_paths(vec![fetched.clone()])
                .await
                .map_err(|e| format!("pulling the fetched input {fetched} failed: {e:#}"))?;
            Ok(response)
        }
        InputOutcome::Failed { message } | InputOutcome::Unplaced { message } => Err(message),
    }
}

async fn in_cache(reporter: &dyn JobReporter, path: &str) -> bool {
    reporter
        .query_cache(vec![path.to_owned()], QueryMode::Pull)
        .await
        .is_ok_and(|cached| cached.iter().any(|c| c.cached && c.path == path))
}

fn store_path_of(json: &str) -> Result<Option<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("malformed input request: {e}"))?;
    Ok(value["storePath"].as_str().map(str::to_owned))
}

async fn import_derivation(
    imports: Imports<'_>,
    drv_path: String,
) -> Result<Vec<Realisation>, String> {
    let drv = read_drv(imports.drv_reader, &drv_path).await?;
    let attr = Imports::import_attr(&mut imports.names.lock(), &drv.system, &drv_path);
    let root = Root {
        drv_path: drv_path.clone(),
        attr,
        ifd: true,
        licenses: vec![],
        exec: None,
    };
    let mut abort = imports.abort.clone();
    walk_and_publish(&imports, vec![root], &mut abort)
        .await
        .map_err(|e| format!("recording import '{drv_path}' failed: {e:#}"))?;

    let outcome = imports
        .reporter
        .request_import(vec![drv_path.clone()])
        .await
        .map_err(|e| format!("import request for '{drv_path}' failed: {e:#}"))?;
    match outcome {
        ImportOutcome::Completed { realisations } => {
            pull_outputs(imports.reporter, &drv_path, &drv, realisations).await
        }
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

async fn pull_outputs(
    reporter: &dyn JobReporter,
    drv_path: &str,
    drv: &Derivation,
    realisations: Vec<Realisation>,
) -> Result<Vec<Realisation>, String> {
    let mut outputs = input_addressed_outputs(drv_path, drv);
    reporter
        .pull_paths(
            outputs
                .iter()
                .chain(&realisations)
                .map(|o| o.path.clone())
                .collect(),
        )
        .await
        .map_err(|e| format!("pulling the outputs of import '{drv_path}' failed: {e:#}"))?;
    if !realisations.is_empty() {
        reporter
            .register_realisations(realisations.clone())
            .await
            .map_err(|e| format!("registering the outputs of import '{drv_path}' failed: {e:#}"))?;
    }

    outputs.extend(realisations);
    Ok(outputs)
}

async fn read_drv(drv_reader: &dyn DrvReader, drv_path: &str) -> Result<Derivation, String> {
    let bytes = drv_reader
        .read_drv(drv_path)
        .await
        .map_err(|e| format!("cannot read import '{drv_path}': {e:#}"))?;
    parse_drv(&bytes).map_err(|e| format!("cannot parse import '{drv_path}': {e:#}"))
}

fn input_addressed_outputs(drv_path: &str, drv: &Derivation) -> Vec<Realisation> {
    drv.outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| Realisation {
            drv_path: drv_path.to_owned(),
            output: o.name.clone(),
            path: o.path.clone(),
        })
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
        let builder =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

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
        let builder =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

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
        let builder =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

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
        let builder =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

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
            Imports::import_attr(&mut names, "aarch64-linux", drv),
            "other.aarch64-linux.hello-2.12"
        );
        assert_eq!(
            Imports::import_attr(&mut names, "aarch64-linux", drv),
            "other.aarch64-linux.hello-2.12",
            "the same derivation keeps its attribute"
        );
    }

    const INPUT: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source";

    fn request() -> String {
        format!(
            r#"{{"builtin":"fetchTree","args":{{"type":"tarball","url":"https://example.com/x.tar.gz"}},"storePath":"{INPUT}"}}"#
        )
    }

    fn fetched() -> InputOutcome {
        InputOutcome::Fetched {
            response: format!(r#"{{"storePath":"{INPUT}","attrs":{{"type":"tarball"}}}}"#),
        }
    }

    fn pulled(reporter: &RecordingJobReporter) -> Vec<Vec<String>> {
        reporter
            .events()
            .into_iter()
            .filter_map(|e| match e {
                ReportedEvent::PathsPulled { paths } => Some(paths),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_input_in_the_cache_is_pulled_without_a_fetch() {
        let reporter = RecordingJobReporter::new().with_cached_paths(vec![INPUT.into()]);
        let reader = reader();
        let source =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

        let answer = source.fetch_input(request()).await;

        assert_eq!(answer, Ok(format!(r#"{{"storePath":"{INPUT}"}}"#)));
        assert_eq!(pulled(&reporter), vec![vec![INPUT.to_owned()]]);
        assert!(reporter.input_requests().is_empty(), "no InputRequest");
    }

    #[tokio::test]
    async fn a_missing_input_is_fetched_through_the_server_and_pulled() {
        let reporter = RecordingJobReporter::new().with_input_outcome(INPUT, fetched());
        let reader = reader();
        let source =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

        let answer = source.fetch_input(request()).await;

        let InputOutcome::Fetched { response } = fetched() else {
            unreachable!()
        };
        assert_eq!(answer, Ok(response));
        assert_eq!(reporter.input_requests(), vec![request()]);
        assert_eq!(pulled(&reporter), vec![vec![INPUT.to_owned()]]);
    }

    #[tokio::test]
    async fn a_failed_input_is_not_requested_twice() {
        let reporter = RecordingJobReporter::new().with_input_outcome(
            INPUT,
            InputOutcome::Failed {
                message: "HTTP error 404".into(),
            },
        );
        let reader = reader();
        let source =
            EvalDependencySource::new(Imports::new(&reporter, &reader, AbortSignal::never()));

        let first = source.fetch_input(request()).await;
        let second = source.fetch_input(request()).await;

        assert_eq!(first, Err("HTTP error 404".to_owned()));
        assert_eq!(second, first);
        assert_eq!(reporter.input_requests().len(), 1);
    }
}
