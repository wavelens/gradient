/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::proto::BuildSpecKind;

/// A substitute or a download runs on any worker, since neither needs a nix store;
/// a build needs a worker of its own architecture. A `builtin` derivation is nix's
/// own builder: fixed-output ones are `builtin:fetchurl`, which a worker executes
/// itself; the rest (`builtin:buildenv`) stay builds the daemon runs.
///
/// Whether an anchor is still worth substituting is not decided here. A spent miss
/// budget clears `substitutable` in the graph actor, on the failure that spends it,
/// so this reads the flag and nothing else.
pub(crate) fn decide_build_spec_kind(
    substitutable: bool,
    architecture: &str,
    is_fixed_output: bool,
) -> BuildSpecKind {
    if substitutable {
        BuildSpecKind::Substitute
    } else if architecture == gradient_types::BUILTIN_ARCH && is_fixed_output {
        BuildSpecKind::Download
    } else {
        BuildSpecKind::Build
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutable_is_a_substitute_and_everything_else_builds_on_its_arch() {
        assert_eq!(
            decide_build_spec_kind(true, "x86_64-linux", false),
            BuildSpecKind::Substitute
        );
        assert_eq!(
            decide_build_spec_kind(false, "x86_64-linux", false),
            BuildSpecKind::Build
        );
    }

    #[test]
    fn a_builtin_fixed_output_downloads_and_a_builtin_buildenv_builds() {
        assert_eq!(
            decide_build_spec_kind(true, "builtin", true),
            BuildSpecKind::Substitute
        );
        assert_eq!(
            decide_build_spec_kind(false, "builtin", true),
            BuildSpecKind::Download
        );
        assert_eq!(
            decide_build_spec_kind(false, "builtin", false),
            BuildSpecKind::Build
        );
        assert_eq!(
            decide_build_spec_kind(false, "x86_64-linux", true),
            BuildSpecKind::Build
        );
    }
}
