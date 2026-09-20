//! Canonical scalar encodings for schema-declared property types (ADR 0012).
//!
//! Physical storage stays UTF-8 in [`crate::ObjectRecord::props`]. These
//! helpers parse and format the accepted subset. Writes may normalize a
//! timestamp with an offset; other types require the canonical form.

use std::cmp::Ordering;
use std::fmt;

/// Clerk-declared logical type of one property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyType {
    String,
    Boolean,
    Integer,
    Timestamp,
    Decimal { scale: u8 },
}

/// Typed view of a canonical property value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyValue {
    String(String),
    Boolean(bool),
    Integer(i64),
    Timestamp(String),
    Decimal(String),
}

impl PropertyType {
    /// Decode a descriptor token (`boolean`, `integer`, `timestamp`,
    /// `string`, or `decimal:<scale>`).
    pub fn from_token(token: &str) -> Result<Self, String> {
        match token {
            "string" => Ok(Self::String),
            "boolean" => Ok(Self::Boolean),
            "integer" => Ok(Self::Integer),
            "timestamp" => Ok(Self::Timestamp),
            other => {
                let Some(scale) = other.strip_prefix("decimal:") else {
                    return Err(format!("unknown property type {other}"));
                };
                if scale.len() > 1 && scale.starts_with('0') {
                    return Err(format!("decimal scale {scale} is not canonical"));
                }
                let scale: u8 = scale
                    .parse()
                    .map_err(|_| format!("decimal scale {scale} is not an integer"))?;
                if scale > 18 {
                    return Err(format!("decimal scale {scale} exceeds 18"));
                }
                Ok(Self::Decimal { scale })
            }
        }
    }

    /// Encode as a descriptor token.
    pub fn token(self) -> String {
        match self {
            Self::String => "string".into(),
            Self::Boolean => "boolean".into(),
            Self::Integer => "integer".into(),
            Self::Timestamp => "timestamp".into(),
            Self::Decimal { scale } => format!("decimal:{scale}"),
        }
    }

    /// Parse a stored canonical form. Offsets and other aliases fail closed.
    pub fn parse_canonical(self, raw: &str) -> Result<PropertyValue, String> {
        match self {
            Self::String => Ok(PropertyValue::String(raw.to_string())),
            Self::Boolean => parse_boolean(raw),
            Self::Integer => parse_integer(raw),
            Self::Timestamp => {
                if !is_canonical_timestamp(raw) {
                    return Err(format!("timestamp {raw} is not canonical UTC millis"));
                }
                parse_timestamp_millis(raw)?;
                Ok(PropertyValue::Timestamp(raw.to_string()))
            }
            Self::Decimal { scale } => parse_decimal(raw, scale),
        }
    }

    /// Inclusive ADR 0012 order. `None` bound is unbounded that side.
    /// Boolean has no range. A stored value that is not canonical is not a
    /// match rather than a request error.
    pub fn in_range(
        self,
        stored: &str,
        min: Option<&str>,
        max: Option<&str>,
    ) -> Result<bool, String> {
        if matches!(self, Self::Boolean) {
            return Err("range is not defined for boolean".into());
        }
        let Ok(value) = self.parse_canonical(stored) else {
            return Ok(false);
        };
        if let Some(min) = min {
            if self.cmp_values(&value, &self.parse_canonical(min)?)? == Ordering::Less {
                return Ok(false);
            }
        }
        if let Some(max) = max {
            if self.cmp_values(&value, &self.parse_canonical(max)?)? == Ordering::Greater {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn cmp_values(self, left: &PropertyValue, right: &PropertyValue) -> Result<Ordering, String> {
        match (self, left, right) {
            (Self::String, PropertyValue::String(a), PropertyValue::String(b))
            | (Self::Timestamp, PropertyValue::Timestamp(a), PropertyValue::Timestamp(b)) => {
                Ok(a.cmp(b))
            }
            (Self::Integer, PropertyValue::Integer(a), PropertyValue::Integer(b)) => Ok(a.cmp(b)),
            (Self::Boolean, PropertyValue::Boolean(a), PropertyValue::Boolean(b)) => Ok(a.cmp(b)),
            (Self::Decimal { .. }, PropertyValue::Decimal(a), PropertyValue::Decimal(b)) => {
                Ok(decimal_units(a)?.cmp(&decimal_units(b)?))
            }
            _ => Err("typed comparison mixed property types".into()),
        }
    }

    /// Form stored on write. Timestamps with a timezone offset normalize to
    /// `Z`; every other type must already be canonical.
    pub fn canonicalize_write(self, raw: &str) -> Result<String, String> {
        match self {
            Self::Timestamp => {
                let millis = parse_timestamp_millis(raw)?;
                format_utc_millis(millis)
            }
            other => Ok(other.parse_canonical(raw)?.canonical()),
        }
    }
}

impl PropertyValue {
    pub fn canonical(&self) -> String {
        match self {
            Self::String(value) | Self::Timestamp(value) | Self::Decimal(value) => value.clone(),
            Self::Boolean(true) => "true".into(),
            Self::Boolean(false) => "false".into(),
            Self::Integer(value) => value.to_string(),
        }
    }
}

impl fmt::Display for PropertyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.token())
    }
}

