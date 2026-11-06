/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::exec::PortRanges;

const PREFERRED_MASTER_PORT: u16 = 29500;

pub fn master_port(allow: &PortRanges, forwarded: &[u16]) -> Option<u16> {
    let free = |port: &u16| !forwarded.contains(port);
    std::iter::once(PREFERRED_MASTER_PORT)
        .filter(|&port| allow.contains(port))
        .chain(allow.ports())
        .find(free)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::exec::PortRange;

    fn ranges(bounds: &[(u16, u16)]) -> PortRanges {
        PortRanges(
            bounds
                .iter()
                .map(|&(first, last)| PortRange { first, last })
                .collect(),
        )
    }

    #[test]
    fn the_master_port_prefers_29500_and_avoids_forwarded_ports() {
        assert_eq!(master_port(&ranges(&[(1024, 65535)]), &[]), Some(29500));
        assert_eq!(master_port(&ranges(&[(1024, 65535)]), &[29500]), Some(1024));
        assert_eq!(
            master_port(&ranges(&[(40000, 40001)]), &[40000]),
            Some(40001)
        );
        assert_eq!(master_port(&ranges(&[(40000, 40000)]), &[40000]), None);
    }

    #[test]
    fn the_master_port_moves_on_to_the_next_range() {
        let allow = ranges(&[(22, 22), (8000, 8001)]);
        assert_eq!(master_port(&allow, &[22]), Some(8000));
        assert_eq!(master_port(&allow, &[22, 8000, 8001]), None);
    }
}
