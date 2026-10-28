/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const PREFERRED_MASTER_PORT: u16 = 29500;

pub fn master_port(allow: (u16, u16), forwarded: &[u16]) -> Option<u16> {
    let (lowest, highest) = allow;
    let free = |port: &u16| !forwarded.contains(port);
    std::iter::once(PREFERRED_MASTER_PORT)
        .filter(|port| (lowest..=highest).contains(port))
        .chain(lowest..=highest)
        .find(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_master_port_prefers_29500_and_avoids_forwarded_ports() {
        assert_eq!(master_port((1024, 65535), &[]), Some(29500));
        assert_eq!(master_port((1024, 65535), &[29500]), Some(1024));
        assert_eq!(master_port((40000, 40001), &[40000]), Some(40001));
        assert_eq!(master_port((40000, 40000), &[40000]), None);
    }
}
