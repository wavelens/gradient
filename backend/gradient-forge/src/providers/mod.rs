/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One [`ForgeProvider`](crate::ForgeProvider) impl per forge. `gitea`
//! serves both Gitea and Forgejo (identical APIs).

pub mod gitea;
pub mod github;
pub mod gitlab;