fn parse_boolean(raw: &str) -> Result<PropertyValue, String> {
    match raw {
        "true" => Ok(PropertyValue::Boolean(true)),
        "false" => Ok(PropertyValue::Boolean(false)),
        _ => Err(format!("invalid boolean {raw}")),
    }
}

fn parse_integer(raw: &str) -> Result<PropertyValue, String> {
    if !is_canonical_integer(raw) {
        return Err(format!("integer {raw} is not canonical"));
    }
    raw.parse::<i64>()
        .map(PropertyValue::Integer)
        .map_err(|_| format!("integer {raw} overflows i64"))
}

fn is_canonical_integer(raw: &str) -> bool {
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if digits.starts_with('0') {
        return digits == "0" && !raw.starts_with('-');
    }
    true
}

fn decimal_units(raw: &str) -> Result<i128, String> {
    let neg = raw.starts_with('-');
    let body = raw.strip_prefix('-').unwrap_or(raw);
    let compact: String = body.chars().filter(|ch| *ch != '.').collect();
    let n: i128 = compact
        .parse()
        .map_err(|_| format!("decimal {raw} overflows comparison"))?;
    Ok(if neg { -n } else { n })
}

fn parse_decimal(raw: &str, scale: u8) -> Result<PropertyValue, String> {
    if !is_canonical_decimal(raw, scale) {
        return Err(format!("decimal {raw} is not canonical for scale {scale}"));
    }
    let digits = raw.bytes().filter(|b| b.is_ascii_digit()).count();
    if !(1..=38).contains(&digits) {
        return Err(format!("decimal {raw} has {digits} digits; want 1..=38"));
    }
    Ok(PropertyValue::Decimal(raw.to_string()))
}

fn is_canonical_decimal(raw: &str, scale: u8) -> bool {
    let body = match raw.strip_prefix('-') {
        Some(rest) => {
            let zero_frac = rest
                .strip_prefix("0.")
                .is_some_and(|frac| frac.bytes().all(|b| b == b'0'));
            if rest == "0" || zero_frac {
                return false;
            }
            rest
        }
        None => raw,
    };
    if scale == 0 {
        return is_canonical_integer(body) && !body.starts_with('-');
    }
    let Some((int, frac)) = body.split_once('.') else {
        return false;
    };
    if frac.len() != scale as usize || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if int.starts_with('0') {
        return int == "0";
    }
    !int.is_empty() && int.bytes().all(|b| b.is_ascii_digit())
}

fn is_canonical_timestamp(raw: &str) -> bool {
    raw.len() == 24
        && raw.as_bytes()[10] == b'T'
        && raw.as_bytes()[19] == b'.'
        && raw.ends_with('Z')
        && raw.as_bytes()[4] == b'-'
        && raw.as_bytes()[7] == b'-'
        && raw.as_bytes()[13] == b':'
        && raw.as_bytes()[16] == b':'
}

fn parse_timestamp_millis(raw: &str) -> Result<i64, String> {
    let (datetime, offset_min) = split_timezone(raw)?;
    let (date, time) = datetime
        .split_once('T')
        .ok_or_else(|| format!("timestamp {raw} must contain T"))?;
    let (year, month, day) = parse_date(date)?;
    let (hour, minute, second, milli) = parse_time(time)?;
    let unix_days = unix_days(year, month, day)?;
    let local = unix_days
        .checked_mul(86_400_000)
        .and_then(|d| d.checked_add(hour as i64 * 3_600_000))
        .and_then(|d| d.checked_add(minute as i64 * 60_000))
        .and_then(|d| d.checked_add(second as i64 * 1_000))
        .and_then(|d| d.checked_add(milli as i64))
        .ok_or_else(|| format!("timestamp {raw} overflows"))?;
    local
        .checked_sub(offset_min as i64 * 60_000)
        .ok_or_else(|| format!("timestamp {raw} overflows"))
}

