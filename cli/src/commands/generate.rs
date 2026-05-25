/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::output::Output;
use clap::Subcommand;
use rand::distr::{Alphanumeric, SampleString};

#[derive(Subcommand, Debug)]
pub enum Commands {
    Apikey,
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Apikey => {
            let (private_key, public_key) = generate_api_key_pair();
            out.human("Generated API key pair:");
            out.human("");
            out.human(format!(
                "Private Key (for state configuration): {}",
                private_key
            ));
            out.human(format!(
                "Public Key (for API authentication): {}",
                public_key
            ));
            out.human("");
            out.human("Save the private key to a file and reference it in key_file.");
            out.human(
                "Use the public key for API authentication with 'Authorization: Bearer <public_key>'",
            );
        }
    }
}

fn generate_api_key_pair() -> (String, String) {
    let private_key: String = Alphanumeric.sample_string(&mut rand::rng(), 64);
    let public_key = format!("GRAD{}", private_key);
    (private_key, public_key)
}
