/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gradient_db::exec_jobs::upcoming::{UpcomingExecJob, without_image};
use gradient_pool::session_port::SessionSignal;
use gradient_pool::{DeviceCount, WorkerInfo};
use gradient_util::sync::Mutex;
use tracing::warn;

use crate::Scheduler;
use crate::loops::{caps_of, serves_project};

#[derive(Clone, Default)]
pub(crate) struct PreWarmCounts {
    sent: Arc<Mutex<HashMap<String, u32>>>,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl PreWarmCounts {
    pub(crate) async fn changed(&self, counts: BTreeMap<String, u32>) -> Vec<(String, u32)> {
        let mut sent = self.sent.lock();
        counts
            .into_iter()
            .filter(|(worker, count)| sent.insert(worker.clone(), *count).unwrap_or(0) != *count)
            .collect()
    }

    pub(crate) fn forget(&self, worker_id: &str) {
        self.sent.lock().remove(worker_id);
    }
}

pub(crate) fn pre_warm_counts(
    upcoming: &[UpcomingExecJob],
    workers: &[WorkerInfo],
) -> BTreeMap<String, u32> {
    workers
        .iter()
        .filter_map(|worker| {
            let caps = caps_of(worker);
            let inventory = caps.exec.as_ref()?;
            let count = upcoming
                .iter()
                .filter(|exec| exec.spec.image.is_none())
                .filter(|exec| serves_project(worker, exec.project))
                .filter(|exec| {
                    let requirement = exec
                        .spec
                        .requirement_on(&exec.architecture, &inventory.defaults);
                    caps.fits_exec(&requirement, DeviceCount::Total)
                })
                .map(|exec| exec.spec.nodes.max(1))
                .sum();
            Some((worker.id.clone(), count))
        })
        .collect()
}

impl Scheduler {
    pub(crate) async fn refresh_pre_warm(&self) {
        let _turn = self.pre_warm.turn.lock().await;
        let upcoming = match without_image(&self.db.worker_db).await {
            Ok(upcoming) => upcoming,
            Err(e) => {
                warn!(error = %e, "reading the upcoming exec jobs failed; no pre-warm count sent");
                return;
            }
        };
        let workers = self.board_workers().await;
        let changed = self
            .pre_warm
            .changed(pre_warm_counts(&upcoming, &workers))
            .await;
        if changed.is_empty() {
            return;
        }

        let signals = changed
            .into_iter()
            .map(|(worker, count)| (worker, SessionSignal::PreWarmVms { count }))
            .collect();
        self.signal_workers(signals).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_jobs::{
        build_only_caps, eight_gpu_exec_caps_with_free, exec_caps_labelled, worker_info,
    };
    use gradient_types::ProjectId;
    use gradient_types::exec::{DeviceRequest, ExecSpec};
    use std::collections::HashSet;

    fn upcoming(spec: ExecSpec) -> UpcomingExecJob {
        upcoming_of(ProjectId::nil(), spec)
    }

    fn upcoming_of(project: ProjectId, spec: ExecSpec) -> UpcomingExecJob {
        UpcomingExecJob {
            spec,
            architecture: "x86_64-linux".into(),
            project,
        }
    }

    #[test]
    fn an_exec_counts_its_nodes_on_every_matching_worker() {
        let upcoming = [upcoming(ExecSpec {
            nodes: 2,
            labels: vec!["lab-a".into()],
            ..ExecSpec::default()
        })];
        let workers = [
            worker_info("w1", exec_caps_labelled("lab-a")),
            worker_info("w2", exec_caps_labelled("lab-a")),
            worker_info("w3", exec_caps_labelled("lab-b")),
            worker_info("b1", build_only_caps()),
        ];
        assert_eq!(
            pre_warm_counts(&upcoming, &workers),
            BTreeMap::from([("w1".into(), 2), ("w2".into(), 2), ("w3".into(), 0)])
        );
    }

    #[test]
    fn busy_devices_still_count_and_a_custom_image_counts_nowhere() {
        let gpus = ExecSpec {
            devices: vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 8,
            }],
            ..ExecSpec::default()
        };
        let image = ExecSpec {
            image: Some("/nix/store/aaaa-toplevel.drv".into()),
            ..ExecSpec::default()
        };
        let workers = [worker_info("w1", eight_gpu_exec_caps_with_free(0))];
        assert_eq!(
            pre_warm_counts(&[upcoming(gpus), upcoming(image)], &workers),
            BTreeMap::from([("w1".into(), 1)])
        );
    }

    #[test]
    fn a_worker_counts_only_jobs_of_projects_it_serves() {
        let mine = ProjectId::now_v7();
        let other = ProjectId::now_v7();
        let upcoming = [
            upcoming_of(mine, ExecSpec::default()),
            upcoming_of(other, ExecSpec::default()),
        ];
        let restricted = worker_info("restricted", exec_caps_labelled("lab-a"));
        let open = worker_info("open", exec_caps_labelled("lab-a"));
        let draining = worker_info("draining", exec_caps_labelled("lab-a"));
        let workers = [
            WorkerInfo {
                authorized_peers: Some(HashSet::from([mine])),
                ..restricted
            },
            open,
            WorkerInfo {
                draining: true,
                ..draining
            },
        ];
        assert_eq!(
            pre_warm_counts(&upcoming, &workers),
            BTreeMap::from([
                ("draining".into(), 0),
                ("open".into(), 2),
                ("restricted".into(), 1),
            ])
        );
    }

    #[tokio::test]
    async fn only_changed_counts_are_sent() {
        let counts = PreWarmCounts::default();
        assert_eq!(
            counts
                .changed(BTreeMap::from([("w1".into(), 2), ("w2".into(), 0)]))
                .await,
            vec![("w1".into(), 2)]
        );
        assert!(
            counts
                .changed(BTreeMap::from([("w1".into(), 2)]))
                .await
                .is_empty()
        );
        assert_eq!(
            counts.changed(BTreeMap::from([("w1".into(), 0)])).await,
            vec![("w1".into(), 0)]
        );
        counts.changed(BTreeMap::from([("w1".into(), 3)])).await;
        counts.forget("w1");
        assert_eq!(
            counts.changed(BTreeMap::from([("w1".into(), 3)])).await,
            vec![("w1".into(), 3)]
        );
    }
}
