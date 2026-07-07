/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BuildDispatchMode {
    RealArch,
    SubstituteBuiltin,
    SubstituteStalled,
}

/// Decide how a ready build should be dispatched.
///
/// - non-substitutable        → `RealArch`
/// - substitutable, under the miss budget → `SubstituteBuiltin` (builtin, any worker)
/// - substitutable, budget spent, a worker for its arch IS connected → `RealArch` (escalate)
/// - substitutable, budget spent, NO worker for its arch → `SubstituteStalled`
pub(crate) fn arch_available(connected: &std::collections::HashSet<String>, arch: &str) -> bool {
    arch == gradient_types::BUILTIN_ARCH || connected.contains(arch)
}

pub(crate) fn decide_dispatch_mode(
    substitutable: bool,
    miss_count: i64,
    threshold: i64,
    arch_has_worker: bool,
) -> BuildDispatchMode {
    if !substitutable {
        BuildDispatchMode::RealArch
    } else if miss_count < threshold {
        BuildDispatchMode::SubstituteBuiltin
    } else if arch_has_worker {
        BuildDispatchMode::RealArch
    } else {
        BuildDispatchMode::SubstituteStalled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_substitutable_is_real_arch() {
        assert_eq!(
            decide_dispatch_mode(false, 0, 2, false),
            BuildDispatchMode::RealArch
        );
        assert_eq!(
            decide_dispatch_mode(false, 5, 2, true),
            BuildDispatchMode::RealArch
        );
    }

    #[test]
    fn substitutable_under_threshold_is_builtin() {
        assert_eq!(
            decide_dispatch_mode(true, 0, 2, false),
            BuildDispatchMode::SubstituteBuiltin
        );
        assert_eq!(
            decide_dispatch_mode(true, 1, 2, false),
            BuildDispatchMode::SubstituteBuiltin
        );
    }

    #[test]
    fn escalates_only_when_arch_worker_present() {
        assert_eq!(
            decide_dispatch_mode(true, 2, 2, true),
            BuildDispatchMode::RealArch
        );
    }

    #[test]
    fn stalls_when_budget_spent_and_no_arch_worker() {
        assert_eq!(
            decide_dispatch_mode(true, 2, 2, false),
            BuildDispatchMode::SubstituteStalled
        );
        assert_eq!(
            decide_dispatch_mode(true, 9, 2, false),
            BuildDispatchMode::SubstituteStalled
        );
    }

    #[test]
    fn arch_available_builtin_always_true() {
        let empty = std::collections::HashSet::new();
        assert!(arch_available(&empty, "builtin"));
        let mut connected = std::collections::HashSet::new();
        connected.insert("x86_64-linux".to_string());
        assert!(arch_available(&connected, "builtin"));
        assert!(arch_available(&connected, "x86_64-linux"));
        assert!(!arch_available(&connected, "aarch64-linux"));
    }
}
