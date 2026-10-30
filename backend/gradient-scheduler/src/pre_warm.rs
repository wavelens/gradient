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
use gradient_types::exec::{WarmGuests, count_warm_guests};
use gradient_util::sync::Mutex;
use tracing::warn;

use crate::Scheduler;
use crate::loops::{caps_of, serves_project};

#[derive(Clone, Default)]
pub(crate) struct PreWarmCounts {
    sent: Arc<Mutex<HashMap<String, Vec<WarmGuests>>>>,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl PreWarmCounts {
    pub(crate) async fn changed(
        &self,
        wanted: BTreeMap<String, Vec<WarmGuests>>,
    ) -> Vec<(String, Vec<WarmGuests>)> {
        let mut sent = self.sent.lock();
        wanted
            .into_iter()
            .filter(|(worker, sizes)| {
                sent.insert(worker.clone(), sizes.clone())
                    .unwrap_or_default()
                    != *sizes
            })
            .collect()
    }

    pub(crate) fn forget(&self, worker_id: &str) {
        self.sent.lock().remove(worker_id);
    }
}

pub(crate) fn pre_warm_counts(
    upcoming: &[UpcomingExecJob],
    workers: &[WorkerInfo],
) -> BTreeMap<String, Vec<WarmGuests>> {
    workers
        .iter()
        .filter_map(|worker| {
            let caps = caps_of(worker);
            let inventory = caps.exec.as_ref()?;
            let sizes = upcoming
                .iter()
                .filter(|exec| exec.spec.image.is_none())
                .filter(|exec| serves_project(worker, exec.project))
                .filter_map(|exec| {
                    let requirement = exec
                        .spec
                        .requirement_on(&exec.architecture, &inventory.defaults);
                    caps.fits_exec(&requirement, DeviceCount::Total)
                        .then(|| (requirement.guest, exec.spec.nodes.max(1)))
                });
            Some((worker.id.clone(), count_warm_guests(sizes)))
        })
        .collect()
}

fn pre_warm_signal(sizes: Vec<WarmGuests>) -> SessionSignal {
    SessionSignal::PreWarmVms {
        count: sizes.iter().map(|warm| warm.count).sum(),
        sizes,
    }
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
            .map(|(worker, sizes)| (worker, pre_warm_signal(sizes)))
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
    use gradient_types::exec::{DeviceRequest, ExecSpec, GuestSize};
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

    fn totals(wanted: BTreeMap<String, Vec<WarmGuests>>) -> BTreeMap<String, u32> {
        wanted
            .into_iter()
            .map(|(worker, sizes)| (worker, sizes.iter().map(|warm| warm.count).sum()))
            .collect()
    }

    fn want(count: u32) -> Vec<WarmGuests> {
        count_warm_guests([(GuestSize::default_request(), count)])
    }

    #[test]
    fn upcoming_jobs_count_per_guest_size() {
        let sized = |cpus, nodes| {
            upcoming(ExecSpec {
                nodes,
                guest: GuestSize {
                    cpus,
                    ..GuestSize::default_request()
                },
                ..ExecSpec::default()
            })
        };
        let mut caps = exec_caps_labelled("lab-a");
        if let Some(inventory) = caps.exec.as_mut() {
            inventory.allow.nodes = 2;
        }
        let workers = [worker_info("w1", caps)];
        let wanted = pre_warm_counts(&[sized(2, 1), sized(8, 2), sized(2, 1)], &workers);
        let warm = |cpus| GuestSize {
            cpus,
            ..GuestSize::default_request()
        };
        assert_eq!(
            wanted["w1"],
            count_warm_guests([(warm(2), 2), (warm(8), 2)])
        );
        let SessionSignal::PreWarmVms { count, sizes } = pre_warm_signal(wanted["w1"].clone())
        else {
            panic!("expected a pre-warm signal");
        };
        assert_eq!((count, sizes.len()), (4, 2));
    }

    #[test]
    fn an_exec_counts_its_nodes_on_every_matching_worker() {
        let upcoming = [upcoming(ExecSpec {
            nodes: 2,
            labels: vec!["lab-a".into()],
            ..ExecSpec::default()
        })];
        let two_node_worker = |id: &str, label: &str| {
            let mut caps = exec_caps_labelled(label);
            if let Some(inventory) = caps.exec.as_mut() {
                inventory.allow.nodes = 2;
            }
            worker_info(id, caps)
        };
        let workers = [
            two_node_worker("w1", "lab-a"),
            two_node_worker("w2", "lab-a"),
            two_node_worker("w3", "lab-b"),
            worker_info("b1", build_only_caps()),
        ];
        assert_eq!(
            totals(pre_warm_counts(&upcoming, &workers)),
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
            totals(pre_warm_counts(
                &[upcoming(gpus), upcoming(image)],
                &workers
            )),
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
            totals(pre_warm_counts(&upcoming, &workers)),
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
                .changed(BTreeMap::from([
                    ("w1".into(), want(2)),
                    ("w2".into(), want(0))
                ]))
                .await,
            vec![("w1".into(), want(2))]
        );
        assert!(
            counts
                .changed(BTreeMap::from([("w1".into(), want(2))]))
                .await
                .is_empty()
        );
        assert_eq!(
            counts
                .changed(BTreeMap::from([("w1".into(), want(0))]))
                .await,
            vec![("w1".into(), want(0))]
        );
        counts
            .changed(BTreeMap::from([("w1".into(), want(3))]))
            .await;
        counts.forget("w1");
        assert_eq!(
            counts
                .changed(BTreeMap::from([("w1".into(), want(3))]))
                .await,
            vec![("w1".into(), want(3))]
        );
    }
}
