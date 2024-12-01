use chrono::{DateTime, NaiveDateTime};
use std::ops::RangeInclusive;
use std::sync::LazyLock;

pub const PORT_RANGE: RangeInclusive<usize> = 1..=65535;

pub static NULL_TIME: LazyLock<NaiveDateTime> =
    LazyLock::new(|| DateTime::from_timestamp(0, 0).unwrap().naive_utc());
