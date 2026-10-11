/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::MockExecResult;

pub(super) fn exec(rows_affected: u64) -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected,
    }
}
