/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_db::DbContext;
use gradient_forge::ForgeRegistry;
use gradient_notify::EmailSender;

/// CI-layer slice: the full [`DbContext`], the shared outbound HTTP client used
/// to deliver project Actions and post forge status checks, the resolved
/// [`ForgeRegistry`], and the outbound email sender for `send_mail` actions.
/// Every `ci` function takes `&CiContext`, so `ci` never names the composed
/// `AppState`.
#[derive(Clone, Debug)]
pub struct CiContext {
    pub db: DbContext,
    pub http: reqwest::Client,
    pub forge: ForgeRegistry,
    pub email: Arc<dyn EmailSender>,
}
