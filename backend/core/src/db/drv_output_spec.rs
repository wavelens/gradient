/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! View type for [`DerivationOutput`] that encodes the three mutually
//! exclusive output kinds as an enum.
//!
//! A `.drv` output can be one of three shapes, discriminated by which string
//! fields are populated:
//!
//! | Shape               | `hash_algo` | `hash` | `path` |
//! |---------------------|-------------|--------|--------|
//! | Fixed-output (FOD)  | non-empty   | non-empty | any  |
//! | Floating CA         | empty       | empty  | **empty** |
//! | Input-addressed     | empty       | empty  | non-empty |
//!
//! Callers that pattern-match on [`DrvOutputSpec`] are protected from
//! forgetting to check which combination of empty/non-empty fields applies,
//! which was a prior source of bugs (treating an FOD as input-addressed causes
//! sandbox network access to be disabled and every `fetchurl` to fail).

use super::derivation::DerivationOutput;

/// The three mutually exclusive kinds of a Nix derivation output.
///
/// Obtain via [`DerivationOutput::as_spec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrvOutputSpec<'a> {
    /// **Fixed-output derivation (FOD)** — both `hash_algo` and `hash` are
    /// present. The daemon requires this variant to enable network access
    /// inside the build sandbox (e.g. `fetchurl`, `fetchGit`).
    ///
    /// Sending a FOD as [`InputAddressed`](DrvOutputSpec::InputAddressed)
    /// silently sandboxes it without DNS, causing every network fetch to fail.
    FixedOutput {
        /// Hash algorithm in Nix `.drv` wire format: `"sha256"`, `"r:sha256"`,
        /// or `"text:sha256"`.
        hash_algo: &'a str,
        /// Hex-encoded expected hash digest.
        hash: &'a str,
    },

    /// **Floating content-addressed derivation** — `path`, `hash_algo`, and
    /// `hash` are all empty. The daemon computes the output path from the
    /// build result.
    Deferred,

    /// **Input-addressed derivation** — `path` is non-empty and `hash_algo` /
    /// `hash` are empty. The output path is fully determined at eval time.
    InputAddressed {
        /// Full `/nix/store/hash-name` output path.
        path: &'a str,
    },
}

impl DerivationOutput {
    /// Return the [`DrvOutputSpec`] for this output.
    ///
    /// The discrimination follows the Nix `.drv` ATerm format:
    /// - `hash_algo` + `hash` both non-empty → [`DrvOutputSpec::FixedOutput`]
    /// - `path` non-empty → [`DrvOutputSpec::InputAddressed`]
    /// - otherwise → [`DrvOutputSpec::Deferred`]
    pub fn as_spec(&self) -> DrvOutputSpec<'_> {
        if !self.hash_algo.is_empty() && !self.hash.is_empty() {
            DrvOutputSpec::FixedOutput {
                hash_algo: &self.hash_algo,
                hash: &self.hash,
            }
        } else if !self.path.is_empty() {
            DrvOutputSpec::InputAddressed { path: &self.path }
        } else {
            DrvOutputSpec::Deferred
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, path: &str, hash_algo: &str, hash: &str) -> DerivationOutput {
        DerivationOutput {
            name: name.into(),
            path: path.into(),
            hash_algo: hash_algo.into(),
            hash: hash.into(),
        }
    }

    #[test]
    fn fod_flat_sha256() {
        let o = output(
            "out",
            "",
            "sha256",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        match o.as_spec() {
            DrvOutputSpec::FixedOutput { hash_algo, hash } => {
                assert_eq!(hash_algo, "sha256");
                assert!(!hash.is_empty());
            }
            other => panic!("expected FixedOutput, got {other:?}"),
        }
    }

    #[test]
    fn fod_recursive_sha256() {
        let o = output(
            "out",
            "/nix/store/aaaa-foo",
            "r:sha256",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        assert!(matches!(
            o.as_spec(),
            DrvOutputSpec::FixedOutput {
                hash_algo: "r:sha256",
                ..
            }
        ));
    }

    #[test]
    fn input_addressed() {
        let o = output("out", "/nix/store/aaaa-foo", "", "");
        match o.as_spec() {
            DrvOutputSpec::InputAddressed { path } => {
                assert_eq!(path, "/nix/store/aaaa-foo");
            }
            other => panic!("expected InputAddressed, got {other:?}"),
        }
    }

    #[test]
    fn deferred_all_empty() {
        let o = output("out", "", "", "");
        assert_eq!(o.as_spec(), DrvOutputSpec::Deferred);
    }

    #[test]
    fn only_hash_algo_without_hash_is_deferred() {
        // Partial FOD (hash_algo set but hash missing) → treat as Deferred
        // to avoid sending a malformed CAFixed to the daemon.
        let o = output("out", "", "sha256", "");
        assert_eq!(o.as_spec(), DrvOutputSpec::Deferred);
    }
}
