/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;

use gradient_wire::types::MAX_PHASE_PAYLOAD;

const MAX_PHASE_NAME_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseRefusal {
    Name,
    TooLarge(usize),
}

impl fmt::Display for PhaseRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PhaseRefusal::Name => write!(
                f,
                "phase name must be 1 to {MAX_PHASE_NAME_BYTES} printable characters"
            ),
            PhaseRefusal::TooLarge(bytes) => write!(
                f,
                "phase payload is {bytes} bytes, the limit is {MAX_PHASE_PAYLOAD}"
            ),
        }
    }
}

impl std::error::Error for PhaseRefusal {}

pub fn check_phase(name: &str, payload: &[u8]) -> Result<(), PhaseRefusal> {
    if name.is_empty() || name.len() > MAX_PHASE_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(PhaseRefusal::Name);
    }
    if payload.len() > MAX_PHASE_PAYLOAD {
        return Err(PhaseRefusal::TooLarge(payload.len()));
    }
    Ok(())
}

pub fn phase_marker(name: &str) -> String {
    format!("::gradient-phase:: {name}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_payload_above_the_limit_is_refused_with_its_size() {
        assert!(check_phase("epoch-3", &vec![0; MAX_PHASE_PAYLOAD]).is_ok());
        let refused = check_phase("epoch-3", &vec![0; MAX_PHASE_PAYLOAD + 1]).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "phase payload is 65537 bytes, the limit is 65536"
        );
        assert!(matches!(check_phase("", b""), Err(PhaseRefusal::Name)));
        assert!(matches!(check_phase("a\nb", b""), Err(PhaseRefusal::Name)));
    }
}
