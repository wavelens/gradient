/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::{BufMut, Bytes, BytesMut};

use super::{DecodeError, EncodeError, Proto, get_u8, get_varint, max, put_varint, take};

macro_rules! unsigned {
    ($($ty:ty),*) => {$(
        impl Proto for $ty {
            fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
                put_varint(out, u64::from(*self));
                Ok(())
            }

            fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
                <$ty>::try_from(get_varint(input)?)
                    .map_err(|_| DecodeError::OutOfRange(stringify!($ty)))
            }

            fn describe(_: u16, out: &mut String) {
                out.push_str(stringify!($ty));
            }
        }
    )*};
}

unsigned!(u16, u32, u64);

impl Proto for u8 {
    fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        out.put_u8(*self);
        Ok(())
    }

    fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
        get_u8(input)
    }

    fn describe(_: u16, out: &mut String) {
        out.push_str("u8");
    }
}

impl Proto for i64 {
    fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        put_varint(out, ((*self << 1) ^ (*self >> 63)) as u64);
        Ok(())
    }

    fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
        let raw = get_varint(input)?;
        Ok((raw >> 1) as i64 ^ -((raw & 1) as i64))
    }

    fn describe(_: u16, out: &mut String) {
        out.push_str("i64");
    }
}

impl Proto for f32 {
    fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        out.put_f32_le(*self);
        Ok(())
    }

    fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
        let raw = take(input, 4)?;
        Ok(f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    fn describe(_: u16, out: &mut String) {
        out.push_str("f32");
    }
}

impl Proto for bool {
    fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        out.put_u8(u8::from(*self));
        Ok(())
    }

    fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
        match get_u8(input)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DecodeError::InvalidBool(other)),
        }
    }

    fn describe(_: u16, out: &mut String) {
        out.push_str("bool");
    }
}

impl Proto for String {
    fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        put_varint(out, self.len() as u64);
        out.put_slice(self.as_bytes());
        Ok(())
    }

    fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
        let len = get_varint(input)?;
        String::from_utf8(take(input, len)?.to_vec()).map_err(|_| DecodeError::InvalidUtf8)
    }

    fn describe(_: u16, out: &mut String) {
        out.push_str("str");
    }
}

impl Proto for Bytes {
    fn encode(&self, _: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        put_varint(out, self.len() as u64);
        out.put_slice(self);
        Ok(())
    }

    fn decode(input: &mut Bytes, _: u16) -> Result<Self, DecodeError> {
        let len = get_varint(input)?;
        take(input, len)
    }

    fn describe(_: u16, out: &mut String) {
        out.push_str("bytes");
    }
}

impl<T: Proto> Proto for Option<T> {
    const OLDEST: u16 = T::OLDEST;
    const NEWEST: u16 = T::NEWEST;

    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        match self {
            None => out.put_u8(0),
            Some(value) => {
                out.put_u8(1);
                value.encode(version, out)?;
            }
        }

        Ok(())
    }

    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError> {
        match get_u8(input)? {
            0 => Ok(None),
            1 => T::decode(input, version).map(Some),
            other => Err(DecodeError::InvalidBool(other)),
        }
    }

    fn describe(version: u16, out: &mut String) {
        out.push_str("opt<");
        T::describe(version, out);
        out.push('>');
    }
}

impl<T: Proto> Proto for Vec<T> {
    const OLDEST: u16 = T::OLDEST;
    const NEWEST: u16 = T::NEWEST;

    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        put_varint(out, self.len() as u64);
        self.iter().try_for_each(|item| item.encode(version, out))
    }

    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError> {
        let count = get_varint(input)?;
        if count > input.len() as u64 {
            return Err(DecodeError::Truncated);
        }

        (0..count).map(|_| T::decode(input, version)).collect()
    }

    fn describe(version: u16, out: &mut String) {
        let mut item = String::new();
        T::describe(version, &mut item);
        if item == "u8" {
            out.push_str("bytes");
        } else {
            out.push_str("seq<");
            out.push_str(&item);
            out.push('>');
        }
    }
}

impl<T: Proto> Proto for Box<T> {
    const OLDEST: u16 = T::OLDEST;
    const NEWEST: u16 = T::NEWEST;

    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        (**self).encode(version, out)
    }

    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError> {
        T::decode(input, version).map(Box::new)
    }

    fn describe(version: u16, out: &mut String) {
        T::describe(version, out);
    }
}

impl<A: Proto, B: Proto> Proto for (A, B) {
    const OLDEST: u16 = max(A::OLDEST, B::OLDEST);
    const NEWEST: u16 = max(A::NEWEST, B::NEWEST);

    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        self.0.encode(version, out)?;
        self.1.encode(version, out)
    }

    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError> {
        Ok((A::decode(input, version)?, B::decode(input, version)?))
    }

    fn describe(version: u16, out: &mut String) {
        out.push('(');
        A::describe(version, out);
        out.push(',');
        B::describe(version, out);
        out.push(')');
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use crate::codec::{DecodeError, from_bytes, to_bytes};

    fn round_trip<T: crate::codec::Proto + PartialEq + std::fmt::Debug>(value: T) {
        let bytes = to_bytes(&value, 27).expect("encodes");
        assert_eq!(from_bytes::<T>(bytes, 27), Ok(value));
    }

    #[test]
    fn every_primitive_round_trips() {
        round_trip(u64::MAX);
        round_trip(300u16);
        round_trip(-5i64);
        round_trip(i64::MIN);
        round_trip(1.5f32);
        round_trip(true);
        round_trip(String::from("grüße"));
        round_trip(Bytes::from_static(b"nar"));
        round_trip(Some(vec![(String::from("a"), String::from("b"))]));
        round_trip(Box::new(None::<u32>));
    }

    #[test]
    fn a_decoded_byte_field_points_into_the_frame() {
        let frame = to_bytes(&Bytes::from(vec![7u8; 64]), 27).expect("encodes");
        let start = frame.as_ptr() as usize;
        let decoded: Bytes = from_bytes(frame.clone(), 27).expect("decodes");
        assert!((start..start + frame.len()).contains(&(decoded.as_ptr() as usize)));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        assert_eq!(
            from_bytes::<bool>(Bytes::from_static(&[1, 0]), 27),
            Err(DecodeError::TrailingBytes(1))
        );
    }

    #[test]
    fn a_bool_other_than_zero_or_one_is_rejected() {
        assert_eq!(
            from_bytes::<bool>(Bytes::from_static(&[2]), 27),
            Err(DecodeError::InvalidBool(2))
        );
    }

    #[test]
    fn a_count_beyond_the_frame_is_truncated_not_allocated() {
        let mut frame = bytes::BytesMut::new();
        crate::codec::put_varint(&mut frame, u64::from(u32::MAX));
        assert_eq!(
            from_bytes::<Vec<u64>>(frame.freeze(), 27),
            Err(DecodeError::Truncated)
        );
    }

    #[test]
    fn a_value_too_large_for_its_type_is_rejected() {
        let frame = to_bytes(&70_000u32, 27).expect("encodes");
        assert_eq!(
            from_bytes::<u16>(frame, 27),
            Err(DecodeError::OutOfRange("u16"))
        );
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        assert_eq!(
            from_bytes::<String>(Bytes::from_static(&[1, 0xff]), 27),
            Err(DecodeError::InvalidUtf8)
        );
    }
}
