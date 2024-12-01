/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use super::types::*;
use std::sync::Arc;

pub async fn check_project_updates(state: Arc<ServerState>, project: &MProject) -> bool {
    println!("Checking for updates on project: {}", project.id);
    // TODO: dummy
    true
}
