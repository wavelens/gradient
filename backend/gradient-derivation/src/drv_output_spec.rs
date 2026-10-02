/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::derivation::DerivationOutput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrvOutputSpec<'a> {
    /// The daemon is enabling sandbox network access only for this variant. An FOD sent as
    /// [`InputAddressed`](DrvOutputSpec::InputAddressed) is losing DNS and failing every fetch.
    FixedOutput {
        hash_algo: &'a str,
        hash: &'a str,
    },

    Deferred,

    InputAddressed {
        path: &'a str,
    },
}

impl DerivationOutput {
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
        let o = output("out", "", "sha256", "");
        assert_eq!(o.as_spec(), DrvOutputSpec::Deferred);
    }
}
