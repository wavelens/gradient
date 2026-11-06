/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::str::FromStr;

use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use crate::codec::{DecodeError, EncodeError, Proto};

pub const PORT_RANGES_SINCE: u16 = 58;

#[derive(Proto, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
pub struct PortRange {
    pub first: u16,
    pub last: u16,
}

impl PortRange {
    pub fn contains(&self, port: u16) -> bool {
        (self.first..=self.last).contains(&port)
    }
}

impl FromStr for PortRange {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let port = |p: &str| {
            p.trim()
                .parse::<u16>()
                .map_err(|e| format!("port {p:?}: {e}"))
        };
        let (first, last) = match s.split_once('-') {
            Some((first, last)) => (port(first)?, port(last)?),
            None => (port(s)?, port(s)?),
        };
        if first > last {
            return Err(format!("port range {first}-{last} is reversed"));
        }

        Ok(Self { first, last })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema)]
#[serde(transparent)]
pub struct PortRanges(pub Vec<PortRange>);

impl PortRanges {
    pub fn unprivileged() -> Self {
        Self(vec![PortRange {
            first: 1024,
            last: u16::MAX,
        }])
    }

    pub fn contains(&self, port: u16) -> bool {
        self.0.iter().any(|range| range.contains(port))
    }

    pub fn ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.0.iter().flat_map(|range| range.first..=range.last)
    }

    fn widest(&self) -> (u16, u16) {
        self.0
            .iter()
            .max_by_key(|range| range.last - range.first)
            .map_or((1, 0), |range| (range.first, range.last))
    }
}

impl FromStr for PortRanges {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        s.split(',')
            .filter(|entry| !entry.trim().is_empty())
            .map(PortRange::from_str)
            .collect::<Result<_, _>>()
            .map(Self)
    }
}

impl Proto for PortRanges {
    const NEWEST: u16 = PORT_RANGES_SINCE;

    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        if version >= PORT_RANGES_SINCE {
            self.0.encode(version, out)
        } else {
            self.widest().encode(version, out)
        }
    }

    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError> {
        if version >= PORT_RANGES_SINCE {
            return Vec::decode(input, version).map(Self);
        }

        let (first, last) = <(u16, u16)>::decode(input, version)?;
        Ok(Self(
            (first <= last)
                .then_some(PortRange { first, last })
                .into_iter()
                .collect(),
        ))
    }

    fn describe(version: u16, out: &mut String) {
        if version >= PORT_RANGES_SINCE {
            Vec::<PortRange>::describe(version, out);
        } else {
            <(u16, u16)>::describe(version, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{from_bytes, to_bytes};

    fn range(first: u16, last: u16) -> PortRange {
        PortRange { first, last }
    }

    #[test]
    fn single_ports_and_ranges_parse_from_one_list() {
        assert_eq!(
            "22,8000-8100,6006".parse::<PortRanges>(),
            Ok(PortRanges(vec![
                range(22, 22),
                range(8000, 8100),
                range(6006, 6006)
            ]))
        );
        assert_eq!("".parse::<PortRanges>(), Ok(PortRanges::default()));
        assert!("8100-8000".parse::<PortRanges>().is_err());
        assert!("22,ssh".parse::<PortRanges>().is_err());
    }

    #[test]
    fn a_port_fits_when_any_range_holds_it() {
        let ranges = PortRanges(vec![range(22, 22), range(8000, 8100)]);
        assert!(ranges.contains(22));
        assert!(ranges.contains(8050));
        assert!(!ranges.contains(23));
        assert!(!PortRanges::default().contains(22));
    }

    #[test]
    fn every_range_crosses_the_wire_since_port_ranges() {
        let ranges = PortRanges(vec![range(22, 22), range(8000, 8100)]);
        let bytes = to_bytes(&ranges, PORT_RANGES_SINCE).expect("encodes");
        assert_eq!(from_bytes(bytes, PORT_RANGES_SINCE), Ok(ranges));
    }

    #[test]
    fn an_older_peer_sees_only_the_widest_range() {
        let older = PORT_RANGES_SINCE - 1;
        let ranges = PortRanges(vec![range(22, 22), range(8000, 8100), range(6006, 6006)]);
        let bytes = to_bytes(&ranges, older).expect("encodes");
        assert_eq!(
            from_bytes(bytes, older),
            Ok(PortRanges(vec![range(8000, 8100)]))
        );

        let none = to_bytes(&PortRanges::default(), older).expect("encodes");
        assert_eq!(from_bytes(none, older), Ok(PortRanges::default()));
    }
}
