/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_db::DbContext;
use gradient_git_host::GitHostRegistry;
use gradient_notify::EmailSender;

/// `ci` must never name the composed `AppState`, so every `ci` function is taking `&CiContext`.
#[derive(Clone, Debug)]
pub struct CiContext {
    pub db: DbContext,
    pub http: reqwest::Client,
    pub git_host: GitHostRegistry,
    pub email: Arc<dyn EmailSender>,
}
