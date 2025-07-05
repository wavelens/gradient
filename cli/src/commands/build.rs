/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use clap::{arg, Subcommand};
use connector::*;
use std::process::exit;
use std::process::Command;
use std::path::Path;
use std::collections::HashMap;

pub async fn handle_build(derivation: String, organization: Option<String>, quiet: bool) {
    let organization = organization.unwrap_or_else(|| {
        if !quiet {
            eprintln!("Organization must be set for build command.");
        }
        exit(1);
    });

    let config = get_request_config(load_config()).unwrap_or_else(|_| {
        if !quiet {
            eprintln!("Not configured. Use 'gradient config' to set server and auth token.");
        }
        exit(1);
    });

    // Check if we're in a git repository
    if !Path::new(".git").exists() {
        if !quiet {
            eprintln!("Current directory is not a git repository.");
        }
        exit(1);
    }

    // Check if flake.nix exists
    if !Path::new("flake.nix").exists() {
        if !quiet {
            eprintln!("No flake.nix found in current directory.");
        }
        exit(1);
    }

    // Parse the derivation to extract just the derivation name (after #)
    let derivation_name = if derivation.contains('#') {
        derivation.split('#').last().unwrap_or(&derivation)
    } else {
        &derivation
    };

    if !quiet {
        println!("Building derivation {} in organization {}", derivation_name, organization);
    }

    // Check if git is available
    if Command::new("git").arg("--version").output().is_err() {
        if !quiet {
            eprintln!("Error: git command not found.");
            eprintln!("Git is required to collect files for upload.");
            eprintln!("Please install git and make sure it's available in PATH.");
        }
        exit(1);
    }

    // Get list of files tracked by git
    let git_files = Command::new("git")
        .args(&["ls-files"])
        .output()
        .map_err(|e| {
            if !quiet {
                eprintln!("Failed to execute git command: {}", e);
                eprintln!("Make sure git is installed and available in PATH.");
            }
            exit(1);
        })
        .unwrap();

    if !git_files.status.success() {
        if !quiet {
            eprintln!("Failed to get git files. Make sure you're in a git repository.");
        }
        exit(1);
    }

    let file_list = String::from_utf8(git_files.stdout)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect::<Vec<String>>();

    if !quiet {
        println!("Collecting {} files for upload...", file_list.len());
    }

    // Read files into memory
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    
    for file_path in file_list {
        if let Ok(content) = std::fs::read(&file_path) {
            files.insert(file_path, content);
        } else {
            if !quiet {
                eprintln!("Warning: Could not read file {}", file_path);
            }
        }
    }

    if !quiet {
        println!("Uploading {} files to server...", files.len());
    }

    // Upload files and start build
    let build_result = builds::post_direct_build(
        config.clone(),
        organization,
        derivation_name.to_string(),
        files,
    ).await;

    let evaluation_id = match build_result {
        Ok(response) => {
            if response.error {
                if !quiet {
                    eprintln!("Failed to start build: {}", response.message);
                }
                exit(1);
            }
            if !quiet {
                println!("Build started successfully: {}", response.message);
            }
            
            // Extract evaluation ID from response message
            // Format: "Direct build started with evaluation ID: <uuid>"
            if let Some(eval_id) = response.message.strip_prefix("Direct build started with evaluation ID: ") {
                eval_id.to_string()
            } else {
                if !quiet {
                    eprintln!("Warning: Could not extract evaluation ID from response");
                }
                return;
            }
        }
        Err(e) => {
            if !quiet {
                eprintln!("Failed to start build: {}", e);
            }
            exit(1);
        }
    };

    // Wait for the evaluation to create builds and complete
    if !quiet {
        println!("Waiting for evaluation to create builds...");
    }
    
    let mut build_ids = Vec::new();
    let mut max_retries = 30; // Wait up to 5 minutes (30 * 10 seconds)
    
    // First, wait for builds to be created
    loop {
        match builds::get_evaluation_builds(config.clone(), evaluation_id.clone()).await {
            Ok(response) => {
                if !response.error && !response.message.is_empty() {
                    build_ids = response.message.iter().map(|b| b.id.clone()).collect();
                    break;
                }
            }
            Err(_) => {}
        }
        
        max_retries -= 1;
        if max_retries <= 0 {
            if !quiet {
                eprintln!("Timeout waiting for builds to be created");
            }
            return;
        }
        
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
    
    if !quiet {
        println!("Builds created. Streaming build logs...");
    }
    
    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    // Stream logs for each build
    for build_id in &build_ids {
        if !quiet {
            println!("\n=== Build {} ===", build_id);
        }
        
        // Stream the build log
        if let Err(e) = builds::post_build(config.clone(), build_id.clone()).await {
            if !quiet {
                eprintln!("Failed to stream logs for build {}: {}", build_id, e);
            }
        }
        
        if !quiet {
            println!("\n=== Build {} completed ===", build_id);
        }
    }
    
    // Get final build status
    match builds::get_evaluation_builds(config, evaluation_id).await {
        Ok(response) => {
            if response.error {
                if !quiet {
                    eprintln!("Warning: Could not fetch build IDs: {:?}", response.message);
                }
            } else if response.message.is_empty() {
                if !quiet {
                    println!("No builds created yet. The evaluation may still be processing.");
                }
            } else {
                if quiet {
                    // In quiet mode, only output the build IDs
                    for build in &response.message {
                        println!("{}", build.id);
                    }
                } else {
                    println!("\nBuild IDs created:");
                    for build in &response.message {
                        println!("  {}", build.id);
                    }
                    
                    if response.message.len() == 1 {
                        println!("\nYou can download files with:");
                        println!("  gradient download -b {}", response.message[0].id);
                    } else {
                        println!("\nYou can download files from any build with:");
                        println!("  gradient download -b <build-id>");
                    }
                }
                
                // Set the first build as selected-build for convenience
                if let Some(first_build) = response.message.first() {
                    use crate::config::{set_get_value, ConfigKey};
                    set_get_value(ConfigKey::SelectedBuild, Some(first_build.id.clone()), true);
                    
                    if !quiet {
                        println!("\nSelected build set to: {}", first_build.id);
                        println!("You can now use 'gradient download' without specifying build ID");
                    }
                }
            }
        }
        Err(e) => {
            if !quiet {
                eprintln!("Warning: Could not fetch build IDs: {}", e);
            }
        }
    }

    if !quiet {
        println!("\nBuild submitted successfully. Check the server for build progress.");
    }
}
