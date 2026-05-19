/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::get_request_config;
use connector::builds;
use std::io::{self, Write};
use std::process::exit;

pub async fn handle_download(build_id: Option<String>, filename: Option<String>) {
    let config = get_request_config(load_config()).unwrap_or_else(|_| {
        eprintln!("Not configured. Use 'gradient config' to set server and auth token.");
        exit(1);
    });

    // Determine which build to use: CLI arg > selected-build config > interactive
    let build_id = build_id.or_else(|| {
        crate::config::set_get_value(crate::config::ConfigKey::SelectedBuild, None, true)
    });

    // If both build_id and filename are provided, download directly
    if let (Some(build_id), Some(filename)) = (build_id.as_ref(), filename.as_ref()) {
        println!("Downloading {} from build {}...", filename, build_id);

        match builds::download_build_file(config, build_id.clone(), filename.clone()).await {
            Ok(data) => match std::fs::write(filename, data) {
                Ok(()) => {
                    println!("Downloaded {} successfully!", filename);
                    return;
                }
                Err(e) => {
                    eprintln!("Failed to write file: {}", e);
                    exit(1);
                }
            },
            Err(e) => {
                eprintln!("Failed to download file: {}", e);
                exit(1);
            }
        }
    }

    // If only build_id is provided, list downloads for that build
    if let Some(build_id) = build_id.as_ref() {
        println!("Fetching downloads for build {}...", build_id);

        // First, let's check the build status for debugging
        match builds::get_build(config.clone(), build_id.clone()).await {
            Ok(response) => {
                if !response.error {
                    println!("Build status: {}", response.message.status);
                }
            }
            Err(e) => {
                println!("Warning: Could not get build status: {}", e);
            }
        }

        let downloads = match builds::get_build_downloads(config.clone(), build_id.clone()).await {
            Ok(response) => {
                if response.error {
                    eprintln!("Failed to get downloads: {:?}", response.message);
                    exit(1);
                }
                response.message
            }
            Err(e) => {
                eprintln!("Failed to get downloads: {}", e);
                exit(1);
            }
        };

        if downloads.is_empty() {
            println!("No downloads available for this build.");
            return;
        }

        // Display available downloads
        println!("\nAvailable downloads:");
        for (index, download) in downloads.iter().enumerate() {
            println!(
                "{}. {} ({}) - {}",
                index + 1,
                download.name,
                download.file_type,
                download.path
            );
        }

        // Get user selection for download
        print!("\nSelect a file to download (1-{}): ", downloads.len());
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        let selection: usize = match input.trim().parse::<usize>() {
            Ok(n) if n > 0 && n <= downloads.len() => n - 1,
            _ => {
                eprintln!("Invalid selection.");
                exit(1);
            }
        };

        let selected_download = &downloads[selection];
        println!("\nDownloading: {}", selected_download.name);

        // Download the file
        match builds::download_build_file(config, build_id.clone(), selected_download.name.clone())
            .await
        {
            Ok(data) => match std::fs::write(&selected_download.name, data) {
                Ok(()) => {
                    println!("Downloaded {} successfully!", selected_download.name);
                    return;
                }
                Err(e) => {
                    eprintln!("Failed to write file: {}", e);
                    exit(1);
                }
            },
            Err(e) => {
                eprintln!("Failed to download file: {}", e);
                exit(1);
            }
        }
    }

    let _ = config;
    eprintln!(
        "No build selected. Pass --build-id or run 'gradient build' to record a selected build."
    );
    exit(1);
}
