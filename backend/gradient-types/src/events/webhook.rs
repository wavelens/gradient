/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{firehose, names};
use crate::ids::WebhookId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ping {
    pub webhook: WebhookId,
}
firehose!(Ping, names::WEBHOOK_PING);
