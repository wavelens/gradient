/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod primitives;

pub use bytes;
use bytes::{Buf, BufMut, Bytes, BytesMut};
pub use gradient_wire_derive::Proto;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("{variant} exists since protocol {since}, the peer speaks {version}")]
    NewerThanPeer {
        variant: &'static str,
        since: u16,
        version: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("frame ended early")]
    Truncated,
    #[error("{0} bytes left after the message")]
    TrailingBytes(usize),
    #[error("varint longer than 64 bits")]
    VarintOverflow,
    #[error("value out of range for {0}")]
    OutOfRange(&'static str),
    #[error("invalid bool byte {0}")]
    InvalidBool(u8),
    #[error("invalid UTF-8")]
    InvalidUtf8,
    #[error("unknown variant {tag} of {ty} at protocol {version}")]
    UnknownVariant {
        ty: &'static str,
        tag: u64,
        version: u16,
    },
}

pub trait Proto: Sized {
    const OLDEST: u16 = 0;
    const NEWEST: u16 = 0;

    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError>;
    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError>;
    fn describe(version: u16, out: &mut String);
}

pub fn to_bytes<T: Proto>(value: &T, version: u16) -> Result<Bytes, EncodeError> {
    let mut out = BytesMut::new();
    value.encode(version, &mut out)?;
    Ok(out.freeze())
}

pub fn from_bytes<T: Proto>(mut input: Bytes, version: u16) -> Result<T, DecodeError> {
    let value = T::decode(&mut input, version)?;
    if !input.is_empty() {
        return Err(DecodeError::TrailingBytes(input.len()));
    }

    Ok(value)
}

pub const fn max(a: u16, b: u16) -> u16 {
    if a > b { a } else { b }
}

pub const fn max_of(values: &[u16]) -> u16 {
    let mut highest = 0;
    let mut i = 0;
    while i < values.len() {
        highest = max(highest, values[i]);
        i += 1;
    }

    highest
}

pub const fn floor(needed: u16, present_since: u16) -> u16 {
    if needed > present_since { needed } else { 0 }
}

pub fn push_separator(out: &mut String, first: &mut bool) {
    if !*first {
        out.push(',');
    }

    *first = false;
}

pub fn put_varint(out: &mut BytesMut, mut value: u64) {
    while value >= 0x80 {
        out.put_u8((value as u8) | 0x80);
        value >>= 7;
    }

    out.put_u8(value as u8);
}

pub fn get_u8(input: &mut Bytes) -> Result<u8, DecodeError> {
    if !input.has_remaining() {
        return Err(DecodeError::Truncated);
    }

    Ok(input.get_u8())
}

pub fn get_varint(input: &mut Bytes) -> Result<u64, DecodeError> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = get_u8(input)?;
        if shift == 63 && byte > 1 {
            return Err(DecodeError::VarintOverflow);
        }

        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }

    Err(DecodeError::VarintOverflow)
}

pub fn take(input: &mut Bytes, len: u64) -> Result<Bytes, DecodeError> {
    let len = usize::try_from(len).map_err(|_| DecodeError::Truncated)?;
    if input.len() < len {
        return Err(DecodeError::Truncated);
    }

    Ok(input.split_to(len))
}
