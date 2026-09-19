/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The export allowlist: every table the report may contain, every column of
//! it, and what gets pseudonymised on the way out.
//!
//! Columns are named one by one and never with `*`, so a table that later gains
//! a secret cannot start exporting it behind our back. Booleans are cast to
//! `int` and everything else to `text`; SQLite's column affinity converts each
//! back on insert, so the report keeps real types while extraction stays
//! uniform.

use crate::redact::Redactor;

/// One row of an exported table. Every value arrives as text from Postgres and
/// is converted by SQLite affinity on insert.
pub type Row = Vec<Option<String>>;

pub struct TableSpec {
    pub name: &'static str,
    pub ddl: &'static str,
    /// Scoped by `$1`: the evaluation for eval tables, the project for the ones
    /// that describe more than it.
    pub sql: &'static str,
    /// What `$1` actually selects, for the manifest to declare. Several tables
    /// hang off the evaluation's *anchors*, which are shared between
    /// evaluations, so their rows are not the evaluation's alone.
    pub scope: &'static str,
    pub columns: &'static [&'static str],
}

/// The single place redaction policy lives, so it can be audited in one read.
/// Anything not named here is exported verbatim.
pub fn redact_value(
    r: &Redactor,
    table: &str,
    column: &str,
    value: Option<String>,
) -> Option<String> {
    let v = value?;
    let out = match (table, column) {
        ("evaluation", "repository") | ("evaluation", "flake_source") => r.identity(&v, "repo"),
        ("evaluation", "started_by") => r.identity(&v, "user"),
        ("commit", "author") | ("commit", "author_name") => r.identity(&v, "user"),
        // A commit message is free text carrying whatever the author wrote, so
        // it gets the same treatment as a build log rather than a column rule.
        ("commit", "message") => r.text(&v),
        ("derivation", "name") | ("derivation", "pname") => r.package(&v),
        ("derivation_output", "package") | ("cached_path", "package") => r.package(&v),
        ("derivation_output", "deriver") | ("cached_path", "deriver") => r.store_path(&v),
        ("derivation_output", "references_list") | ("cached_path", "references") => {
            r.store_path_list(&v)
        }
        ("evaluation_metric", "worker_id")
        | ("phase_event", "worker_id")
        | ("dispatched_job", "worker_id")
        | ("worker_connection", "worker_id")
        | ("worker_sample", "worker_id")
        | ("worker_registration", "worker_id") => r.identity(&v, "worker"),
        ("worker_connection", "display_name") | ("worker_registration", "display_name") => {
            r.identity(&v, "worker")
        }
        ("base_worker", "worker_id") | ("base_worker", "display_name") => r.identity(&v, "worker"),
        ("worker_registration", "url") | ("cache_upstream", "url") | ("base_worker", "url") => {
            r.identity(&v, "url")
        }
        ("upstream_metric", "upstream_url") => r.identity(&v, "url"),
        ("worker_registration", "created_by")
        | ("base_worker", "created_by")
        | ("project_base_worker", "created_by") => r.identity(&v, "user"),
        ("cache_upstream", "display_name")
        | ("cache_upstream", "remote_cache_name")
        | ("cached_path_signature", "cache_name") => r.identity(&v, "cache"),
        _ => v,
    };

    Some(out)
}

macro_rules! spec {
    ($name:literal, $ddl:literal, $sql:expr, $scope:literal, [$($col:literal),* $(,)?]) => {
        TableSpec {
            name: $name,
            ddl: $ddl,
            sql: $sql,
            scope: $scope,
            columns: &[$($col),*],
        }
    };
}

/// The derivations this evaluation drove: the ones it opened a `build_job` for.
macro_rules! own_derivations {
    () => {
        "SELECT derivation FROM build_job WHERE evaluation = $1"
    };
}

/// The anchors behind those derivations.
macro_rules! own_anchors {
    () => {
        "SELECT derivation_build FROM build_job WHERE evaluation = $1"
    };
}

/// Those derivations plus every direct dependency of one.
///
/// `unready_deps` is one count per EDGE over exactly this set, and the readiness it
/// counts is a property of the dependency's own anchor, outputs and cached paths.
/// Export the edges without their far end and neither counter can be checked: the
/// `LEFT JOIN ... IS NULL` rule that makes a missing dependency count as unready
/// fires on every dependency the file merely left out, which turned a correct
/// stored `1` into a recomputed `24` on a real report. One hop is the whole
/// requirement - a dependency's own dependencies are already summarised in its
/// stored `unready_deps`, which is why this is a boundary and not a closure.
macro_rules! derivation_scope {
    () => {
        concat!(
            own_derivations!(),
            " UNION SELECT e.dependency FROM derivation_dependency e WHERE e.derivation IN (",
            own_derivations!(),
            ")"
        )
    };
}

/// Every output hash of every derivation the report carries.
macro_rules! output_hashes {
    () => {
        concat!(
            "SELECT o.hash FROM derivation_output o WHERE o.derivation IN (",
            derivation_scope!(),
            ")"
        )
    };
}

/// The same set: a runtime reference is an edge of the derivation graph now, so
/// the one-hop dependency boundary `derivation_scope` carries already names the
/// producer of every path an exported output references, and `output_hashes`
/// carries its paths. That is what makes an absent row mean the instance never had
/// the path rather than the export never asking for it, which is the whole
/// diagnosis on an unwhole closure.
macro_rules! cached_path_scope {
    () => {
        output_hashes!()
    };
}

