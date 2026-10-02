/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_worker_client::connection::ProtoConnection;

pub struct Connected {
    pub(crate) conn: ProtoConnection,
}

pub struct Disconnected;
