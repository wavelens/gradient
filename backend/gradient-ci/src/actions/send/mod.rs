/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The three action executors plus the forge-reporter construction shared by
//! the forge-status executor and the PR-approval trust probe.

mod forge_status;
mod mail;
mod open_pr;
mod web_request;

pub(crate) use forge_status::execute_forge_status_report;
pub(crate) use mail::execute_send_mail;
pub(crate) use open_pr::execute_open_pr;
pub(crate) use web_request::execute_send_web_request;
pub use forge_status::{reporter_for_project, verify_forge_action};
