/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ExecJobId;
use sha2::{Digest, Sha256};

const SHORT_HEX: usize = 8;

pub fn default_branch_name(job: Option<ExecJobId>, touched: &[String]) -> String {
    let mut paths: Vec<&str> = touched.iter().map(String::as_str).collect();
    paths.sort_unstable();
    paths.dedup();
    let paths_hash = hex::encode(Sha256::digest(paths.join("\n").as_bytes()));
    let paths_hash = &paths_hash[..SHORT_HEX];
    match job {
        Some(job) => {
            let job_hex = job.into_inner().simple().to_string();
            format!("gradient/{}/{paths_hash}", &job_hex[..SHORT_HEX])
        }
        None => format!("gradient/patch/{paths_hash}"),
    }
}
