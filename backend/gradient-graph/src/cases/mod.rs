/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod signing;

use std::collections::HashMap;
use std::sync::Arc;

use gradient_db::graph::consistency::{ConsistencyReport, graph_consistency_report};
use gradient_db::{DbContext, WebDb};
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_entity::{
    build_attempt, cache, cached_path, cached_path_signature, derivation, derivation_build,
    derivation_dependency, derivation_output, evaluation,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::{Shape, shape};
use gradient_wire::types::{DerivationOutput, DiscoveredDerivation};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::Graph;
use crate::messages::{
    DemoteReport, Demotion, GcReport, GcRequest, NarCommit, NarCommitted, RequeueScope, Transition,
    TransitionReport, UpstreamHit,
};
use crate::test_ctx::{Batch, BatchReport, ctx_with_crypt_file, secret_file, send, signing_key};
use crate::writer::GraphMsg;

pub(crate) const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
pub(crate) const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";
pub(crate) const A_OUT: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
pub(crate) const B_OUT: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

pub(crate) struct Case {
    pub ctx: DbContext,
    pub db: TestDatabase,
    pub graph: Arc<Graph>,
    writer: ractor::ActorRef<GraphMsg>,
}

thread_local! {
    static SECRET: (tempfile::NamedTempFile, String) = secret_file();
}

impl Case {
    pub fn key() -> String {
        SECRET.with(|(_, path)| signing_key(path))
    }

    pub async fn start(shape: Shape) -> Self {
        let declared = shape.declared_statuses();
        let secret = SECRET.with(|(_, path)| path.clone());
        let db = shape.into_database().await;
        let (ctx, _) = ctx_with_crypt_file(db.connection(), &secret).await;
        let ctx = DbContext {
            web_db: WebDb::new(db.connection()),
            ..ctx
        };

        graph_consistency_report(&ctx)
            .await
            .expect("the seed settles");

        for (derivation, status) in declared {
            let settled = db.shared_build(&derivation).await.status;
            assert_eq!(
                settled, status,
                "seed of {derivation}: declared {status:?}, production settles it to {settled:?}"
            );
        }

        let graph = Graph::new();
        let writer = graph
            .spawn(ctx.clone(), None, None)
            .await
            .expect("the graph writer starts");

        let case = Self {
            ctx,
            db,
            graph,
            writer,
        };

        case.zero_drift("the seed").await;
        case
    }

    pub async fn walk(&self, batch: Batch) -> BatchReport {
        self.walk_result(batch).await.expect("the batch is written")
    }

    pub async fn walk_result(&self, batch: Batch) -> anyhow::Result<BatchReport> {
        send(&self.graph, batch).await
    }

    pub async fn commit_nar(&self, commit: NarCommit) -> NarCommitted {
        self.commit_nar_result(commit)
            .await
            .expect("the NAR is committed")
    }

    pub async fn commit_nar_result(&self, commit: NarCommit) -> anyhow::Result<NarCommitted> {
        self.graph.commit_nar(commit).await
    }

    pub async fn sb(&self, path: &str) -> derivation_build::Model {
        self.db.shared_build(path).await
    }

    pub async fn cached_path(&self, hash: &str) -> cached_path::Model {
        cached_path::Entity::find()
            .filter(cached_path::Column::Hash.eq(hash))
            .one(&self.db.connection())
            .await
            .expect("the cached path reads")
            .unwrap_or_else(|| panic!("no cached path {hash}"))
    }

    pub async fn signatures(&self, hash: &str) -> Vec<cached_path_signature::Model> {
        self.cached_path(hash).await;
        self.db.signatures(hash).await
    }

    pub async fn signature(
        &self,
        hash: &str,
        cache: CacheId,
    ) -> Option<cached_path_signature::Model> {
        self.signatures(hash)
            .await
            .into_iter()
            .find(|s| s.cache == cache)
    }

    pub async fn verifies(
        &self,
        signature: &cached_path_signature::Model,
        content: &NarCommit,
    ) -> bool {
        let Some(bytes) = &signature.signature else {
            return false;
        };

        let cache = cache::Entity::find_by_id(signature.cache)
            .one(&self.db.connection())
            .await
            .expect("the cache reads")
            .unwrap_or_else(|| panic!("no cache {}", signature.cache));

        let (secret, url) = (
            &self.ctx.config.secrets.crypt_file,
            &self.ctx.config.server.serve_url,
        );
        let token = gradient_sources::full_signature_token(bytes, url, &cache.name);
        let key = gradient_sources::format_cache_public_key(
            secret,
            cache::Model {
                public_key: String::new(),
                ..cache
            },
            url.clone(),
        )
        .expect("the public key of the cache");

        let narinfo = format!(
            "StorePath: {}\nNarHash: {}\nNarSize: {}\nReferences: {}\nSig: {token}\n",
            content.store_path,
            content.nar_hash,
            content.nar_size,
            content.references.join(" "),
        );

        gradient_sources::verify_narinfo_signature(&key, &narinfo)
    }

    pub async fn drift(&self) -> ConsistencyReport {
        graph_consistency_report(&self.ctx)
            .await
            .expect("the consistency check runs")
    }

    pub async fn zero_drift(&self, after: &str) {
        let report = self.drift().await;
        assert_eq!(report.total(), 0, "drift after {after}: {report:?}");
    }

    pub async fn finish(self) {
        self.writer
            .stop_and_wait(None, None)
            .await
            .expect("the graph writer stops");

        self.zero_drift("the last step").await;
        drop((self.ctx, self.graph));
        self.db.finish().await;
    }
}

#[expect(dead_code, reason = "the themes after signing call these")]
impl Case {
    pub async fn upstream_hits(&self, hits: HashMap<String, UpstreamHit>) {
        self.graph
            .upstream_hits(hits)
            .await
            .expect("the hits are written");
    }

    pub async fn upstream_probed(&self, paths: &[&str]) {
        let mut ids = Vec::new();
        for path in paths {
            ids.push(self.db.id_of(path).await);
        }

        self.graph
            .upstream_probed(ids)
            .await
            .expect("the probe answer is written");
    }

    pub async fn transition(&self, transition: Transition) -> TransitionReport {
        self.graph
            .transition(transition)
            .await
            .expect("the transition is written")
    }

    pub async fn requeue(&self) -> u64 {
        self.graph
            .requeue(RequeueScope::TransientRetries)
            .await
            .expect("the requeue runs")
    }

    pub async fn demote(&self, demotion: Demotion) -> DemoteReport {
        self.graph
            .demote(demotion)
            .await
            .expect("the demotion runs")
    }

    pub async fn gc(&self, request: GcRequest) -> GcReport {
        self.graph.gc(request).await.expect("the collection runs")
    }

    pub async fn known(&self, drv_hashes: &[&str]) -> Vec<String> {
        let hashes = drv_hashes.iter().map(|h| (*h).to_owned()).collect();
        self.graph
            .known_derivations(hashes)
            .await
            .expect("the known derivations read")
    }

    pub async fn derivation(&self, path: &str) -> derivation::Model {
        self.db.derivation(path).await
    }

    pub async fn edge(&self, from: &str, to: &str) -> Option<derivation_dependency::Model> {
        self.db.edge(from, to).await
    }

    pub async fn output(&self, hash: &str) -> derivation_output::Model {
        self.db.output(hash).await
    }

    pub async fn evaluation(&self, id: EvaluationId) -> evaluation::Model {
        evaluation::Entity::find_by_id(id)
            .one(&self.db.connection())
            .await
            .expect("the evaluation reads")
            .unwrap_or_else(|| panic!("no evaluation {id}"))
    }

    pub async fn counters(&self, id: EvaluationId) -> [i64; 5] {
        let c = gradient_db::evaluations::counters::eval_counters(&self.db.connection(), id)
            .await
            .expect("the counters read")
            .unwrap_or_else(|| panic!("no counters of {id}"));

        [c.named, c.active, c.failed, c.queued, c.building]
    }

    pub async fn gv(&self, id: EvaluationId) -> i64 {
        self.evaluation(id).await.graph_version
    }

    pub async fn attempts(&self, path: &str) -> Vec<build_attempt::Model> {
        build_attempt::Entity::find()
            .filter(build_attempt::Column::DerivationBuild.eq(self.sb(path).await.id))
            .all(&self.db.connection())
            .await
            .expect("the attempts read")
    }
}

pub(crate) fn chain(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .depends_on(B)
        .derivation(B)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(B_OUT)
        .build_status(BuildStatus::Queued)
}

pub(crate) fn batch(evaluation: EvaluationId) -> Batch {
    Batch {
        evaluation,
        ..Default::default()
    }
}

pub(crate) fn drv(path: &str, dependencies: &[&str]) -> DiscoveredDerivation {
    DiscoveredDerivation {
        drv_path: path.to_owned(),
        dependencies: dependencies.iter().map(|d| (*d).to_owned()).collect(),
        architecture: "x86_64-linux".to_owned(),
        allow_substitutes: true,
        ..Default::default()
    }
}

pub(crate) fn with_output(
    mut derivation: DiscoveredDerivation,
    hash: &str,
) -> DiscoveredDerivation {
    let name = derivation
        .drv_path
        .split_once('-')
        .map_or("out", |(_, name)| name.trim_end_matches(".drv"))
        .to_owned();

    derivation.outputs.push(DerivationOutput {
        name: "out".into(),
        path: format!("/nix/store/{hash}-{name}"),
    });

    derivation
}

pub(crate) fn with_attr(mut derivation: DiscoveredDerivation, attr: &str) -> DiscoveredDerivation {
    derivation.attr = attr.to_owned();
    derivation
}

pub(crate) fn nar(hash: &str, package: &str) -> NarCommit {
    NarCommit {
        store_path: format!("/nix/store/{hash}-{package}"),
        file_hash: "sha256:abc".into(),
        file_size: 5,
        nar_size: 5,
        nar_hash: "sha256:def".into(),
        confirmed: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[should_panic(expected = "declared Queued")]
    async fn a_seed_production_would_repair_fails_before_the_first_step() {
        Case::start(
            shape()
                .evaluation(EvaluationId::now_v7())
                .derivation(A)
                .build_status(BuildStatus::Queued),
        )
        .await;
    }

    #[tokio::test]
    async fn an_empty_shape_is_a_consistent_seed() {
        Case::start(shape()).await.finish().await;
    }
}
