/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::proto::BuildSpecKind;

/// A substitute runs on any worker, since it only moves bytes between two caches;
/// everything else needs a worker of its own architecture.
///
/// Whether an anchor is still worth substituting is not decided here. A spent miss
/// budget clears `substitutable` in the graph actor, on the failure that spends it,
/// so this reads the flag and nothing else.
pub(crate) fn decide_build_spec_kind(substitutable: bool) -> BuildSpecKind {
    if substitutable {
        BuildSpecKind::Substitute
    } else {
        BuildSpecKind::Build
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutable_is_a_substitute_and_everything_else_builds_on_its_arch() {
        assert_eq!(decide_build_spec_kind(true), BuildSpecKind::Substitute);
        assert_eq!(decide_build_spec_kind(false), BuildSpecKind::Build);
    }
}
