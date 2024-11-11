use chrono::{DateTime, NaiveDateTime};
use std::sync::LazyLock;
use std::ops::RangeInclusive;


pub const PORT_RANGE: RangeInclusive<usize> = 1..=65535;

pub const NULL_TIME: LazyLock<NaiveDateTime> = LazyLock::new(|| {
    DateTime::from_timestamp(0, 0).unwrap().naive_utc()
});
