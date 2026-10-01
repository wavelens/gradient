/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::DerivationId;
use sea_orm::{MockExecResult, Value};
use std::collections::BTreeMap;

pub(super) fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn drv(id: DerivationId) -> BTreeMap<String, Value> {
    BTreeMap::from([("derivation".to_owned(), Value::from(id.into_inner()))])
}

pub(super) fn transition_row(id: DerivationId, from: i32, to: i32) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("derivation".to_owned(), Value::from(id.into_inner())),
        ("from_status".to_owned(), Value::from(from)),
        ("to_status".to_owned(), Value::from(to)),
    ])
}

pub(super) fn exec(rows_affected: u64) -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected,
    }
}
