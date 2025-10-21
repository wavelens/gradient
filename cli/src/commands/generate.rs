/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Subcommand;
use rand::{distributions::Alphanumeric, Rng};

#[derive(Subcommand, Debug)]
pub enum Commands {
    Apikey,
}

pub async fn handle(cmd: Commands) {
    match cmd {
        Commands::Apikey => {
            let (private_key, public_key) = generate_api_key_pair();
            println!("Generated API key pair:");
            println!();
            println!("Private Key (for state configuration): {}", private_key);
            println!("Public Key (for API authentication): {}", public_key);
            println!();
            println!("Save the private key to a file and reference it in key_file.");
            println!(
                "Use the public key for API authentication with 'Authorization: Bearer <public_key>'"
            );
        }
    }
}

fn generate_api_key_pair() -> (String, String) {
    // Generate a 64-character alphanumeric key that matches the backend format
    let private_key: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    // The public key is the private key with "GRAD" prefix for API authentication
    let public_key = format!("GRAD{}", private_key);

    (private_key, public_key)
}
