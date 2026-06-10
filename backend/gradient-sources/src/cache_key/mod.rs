/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cache signing-key lifecycle: generation, encrypted storage format, narinfo
//! signing, and signature verification. `narinfo` holds the fingerprint
//! construction shared by the signing and verification paths.

mod format;
mod generate;
mod narinfo;
mod signing;
mod verification;

pub use format::{decrypt_signing_key, format_cache_key, format_cache_public_key};
pub use generate::generate_signing_key;
pub use signing::{CacheSigner, sign_narinfo_fingerprint};
pub use verification::verify_narinfo_signature;

#[cfg(test)]
mod tests;
