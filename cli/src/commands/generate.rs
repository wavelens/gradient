/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::output::Output;
use clap::Subcommand;
use rand::distr::{Alphanumeric, SampleString};
use sha2::{Digest, Sha256};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Generate an API token and the digest for a declarative `api_keys.<name>.key_file`
    Apikey,
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Apikey => {
            let token = generate_api_token();
            out.human(format!("API token: {token}"));
            out.human(format!("key_file digest: {}", key_file_digest(&token)));
            out.human("");
            out.human("Write the digest to the file referenced by key_file.");
            out.human("Authenticate with 'Authorization: Bearer <API token>'.");
        }
    }
}

fn generate_api_token() -> String {
    format!("GRAD{}", Alphanumeric.sample_string(&mut rand::rng(), 64))
}

/// `api_keys.<name>.key_file` must contain this digest, the form the server is storing.
fn key_file_digest(token: &str) -> String {
    let raw = token.strip_prefix("GRAD").unwrap_or(token);
    Sha256::digest(raw.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_digest_hashes_the_token_without_its_prefix() {
        assert_eq!(
            key_file_digest("GRADabc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn generated_token_carries_the_prefix_and_a_digest_the_server_accepts() {
        let token = generate_api_token();
        let digest = key_file_digest(&token);

        assert!(token.starts_with("GRAD"));
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
}