fn split_timezone(raw: &str) -> Result<(&str, i32), String> {
    if let Some(body) = raw.strip_suffix('Z') {
        return Ok((body, 0));
    }
    let bytes = raw.as_bytes();
    let Some(idx) = bytes
        .iter()
        .rposition(|b| *b == b'+' || *b == b'-')
        .filter(|idx| *idx > 10)
    else {
        return Err(format!("timestamp {raw} is missing a timezone"));
    };
    let sign = if bytes[idx] == b'+' { 1 } else { -1 };
    let offset = &raw[idx + 1..];
    let parts: Vec<&str> = offset.split(':').collect();
    if parts.len() != 2 || parts[0].len() != 2 || parts[1].len() != 2 {
        return Err(format!("timestamp offset {offset} is invalid"));
    }
    let hour: i32 = parse_two_digits(parts[0], "offset hour")?;
    let minute: i32 = parse_two_digits(parts[1], "offset minute")?;
    if hour > 23 || minute > 59 {
        return Err(format!("timestamp offset {offset} is out of range"));
    }
    Ok((&raw[..idx], sign * (hour * 60 + minute)))
}

fn parse_date(date: &str) -> Result<(i32, u32, u32), String> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return Err(format!("date {date} is invalid"));
    }
    if !parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())) {
        return Err(format!("date {date} is invalid"));
    }
    let year: i32 = parts[0]
        .parse()
        .map_err(|_| format!("year {} is invalid", parts[0]))?;
    let month: u32 = parse_two_digits(parts[1], "month")? as u32;
    let day: u32 = parse_two_digits(parts[2], "day")? as u32;
    Ok((year, month, day))
}

fn parse_time(time: &str) -> Result<(u32, u32, u32, u32), String> {
    let (hms, frac) = match time.split_once('.') {
        Some((hms, frac)) => (hms, Some(frac)),
        None => (time, None),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.len() != 2) {
        return Err(format!("time {time} is invalid"));
    }
    let hour: u32 = parse_two_digits(parts[0], "hour")? as u32;
    let minute: u32 = parse_two_digits(parts[1], "minute")? as u32;
    let second: u32 = parse_two_digits(parts[2], "second")? as u32;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("time {time} is out of range"));
    }
    let milli = match frac {
        None => 0,
        Some(frac) => {
            if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("timestamp fraction {frac} is invalid"));
            }
            let padded = format!("{frac:0<3}");
            padded.parse().expect("digits")
        }
    };
    Ok((hour, minute, second, milli))
}

fn parse_two_digits(raw: &str, label: &str) -> Result<i32, String> {
    if raw.len() != 2 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{label} {raw} is invalid"));
    }
    raw.parse().map_err(|_| format!("{label} {raw} is invalid"))
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_days(year: i32) -> [u32; 12] {
    [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ]
}

fn unix_days(year: i32, month: u32, day: u32) -> Result<i64, String> {
    if !(1..=9999).contains(&year) {
        return Err(format!("year {year} is out of range"));
    }
    if !(1..=12).contains(&month) {
        return Err(format!("month {month} is out of range"));
    }
    let dim = month_days(year)[month as usize - 1];
    if day == 0 || day > dim {
        return Err(format!("day {day} is out of range"));
    }
    let mut days: i64 = 0;
    if year >= 1970 {
        for y in 1970..year {
            days += if is_leap(y) { 366 } else { 365 };
        }
    } else {
        for y in year..1970 {
            days -= if is_leap(y) { 366 } else { 365 };
        }
    }
    for m in 1..month {
        days += month_days(year)[m as usize - 1] as i64;
    }
    Ok(days + i64::from(day) - 1)
}

fn format_utc_millis(millis: i64) -> Result<String, String> {
    let days = millis.div_euclid(86_400_000);
    let rem = millis.rem_euclid(86_400_000);
    let hour = rem / 3_600_000;
    let rem = rem % 3_600_000;
    let minute = rem / 60_000;
    let rem = rem % 60_000;
    let second = rem / 1_000;
    let milli = rem % 1_000;
    let (year, month, day) = civil_from_unix_days(days)?;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z"
    ))
}

