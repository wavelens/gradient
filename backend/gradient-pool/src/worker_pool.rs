/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use gradient_wire::types::{
    BuildMetrics, BuildStage, DeviceRequest, ExecInventory, GradientCapabilities, GuestSize,
    JobKind,
};
use gradient_wire::worker_store_path_bloom_filter::WorkerStorePathBloomFilter;

use crate::job_policy::{AllowedJobs, EveryJobAllowed, WorkerJobPolicy};
use crate::project_access::ProjectAccess;
use crate::session_port::{SessionPort, SessionSignal};
use crate::worker_state::{Active, AssignedJob, Draining, TypedWorker, WorkerShared};
pub use gradient_api_types::ExecOffer;

pub enum WorkerSlot {
    Active(TypedWorker<Active>),
    Draining(TypedWorker<Draining>),
}

impl WorkerSlot {
    fn shared(&self) -> &crate::worker_state::WorkerShared {
        match self {
            Self::Active(w) => w,
            Self::Draining(w) => w,
        }
    }

    fn shared_mut(&mut self) -> &mut crate::worker_state::WorkerShared {
        match self {
            Self::Active(w) => w,
            Self::Draining(w) => w,
        }
    }

    pub fn is_draining(&self) -> bool {
        matches!(self, Self::Draining(_))
    }
}

impl std::fmt::Debug for WorkerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active(w) => f.debug_tuple("Active").field(&w.shared).finish(),
            Self::Draining(w) => f.debug_tuple("Draining").field(&w.shared).finish(),
        }
    }
}

#[derive(Debug)]
pub struct WorkerPool {
    workers: HashMap<String, WorkerSlot>,
    job_policy: Arc<dyn WorkerJobPolicy>,
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::with_job_policy(Arc::new(EveryJobAllowed))
    }
}

