/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::db::DbContext;

/// CI-layer slice: the full [`DbContext`] plus the shared outbound HTTP client
/// used to deliver project Actions and post forge status checks. Every `ci`
/// function takes `&CiContext`, so `ci` never names the composed `AppState`.
#[derive(Clone, Debug)]
pub struct CiContext {
    pub db: DbContext,
    pub http: reqwest::Client,
}