fn civil_from_unix_days(mut days: i64) -> Result<(i32, u32, u32), String> {
    let mut year: i32 = 1970;
    if days >= 0 {
        loop {
            let ydays = if is_leap(year) { 366 } else { 365 };
            if days < ydays {
                break;
            }
            days -= ydays;
            year += 1;
            if year > 9999 {
                return Err("timestamp year overflows".into());
            }
        }
    } else {
        loop {
            year -= 1;
            if year < 1 {
                return Err("timestamp year underflows".into());
            }
            let ydays = if is_leap(year) { 366 } else { 365 };
            days += ydays;
            if days >= 0 {
                break;
            }
        }
    }
    let mut month = 1u32;
    for m in 1..=12 {
        let dim = month_days(year)[m as usize - 1] as i64;
        if days < dim {
            month = m;
            break;
        }
        days -= dim;
    }
    Ok((year, month, (days + 1) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_and_integer_canonical_forms() {
        assert_eq!(
            PropertyType::Boolean.parse_canonical("true").unwrap(),
            PropertyValue::Boolean(true)
        );
        assert!(PropertyType::Boolean.parse_canonical("TRUE").is_err());
        assert!(PropertyType::Boolean.parse_canonical("1").is_err());
        assert!(PropertyType::Boolean.parse_canonical("").is_err());
        assert_eq!(
            PropertyType::Integer.parse_canonical("0").unwrap(),
            PropertyValue::Integer(0)
        );
        assert_eq!(
            PropertyType::Integer
                .parse_canonical("-9223372036854775808")
                .unwrap(),
            PropertyValue::Integer(i64::MIN)
        );
        assert!(PropertyType::Integer.parse_canonical("01").is_err());
        assert!(PropertyType::Integer.parse_canonical("+1").is_err());
        assert!(PropertyType::Integer.parse_canonical("-0").is_err());
        assert!(PropertyType::Integer
            .parse_canonical("9223372036854775808")
            .is_err());
        assert_eq!(
            PropertyType::Integer
                .parse_canonical("9007199254740993")
                .unwrap(),
            PropertyValue::Integer(9007199254740993)
        );
    }

    #[test]
    fn decimal_requires_exact_scale() {
        let cost = PropertyType::Decimal { scale: 2 };
        assert_eq!(
            cost.parse_canonical("1500.00").unwrap().canonical(),
            "1500.00"
        );
        assert!(cost.parse_canonical("1.5").is_err());
        assert!(cost.parse_canonical("1.500").is_err());
        assert!(cost.parse_canonical("1.005").is_err());
        assert!(cost.parse_canonical("-0.00").is_err());
        assert!(cost.parse_canonical("01.00").is_err());
        assert_eq!(
            PropertyType::Decimal { scale: 0 }
                .parse_canonical("10")
                .unwrap()
                .canonical(),
            "10"
        );
        assert!(PropertyType::Decimal { scale: 0 }
            .parse_canonical("10.0")
            .is_err());
    }

    #[test]
    fn timestamp_normalizes_offset_and_rejects_naive() {
        let ts = PropertyType::Timestamp;
        assert_eq!(
            ts.canonicalize_write("2026-09-19T17:00:00.000Z").unwrap(),
            "2026-09-19T17:00:00.000Z"
        );
        assert_eq!(
            ts.canonicalize_write("2026-09-19T18:00:00+01:00").unwrap(),
            "2026-09-19T17:00:00.000Z"
        );
        assert_eq!(
            ts.canonicalize_write("2026-09-19T17:00:00Z").unwrap(),
            "2026-09-19T17:00:00.000Z"
        );
        assert!(ts.canonicalize_write("2026-09-19").is_err());
        assert!(ts.canonicalize_write("2026-09-19T17:00:00").is_err());
        assert!(ts.canonicalize_write("2026-09-19T17:00:00.1234Z").is_err());
        assert!(ts.parse_canonical("2026-09-19T18:00:00+01:00").is_err());
        assert_eq!(
            ts.parse_canonical("1970-01-01T00:00:00.000Z")
                .unwrap()
                .canonical(),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn range_uses_typed_order() {
        let int = PropertyType::Integer;
        assert!(int.in_range("2", Some("1"), Some("3")).unwrap());
        assert!(!int.in_range("2", Some("3"), None).unwrap());
        assert!(int.in_range("2", None, Some("2")).unwrap());
        assert!(!int.in_range("not-int", Some("1"), Some("3")).unwrap());
        let cost = PropertyType::Decimal { scale: 2 };
        assert!(cost.in_range("10.00", Some("2.00"), None).unwrap());
        assert!(!cost.in_range("2.00", Some("10.00"), None).unwrap());
        let ts = PropertyType::Timestamp;
        assert!(ts
            .in_range(
                "2026-09-19T17:00:00.000Z",
                Some("2026-09-19T16:00:00.000Z"),
                Some("2026-09-19T18:00:00.000Z"),
            )
            .unwrap());
        assert!(PropertyType::Boolean.in_range("true", None, None).is_err());
        assert!(PropertyType::String
            .in_range("prod", None, Some("prod"))
            .unwrap());
        assert!(!PropertyType::String
            .in_range("staging", None, Some("prod"))
            .unwrap());
    }

    #[test]
    fn type_tokens_roundtrip() {
        assert_eq!(
            PropertyType::from_token("boolean").unwrap().token(),
            "boolean"
        );
        assert_eq!(
            PropertyType::from_token("decimal:2").unwrap(),
            PropertyType::Decimal { scale: 2 }
        );
        assert!(PropertyType::from_token("decimal:19").is_err());
        assert!(PropertyType::from_token("float").is_err());
    }
}