impl WorkerPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_job_policy(job_policy: Arc<dyn WorkerJobPolicy>) -> Self {
        Self {
            workers: HashMap::new(),
            job_policy,
        }
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.workers.contains_key(id)
    }

    /// The prior connection's sizing is carried forward because a reconnect may arrive without
    /// fresh `WorkerCapabilities`. The `last_seen` Arc is reused for readers still holding the old
    /// handle.
    pub fn register(
        &mut self,
        id: String,
        capabilities: GradientCapabilities,
        project_access: ProjectAccess,
        session: Arc<dyn SessionPort>,
    ) -> Arc<AtomicI64> {
        let prior = self.workers.get(&id).map(|slot| {
            (
                slot.shared().profile(),
                Arc::clone(&slot.shared().last_seen),
            )
        });
        let mut worker = TypedWorker::<Active>::new(capabilities, project_access, session);
        if let Some((profile, last_seen)) = prior {
            worker.apply_profile(profile);
            last_seen.store(
                gradient_types::now().and_utc().timestamp_millis(),
                Ordering::Relaxed,
            );
            worker.last_seen = last_seen;
        }

        let last_seen = Arc::clone(&worker.last_seen);
        self.workers.insert(id, WorkerSlot::Active(worker));
        last_seen
    }

    pub fn set_reclaims_jobs(&mut self, id: &str, reclaims_jobs: bool) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().reclaims_jobs = reclaims_jobs;
        }
    }

    pub fn reclaims_jobs(&self, id: &str) -> bool {
        self.workers
            .get(id)
            .is_some_and(|slot| slot.shared().reclaims_jobs)
    }

    pub fn request_reauth(&self, worker_id: &str) {
        if let Some(slot) = self.workers.get(worker_id) {
            slot.shared().session.signal(SessionSignal::Reauth);
        }
    }

    pub fn signal(&self, worker_id: &str, signal: SessionSignal) -> bool {
        match self.workers.get(worker_id) {
            Some(slot) => {
                slot.shared().session.signal(signal);
                true
            }
            None => false,
        }
    }

    pub fn session_port(&self, worker_id: &str) -> Option<Arc<dyn SessionPort>> {
        self.workers
            .get(worker_id)
            .map(|slot| Arc::clone(&slot.shared().session))
    }

    pub fn send_abort(&self, worker_id: &str, job_id: String, reason: String) -> bool {
        self.signal(worker_id, SessionSignal::Abort { job_id, reason })
    }

    pub fn replace_worker_authorization(
        &mut self,
        id: &str,
        project_access: ProjectAccess,
        capabilities: GradientCapabilities,
    ) -> bool {
        let Some(slot) = self.workers.get_mut(id) else {
            return false;
        };
        let shared = slot.shared_mut();
        shared.project_access = project_access;
        shared.capabilities = capabilities;
        true
    }

    pub fn project_access_for_worker_id(&self, id: &str) -> Option<&ProjectAccess> {
        self.workers
            .get(id)
            .map(|slot| &slot.shared().project_access)
    }

    pub fn gradient_caps_for(&self, id: &str) -> Option<GradientCapabilities> {
        self.workers
            .get(id)
            .map(|slot| slot.shared().capabilities.clone())
    }

    pub fn worker_caps(&self, id: &str) -> Option<crate::WorkerCaps> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            crate::WorkerCaps {
                architectures: s.architectures.clone(),
                system_features: s.system_features.clone(),
                capabilities: s.capabilities.clone(),
                metrics: self.metrics_for(id),
                zone: s.zone.clone(),
                endpoint: s.endpoint.clone(),
                exec: s.exec_inventory(),
                allowed_jobs: AllowedJobs::of(id, &self.job_policy),
            }
        })
    }

    pub fn worker_local_store_bloom_filter(&self, id: &str) -> Option<&WorkerStorePathBloomFilter> {
        self.workers
            .get(id)
            .and_then(|slot| slot.shared().local_store_bloom_filter.as_ref())
    }

    pub fn replace_worker_local_store_bloom_filter(
        &mut self,
        id: &str,
        filter: WorkerStorePathBloomFilter,
    ) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().local_store_bloom_filter = Some(filter);
        }
    }

    pub fn add_store_path_probe_words_to_worker_local_store_bloom_filter(
        &mut self,
        id: &str,
        probe_words: impl IntoIterator<Item = [u64; 2]>,
    ) {
        let Some(filter) = self
            .workers
            .get_mut(id)
            .and_then(|slot| slot.shared_mut().local_store_bloom_filter.as_mut())
        else {
            return;
        };
        for words in probe_words {
            filter.insert_probe_words_into_filter(words);
        }
    }

    pub fn update_capabilities(&mut self, id: &str, profile: crate::WorkerProfile) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().apply_profile(profile);
        }
    }

    pub fn update_exec_inventory(&mut self, id: &str, exec: Option<ExecInventory>) {
        if let Some(slot) = self.workers.get_mut(id) {
            slot.shared_mut().exec = exec;
        }
    }

    pub fn update_metrics(
        &mut self,
        id: &str,
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
        upload_speed_mbps: Option<f32>,
        download_speed_mbps: Option<f32>,
    ) {
        if let Some(slot) = self.workers.get_mut(id) {
            let s = slot.shared_mut();
            s.cpu_usage_pct = Some(cpu_usage_pct);
            s.ram_free_mb = Some(ram_free_mb);
            s.disk_speed_mbps = disk_speed_mbps;
            s.upload_speed_mbps = upload_speed_mbps;
            s.download_speed_mbps = download_speed_mbps;
        }
    }

    /// `None` is a worker without access to the build cgroups. Builds missing from the list have
    /// not started, and count as using nothing.
    pub fn observe_build_ram(&mut self, id: &str, peak_ram_mb: Option<Vec<(String, u64)>>) {
        let Some(slot) = self.workers.get_mut(id) else {
            return;
        };
        let s = slot.shared_mut();
        s.observes_build_ram = peak_ram_mb.is_some();
        let ram_total_mb = s.ram_total_mb;
        for (job_id, peak) in peak_ram_mb.into_iter().flatten() {
            if let Some(job) = s.assigned_jobs.get_mut(&job_id) {
                if peak > ram_total_mb {
                    tracing::debug!(worker = %id, %job_id, ram_total_mb, "A worker reported a peak RAM above the advertised RAM for a build in progress. The server clamps that value to the advertised RAM.");
                }
                job.observed_ram_mb = job.observed_ram_mb.max(peak.min(ram_total_mb));
            }
        }
    }

    pub fn clamp_reported_build_metrics_to_advertised_worker_capabilities(
        &self,
        id: &str,
        job_id: &str,
        metrics: BuildMetrics,
    ) -> BuildMetrics {
        let (ram_total_mb, cpu_count) = self
            .workers
            .get(id)
            .map(|slot| (slot.shared().ram_total_mb, slot.shared().cpu_count))
            .unwrap_or_default();
        let worker_reported_peak_ram_or_core_count_above_advertised =
            metrics.peak_ram_mb.is_some_and(|peak| peak > ram_total_mb)
                || metrics.build_cores.is_some_and(|cores| cores > cpu_count);
        let clamped = BuildMetrics {
            peak_ram_mb: metrics
                .peak_ram_mb
                .filter(|&peak| ram_total_mb > 0 && peak > 0)
                .map(|peak| peak.min(ram_total_mb)),
            build_cores: metrics
                .build_cores
                .filter(|&cores| cpu_count > 0 && cores > 0)
                .map(|cores| cores.min(cpu_count)),
            ..metrics.clone()
        };
        if worker_reported_peak_ram_or_core_count_above_advertised {
            tracing::warn!(worker = %id, %job_id, ram_total_mb, cpu_count, "A worker reported values above the limits that the worker advertised. Build metrics stay clamped to the RAM and core count that the worker advertised.");
        }
        clamped
    }

    pub fn observed_build_ram(&self, id: &str) -> HashMap<String, u64> {
        self.workers
            .get(id)
            .into_iter()
            .flat_map(|slot| slot.shared().observed_build_ram())
            .collect()
    }

    pub fn fleet_observed_build_ram(&self) -> Option<Vec<(String, u64)>> {
        let workers = || self.workers.values().map(WorkerSlot::shared);
        workers().all(|w| w.observes_build_ram).then(|| {
            workers()
                .flat_map(WorkerShared::observed_build_ram)
                .collect()
        })
    }

    pub fn metrics_for(&self, id: &str) -> Option<crate::score::WorkerMetricsView> {
        self.workers.get(id).map(|slot| {
            let s = slot.shared();
            crate::score::WorkerMetricsView {
                cpu_count: s.cpu_count,
                build_cores: s.build_cores.unwrap_or(0),
                cpu_core_score: s.cpu_core_score,
                ram_total_mb: s.ram_total_mb,
                ram_free_mb: s.ram_free_mb,
                cpu_usage_pct: s.cpu_usage_pct,
                disk_speed_mbps: s.disk_speed_mbps,
                upload_speed_mbps: s.upload_speed_mbps,
                download_speed_mbps: s.download_speed_mbps,
                running_builds: s.jobs_in(BuildStage::Build),
                ram_reserved_mb: s.ram_reserved_mb(),
                ram_reserved_unused_mb: s.ram_reserved_unused_mb(),
                ram_reserved_for_waiting_build_mb: 0,
            }
        })
    }

    /// The session must be closed too. A dropped slot alone would leave an untracked live socket,
    /// and the evicted worker would never reconnect.
    pub fn unregister(&mut self, id: &str) -> Vec<String> {
        self.workers
            .remove(id)
            .map(|slot| {
                let shared = slot.shared();
                shared.session.signal(SessionSignal::Close {
                    reason: "unregistered by the scheduler".into(),
                });
                shared.assigned_jobs.keys().cloned().collect()
            })
            .unwrap_or_default()
    }

    pub fn aggregate(&self) -> crate::Aggregate {
        crate::aggregate(
            self.workers
                .values()
                .filter(|slot| !slot.is_draining())
                .map(WorkerSlot::shared),
        )
    }

    pub fn mark_draining(&mut self, id: &str) {
        if let Some(slot) = self.workers.remove(id) {
            let new_slot = match slot {
                WorkerSlot::Active(w) => WorkerSlot::Draining(w.into_draining()),
                already_draining => already_draining,
            };
            self.workers.insert(id.to_owned(), new_slot);
        }
    }

    pub fn cancel_draining(&mut self, id: &str) {
        if let Some(slot) = self.workers.remove(id) {
            let new_slot = match slot {
                WorkerSlot::Draining(w) => WorkerSlot::Active(w.into_active()),
                active => active,
            };
            self.workers.insert(id.to_owned(), new_slot);
        }
    }

    pub fn has_capacity(&self, worker_id: &str, kind: &JobKind) -> bool {
        match self.workers.get(worker_id) {
            Some(WorkerSlot::Active(w)) => match kind {
                JobKind::Flake => true,
                JobKind::Build => w.has_build_capacity(),
                JobKind::Exec => w.exec.is_some(),
            },
            Some(WorkerSlot::Draining(_)) => false,
            None => false,
        }
    }

    pub fn has_idle_eval_only_worker(&self) -> bool {
        self.workers.values().any(|slot| match slot {
            WorkerSlot::Active(w) => {
                w.capabilities.eval && !w.capabilities.network && w.assigned_jobs.is_empty()
            }
            WorkerSlot::Draining(_) => false,
        })
    }

    pub fn assign_job(
        &mut self,
        worker_id: &str,
        job_id: &str,
        kind: JobKind,
        reserved_ram_mb: u64,
    ) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            let jobs = &mut slot.shared_mut().assigned_jobs;
            let (devices, guest) = jobs
                .remove(job_id)
                .map(|held| (held.devices, held.guest))
                .unwrap_or_default();
            jobs.insert(
                job_id.to_owned(),
                AssignedJob {
                    kind,
                    stage: None,
                    reserved_ram_mb,
                    observed_ram_mb: 0,
                    devices,
                    guest,
                },
            );
        }
    }

    pub fn hold_exec(
        &mut self,
        worker_id: &str,
        job_id: &str,
        devices: Vec<DeviceRequest>,
        guest: GuestSize,
    ) {
        if let Some(slot) = self.workers.get_mut(worker_id) {
            let held = slot
                .shared_mut()
                .assigned_jobs
                .entry(job_id.to_owned())
                .or_insert_with(|| AssignedJob {
                    kind: JobKind::Exec,
                    stage: None,
                    reserved_ram_mb: 0,
                    observed_ram_mb: 0,
                    devices: Vec::new(),
                    guest: GuestSize::default(),
                });
            held.devices = devices;
            held.guest = guest;
        }
    }

    pub fn enter_stage(&mut self, worker_id: &str, job_id: &str, stage: BuildStage) {
        if let Some(current) = self
            .workers
            .get_mut(worker_id)
            .and_then(|slot| slot.shared_mut().assigned_jobs.get_mut(job_id))
        {
            current.stage = Some(stage);
        }
    }

    pub fn fleet_jobs_in(&self, stage: BuildStage) -> u32 {
        self.workers
            .values()
            .map(|slot| slot.shared().jobs_in(stage))
            .sum()
    }

    pub fn release_job(&mut self, worker_id: &str, job_id: &str) -> bool {
        match self.workers.get_mut(worker_id) {
            Some(slot) => {
                let shared = slot.shared_mut();
                shared.assigned_jobs.remove(job_id);
                shared.assigned_jobs.is_empty()
            }
            None => false,
        }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn worker_counts(&self) -> (u32, u32) {
        let total = u32::try_from(self.workers.len()).unwrap_or(u32::MAX);
        let idle = self
            .workers
            .values()
            .filter(|slot| slot.shared().assigned_jobs.is_empty())
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        (total, idle)
    }

    pub fn mean_cpu_core_score(&self) -> Option<f64> {
        self.mean_of(|s| {
            Some(s.cpu_core_score)
                .filter(|score| *score > 0)
                .map(f64::from)
        })
    }

    pub fn mean_upload_speed_mbps(&self) -> Option<f64> {
        self.mean_of(|s| s.upload_speed_mbps.map(f64::from))
    }

    pub fn mean_download_speed_mbps(&self) -> Option<f64> {
        self.mean_of(|s| s.download_speed_mbps.map(f64::from))
    }

    fn mean_of(&self, value: impl Fn(&WorkerShared) -> Option<f64>) -> Option<f64> {
        let values: Vec<f64> = self
            .workers
            .values()
            .filter_map(|slot| value(slot.shared()))
            .collect();
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    }

    fn info_for(&self, id: &str, slot: &WorkerSlot) -> WorkerInfo {
        let s = slot.shared();
        WorkerInfo {
            id: id.to_owned(),
            capabilities: s.capabilities.clone(),
            architectures: s.architectures.clone(),
            system_features: s.system_features.clone(),
            max_concurrent_builds: s.max_concurrent_builds,
            assigned_job_count: s.assigned_jobs.len(),
            assigned_build_count: s.build_jobs() as usize,
            draining: slot.is_draining(),
            project_access: s.project_access.clone(),
            cpu_usage_pct: s.cpu_usage_pct,
            ram_free_mb: s.ram_free_mb,
            ram_total_mb: s.ram_total_mb,
            disk_speed_mbps: s.disk_speed_mbps,
            upload_speed_mbps: s.upload_speed_mbps,
            download_speed_mbps: s.download_speed_mbps,
            zone: s.zone.clone(),
            exec: s.exec_inventory(),
        }
    }

    pub fn all_workers(&self) -> Vec<WorkerInfo> {
        self.workers
            .iter()
            .map(|(id, slot)| self.info_for(id, slot))
            .collect()
    }

    pub fn stale_worker_ids(&self, now_ms: i64, timeout_ms: i64) -> Vec<String> {
        self.workers
            .iter()
            .filter(|(_, slot)| {
                now_ms - slot.shared().last_seen.load(Ordering::Relaxed) > timeout_ms
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub id: String,
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_job_count: usize,
    pub assigned_build_count: usize,
    pub draining: bool,
    pub project_access: ProjectAccess,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<u64>,
    pub ram_total_mb: u64,
    pub disk_speed_mbps: Option<f32>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub zone: Option<String>,
    pub exec: Option<ExecInventory>,
}

impl From<&WorkerInfo> for gradient_api_types::WorkerInfo {
    fn from(worker: &WorkerInfo) -> Self {
        Self {
            id: worker.id.clone(),
            capabilities: worker.capabilities.clone(),
            architectures: worker.architectures.clone(),
            system_features: worker.system_features.clone(),
            max_concurrent_builds: worker.max_concurrent_builds,
            assigned_job_count: worker.assigned_job_count,
            draining: worker.draining,
            authorized_peers: match &worker.project_access {
                ProjectAccess::AllProjects => None,
                ProjectAccess::OnlyListedProjects(projects) => Some(projects.clone()),
            },
            exec: worker.exec.clone().map(ExecOffer::from),
        }
    }
}

impl From<&WorkerInfo> for gradient_api_types::WorkerLiveInfo {
    fn from(w: &WorkerInfo) -> Self {
        Self {
            capabilities: w.capabilities.clone(),
            architectures: w.architectures.clone(),
            system_features: w.system_features.clone(),
            max_concurrent_builds: w.max_concurrent_builds,
            assigned_job_count: w.assigned_job_count,
            draining: w.draining,
            ram_total_mb: (w.ram_total_mb > 0).then_some(w.ram_total_mb),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_port::{SessionPort, SessionSignal};
    use crate::worker_state::WorkerProfile;
    use gradient_types::ids::ProjectId;
    use gradient_wire::types::DeviceClass;
    use std::collections::HashSet;
    use tokio::sync::mpsc;

    fn caps() -> GradientCapabilities {
        GradientCapabilities::default()
    }

    fn port() -> (Arc<dyn SessionPort>, mpsc::UnboundedReceiver<SessionSignal>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(tx), rx)
    }

    fn eval_caps(eval: bool, network: bool) -> GradientCapabilities {
        GradientCapabilities {
            eval,
            network,
            ..GradientCapabilities::default()
        }
    }

    #[test]
    fn idle_eval_only_worker_detected() {
        let mut pool = WorkerPool::new();
        pool.register(
            "f1".into(),
            eval_caps(true, true),
            ProjectAccess::AllProjects,
            port().0,
        );
        assert!(
            !pool.has_idle_eval_only_worker(),
            "only a network worker present"
        );

        pool.register(
            "e1".into(),
            eval_caps(true, false),
            ProjectAccess::AllProjects,
            port().0,
        );
        assert!(
            pool.has_idle_eval_only_worker(),
            "idle eval-only worker present"
        );

        pool.assign_job("e1", "j1", JobKind::Flake, 0);
        assert!(
            !pool.has_idle_eval_only_worker(),
            "eval-only worker is busy"
        );
    }

    #[test]
    fn draining_eval_only_worker_does_not_count() {
        let mut pool = WorkerPool::new();
        pool.register(
            "e1".into(),
            eval_caps(true, false),
            ProjectAccess::AllProjects,
            port().0,
        );
        pool.mark_draining("e1");
        assert!(
            !pool.has_idle_eval_only_worker(),
            "draining worker excluded"
        );
    }

    #[test]
    fn test_register_and_is_connected() {
        let mut pool = WorkerPool::new();
        assert!(!pool.is_connected("w1"));
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        assert!(pool.is_connected("w1"));
        assert_eq!(pool.worker_count(), 1);
    }

    #[test]
    fn test_unregister_returns_assigned_jobs() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.assign_job("w1", "j1", JobKind::Build, 0);
        pool.assign_job("w1", "j2", JobKind::Build, 0);

        let mut jobs = pool.unregister("w1");
        jobs.sort();
        assert_eq!(jobs, vec!["j1", "j2"]);
        assert!(!pool.is_connected("w1"));
        assert_eq!(pool.worker_count(), 0);
    }

    #[test]
    fn fleet_speed_means_skip_workers_that_measured_nothing() {
        let mut pool = WorkerPool::new();
        for (id, upload) in [("w1", Some(100.0)), ("w2", Some(300.0)), ("w3", None)] {
            pool.register(id.into(), caps(), ProjectAccess::AllProjects, port().0);
            pool.update_metrics(id, 0.0, 0, None, upload, None);
        }

        assert_eq!(pool.mean_upload_speed_mbps(), Some(200.0));
        assert_eq!(pool.mean_download_speed_mbps(), None);
    }

    #[test]
    fn mean_cpu_core_score_skips_workers_without_one() {
        let mut pool = WorkerPool::new();
        assert_eq!(pool.mean_cpu_core_score(), None);
        for (id, score) in [("w1", 1_000), ("w2", 3_000), ("w3", 0)] {
            pool.register(id.into(), caps(), ProjectAccess::AllProjects, port().0);
            pool.update_capabilities(
                id,
                WorkerProfile {
                    max_concurrent_builds: 1,
                    cpu_count: 1,
                    ram_total_mb: 1,
                    cpu_core_score: score,
                    ..Default::default()
                },
            );
        }
        assert_eq!(pool.mean_cpu_core_score(), Some(2_000.0));
    }

    #[test]
    fn reregister_preserves_reported_capabilities() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                system_features: vec!["kvm".into()],
                max_concurrent_builds: 4,
                cpu_count: 8,
                ram_total_mb: 16384,
                cpu_core_score: 1200,
                zone: Some("fra1".into()),
                endpoint: Some("10.0.0.7:7000".into()),
                build_cores: Some(6),
                exec: None,
            },
        );

        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);

        let workers = pool.all_workers();
        assert_eq!(workers[0].architectures, vec!["x86_64-linux"]);
        assert_eq!(workers[0].system_features, vec!["kvm"]);
        assert_eq!(workers[0].max_concurrent_builds, 4);
        let shared = pool.workers["w1"].shared();
        assert_eq!(shared.zone.as_deref(), Some("fra1"));
        assert_eq!(shared.endpoint.as_deref(), Some("10.0.0.7:7000"));
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!((view.cpu_count, view.build_cores), (8, 6));
        assert_eq!(view.ram_total_mb, 16384);
    }

    #[test]
    fn an_empty_zone_is_no_zone() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                zone: Some(String::new()),
                endpoint: Some(String::new()),
                ..Default::default()
            },
        );

        let shared = pool.workers["w1"].shared();
        assert_eq!(shared.zone, None);
        assert_eq!(shared.endpoint, None);
    }

    #[test]
    fn worker_caps_carry_the_zone() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                zone: Some("fra1".into()),
                ..Default::default()
            },
        );

        assert_eq!(
            pool.worker_caps("w1").unwrap().zone.as_deref(),
            Some("fra1")
        );
    }

    #[test]
    fn test_update_metrics_updates_view() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                max_concurrent_builds: 1,
                cpu_count: 4,
                ram_total_mb: 8192,
                cpu_core_score: 1000,
                ..Default::default()
            },
        );

        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_usage_pct, None);
        assert_eq!(view.ram_free_mb, None);
        assert_eq!(view.disk_speed_mbps, None);
        assert_eq!(view.upload_speed_mbps, None);
        assert_eq!(view.download_speed_mbps, None);

        pool.update_metrics("w1", 42.5, 3000, Some(550.0), Some(120.0), Some(900.0));
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.cpu_usage_pct, Some(42.5));
        assert_eq!(view.ram_free_mb, Some(3000));
        assert_eq!(view.disk_speed_mbps, Some(550.0));
        assert_eq!(view.upload_speed_mbps, Some(120.0));
        assert_eq!(view.download_speed_mbps, Some(900.0));
        assert_eq!(view.cpu_count, 4);
        assert_eq!(view.ram_total_mb, 8192);

        assert!(pool.metrics_for("unknown").is_none());
    }

    #[test]
    fn worker_caps_snapshot_is_coherent() {
        let mut pool = WorkerPool::new();
        pool.register(
            "w1".into(),
            GradientCapabilities {
                network: true,
                secrets: true,
                eval: true,
                ..Default::default()
            },
            ProjectAccess::AllProjects,
            port().0,
        );
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                system_features: vec!["kvm".into()],
                max_concurrent_builds: 4,
                cpu_count: 8,
                ram_total_mb: 16384,
                cpu_core_score: 1200,
                ..Default::default()
            },
        );
        pool.update_metrics("w1", 12.5, 9000, None, None, None);

        let caps = pool.worker_caps("w1").unwrap();
        assert!(caps.capabilities.network);
        assert!(caps.capabilities.eval);
        assert_eq!(caps.architectures, vec!["x86_64-linux"]);
        assert_eq!(caps.system_features, vec!["kvm"]);
        assert_eq!(caps.metrics.unwrap().ram_free_mb, Some(9000));

        assert!(pool.worker_caps("unknown").is_none());
    }

    #[test]
    fn test_draining_worker_has_no_capacity() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                max_concurrent_builds: 10,
                ..Default::default()
            },
        );

        assert!(pool.has_capacity("w1", &JobKind::Build));
        assert!(pool.has_capacity("w1", &JobKind::Flake));

        pool.mark_draining("w1");
        assert!(!pool.has_capacity("w1", &JobKind::Build));
        assert!(!pool.has_capacity("w1", &JobKind::Flake));

        pool.cancel_draining("w1");
        assert!(pool.has_capacity("w1", &JobKind::Build));
        assert!(pool.has_capacity("w1", &JobKind::Flake));
    }

    #[test]
    fn test_project_access_for_worker_id_returns_project_set_for_registered_worker_and_none_for_unknown_worker()
     {
        let mut pool = WorkerPool::new();
        let peer_a = ProjectId::now_v7();
        let peer_b = ProjectId::now_v7();

        pool.register(
            "w1".into(),
            caps(),
            ProjectAccess::OnlyListedProjects(HashSet::from([peer_a, peer_b])),
            port().0,
        );
        let access = pool.project_access_for_worker_id("w1").unwrap();
        assert!(access.contains(&peer_a));
        assert!(access.contains(&peer_b));
        assert!(!access.contains(&ProjectId::now_v7()));

        assert!(pool.project_access_for_worker_id("w2").is_none());
    }

    #[test]
    fn test_replace_worker_authorization_updates_projects_and_capabilities_and_returns_false_for_unknown_worker()
     {
        let mut pool = WorkerPool::new();
        let peer_a = ProjectId::now_v7();
        let peer_b = ProjectId::now_v7();
        let with_exec = GradientCapabilities {
            exec: true,
            secrets: true,
            ..GradientCapabilities::default()
        };

        pool.register(
            "w1".into(),
            with_exec,
            ProjectAccess::OnlyListedProjects(HashSet::from([peer_a, peer_b])),
            port().0,
        );

        assert!(pool.replace_worker_authorization(
            "w1",
            ProjectAccess::OnlyListedProjects(HashSet::from([peer_b])),
            caps(),
        ));
        let access = pool.project_access_for_worker_id("w1").unwrap();
        assert!(!access.contains(&peer_a));
        assert!(access.contains(&peer_b));
        let replaced = pool.gradient_caps_for("w1").unwrap();
        assert!(!replaced.exec && !replaced.secrets);

        assert!(!pool.replace_worker_authorization(
            "w2",
            ProjectAccess::no_project_access(),
            caps()
        ));
        assert!(pool.project_access_for_worker_id("w2").is_none());
    }

    #[test]
    fn worker_with_empty_only_projects_set_serves_no_project() {
        let mut pool = WorkerPool::new();
        pool.register(
            "w1".into(),
            caps(),
            ProjectAccess::OnlyListedProjects(HashSet::new()),
            port().0,
        );
        assert!(
            !pool
                .project_access_for_worker_id("w1")
                .unwrap()
                .contains(&ProjectId::now_v7())
        );

        pool.register(
            "w2".into(),
            caps(),
            ProjectAccess::OnlyListedProjects(HashSet::from([ProjectId::now_v7()])),
            port().0,
        );
        pool.replace_worker_authorization(
            "w2",
            ProjectAccess::OnlyListedProjects(HashSet::new()),
            caps(),
        );
        assert!(
            !pool
                .project_access_for_worker_id("w2")
                .unwrap()
                .contains(&ProjectId::now_v7())
        );

        let shown = serde_json::to_value(
            pool.all_workers()
                .iter()
                .map(gradient_api_types::WorkerInfo::from)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for worker in shown.as_array().unwrap() {
            assert_eq!(worker["authorized_peers"], serde_json::json!([]));
        }
    }

    #[test]
    fn build_capacity_strict_at_limit() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                max_concurrent_builds: 2,
                ..Default::default()
            },
        );

        assert!(pool.has_capacity("w1", &JobKind::Build), "0/2 has capacity");
        pool.assign_job("w1", "j1", JobKind::Build, 0);
        assert!(pool.has_capacity("w1", &JobKind::Build), "1/2 has capacity");
        pool.assign_job("w1", "j2", JobKind::Build, 0);
        assert!(
            !pool.has_capacity("w1", &JobKind::Build),
            "2/2 is at limit - must reject"
        );
        pool.release_job("w1", "j2");
        assert!(pool.has_capacity("w1", &JobKind::Build), "1/2 again");
    }

    #[test]
    fn an_eval_job_leaves_build_capacity_alone() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                max_concurrent_builds: 1,
                ..Default::default()
            },
        );

        pool.assign_job("w1", "eval", JobKind::Flake, 0);
        assert!(
            pool.has_capacity("w1", &JobKind::Build),
            "an evaluation waiting on its import still lets the import build here"
        );
        pool.assign_job("w1", "import", JobKind::Build, 0);
        assert!(!pool.has_capacity("w1", &JobKind::Build));
    }

    #[test]
    fn the_worker_info_counts_build_jobs_apart_from_eval_jobs() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.assign_job("w1", "eval", JobKind::Flake, 0);
        pool.assign_job("w1", "build", JobKind::Build, 0);

        let info = &pool.all_workers()[0];
        assert_eq!(info.assigned_job_count, 2);
        assert_eq!(info.assigned_build_count, 1);
    }

    #[test]
    fn test_assign_and_release_job() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);

        pool.assign_job("w1", "j1", JobKind::Build, 0);
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        pool.assign_job("w1", "j2", JobKind::Build, 0);
        assert_eq!(pool.all_workers()[0].assigned_job_count, 2);

        assert!(!pool.release_job("w1", "j1"));
        assert_eq!(pool.all_workers()[0].assigned_job_count, 1);

        assert!(pool.release_job("w1", "j2"));
        assert_eq!(pool.all_workers()[0].assigned_job_count, 0);
    }

    #[test]
    fn a_jobs_stage_counts_for_its_worker_and_the_fleet_until_it_is_released() {
        let mut pool = WorkerPool::new();
        for id in ["w1", "w2"] {
            pool.register(id.into(), caps(), ProjectAccess::AllProjects, port().0);
        }
        pool.assign_job("w1", "a", JobKind::Build, 0);
        pool.assign_job("w1", "b", JobKind::Build, 0);
        pool.assign_job("w2", "c", JobKind::Build, 0);
        pool.enter_stage("w1", "a", BuildStage::Build);
        pool.enter_stage("w1", "b", BuildStage::Prefetch);
        pool.enter_stage("w2", "c", BuildStage::Prefetch);
        pool.enter_stage("w2", "never-assigned", BuildStage::Build);

        assert_eq!(pool.metrics_for("w1").unwrap().running_builds, 1);
        assert_eq!(pool.metrics_for("w2").unwrap().running_builds, 0);
        assert_eq!(pool.fleet_jobs_in(BuildStage::Prefetch), 2);

        pool.enter_stage("w1", "b", BuildStage::Build);
        pool.release_job("w1", "a");
        assert_eq!(pool.metrics_for("w1").unwrap().running_builds, 1);
        assert_eq!(pool.fleet_jobs_in(BuildStage::Prefetch), 1);
    }

    #[test]
    fn test_reported_build_peaks_are_none_when_worker_advertises_no_ram_or_cores() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);

        let BuildMetrics {
            peak_ram_mb,
            build_cores,
            ..
        } = pool.clamp_reported_build_metrics_to_advertised_worker_capabilities(
            "w1",
            "job",
            BuildMetrics {
                peak_ram_mb: Some(4_000),
                build_cores: Some(8),
                oom_killed: true,
                ..Default::default()
            },
        );

        assert_eq!(
            (peak_ram_mb, build_cores),
            (None, None),
            "A worker can leave both total RAM and core count unset. The stored peak RAM and build cores for that worker stay absent. Neither field can equal measured 0."
        );
    }

    #[test]
    fn test_zero_reported_build_cores_and_peak_ram_are_stored_as_none() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                cpu_count: 8,
                ram_total_mb: 16_000,
                ..Default::default()
            },
        );

        let BuildMetrics {
            peak_ram_mb,
            build_cores,
            ..
        } = pool.clamp_reported_build_metrics_to_advertised_worker_capabilities(
            "w1",
            "job",
            BuildMetrics {
                peak_ram_mb: Some(0),
                build_cores: Some(0),
                oom_killed: true,
                ..Default::default()
            },
        );

        assert_eq!(
            (peak_ram_mb, build_cores),
            (None, None),
            "A worker can advertise RAM and cores, yet report 0 for both. That reported 0 is not a measurement, so the saved peak RAM and build cores do not exist."
        );
    }

    #[test]
    fn a_build_outgrowing_its_prediction_reserves_its_observed_peak() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            crate::WorkerProfile {
                ram_total_mb: 128_000,
                ..Default::default()
            },
        );
        pool.assign_job("w1", "young", JobKind::Build, 50_000);
        pool.assign_job("w1", "runaway", JobKind::Build, 2_000);
        assert_eq!(pool.metrics_for("w1").unwrap().ram_reserved_unused_mb, None);

        pool.observe_build_ram("w1", Some(vec![("runaway".into(), 30_000)]));
        pool.observe_build_ram("w1", Some(vec![("runaway".into(), 20_000)]));

        let metrics = pool.metrics_for("w1").unwrap();
        assert_eq!(metrics.ram_reserved_mb, 80_000);
        assert_eq!(metrics.ram_reserved_unused_mb, Some(50_000));
        assert_eq!(
            pool.observed_build_ram("w1"),
            HashMap::from([("runaway".to_owned(), 30_000)])
        );
    }

    #[test]
    fn the_fleet_reports_observed_build_ram_only_once_every_worker_observes_it() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.register("w2".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            crate::WorkerProfile {
                ram_total_mb: 128_000,
                ..Default::default()
            },
        );
        pool.update_capabilities(
            "w2",
            crate::WorkerProfile {
                ram_total_mb: 128_000,
                ..Default::default()
            },
        );
        pool.assign_job("w1", "a", JobKind::Build, 0);
        pool.assign_job("w2", "b", JobKind::Build, 0);
        pool.assign_job("w2", "not-started", JobKind::Build, 0);

        pool.observe_build_ram("w1", Some(vec![("a".into(), 4_000)]));
        assert_eq!(pool.fleet_observed_build_ram(), None);

        pool.observe_build_ram("w2", Some(vec![("b".into(), 9_000)]));
        let observed: HashMap<String, u64> = pool
            .fleet_observed_build_ram()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            observed,
            HashMap::from([("a".to_owned(), 4_000), ("b".to_owned(), 9_000)])
        );
    }

    #[test]
    fn an_assigned_job_reserves_its_predicted_peak_ram_until_released() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            crate::WorkerProfile {
                ram_total_mb: 128_000,
                ..Default::default()
            },
        );
        pool.update_metrics("w1", 0.0, 118_000, None, None, None);

        pool.assign_job("w1", "clang", JobKind::Build, 54_000);
        pool.assign_job("w1", "pinocchio", JobKind::Build, 67_000);
        pool.assign_job("w1", "substitution", JobKind::Build, 0);
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.ram_reserved_mb, 121_000);
        assert_eq!(view.ram_available_mb(), Some(7_000));

        pool.release_job("w1", "pinocchio");
        let view = pool.metrics_for("w1").unwrap();
        assert_eq!(view.ram_reserved_mb, 54_000);
        assert_eq!(view.ram_available_mb(), Some(74_000));
    }

    #[test]
    fn test_all_workers_info() {
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.register("w2".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_capabilities(
            "w1",
            WorkerProfile {
                architectures: vec!["x86_64-linux".into()],
                max_concurrent_builds: 2,
                ..Default::default()
            },
        );
        pool.assign_job("w1", "j1", JobKind::Build, 0);
        pool.mark_draining("w2");

        let mut workers = pool.all_workers();
        workers.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(workers[0].id, "w1");
        assert_eq!(workers[0].assigned_job_count, 1);
        assert!(!workers[0].draining);

        assert_eq!(workers[1].id, "w2");
        assert_eq!(workers[1].assigned_job_count, 0);
        assert!(workers[1].draining);
    }

    #[test]
    fn test_all_worker_info_entries_expose_project_access_for_each_worker() {
        let mut pool = WorkerPool::new();
        let project_a = ProjectId::now_v7();
        let project_b = ProjectId::now_v7();

        pool.register(
            "w1".into(),
            caps(),
            ProjectAccess::OnlyListedProjects(HashSet::from([project_a])),
            port().0,
        );
        pool.register("w2".into(), caps(), ProjectAccess::AllProjects, port().0);

        let mut workers = pool.all_workers();
        workers.sort_by(|a, b| a.id.cmp(&b.id));

        assert!(workers[0].project_access.contains(&project_a));
        assert!(!workers[0].project_access.contains(&project_b));
        assert_eq!(workers[1].project_access, ProjectAccess::AllProjects);

        let shown = serde_json::to_value(
            workers
                .iter()
                .map(gradient_api_types::WorkerInfo::from)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(shown[0]["authorized_peers"], serde_json::json!([project_a]));
        assert_eq!(shown[1]["authorized_peers"], serde_json::Value::Null);
    }

    #[test]
    fn test_request_reauth_notifies_connected_worker() {
        let mut pool = WorkerPool::new();
        let (session, mut signals) = port();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, session);

        pool.request_reauth("w1");

        assert_eq!(signals.try_recv(), Ok(SessionSignal::Reauth));
    }

    #[test]
    fn send_abort_reaches_the_session_and_reports_unknown_workers() {
        let mut pool = WorkerPool::new();
        let (session, mut signals) = port();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, session);

        assert!(pool.send_abort("w1", "j1".into(), "why".into()));
        assert!(!pool.send_abort("nope", "j1".into(), "why".into()));
        assert_eq!(
            signals.try_recv(),
            Ok(SessionSignal::Abort {
                job_id: "j1".into(),
                reason: "why".into()
            })
        );
    }

    #[test]
    fn unregister_closes_the_evicted_session() {
        let mut pool = WorkerPool::new();
        let (session, mut signals) = port();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, session);

        pool.unregister("w1");

        assert_eq!(
            signals.try_recv(),
            Ok(SessionSignal::Close {
                reason: "unregistered by the scheduler".into()
            }),
            "an evicted worker must be told, or it keeps a session the pool no longer knows about"
        );
    }

    #[test]
    fn unregister_of_an_unknown_worker_signals_nothing() {
        let mut pool = WorkerPool::new();
        assert!(pool.unregister("nope").is_empty());
    }

    #[test]
    fn stale_worker_ids_flags_only_silent_workers() {
        let mut pool = WorkerPool::new();
        let handle = pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);

        let now_ms = 1_000_000_000_000i64;
        let timeout_ms = 30_000i64;

        handle.store(now_ms, Ordering::Relaxed);
        assert!(pool.stale_worker_ids(now_ms, timeout_ms).is_empty());

        handle.store(now_ms - timeout_ms, Ordering::Relaxed);
        assert!(pool.stale_worker_ids(now_ms, timeout_ms).is_empty());

        handle.store(now_ms - timeout_ms - 1, Ordering::Relaxed);
        assert_eq!(
            pool.stale_worker_ids(now_ms, timeout_ms),
            vec!["w1".to_string()]
        );

        pool.register("w2".into(), caps(), ProjectAccess::AllProjects, port().0);
        let real_now = gradient_types::now().and_utc().timestamp_millis();
        assert!(
            !pool
                .stale_worker_ids(real_now, timeout_ms)
                .contains(&"w2".to_string())
        );
    }

    #[test]
    fn reregistering_a_worker_keeps_the_liveness_handle_its_reader_holds() {
        let mut pool = WorkerPool::new();
        let first = pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        let second = pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        assert!(
            Arc::ptr_eq(&first, &second),
            "a re-registration must hand back the Arc the prior reader still stamps"
        );

        let now_ms = 1_000_000_000_000i64;
        let timeout_ms = 30_000i64;

        first.store(now_ms - timeout_ms - 1, Ordering::Relaxed);
        assert_eq!(
            pool.stale_worker_ids(now_ms, timeout_ms),
            vec!["w1".to_string()],
            "the pool reads the value stamped through the first handle"
        );

        first.store(now_ms, Ordering::Relaxed);
        assert!(
            pool.stale_worker_ids(now_ms, timeout_ms).is_empty(),
            "a stamp through the first handle keeps the worker live"
        );

        let other = pool.register("w2".into(), caps(), ProjectAccess::AllProjects, port().0);
        assert!(
            !Arc::ptr_eq(&first, &other),
            "a different worker gets its own handle"
        );
    }

    #[test]
    fn a_held_device_is_not_free_before_the_worker_reports_it() {
        use gradient_wire::types::{DeviceClass, DeviceRequest, ExecInventory};
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_exec_inventory(
            "w1",
            Some(ExecInventory {
                devices: vec![DeviceClass {
                    name: "gpu:h100".into(),
                    total: 8,
                    free: 8,
                }],
                ..Default::default()
            }),
        );
        let free = |pool: &WorkerPool| {
            pool.worker_caps("w1")
                .and_then(|caps| caps.exec)
                .map(|exec| exec.devices[0].free)
        };
        pool.hold_exec(
            "w1",
            "exec:1",
            vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 8,
            }],
            GuestSize::default(),
        );
        assert_eq!(free(&pool), Some(0));
        pool.assign_job("w1", "exec:1", JobKind::Exec, 0);
        assert_eq!(free(&pool), Some(0), "the assignment keeps the hold");
        pool.release_job("w1", "exec:1");
        assert_eq!(free(&pool), Some(8));
    }

    #[test]
    fn held_guest_sizes_lower_the_free_budget() {
        use crate::worker_caps::DeviceCount;
        use gradient_wire::types::{
            ExecAllow, ExecInventory, ExecRequirement, GuestCapacity, ResourceAmount,
        };
        let plenty = ResourceAmount {
            total: 1_000_000,
            free: 1_000_000,
        };
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_exec_inventory(
            "w1",
            Some(ExecInventory {
                allow: ExecAllow {
                    nodes: 1,
                    ..ExecAllow::default()
                },
                guest_capacity: GuestCapacity {
                    cpus: plenty,
                    memory_mb: ResourceAmount {
                        total: 1000,
                        free: 1000,
                    },
                    disk_mb: plenty,
                },
                ..Default::default()
            }),
        );
        for job in ["exec:1", "exec:2"] {
            pool.hold_exec("w1", job, Vec::new(), GuestSize::default_request());
            pool.assign_job("w1", job, JobKind::Exec, 500);
        }
        let mut worker = pool.worker_caps("w1").unwrap();
        let inventory = worker.exec.clone().unwrap();
        assert_eq!(inventory.guest_capacity.memory_mb.free, 0);
        worker.capabilities.exec = true;
        worker.architectures = vec!["x86_64-linux".into()];
        let third = ExecRequirement {
            architecture: "x86_64-linux".into(),
            guest: GuestSize::default_request(),
            ..ExecRequirement::default()
        };
        assert!(!worker.fits_exec(&third, DeviceCount::Free, ProjectId::nil()));
        assert!(worker.fits_exec(&third, DeviceCount::Total, ProjectId::nil()));
    }

    #[test]
    fn a_listed_worker_shows_only_exec_labels_and_devices() {
        use gradient_wire::types::{ExecAllow, ExecDefaults};
        let mut pool = WorkerPool::new();
        pool.register("w1".into(), caps(), ProjectAccess::AllProjects, port().0);
        pool.update_exec_inventory(
            "w1",
            Some(ExecInventory {
                labels: vec!["w1".into()],
                devices: vec![DeviceClass {
                    name: "kvm".into(),
                    total: 1,
                    free: 1,
                }],
                mounts: vec!["datasets".into()],
                secrets: vec!["hf-token".into()],
                allow: ExecAllow {
                    custom_images: true,
                    ..ExecAllow::default()
                },
                defaults: ExecDefaults {
                    network: true,
                    ..ExecDefaults::default()
                },
                warm_guests: 2,
                ..ExecInventory::default()
            }),
        );

        let listed =
            serde_json::to_value(gradient_api_types::WorkerInfo::from(&pool.all_workers()[0]))
                .expect("serializes");

        assert_eq!(
            listed["exec"],
            serde_json::json!({
                "labels": ["w1"],
                "devices": [{ "name": "kvm", "total": 1, "free": 1 }],
            })
        );
    }
}
