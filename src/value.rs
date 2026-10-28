//! Canonical encodings for schema-declared property types (ADR 0012, ADR 0043).
//!
//! Physical storage stays UTF-8 in [`crate::ObjectRecord::props`]. These
//! helpers parse and format the accepted subset. Writes may normalize a
//! timestamp with an offset; other types require the canonical form.
//! Vectors, geo, and references never store the large payload itself.

use std::cmp::Ordering;
use std::fmt;

/// Largest admitted vector dimension. An all-zero vector of this dim
/// fits on a 4 KiB page; the page still bounds longer encodings.
pub const VECTOR_DIM_MAX: u16 = 1024;

/// Clerk-declared logical type of one property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyType {
    String,
    Boolean,
    Integer,
    Timestamp,
    Decimal {
        scale: u8,
    },
    /// Fixed-dimension embedding. Canonical form is `dim` comma-separated
    /// numbers; the payload stays in `props`.
    Vector {
        dim: u16,
    },
    /// External time-series reference plus inclusive `[start, end]` timestamps.
    TimeSeries,
    GeoPoint,
    /// External shape reference plus a bounding envelope.
    GeoShape,
    /// External media or attachment reference.
    Media,
}

/// Typed view of a canonical property value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyValue {
    String(String),
    Boolean(bool),
    Integer(i64),
    Timestamp(String),
    Decimal(String),
    Vector(String),
    TimeSeries(String),
    GeoPoint(String),
    GeoShape(String),
    Media(String),
}

