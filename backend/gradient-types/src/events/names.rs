/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub const BUILD_CREATED: &str = "build.created";
pub const BUILD_QUEUED: &str = "build.queued";
pub const BUILD_STARTED: &str = "build.started";
pub const BUILD_COMPLETED: &str = "build.completed";
pub const BUILD_FAILED: &str = "build.failed";
pub const BUILD_FAILED_TRANSIENT: &str = "build.failed_transient";
pub const BUILD_SUBSTITUTED: &str = "build.substituted";
pub const BUILD_STATUS_CHANGED: &str = "build.status_changed";
pub const BUILD_PROGRESS: &str = "build.progress";
pub const BUILD_UNKNOWN: &str = "build.unknown";

pub const EXEC_JOB_QUEUED: &str = "exec_job.queued";
pub const EXEC_JOB_STARTED: &str = "exec_job.started";
pub const EXEC_JOB_PHASE: &str = "exec_job.phase";
pub const EXEC_JOB_COMPLETED: &str = "exec_job.completed";
pub const EXEC_JOB_FAILED: &str = "exec_job.failed";
pub const EXEC_JOB_ABORTED: &str = "exec_job.aborted";
pub const EXEC_JOB_UNKNOWN: &str = "exec_job.unknown";

pub const EVALUATION_QUEUED: &str = "evaluation.queued";
pub const EVALUATION_STARTED: &str = "evaluation.started";
pub const EVALUATION_BUILDING: &str = "evaluation.building";
pub const EVALUATION_WAITING: &str = "evaluation.waiting";
pub const EVALUATION_COMPLETED: &str = "evaluation.completed";
pub const EVALUATION_FAILED: &str = "evaluation.failed";
pub const EVALUATION_ABORTED: &str = "evaluation.aborted";
pub const EVALUATION_ACTION_REQUIRED: &str = "evaluation.action_required";
pub const EVALUATION_APPROVAL_GRANTED: &str = "evaluation.approval_granted";
pub const EVALUATION_PROGRESS: &str = "evaluation.progress";
pub const EVALUATION_ACTIVITY: &str = "evaluation.activity";

pub const CACHE_CHANGED: &str = "cache.changed";
pub const CACHE_NAR_FETCHED: &str = "cache.nar.fetched";
pub const CACHE_NAR_SIGNED: &str = "cache.nar.signed";

pub const WORKER_CONNECTED: &str = "worker.connected";
pub const WORKER_DISCONNECTED: &str = "worker.disconnected";
pub const WORKER_JOB_DISPATCHED: &str = "worker.job_dispatched";
pub const WORKER_QUEUE_DEPTH: &str = "worker.queue_depth";
pub const WORKER_DEMAND: &str = "worker.demand";

pub const GRAPH_NAR_COMMITTED: &str = "graph.nar_committed";
pub const GRAPH_TRANSITIONED: &str = "graph.transitioned";
pub const GRAPH_REQUEUED: &str = "graph.requeued";
pub const GRAPH_DEMOTED: &str = "graph.demoted";
pub const GRAPH_COLLECTED: &str = "graph.collected";

pub const GC_SWEPT: &str = "gc.swept";
pub const GC_DEEP_FINISHED: &str = "gc.deep_finished";

pub const WEBHOOK_PING: &str = "webhook.ping";
