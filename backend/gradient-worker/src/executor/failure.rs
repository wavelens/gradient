/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Failure classification - the one place an error becomes a
//! [`BuildFailureKind`] on its way to the server.

use gradient_proto::messages::BuildFailureKind;

use crate::proto::prefetch::{CorruptCachedNar, MissingInputs, SubstituteNotOnUpstream};

// ── BuildError ────────────────────────────────────────────────────────────────

/// A build failure carrying its classification, so the dispatch layer can
/// report the right `BuildFailureKind` to the server.
#[derive(Debug)]
pub struct BuildError {
    pub kind: BuildFailureKind,
    pub source: anyhow::Error,
    /// For `BuildFailureKind::InputsUnavailable`: the required input store paths
    /// the cache could not serve. Empty for every other kind.
    pub missing_paths: Vec<String>,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.source)
    }
}
impl std::error::Error for BuildError {}

impl BuildError {
    pub(super) fn new(kind: BuildFailureKind, source: anyhow::Error) -> Self {
        Self {
            kind,
            source,
            missing_paths: Vec::new(),
        }
    }
    pub(crate) fn transient(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::Transient, e.into())
    }
    pub(crate) fn permanent(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::Permanent, e.into())
    }
    pub(crate) fn timeout(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::Timeout, e.into())
    }
    /// A substitute attempt missed: this worker could not pull the output from
    /// cache. Never falls back to a local build (wrong-arch); the scheduler
    /// re-dispatches or escalates to a real build.
    pub(crate) fn substitute_unavailable(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::SubstituteUnavailable, e.into())
    }
    /// Prefetch found required inputs the gradient cache cannot serve. Carries
    /// the offending paths so the server demotes them and re-queues their
    /// producers; terminal for this build.
    pub(crate) fn inputs_unavailable(
        missing_paths: Vec<String>,
        e: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind: BuildFailureKind::InputsUnavailable,
            source: e.into(),
            missing_paths,
        }
    }
    /// The server sent `AbortJob` while the daemon was building. Terminal: the
    /// build is already in a terminal state server-side, so retrying is wrong.
    pub(crate) fn aborted(drv_path: &str) -> Self {
        Self::new(
            BuildFailureKind::Permanent,
            anyhow::anyhow!("build aborted by server: {}", drv_path),
        )
    }
}

// ── Builder-message classification ────────────────────────────────────────────

/// Best-effort OOM signature scan. OOM presents as a generic build failure but
/// is transient (retry on a less-loaded builder).
pub(super) fn looks_like_oom(msg: &str) -> bool {
    let l = msg.to_ascii_lowercase();
    l.contains("out of memory")
        || l.contains("cannot allocate memory")
        || l.contains("oom-killer")
        || l.contains("killed")
}

/// Classify a builder-reported failure message: OOM -> Transient, otherwise a
/// real build error -> Permanent.
pub(super) fn classify_build_error(msg: &str) -> BuildFailureKind {
    if looks_like_oom(msg) {
        BuildFailureKind::Transient
    } else {
        BuildFailureKind::Permanent
    }
}

// ── Transfer-error classification ─────────────────────────────────────────────

/// Classify an input-prefetch failure.
///
/// A "required inputs not in cache" miss is terminal and self-healing
/// server-side: forward the paths so the server demotes them and re-queues
/// their producers. A cached NAR that fails integrity (its bytes don't match
/// the recorded nar_hash, e.g. a non-reproducible local build desynced from
/// upstream-substitute metadata) is the same class: report the path so the
/// server demotes the corrupt object and rebuilds it. Every other prefetch
/// error is infrastructure-transient.
pub(super) fn classify_prefetch_error(build_id: &str, e: anyhow::Error) -> BuildError {
    tracing::error!(%build_id, error = %e, "input prefetch failed; aborting build");
    if let Some(mi) = e.downcast_ref::<MissingInputs>() {
        BuildError::inputs_unavailable(mi.0.clone(), e)
    } else if let Some(corrupt) = e.chain().find_map(|s| s.downcast_ref::<CorruptCachedNar>()) {
        BuildError::inputs_unavailable(vec![corrupt.0.clone()], e)
    } else {
        BuildError::transient(e)
    }
}

/// Classify an `external_cached` substitute-relay failure.
pub(super) fn classify_substitute_failure(build_id: &str, e: anyhow::Error) -> BuildError {
    if e.chain().any(|c| c.is::<SubstituteNotOnUpstream>()) {
        tracing::warn!(%build_id, error = %e, "external_cached relay: output on no upstream; SubstituteUnavailable");
        BuildError::substitute_unavailable(e)
    } else if let Some(mi) = e.chain().find_map(|c| c.downcast_ref::<MissingInputs>()) {
        // The upstream advertised the path but the object GET 404'd: surface
        // the paths so the server's demote/reconcile self-heal clears the
        // stale record instead of this build retrying against it forever.
        tracing::warn!(%build_id, error = %e, "external_cached relay: advertised NAR object missing; InputsUnavailable");
        BuildError::inputs_unavailable(mi.0.clone(), e)
    } else {
        tracing::warn!(%build_id, error = %e, "external_cached relay failed transiently; retrying without escalating");
        BuildError::transient(e)
    }
}

