/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::JobPhase;

#[test]
fn a_live_code_names_its_phase() {
    assert_eq!(JobPhase::name_of(JobPhase::Build.as_i16()), "build");
}

#[test]
fn a_retired_code_keeps_its_historical_name() {
    assert_eq!(JobPhase::name_of(9), "substitute_relay");
}

#[test]
fn a_code_never_assigned_is_unknown() {
    assert_eq!(JobPhase::name_of(99), "unknown_99");
}
