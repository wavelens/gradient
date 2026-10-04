/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::JobPhase;

#[test]
fn a_retired_code_keeps_its_historical_name() {
    assert_eq!(JobPhase::name_of(9), "substitute_passthrough");
}

#[test]
fn a_code_never_assigned_is_unknown() {
    assert_eq!(JobPhase::name_of(99), "unknown_99");
}

#[test]
fn every_phase_round_trips_through_its_own_code() {
    let mut codes: Vec<i16> = JobPhase::ALL.iter().map(|p| p.as_i16()).collect();
    for phase in JobPhase::ALL {
        assert_eq!(JobPhase::from_i16(phase.as_i16()), Some(phase));
    }

    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), JobPhase::ALL.len());
}

#[test]
fn every_phase_encodes_for_every_supported_peer() {
    for version in gradient_wire::PROTO_VERSIONS {
        for phase in JobPhase::ALL {
            assert!(
                gradient_wire::codec::to_bytes(&phase, version).is_ok(),
                "{phase:?} at protocol {version}"
            );
        }
    }
}