/// Tables owned by, or reachable from, the evaluation itself.
pub fn eval_scope_tables() -> &'static [TableSpec] {
    const SPECS: &[TableSpec] = &[
        spec!(
            "evaluation",
            "CREATE TABLE evaluation (id TEXT, task TEXT, repository TEXT, commit_id TEXT, wildcard TEXT, status INTEGER, previous TEXT, next TEXT, created_at TEXT, updated_at TEXT, flake_source TEXT, waiting_reason TEXT, trigger_id TEXT, concurrent INTEGER, fetch_started_at TEXT, eval_flake_started_at TEXT, eval_drv_started_at TEXT, building_started_at TEXT, finished_at TEXT, started_by TEXT, cache_status INTEGER, kind INTEGER)",
            "SELECT id::text, task::text, repository::text, commit::text, wildcard::text, status::text, previous::text, next::text, created_at::text, updated_at::text, flake_source::text, waiting_reason::text, trigger::text, concurrent::int::text, fetch_started_at::text, eval_flake_started_at::text, eval_drv_started_at::text, building_started_at::text, finished_at::text, started_by::text, cache_status::text, kind::text FROM evaluation WHERE id = $1",
            "the evaluation",
            [
                "id",
                "task",
                "repository",
                "commit_id",
                "wildcard",
                "status",
                "previous",
                "next",
                "created_at",
                "updated_at",
                "flake_source",
                "waiting_reason",
                "trigger_id",
                "concurrent",
                "fetch_started_at",
                "eval_flake_started_at",
                "eval_drv_started_at",
                "building_started_at",
                "finished_at",
                "started_by",
                "cache_status",
                "kind"
            ]
        ),
        spec!(
            "commit",
            "CREATE TABLE \"commit\" (id TEXT, hash TEXT, author TEXT, author_name TEXT, message TEXT)",
            "SELECT c.id::text, encode(c.hash, 'hex'), c.author::text, c.author_name::text, c.message::text FROM commit c WHERE c.id IN (SELECT commit FROM evaluation WHERE id = $1)",
            "the evaluation's commit",
            ["id", "hash", "author", "author_name", "message"]
        ),
        spec!(
            "evaluation_message",
            "CREATE TABLE evaluation_message (id TEXT, evaluation TEXT, level INTEGER, message TEXT, source TEXT, created_at TEXT)",
            "SELECT id::text, evaluation::text, level::text, message::text, source::text, created_at::text FROM evaluation_message WHERE evaluation = $1",
            "the evaluation",
            [
                "id",
                "evaluation",
                "level",
                "message",
                "source",
                "created_at"
            ]
        ),
        spec!(
            "evaluation_metric",
            "CREATE TABLE evaluation_metric (id TEXT, evaluation TEXT, total_thunks INTEGER, fn_calls INTEGER, primop_calls INTEGER, lookups INTEGER, alloc_bytes INTEGER, peak_heap_mb INTEGER, peak_rss_mb INTEGER, fetch_ms INTEGER, eval_flake_ms INTEGER, eval_drv_ms INTEGER, total_eval_ms INTEGER, worker_id TEXT, created_at TEXT)",
            "SELECT id::text, evaluation::text, total_thunks::text, fn_calls::text, primop_calls::text, lookups::text, alloc_bytes::text, peak_heap_mb::text, peak_rss_mb::text, fetch_ms::text, eval_flake_ms::text, eval_drv_ms::text, total_eval_ms::text, worker_id::text, created_at::text FROM evaluation_metric WHERE evaluation = $1",
            "the evaluation",
            [
                "id",
                "evaluation",
                "total_thunks",
                "fn_calls",
                "primop_calls",
                "lookups",
                "alloc_bytes",
                "peak_heap_mb",
                "peak_rss_mb",
                "fetch_ms",
                "eval_flake_ms",
                "eval_drv_ms",
                "total_eval_ms",
                "worker_id",
                "created_at"
            ]
        ),
        spec!(
            "entry_point",
            "CREATE TABLE entry_point (id TEXT, task TEXT, evaluation TEXT, created_at TEXT, eval TEXT, repo_check_id TEXT, derivation TEXT)",
            "SELECT id::text, task::text, evaluation::text, created_at::text, eval::text, repo_check_id::text, derivation::text FROM entry_point WHERE evaluation = $1",
            "the evaluation",
            [
                "id",
                "task",
                "evaluation",
                "created_at",
                "eval",
                "repo_check_id",
                "derivation"
            ]
        ),
        spec!(
            "build_job",
            "CREATE TABLE build_job (id TEXT, evaluation TEXT, derivation TEXT, derivation_build TEXT, score REAL, score_breakdown TEXT, created_at TEXT)",
            concat!(
                "SELECT id::text, evaluation::text, derivation::text, derivation_build::text, score::text, score_breakdown::text, created_at::text FROM build_job WHERE evaluation = $1 OR id IN (SELECT a.build_job FROM build_attempt a WHERE a.derivation_build IN (",
                own_anchors!(),
                "))"
            ),
            "the evaluation, plus every job an exported attempt was made under",
            [
                "id",
                "evaluation",
                "derivation",
                "derivation_build",
                "score",
                "score_breakdown",
                "created_at"
            ]
        ),
        spec!(
            "derivation_build",
            "CREATE TABLE derivation_build (id TEXT, derivation TEXT, status INTEGER, substitutable INTEGER, probed INTEGER, substituted INTEGER, attempt INTEGER, timeout_secs INTEGER, max_silent_secs INTEGER, created_at TEXT, updated_at TEXT, queued_at TEXT, ready_at TEXT, dispatched_at TEXT, fetchable INTEGER, unready_deps INTEGER, demanded INTEGER, missing_runtime_deps INTEGER)",
            concat!(
                "SELECT db.id::text, db.derivation::text, db.status::text, db.substitutable::int::text, db.probed::int::text, db.substituted::int::text, db.attempt::text, db.timeout_secs::text, db.max_silent_secs::text, db.created_at::text, db.updated_at::text, db.queued_at::text, db.ready_at::text, db.dispatched_at::text, db.fetchable::int::text, db.unready_deps::text, db.demanded::int::text, db.missing_runtime_deps::text FROM derivation_build db WHERE db.derivation IN (",
                derivation_scope!(),
                ")"
            ),
            "the evaluation's derivations and their direct dependencies, shared with every other evaluation that built them",
            [
                "id",
                "derivation",
                "status",
                "substitutable",
                "probed",
                "substituted",
                "attempt",
                "timeout_secs",
                "max_silent_secs",
                "created_at",
                "updated_at",
                "queued_at",
                "ready_at",
                "dispatched_at",
                "fetchable",
                "unready_deps",
                "demanded",
                "missing_runtime_deps"
            ]
        ),
        spec!(
            "derivation",
            "CREATE TABLE derivation (id TEXT, created_at TEXT, architecture TEXT, hash TEXT, name TEXT, pname TEXT, prefer_local_build INTEGER, allow_substitutes INTEGER, closure_size INTEGER, is_fixed_output INTEGER, walked INTEGER, unwalked_inputs INTEGER)",
            concat!(
                "SELECT d.id::text, d.created_at::text, d.architecture::text, d.hash::text, d.name::text, d.pname::text, d.prefer_local_build::int::text, d.allow_substitutes::int::text, d.closure_size::text, d.is_fixed_output::int::text, d.walked::int::text, d.unwalked_inputs::text FROM derivation d WHERE d.id IN (",
                derivation_scope!(),
                ")"
            ),
            "the evaluation's derivations and their direct dependencies, shared with every other evaluation that built them",
            [
                "id",
                "created_at",
                "architecture",
                "hash",
                "name",
                "pname",
                "prefer_local_build",
                "allow_substitutes",
                "closure_size",
                "is_fixed_output",
                "walked",
                "unwalked_inputs"
            ]
        ),
        spec!(
            "derivation_output",
            "CREATE TABLE derivation_output (id TEXT, derivation TEXT, name TEXT, hash TEXT, package TEXT, ca TEXT, nar_size INTEGER, is_cached INTEGER, created_at TEXT, cached_path TEXT, external_url TEXT, nar_hash TEXT, file_size INTEGER, references_list TEXT, deriver TEXT, file_hash TEXT)",
            concat!(
                "SELECT o.id::text, o.derivation::text, o.name::text, o.hash::text, o.package::text, o.ca::text, o.nar_size::text, o.is_cached::int::text, o.created_at::text, o.cached_path::text, o.external_url::text, o.nar_hash::text, o.file_size::text, o.references_list::text, o.deriver::text, o.file_hash::text FROM derivation_output o WHERE o.derivation IN (",
                derivation_scope!(),
                ")"
            ),
            "the evaluation's derivations and their direct dependencies, shared with every other evaluation that built them",
            [
                "id",
                "derivation",
                "name",
                "hash",
                "package",
                "ca",
                "nar_size",
                "is_cached",
                "created_at",
                "cached_path",
                "external_url",
                "nar_hash",
                "file_size",
                "references_list",
                "deriver",
                "file_hash"
            ]
        ),
        spec!(
            "build_attempt",
            "CREATE TABLE build_attempt (id TEXT, build_job TEXT, derivation_build TEXT, dispatched_job TEXT, substitute INTEGER, outcome INTEGER, reason INTEGER, failure_message TEXT, build_context TEXT, build_started_at TEXT, build_finished_at TEXT, created_at TEXT)",
            concat!(
                "SELECT a.id::text, a.build_job::text, a.derivation_build::text, a.dispatched_job::text, a.substitute::int::text, a.outcome::text, a.reason::text, a.failure_message::text, a.build_context::text, a.build_started_at::text, a.build_finished_at::text, a.created_at::text FROM build_attempt a WHERE a.derivation_build IN (",
                own_anchors!(),
                ")"
            ),
            "the evaluation's build anchors, so attempts made for other evaluations are included",
            [
                "id",
                "build_job",
                "derivation_build",
                "dispatched_job",
                "substitute",
                "outcome",
                "reason",
                "failure_message",
                "build_context",
                "build_started_at",
                "build_finished_at",
                "created_at"
            ]
        ),
        spec!(
            "dispatched_job",
            "CREATE TABLE dispatched_job (id TEXT, kind INTEGER, evaluation_id TEXT, project TEXT, task TEXT, worker_id TEXT, score REAL, queued_at TEXT, ready_at TEXT, dispatched_at TEXT, finished_at TEXT, outcome INTEGER, score_breakdown TEXT, worker_context TEXT, job_context TEXT, candidates TEXT, created_at TEXT, instance_context TEXT)",
            "SELECT id::text, kind::text, evaluation_id::text, project::text, task::text, worker_id::text, score::text, queued_at::text, ready_at::text, dispatched_at::text, finished_at::text, outcome::text, score_breakdown::text, worker_context::text, job_context::text, candidates::text, created_at::text, instance_context::text FROM dispatched_job WHERE evaluation_id = $1",
            "the evaluation",
            [
                "id",
                "kind",
                "evaluation_id",
                "project",
                "task",
                "worker_id",
                "score",
                "queued_at",
                "ready_at",
                "dispatched_at",
                "finished_at",
                "outcome",
                "score_breakdown",
                "worker_context",
                "job_context",
                "candidates",
                "created_at",
                "instance_context"
            ]
        ),
        spec!(
            "dispatched_job_phase",
            "CREATE TABLE dispatched_job_phase (id TEXT, dispatched_job TEXT, seq INTEGER, parent_seq INTEGER, phase INTEGER, start_ms INTEGER, end_ms INTEGER, paths INTEGER, bytes INTEGER, created_at TEXT)",
            "SELECT p.id::text, p.dispatched_job::text, p.seq::text, p.parent_seq::text, p.phase::text, p.start_ms::text, p.end_ms::text, p.paths::text, p.bytes::text, p.created_at::text FROM dispatched_job_phase p WHERE p.dispatched_job IN (SELECT id FROM dispatched_job WHERE evaluation_id = $1)",
            "the evaluation",
            [
                "id",
                "dispatched_job",
                "seq",
                "parent_seq",
                "phase",
                "start_ms",
                "end_ms",
                "paths",
                "bytes",
                "created_at"
            ]
        ),
        spec!(
            "phase_event",
            "CREATE TABLE phase_event (id TEXT, subject_kind INTEGER, subject_id TEXT, phase INTEGER, event INTEGER, at TEXT, worker_id TEXT, detail TEXT)",
            concat!(
                "SELECT p.id::text, p.subject_kind::text, p.subject_id::text, p.phase::text, p.event::text, p.at::text, p.worker_id::text, p.detail::text FROM phase_event p WHERE p.subject_id = $1 OR p.subject_id IN (",
                own_anchors!(),
                ")"
            ),
            "the evaluation and its build anchors, so events from other evaluations are included",
            [
                "id",
                "subject_kind",
                "subject_id",
                "phase",
                "event",
                "at",
                "worker_id",
                "detail"
            ]
        ),
        spec!(
            "derivation_dependency",
            "CREATE TABLE derivation_dependency (derivation TEXT, dependency TEXT, kind INTEGER)",
            concat!(
                "SELECT dd.derivation::text, dd.dependency::text, dd.kind::text FROM derivation_dependency dd WHERE dd.derivation IN (",
                own_derivations!(),
                ")"
            ),
            "the evaluation's own derivations, with both ends of every edge exported",
            ["derivation", "dependency", "kind"]
        ),
        spec!(
            "cached_path",
            "CREATE TABLE cached_path (id TEXT, hash TEXT, package TEXT, file_hash TEXT, file_size INTEGER, nar_size INTEGER, nar_hash TEXT, ca TEXT, created_at TEXT, deriver TEXT, \"references\" TEXT)",
            concat!(
                "SELECT c.id::text, c.hash::text, c.package::text, c.file_hash::text, c.file_size::text, c.nar_size::text, c.nar_hash::text, c.ca::text, c.created_at::text, c.deriver::text, c.\"references\"::text FROM cached_path c WHERE c.hash IN (",
                cached_path_scope!(),
                ")"
            ),
            "the exported derivations' output hashes and every path they reference, shared with every other evaluation that produced them",
            [
                "id",
                "hash",
                "package",
                "file_hash",
                "file_size",
                "nar_size",
                "nar_hash",
                "ca",
                "created_at",
                "deriver",
                "references"
            ]
        ),
        spec!(
            "cached_path_signature",
            "CREATE TABLE cached_path_signature (id TEXT, cached_path TEXT, cache TEXT, cache_name TEXT, signed INTEGER, last_fetched_at TEXT, fetch_count INTEGER, created_at TEXT)",
            concat!(
                "SELECT s.id::text, s.cached_path::text, s.cache::text, c.name::text, (s.signature IS NOT NULL)::int::text, s.last_fetched_at::text, s.fetch_count::text, s.created_at::text FROM cached_path_signature s LEFT JOIN cache c ON c.id = s.cache WHERE s.cached_path IN (SELECT cp.id FROM cached_path cp WHERE cp.hash IN (",
                cached_path_scope!(),
                "))"
            ),
            "the exported cached paths, one row per cache holding the path",
            [
                "id",
                "cached_path",
                "cache",
                "cache_name",
                "signed",
                "last_fetched_at",
                "fetch_count",
                "created_at"
            ]
        ),
        spec!(
            "evaluation_input_update",
            "CREATE TABLE evaluation_input_update (id TEXT, evaluation TEXT, base_commit TEXT, generator TEXT, target_inputs TEXT, bumped_inputs TEXT, discover_only INTEGER, created_at TEXT, updated_at TEXT)",
            "SELECT id::text, evaluation::text, base_commit::text, generator::text, target_inputs::text, bumped_inputs::text, discover_only::int::text, created_at::text, updated_at::text FROM evaluation_input_update WHERE evaluation = $1",
            "the evaluation's input-update sidecar",
            [
                "id",
                "evaluation",
                "base_commit",
                "generator",
                "target_inputs",
                "bumped_inputs",
                "discover_only",
                "created_at",
                "updated_at"
            ]
        ),
    ];

    SPECS
}