// ── Wire mapping ──────────────────────────────────────────────────────────────

/// Map a finished job's error to the `(kind, missing_paths)` pair reported in
/// `ClientMessage::JobFailed`. Anything that isn't a [`BuildError`] (eval-job
/// failures, plumbing errors) is an explicit, logged Permanent fallthrough -
/// never a silent default.
pub(crate) fn wire_failure(e: &anyhow::Error) -> (BuildFailureKind, Vec<String>) {
    match e.downcast_ref::<BuildError>() {
        Some(be) => (be.kind, be.missing_paths.clone()),
        None => {
            tracing::warn!(error = %format!("{e:#}"), "unclassified job error reported as Permanent");
            (BuildFailureKind::Permanent, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_and_looks_like_oom() {
        assert_eq!(
            classify_build_error("gcc: fatal error: Killed signal terminated program cc1plus"),
            BuildFailureKind::Transient
        );
        assert_eq!(
            classify_build_error("error: undefined reference to `foo'"),
            BuildFailureKind::Permanent
        );
        assert!(looks_like_oom("Cannot allocate memory"));
        assert!(looks_like_oom("Killed"));
        assert!(looks_like_oom("oom-killer: invoked"));
        assert!(!looks_like_oom("error: undefined reference to `foo'"));
    }

    #[test]
    fn wire_failure_downcasts_build_error() {
        let be = BuildError::inputs_unavailable(
            vec!["/nix/store/x-y".into()],
            anyhow::anyhow!("missing"),
        );
        let e: anyhow::Error = be.into();
        let (kind, missing) = wire_failure(&e);
        assert_eq!(kind, BuildFailureKind::InputsUnavailable);
        assert_eq!(missing, vec!["/nix/store/x-y".to_owned()]);
    }

    #[test]
    fn wire_failure_unclassified_is_explicit_permanent() {
        let e = anyhow::anyhow!("some plumbing exploded");
        let (kind, missing) = wire_failure(&e);
        assert_eq!(kind, BuildFailureKind::Permanent);
        assert!(missing.is_empty());
    }

    #[test]
    fn prefetch_missing_inputs_carries_paths() {
        let e = anyhow::Error::new(crate::proto::prefetch::MissingInputs(vec![
            "/nix/store/a-b".into(),
        ]));
        let be = classify_prefetch_error("b1", e);
        assert_eq!(be.kind, BuildFailureKind::InputsUnavailable);
        assert_eq!(be.missing_paths, vec!["/nix/store/a-b".to_owned()]);
    }

    #[test]
    fn substitute_relay_404_is_inputs_unavailable() {
        let e = anyhow::Error::new(crate::proto::prefetch::MissingInputs(vec![
            "/nix/store/a-b".into(),
        ]))
        .context("download upstream NAR");
        let be = classify_substitute_failure("b1", e);
        assert_eq!(be.kind, BuildFailureKind::InputsUnavailable);
        assert_eq!(be.missing_paths, vec!["/nix/store/a-b".to_owned()]);
    }

    /// Only a genuine "not on any upstream" miss escalates; a transient relay
    /// timeout (Pull RPC / NAR download / presigned PUT) retries as a substitute
    /// instead of counting toward miss-escalation - two transient timeouts must
    /// not turn a substitutable build into a from-scratch one.
    #[test]
    fn substitute_wrapped_and_transient_classification() {
        use crate::proto::prefetch::SubstituteNotOnUpstream;

        let wrapped = classify_substitute_failure(
            "b",
            anyhow::Error::new(SubstituteNotOnUpstream("/nix/store/p".into())).context("relay"),
        );
        assert!(matches!(
            wrapped.kind,
            BuildFailureKind::SubstituteUnavailable
        ));

        let timeout = classify_substitute_failure("b", anyhow::anyhow!("operation timed out"));
        assert!(matches!(timeout.kind, BuildFailureKind::Transient));
    }

    #[test]
    fn substitute_not_on_upstream_wins() {
        let e = anyhow::Error::new(crate::proto::prefetch::SubstituteNotOnUpstream(
            "/nix/store/a-b".into(),
        ));
        let be = classify_substitute_failure("b1", e);
        assert_eq!(be.kind, BuildFailureKind::SubstituteUnavailable);
    }
}