impl PropertyType {
    /// Decode a descriptor token (`boolean`, `integer`, `timestamp`,
    /// `string`, `decimal:<scale>`, `vector:<dim>`, `tsref`, `geo:point`,
    /// `geo:shape`, or `media`).
    pub fn from_token(token: &str) -> Result<Self, String> {
        match token {
            "string" => Ok(Self::String),
            "boolean" => Ok(Self::Boolean),
            "integer" => Ok(Self::Integer),
            "timestamp" => Ok(Self::Timestamp),
            "tsref" => Ok(Self::TimeSeries),
            "geo:point" => Ok(Self::GeoPoint),
            "geo:shape" => Ok(Self::GeoShape),
            "media" => Ok(Self::Media),
            other => {
                if let Some(dim) = other.strip_prefix("vector:") {
                    return parse_vector_dim(dim).map(|dim| Self::Vector { dim });
                }
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
            Self::Vector { dim } => format!("vector:{dim}"),
            Self::TimeSeries => "tsref".into(),
            Self::GeoPoint => "geo:point".into(),
            Self::GeoShape => "geo:shape".into(),
            Self::Media => "media".into(),
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
            Self::Vector { dim } => parse_vector(raw, dim),
            Self::TimeSeries => parse_time_series(raw),
            Self::GeoPoint => parse_geo_point(raw),
            Self::GeoShape => parse_geo_shape(raw),
            Self::Media => parse_media(raw),
        }
    }

    /// Inclusive ADR 0012 order. `None` bound is unbounded that side.
    /// Boolean, vector, geo, and media have no range. Time-series range is
    /// overlap of `[start, end]` with timestamp bounds. A stored value that
    /// is not canonical is not a match rather than a request error.
    pub fn in_range(
        self,
        stored: &str,
        min: Option<&str>,
        max: Option<&str>,
    ) -> Result<bool, String> {
        match self {
            Self::Boolean => return Err("range is not defined for boolean".into()),
            Self::Vector { .. } => return Err("range is not defined for vector".into()),
            Self::GeoPoint => return Err("range is not defined for geo:point".into()),
            Self::GeoShape => return Err("range is not defined for geo:shape".into()),
            Self::Media => return Err("range is not defined for media".into()),
            Self::TimeSeries => return time_series_overlaps(stored, min, max),
            Self::String | Self::Integer | Self::Timestamp | Self::Decimal { .. } => {}
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

    /// ADR 0012 order of two canonical stored forms.
    pub fn cmp_canonical(self, left: &str, right: &str) -> Result<Ordering, String> {
        self.cmp_values(&self.parse_canonical(left)?, &self.parse_canonical(right)?)
    }

    pub(crate) fn cmp_values(
        self,
        left: &PropertyValue,
        right: &PropertyValue,
    ) -> Result<Ordering, String> {
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
            (Self::TimeSeries, PropertyValue::TimeSeries(a), PropertyValue::TimeSeries(b))
            | (Self::GeoPoint, PropertyValue::GeoPoint(a), PropertyValue::GeoPoint(b))
            | (Self::GeoShape, PropertyValue::GeoShape(a), PropertyValue::GeoShape(b))
            | (Self::Media, PropertyValue::Media(a), PropertyValue::Media(b))
            | (Self::Vector { .. }, PropertyValue::Vector(a), PropertyValue::Vector(b)) => {
                Ok(a.cmp(b))
            }
            _ => Err("typed comparison mixed property types".into()),
        }
    }

    /// Squared L2 distance of two canonical vectors as IEEE f64 of their
    /// decimal components. Used only for nearest-neighbour ranking.
    pub fn vector_distance(self, stored: &str, query: &str) -> Result<f64, String> {
        let Self::Vector { dim } = self else {
            return Err("nearest requires a vector property".into());
        };
        let left = vector_components(stored, dim)?;
        let right = vector_components(query, dim)?;
        let distance: f64 = left
            .iter()
            .zip(&right)
            .map(|(a, b)| {
                let d = a - b;
                d * d
            })
            .sum();
        if !distance.is_finite() {
            return Err("vector distance is not finite".into());
        }
        Ok(distance)
    }

    /// Point-in-box or shape-envelope intersection. Bounds are canonical numbers.
    pub fn in_bbox(
        self,
        stored: &str,
        min_lat: &str,
        min_lon: &str,
        max_lat: &str,
        max_lon: &str,
    ) -> Result<bool, String> {
        let min_lat = parse_lat(min_lat)?;
        let min_lon = parse_lon(min_lon)?;
        let max_lat = parse_lat(max_lat)?;
        let max_lon = parse_lon(max_lon)?;
        if min_lat > max_lat || min_lon > max_lon {
            return Err("bbox min must not exceed max".into());
        }
        match self {
            Self::GeoPoint => {
                let Ok((lat, lon)) = geo_point_parts(stored) else {
                    return Ok(false);
                };
                Ok(lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon)
            }
            Self::GeoShape => {
                let Ok((_, smin_lat, smin_lon, smax_lat, smax_lon)) = geo_shape_parts(stored)
                else {
                    return Ok(false);
                };
                Ok(smin_lat <= max_lat
                    && smax_lat >= min_lat
                    && smin_lon <= max_lon
                    && smax_lon >= min_lon)
            }
            _ => Err(format!("bbox is not defined for {}", self.token())),
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
            Self::String(value)
            | Self::Timestamp(value)
            | Self::Decimal(value)
            | Self::Vector(value)
            | Self::TimeSeries(value)
            | Self::GeoPoint(value)
            | Self::GeoShape(value)
            | Self::Media(value) => value.clone(),
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

fn parse_vector_dim(raw: &str) -> Result<u16, String> {
    if raw.len() > 1 && raw.starts_with('0') {
        return Err(format!("vector dim {raw} is not canonical"));
    }
    let dim: u16 = raw
        .parse()
        .map_err(|_| format!("vector dim {raw} is not an integer"))?;
    if !(1..=VECTOR_DIM_MAX).contains(&dim) {
        return Err(format!("vector dim {dim} is not in 1..={VECTOR_DIM_MAX}"));
    }
    Ok(dim)
}

/// Restricted decimal: optional `-` (not `-0`), integer `0` or `[1-9][0-9]*`,
/// optional `.` plus digits that do not end in `0`.
fn is_canonical_number(raw: &str) -> bool {
    let body = match raw.strip_prefix('-') {
        Some("0") => return false,
        Some(rest) => rest,
        None => raw,
    };
    match body.split_once('.') {
        None => is_canonical_integer(body) && !body.starts_with('-'),
        Some((int, frac)) => {
            if frac.is_empty() || frac.ends_with('0') || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            if int.starts_with('0') {
                return int == "0";
            }
            !int.is_empty() && int.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

fn parse_vector(raw: &str, dim: u16) -> Result<PropertyValue, String> {
    vector_components(raw, dim)?;
    Ok(PropertyValue::Vector(raw.to_string()))
}

fn vector_components(raw: &str, dim: u16) -> Result<Vec<f64>, String> {
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() != dim as usize {
        return Err(format!(
            "vector wants {dim} components, got {}",
            parts.len()
        ));
    }
    parts
        .into_iter()
        .map(|part| {
            if !is_canonical_number(part) {
                return Err(format!("vector component {part} is not canonical"));
            }
            let n: f64 = part
                .parse()
                .map_err(|_| format!("vector component {part} is not a number"))?;
            if !n.is_finite() {
                return Err(format!("vector component {part} is not finite"));
            }
            Ok(n)
        })
        .collect()
}

/// External reference: non-empty, no comma or whitespace, at most 2048 bytes.
fn parse_ref_token(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err("reference is empty".into());
    }
    if raw.len() > 2048 {
        return Err("reference exceeds 2048 bytes".into());
    }
    if raw.bytes().any(|b| b == b',' || b.is_ascii_whitespace()) {
        return Err("reference must not contain comma or whitespace".into());
    }
    Ok(())
}

fn parse_time_series(raw: &str) -> Result<PropertyValue, String> {
    let mut parts = raw.split(' ');
    let uri = parts
        .next()
        .ok_or_else(|| "time-series must be uri start_ts end_ts".to_string())?;
    let start = parts
        .next()
        .ok_or_else(|| "time-series must be uri start_ts end_ts".to_string())?;
    let end = parts
        .next()
        .ok_or_else(|| "time-series must be uri start_ts end_ts".to_string())?;
    if parts.next().is_some() {
        return Err("time-series must be uri start_ts end_ts".into());
    }
    parse_ref_token(uri)?;
    if !is_canonical_timestamp(start) {
        return Err(format!(
            "time-series start {start} is not canonical UTC millis"
        ));
    }
    parse_timestamp_millis(start)?;
    if !is_canonical_timestamp(end) {
        return Err(format!("time-series end {end} is not canonical UTC millis"));
    }
    parse_timestamp_millis(end)?;
    if start > end {
        return Err("time-series start is after end".into());
    }
    Ok(PropertyValue::TimeSeries(raw.to_string()))
}

fn time_series_overlaps(
    stored: &str,
    min: Option<&str>,
    max: Option<&str>,
) -> Result<bool, String> {
    let Ok(value) = parse_time_series(stored) else {
        return Ok(false);
    };
    let canon = value.canonical();
    let mut parts = canon.split(' ');
    let _uri = parts.next();
    let start = parts.next().expect("canonical tsref has a start");
    let end = parts.next().expect("canonical tsref has an end");
    if let Some(min) = min {
        if !is_canonical_timestamp(min) {
            return Err(format!("range bound {min} is not canonical UTC millis"));
        }
        parse_timestamp_millis(min)?;
    }
    if let Some(max) = max {
        if !is_canonical_timestamp(max) {
            return Err(format!("range bound {max} is not canonical UTC millis"));
        }
        parse_timestamp_millis(max)?;
    }
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Ok(false);
        }
    }
    if let Some(min) = min {
        if end < min {
            return Ok(false);
        }
    }
    if let Some(max) = max {
        if start > max {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parse_lat(raw: &str) -> Result<f64, String> {
    parse_geo_coord(raw, "lat", -90.0, 90.0)
}

fn parse_lon(raw: &str) -> Result<f64, String> {
    parse_geo_coord(raw, "lon", -180.0, 180.0)
}

fn parse_geo_coord(raw: &str, label: &str, min: f64, max: f64) -> Result<f64, String> {
    if !is_canonical_number(raw) {
        return Err(format!("{label} {raw} is not canonical"));
    }
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("{label} {raw} is not a number"))?;
    if value < min || value > max {
        return Err(format!("{label} {raw} is out of range"));
    }
    Ok(value)
}

fn parse_geo_point(raw: &str) -> Result<PropertyValue, String> {
    geo_point_parts(raw)?;
    Ok(PropertyValue::GeoPoint(raw.to_string()))
}

fn geo_point_parts(raw: &str) -> Result<(f64, f64), String> {
    let Some((lat, lon)) = raw.split_once(',') else {
        return Err(format!("geo:point {raw} must be lat,lon"));
    };
    if lat.contains(',') || lon.contains(',') {
        return Err(format!("geo:point {raw} must be lat,lon"));
    }
    Ok((parse_lat(lat)?, parse_lon(lon)?))
}

fn parse_geo_shape(raw: &str) -> Result<PropertyValue, String> {
    geo_shape_parts(raw)?;
    Ok(PropertyValue::GeoShape(raw.to_string()))
}

fn geo_shape_parts(raw: &str) -> Result<(&str, f64, f64, f64, f64), String> {
    let Some((uri, envelope)) = raw.split_once(' ') else {
        return Err("geo:shape must be uri minlat,minlon,maxlat,maxlon".into());
    };
    parse_ref_token(uri)?;
    let parts: Vec<&str> = envelope.split(',').collect();
    if parts.len() != 4 {
        return Err("geo:shape must be uri minlat,minlon,maxlat,maxlon".into());
    }
    let min_lat = parse_lat(parts[0])?;
    let min_lon = parse_lon(parts[1])?;
    let max_lat = parse_lat(parts[2])?;
    let max_lon = parse_lon(parts[3])?;
    if min_lat > max_lat || min_lon > max_lon {
        return Err("geo:shape envelope min must not exceed max".into());
    }
    Ok((uri, min_lat, min_lon, max_lat, max_lon))
}

fn parse_media(raw: &str) -> Result<PropertyValue, String> {
    parse_ref_token(raw)?;
    Ok(PropertyValue::Media(raw.to_string()))
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
        assert_eq!(
            PropertyType::from_token("vector:8").unwrap(),
            PropertyType::Vector { dim: 8 }
        );
        assert_eq!(PropertyType::from_token("tsref").unwrap().token(), "tsref");
        assert_eq!(
            PropertyType::from_token("geo:point").unwrap().token(),
            "geo:point"
        );
        assert_eq!(
            PropertyType::from_token("geo:shape").unwrap().token(),
            "geo:shape"
        );
        assert_eq!(PropertyType::from_token("media").unwrap().token(), "media");
        assert!(PropertyType::from_token("vector:0").is_err());
        assert_eq!(
            PropertyType::from_token("vector:1024").unwrap(),
            PropertyType::Vector {
                dim: VECTOR_DIM_MAX
            }
        );
        assert!(PropertyType::from_token("vector:1025").is_err());
        assert!(PropertyType::from_token("vector:4096").is_err());
        assert!(PropertyType::from_token("vector:08").is_err());
    }

    #[test]
    fn vector_geo_and_refs_are_canonical_and_queriable() {
        let vec2 = PropertyType::Vector { dim: 2 };
        assert_eq!(
            vec2.parse_canonical("1,-0.5").unwrap().canonical(),
            "1,-0.5"
        );
        assert!(vec2.parse_canonical("1.0,0").is_err());
        assert!(vec2.parse_canonical("1,0,0").is_err());
        assert!(vec2.parse_canonical("1").is_err());
        assert!(vec2
            .parse_canonical(&format!("{},0", "1".to_string() + &"0".repeat(400)))
            .is_err());
        assert!((vec2.vector_distance("0,0", "3,4").unwrap() - 25.0).abs() < 1e-9);
        assert!(vec2.in_range("0,0", None, None).is_err());

        let ts = PropertyType::TimeSeries;
        let stored = "sensor:1 2026-09-19T17:00:00.000Z 2026-09-19T18:00:00.000Z";
        assert_eq!(ts.parse_canonical(stored).unwrap().canonical(), stored);
        assert!(ts
            .in_range(
                stored,
                Some("2026-09-19T17:30:00.000Z"),
                Some("2026-09-19T17:45:00.000Z"),
            )
            .unwrap());
        assert!(!ts
            .in_range(
                stored,
                Some("2026-09-19T18:00:00.001Z"),
                Some("2026-09-19T19:00:00.000Z"),
            )
            .unwrap());
        assert!(!ts
            .in_range(
                stored,
                Some("2026-09-19T19:00:00.000Z"),
                Some("2026-09-19T18:00:00.000Z"),
            )
            .unwrap());
        assert!(ts
            .parse_canonical("sensor:1 2026-09-19T18:00:00.000Z 2026-09-19T17:00:00.000Z")
            .is_err());
        assert!(ts
            .parse_canonical(
                "sensor 1 with space 2026-09-19T17:00:00.000Z 2026-09-19T18:00:00.000Z"
            )
            .is_err());

        let point = PropertyType::GeoPoint;
        assert_eq!(
            point.parse_canonical("51.5,-0.1").unwrap().canonical(),
            "51.5,-0.1"
        );
        assert!(point.in_bbox("51.5,-0.1", "51", "-1", "52", "0").unwrap());
        assert!(!point.in_bbox("51.5,-0.1", "0", "0", "1", "1").unwrap());
        assert!(point.parse_canonical("91,0").is_err());
        assert!(point.parse_canonical("0,181").is_err());
        assert!(point.in_range("0,0", None, None).is_err());

        let shape = PropertyType::GeoShape;
        let stored_shape = "shape:1 51,-1,52,0";
        assert_eq!(
            shape.parse_canonical(stored_shape).unwrap().canonical(),
            stored_shape
        );
        assert!(shape
            .in_bbox(stored_shape, "51.4", "-0.2", "51.6", "0")
            .unwrap());
        assert!(!shape.in_bbox(stored_shape, "0", "0", "1", "1").unwrap());
        assert!(shape.parse_canonical("shape:1 52,-1,51,0").is_err());

        let media = PropertyType::Media;
        assert_eq!(
            media.parse_canonical("blob:abc").unwrap().canonical(),
            "blob:abc"
        );
        assert!(media.parse_canonical("").is_err());
        assert!(media.parse_canonical("a b").is_err());
        assert!(media.parse_canonical("a,b").is_err());
        assert!(media.in_range("blob:abc", None, None).is_err());
        assert!(media.in_bbox("blob:abc", "0", "0", "1", "1").is_err());
    }
}