/// Fleet and upstream state, scoped to the evaluation's project. Gated behind
/// `include_instance` and the `ManageWorkers` permission, since it describes
/// more than the evaluation that asked for it.
pub fn instance_tables() -> &'static [TableSpec] {
    const SPECS: &[TableSpec] = &[
        spec!(
            "worker_registration",
            "CREATE TABLE worker_registration (id TEXT, peer_id TEXT, worker_id TEXT, created_at TEXT, managed INTEGER, url TEXT, active INTEGER, display_name TEXT, created_by TEXT, enable_fetch INTEGER, enable_eval INTEGER, enable_build INTEGER)",
            "SELECT id::text, peer_id::text, worker_id::text, created_at::text, managed::int::text, url::text, active::int::text, display_name::text, created_by::text, enable_fetch::int::text, enable_eval::int::text, enable_build::int::text FROM worker_registration WHERE $1 IS NOT NULL",
            "the whole instance",
            [
                "id",
                "peer_id",
                "worker_id",
                "created_at",
                "managed",
                "url",
                "active",
                "display_name",
                "created_by",
                "enable_fetch",
                "enable_eval",
                "enable_build"
            ]
        ),
        spec!(
            "worker_connection",
            "CREATE TABLE worker_connection (id TEXT, worker_id TEXT, project TEXT, display_name TEXT, connected_at TEXT, disconnected_at TEXT, capabilities TEXT, reason INTEGER)",
            "WITH w AS (SELECT created_at AS started, COALESCE(finished_at, (now() AT TIME ZONE \'UTC\')) AS ended FROM evaluation WHERE id = $1) SELECT c.id::text, c.worker_id::text, c.project::text, c.display_name::text, c.connected_at::text, c.disconnected_at::text, c.capabilities::text, c.reason::text FROM worker_connection c, w WHERE c.worker_id IN (SELECT worker_id FROM dispatched_job WHERE evaluation_id = $1) AND c.connected_at <= w.ended AND (c.disconnected_at IS NULL OR c.disconnected_at >= w.started)",
            "the workers that ran this evaluation, while it ran",
            [
                "id",
                "worker_id",
                "project",
                "display_name",
                "connected_at",
                "disconnected_at",
                "capabilities",
                "reason"
            ]
        ),
        spec!(
            "worker_sample",
            "CREATE TABLE worker_sample (id TEXT, worker_id TEXT, project TEXT, at TEXT, cpu_usage_pct REAL, ram_free_mb INTEGER, ram_total_mb INTEGER, disk_speed_mbps REAL, network_speed_mbps REAL, assigned_jobs INTEGER, max_concurrent_builds INTEGER, state INTEGER, capabilities TEXT)",
            "WITH w AS (SELECT created_at AS started, COALESCE(finished_at, (now() AT TIME ZONE \'UTC\')) AS ended FROM evaluation WHERE id = $1) SELECT s.id::text, s.worker_id::text, s.project::text, s.at::text, s.cpu_usage_pct::text, s.ram_free_mb::text, s.ram_total_mb::text, s.disk_speed_mbps::text, s.network_speed_mbps::text, s.assigned_jobs::text, s.max_concurrent_builds::text, s.state::text, s.capabilities::text FROM worker_sample s, w WHERE s.worker_id IN (SELECT worker_id FROM dispatched_job WHERE evaluation_id = $1) AND s.at BETWEEN w.started AND w.ended",
            "the workers that ran this evaluation, while it ran",
            [
                "id",
                "worker_id",
                "project",
                "at",
                "cpu_usage_pct",
                "ram_free_mb",
                "ram_total_mb",
                "disk_speed_mbps",
                "network_speed_mbps",
                "assigned_jobs",
                "max_concurrent_builds",
                "state",
                "capabilities"
            ]
        ),
        spec!(
            "cache_upstream",
            "CREATE TABLE cache_upstream (id TEXT, cache TEXT, display_name TEXT, mode INTEGER, upstream_cache TEXT, url TEXT, public_key TEXT, kind INTEGER, remote_cache_name TEXT)",
            "SELECT u.id::text, u.cache::text, u.display_name::text, u.mode::text, u.upstream_cache::text, u.url::text, u.public_key::text, u.kind::text, u.remote_cache_name::text FROM cache_upstream u WHERE u.cache IN (SELECT cache FROM project_cache WHERE project = $1)",
            "the caches this project subscribes to",
            [
                "id",
                "cache",
                "display_name",
                "mode",
                "upstream_cache",
                "url",
                "public_key",
                "kind",
                "remote_cache_name"
            ]
        ),
        spec!(
            "upstream_metric",
            "CREATE TABLE upstream_metric (id TEXT, bucket_time TEXT, latency_ms_sum INTEGER, request_count INTEGER, narinfo_hits INTEGER, narinfo_misses INTEGER, upstream_url TEXT)",
            "SELECT id::text, bucket_time::text, latency_ms_sum::text, request_count::text, narinfo_hits::text, narinfo_misses::text, upstream_url::text FROM upstream_metric WHERE $1 IS NOT NULL AND bucket_time > (now() AT TIME ZONE \'UTC\') - interval \'7 days\'",
            "the whole instance, last 7 days",
            [
                "id",
                "bucket_time",
                "latency_ms_sum",
                "request_count",
                "narinfo_hits",
                "narinfo_misses",
                "upstream_url"
            ]
        ),
        spec!(
            "base_worker",
            "CREATE TABLE base_worker (id TEXT, worker_id TEXT, display_name TEXT, url TEXT, enabled INTEGER, enable_fetch INTEGER, enable_eval INTEGER, enable_build INTEGER, created_by TEXT, created_at TEXT)",
            "SELECT id::text, worker_id::text, display_name::text, url::text, enabled::int::text, enable_fetch::int::text, enable_eval::int::text, enable_build::int::text, created_by::text, created_at::text FROM base_worker WHERE $1 IS NOT NULL",
            "the whole instance",
            [
                "id",
                "worker_id",
                "display_name",
                "url",
                "enabled",
                "enable_fetch",
                "enable_eval",
                "enable_build",
                "created_by",
                "created_at"
            ]
        ),
        spec!(
            "project_base_worker",
            "CREATE TABLE project_base_worker (id TEXT, project TEXT, base_worker TEXT, created_by TEXT, created_at TEXT)",
            "SELECT id::text, project::text, base_worker::text, created_by::text, created_at::text FROM project_base_worker WHERE project = $1",
            "this project's base worker opt-ins",
            ["id", "project", "base_worker", "created_by", "created_at"]
        ),
    ];

    SPECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ReportOptions;

    /// The report must never carry a credential, and the guard has to be an
    /// allowlist: a denylist starts leaking the day a table gains a column.
    #[test]
    fn no_exported_query_touches_a_secret_table_or_column() {
        const FORBIDDEN: &[&str] = &[
            "token_hash",
            "api_key",
            "password",
            "cli_device_authorization",
            "from api",
            "from session",
            "github_installation",
        ];
        for spec in eval_scope_tables().iter().chain(instance_tables()) {
            let sql = spec.sql.to_ascii_lowercase();
            assert!(
                !sql.contains('*'),
                "{}: SELECT * would export whatever the table gains later",
                spec.name
            );
            for needle in FORBIDDEN {
                assert!(
                    !sql.contains(needle),
                    "{} exports a secret: {needle}",
                    spec.name
                );
            }
        }
    }

    /// The report inspector's `why_stuck` weighs the `build_job` promotion gate
    /// by looking the row up in the exported `build_job` table, which only works
    /// because the anchor scope starts from that same set. The boundary rows the
    /// scope adds on top have no `build_job`, and that is the difference the
    /// inspector reads: a rewrite that scoped `derivation_build` some other way
    /// would leave it unable to tell the two apart.
    #[test]
    fn the_anchor_scope_starts_from_the_build_jobs_that_open_that_gate() {
        let specs = eval_scope_tables();
        let spec = specs
            .iter()
            .find(|s| s.name == "derivation_build")
            .expect("the report exports derivation_build");
        assert!(
            spec.sql
                .contains("SELECT derivation FROM build_job WHERE evaluation = $1"),
            "why_stuck reads the build_job gate against this set: {}",
            spec.sql
        );
    }

    /// Every table an offline reader re-derives `unready_deps` from is scoped to
    /// the evaluation's derivations AND their direct dependencies. Without the
    /// boundary the `LEFT JOIN ... IS NULL` rule that makes an absent dependency
    /// count as unready fires on every dependency the export merely left out, so
    /// a correct stored counter recomputes as a wildly larger number and the file
    /// accuses the instance of a dead zone it does not have.
    #[test]
    fn the_readiness_tables_carry_the_dependency_boundary() {
        for name in ["derivation", "derivation_build", "derivation_output"] {
            let sql = spec_named(name).sql;
            assert!(
                sql.contains(
                    "UNION SELECT e.dependency FROM derivation_dependency e WHERE e.derivation IN ("
                ),
                "{name} stops at the evaluation's own derivations: {sql}"
            );
        }
    }

    /// One fragment behind all of them, for the reason `readiness.rs` keeps one
    /// behind its seed and its recount: two spellings of the same scope drift,
    /// and a reader cannot see that they have.
    #[test]
    fn every_derivation_keyed_scope_is_the_same_fragment() {
        let anchor = spec_named("derivation_build").sql;
        let scope = anchor
            .split_once("WHERE db.derivation IN (")
            .and_then(|(_, rest)| rest.rsplit_once(')'))
            .map(|(scope, _)| scope)
            .expect("the anchor spec is scoped by a derivation set");

        for name in ["derivation", "derivation_output"] {
            assert!(
                spec_named(name).sql.contains(scope),
                "{name} spells the derivation scope differently"
            );
        }
    }

    /// A runtime reference is an edge of the graph now, so the dependency
    /// boundary already carries the producer of every path an exported output
    /// references and `cached_path` carries their paths. That is what separates
    /// "the instance never had this path" - the finding on an unwhole closure -
    /// from "the export did not ask for it".
    #[test]
    fn the_reference_boundary_is_exported_so_an_absent_path_means_absent() {
        let sql = spec_named("cached_path").sql;
        assert!(
            sql.contains(
                "UNION SELECT e.dependency FROM derivation_dependency e WHERE e.derivation IN ("
            ),
            "cached_path stops at the evaluation's own outputs: {sql}"
        );
        assert!(
            sql.contains("c.\"references\"::text"),
            "the narinfo line is what a reference closure is read from now: {sql}"
        );
    }

    /// An attempt's substitute-miss budget is scoped per `(anchor, evaluation)`
    /// through its `build_job`. Export the attempts without those rows and the
    /// budget cannot be bucketed at all, which is how a loop that ran 788 misses
    /// against a threshold of 2 read as an ordinary retry history.
    #[test]
    fn the_jobs_behind_the_exported_attempts_are_exported() {
        let sql = spec_named("build_job").sql;
        assert!(
            sql.contains("SELECT a.build_job FROM build_attempt a WHERE a.derivation_build IN ("),
            "an attempt from another evaluation cannot be bucketed: {sql}"
        );
    }

    /// The gate the promoter reads is
    /// `walked AND build_job AND demanded AND (substitutable OR unready_deps = 0)`.
    /// A report that carries every term but `demanded` reports "every gate open"
    /// for an anchor held by the one gate it cannot see, which is exactly the
    /// state an undemanded relay sits in forever.
    #[test]
    fn an_anchor_exports_the_demand_gate() {
        let spec = spec_named("derivation_build");
        assert!(spec.columns.contains(&"demanded"), "{:?}", spec.columns);
        assert!(spec.ddl.contains("demanded INTEGER"), "{}", spec.ddl);
        assert!(spec.sql.contains("db.demanded::int::text"), "{}", spec.sql);
    }

    /// Demand stops at an anchor the upstream probe has not answered for, so its
    /// inputs read as undemanded with no reason of their own. `probed` is that
    /// reason, and nothing else in the export carries it.
    #[test]
    fn an_anchor_exports_whether_the_probe_answered() {
        let spec = spec_named("derivation_build");
        assert!(spec.columns.contains(&"probed"), "{:?}", spec.columns);
        assert!(spec.ddl.contains("probed INTEGER"), "{}", spec.ddl);
        assert!(spec.sql.contains("db.probed::int::text"), "{}", spec.sql);
    }

    #[test]
    fn every_spec_is_scoped_and_internally_consistent() {
        for spec in eval_scope_tables().iter().chain(instance_tables()) {
            assert!(
                spec.ddl.contains(spec.name),
                "{}: ddl names another table",
                spec.name
            );
            assert!(
                spec.sql.contains("$1"),
                "{}: every export must be scoped to the evaluation",
                spec.name
            );
            assert_eq!(
                spec.columns.len(),
                spec.ddl.matches(',').count() + 1,
                "{}: column list and ddl disagree on width",
                spec.name
            );
            assert!(
                !spec.scope.is_empty(),
                "{}: the manifest has to say what $1 selected",
                spec.name
            );
        }
    }

    fn spec_named(name: &str) -> &'static TableSpec {
        eval_scope_tables()
            .iter()
            .chain(instance_tables())
            .find(|s| s.name == name)
            .expect("spec exists")
    }

    /// A build's phase events are recorded against its `derivation_build`
    /// anchor, never the per-eval `build_job` row, so joining on `build_job.id`
    /// silently exported an evaluation with no build timing at all.
    #[test]
    fn build_phase_events_hang_off_the_anchor_not_the_build_job() {
        let sql = spec_named("phase_event").sql;
        assert!(
            sql.contains("SELECT derivation_build FROM build_job"),
            "{sql}"
        );
        assert!(!sql.contains("SELECT id FROM build_job"), "{sql}");
    }

    /// `evaluation.commit` is a foreign key, so without the commit itself a
    /// report names the repository but never the revision that broke.
    #[test]
    fn the_commit_is_exported_and_its_hash_is_readable() {
        let spec = spec_named("commit");
        assert!(
            spec.sql.contains("encode(c.hash, 'hex')"),
            "a bytea hash has to be hex to be greppable: {}",
            spec.sql
        );
        assert!(spec.columns.contains(&"hash"));
    }

    /// The commit message is free text carrying whatever the author wrote, so
    /// it takes the log treatment rather than passing through verbatim.
    #[test]
    fn a_commit_message_is_redacted_against_the_minted_pseudonyms() {
        let r = redactor(true, false);
        let repo = "git@example.invalid:acme/infra.git";
        r.identity(repo, "repo");

        let out = redact_value(
            &r,
            "commit",
            "message",
            Some(format!("fix the build for {repo}")),
        )
        .expect("value");
        assert!(!out.contains("acme/infra"), "{out}");
    }

    /// Worker history used to be scoped by project, and a connection is
    /// attributed to whichever project registered the worker first, so a shared
    /// worker's history landed under someone else's project and this report's
    /// instance section came back empty. It follows the jobs instead.
    #[test]
    fn worker_history_follows_the_workers_that_ran_the_evaluation() {
        for name in ["worker_connection", "worker_sample"] {
            let sql = spec_named(name).sql;
            assert!(
                sql.contains("FROM dispatched_job WHERE evaluation_id = $1"),
                "{name}: {sql}"
            );
            assert!(
                !sql.contains("project = $1"),
                "{name} must not scope by project: {sql}"
            );
        }
    }

    /// A phase span belongs to a dispatched job, not to the evaluation, so it
    /// is scoped through the jobs this evaluation dispatched. Scoping it any
    /// other way would either miss the build jobs or pull in a stranger's.
    #[test]
    fn phase_spans_are_scoped_through_the_evaluations_dispatched_jobs() {
        let sql = spec_named("dispatched_job_phase").sql;
        assert!(
            sql.contains("SELECT id FROM dispatched_job WHERE evaluation_id = $1"),
            "{sql}"
        );
    }

    /// The outcome is what separates "the worker finished" from "the worker
    /// vanished" when reading a report offline, so it must be exported.
    #[test]
    fn a_dispatched_job_exports_its_outcome() {
        let spec = spec_named("dispatched_job");
        assert!(spec.columns.contains(&"outcome"), "{:?}", spec.columns);
        assert!(spec.ddl.contains("outcome INTEGER"), "{}", spec.ddl);
        assert!(spec.sql.contains("outcome::text"), "{}", spec.sql);
    }

    /// `fetchable` and `unready_deps` are what a stalled anchor is waiting on,
    /// so a report that stops at `status` cannot say why nothing dispatched.
    #[test]
    fn an_anchor_exports_its_readiness_counters() {
        let spec = spec_named("derivation_build");
        for column in ["fetchable", "unready_deps"] {
            assert!(spec.columns.contains(&column), "{:?}", spec.columns);
            assert!(
                spec.ddl.contains(&format!("{column} INTEGER")),
                "{}",
                spec.ddl
            );
        }

        assert!(spec.sql.contains("db.fetchable::int::text"), "{}", spec.sql);
        assert!(spec.sql.contains("db.unready_deps::text"), "{}", spec.sql);
    }

    /// A stuck evaluation is exactly the one worth reporting on, and
    /// `updated_at` freezes at the moment it wedged: bounding the worker window
    /// there ends it before the interesting period instead of at "now".
    #[test]
    fn the_worker_window_stays_open_while_the_evaluation_is_unfinished() {
        for name in ["worker_connection", "worker_sample"] {
            let sql = spec_named(name).sql;
            assert!(
                sql.contains("COALESCE(finished_at, (now() AT TIME ZONE 'UTC'))"),
                "{name}: {sql}"
            );
            assert!(
                !sql.contains("COALESCE(finished_at, updated_at)"),
                "{name} still bounds the window at the wedge: {sql}"
            );
        }
    }

    /// `record_worker_connection` only opens a row for a worker that has a
    /// `worker_registration`, so on an instance running base workers all three
    /// worker tables are empty and the report reads as "no workers at all".
    /// The fleet itself has to be exported for that to be distinguishable.
    #[test]
    fn the_base_worker_fleet_is_exported() {
        let spec = spec_named("base_worker");
        assert!(spec.columns.contains(&"enable_eval"), "{:?}", spec.columns);
        assert!(spec.columns.contains(&"enabled"), "{:?}", spec.columns);
        assert!(
            !spec.columns.contains(&"token_hash"),
            "a base worker's token must never leave the instance"
        );
        assert!(
            spec_named("project_base_worker")
                .sql
                .contains("project = $1")
        );
    }

    /// An `input_update` evaluation blocks every later flake-lock run for its
    /// task while it stays active, and the sidecar is the only row saying which
    /// inputs it holds.
    #[test]
    fn an_input_update_evaluation_exports_its_sidecar() {
        let spec = spec_named("evaluation_input_update");
        assert!(spec.sql.contains("WHERE evaluation = $1"), "{}", spec.sql);
        for column in ["target_inputs", "discover_only", "generator"] {
            assert!(spec.columns.contains(&column), "{:?}", spec.columns);
        }
        assert!(
            !spec.columns.contains(&"candidate_lock"),
            "a whole generated flake.lock is not scheduling evidence"
        );
    }

    /// A narinfo is only served when a `cached_path_signature` row exists for
    /// the asking cache *and* carries a signature; without one the cache 404s a
    /// path whose `cached_path` row says it is right there. That gate is
    /// invisible in a report that stops at `cached_path`.
    #[test]
    fn the_narinfo_signature_gate_is_exported() {
        let spec = spec_named("cached_path_signature");
        assert!(spec.columns.contains(&"signed"), "{:?}", spec.columns);
        assert!(
            !spec.columns.contains(&"signature"),
            "the raw Ed25519 blob is not evidence; whether it exists is"
        );
        assert!(
            !spec.sql.contains("s.signature::"),
            "a bytea signature must never be cast out verbatim: {}",
            spec.sql
        );
        assert!(
            spec.sql.contains("(s.signature IS NOT NULL)"),
            "{}",
            spec.sql
        );
        let paths = spec_named("cached_path").sql;
        let scope = paths
            .split_once("WHERE c.hash IN (")
            .and_then(|(_, rest)| rest.rsplit_once(')'))
            .map(|(scope, _)| scope)
            .expect("cached_path is scoped by a hash set");
        assert!(
            spec.sql.contains(scope),
            "the gate has to cover exactly the paths cached_path exports: {}",
            spec.sql
        );
    }

    /// The gate is per cache, so a row that names no cache cannot say whether
    /// the cache the client asked is the one holding the signature.
    #[test]
    fn a_signature_row_names_its_cache() {
        let spec = spec_named("cached_path_signature");
        assert!(spec.columns.contains(&"cache_name"), "{:?}", spec.columns);
        let r = redactor(true, false);
        assert_ne!(
            redact_value(
                &r,
                "cached_path_signature",
                "cache_name",
                Some("wavelens-prod".into())
            ),
            Some("wavelens-prod".into()),
            "a cache name is an identity and follows cache_upstream's policy"
        );
    }

    fn redactor(identities: bool, packages: bool) -> Redactor {
        Redactor::new(ReportOptions {
            anonymize_identities: identities,
            anonymize_packages: packages,
            include_logs: true,
            include_instance: true,
        })
    }

    #[test]
    fn redaction_policy_covers_the_identifying_columns() {
        let r = redactor(true, true);
        let repo = "git@git.supersandro.de:sandro/nixos-config.git";
        assert_ne!(
            redact_value(&r, "evaluation", "repository", Some(repo.into())),
            Some(repo.into())
        );
        assert_ne!(
            redact_value(&r, "derivation", "name", Some("hello-2.12".into())),
            Some("hello-2.12".into())
        );
        assert_ne!(
            redact_value(
                &r,
                "worker_connection",
                "worker_id",
                Some("builder-1".into())
            ),
            Some("builder-1".into())
        );
    }

    #[test]
    fn a_column_with_no_policy_passes_through_untouched() {
        let r = redactor(true, true);
        assert_eq!(
            redact_value(&r, "evaluation", "status", Some("7".into())),
            Some("7".into())
        );
        assert_eq!(redact_value(&r, "evaluation", "repository", None), None);
    }
}
