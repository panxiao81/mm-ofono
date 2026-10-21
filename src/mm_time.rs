//! `org.freedesktop.ModemManager1.Modem.Time`, from oFono's NITZ-based
//! `org.ofono.NetworkTime`. oFono reports the network time as it was when
//! received (`UTC`, epoch seconds, at `Received`, CLOCK_BOOTTIME seconds),
//! the zone offset in seconds and the DST adjustment in hours.

use std::collections::HashMap;

use zbus::{Connection, OwnedVariant, Variant, interface, object_server::SignalEmitter};

use crate::error::{CoreError, Result};
use crate::ofono::{self, Props};

pub const NETWORK_TIME: &str = "org.ofono.NetworkTime";

fn int(p: &Props, key: &str) -> Option<i64> {
    match ofono::get(p, key)? {
        Variant::I64(v) => Some(*v),
        Variant::I32(v) => Some((*v).into()),
        Variant::U32(v) => Some((*v).into()),
        _ => None,
    }
}

/// Seconds since boot, suspend included (CLOCK_BOOTTIME, as `Received`).
fn boottime() -> Option<i64> {
    let up = std::fs::read_to_string("/proc/uptime").ok()?;
    up.split('.').next()?.parse().ok()
}

/// (year, month, day) of a day count since 1970-01-01 (H. Hinnant's
/// civil_from_days).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// ISO 8601 local time with offset, like ModemManager's.
fn iso8601(utc: i64, offset: i64) -> String {
    let local = utc + offset;
    let (y, mo, d) = civil(local.div_euclid(86400));
    let s = local.rem_euclid(86400);
    let sign = if offset < 0 { '-' } else { '+' };
    let o = offset.abs() / 60;
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, o / 60, o % 60)
}

/// The network time now, if oFono has received one.
pub fn network_time(p: &Props, now_boottime: i64) -> Option<String> {
    let utc = int(p, "UTC")?;
    let elapsed = int(p, "Received").map_or(0, |r| (now_boottime - r).max(0));
    Some(iso8601(utc + elapsed, int(p, "Timezone").unwrap_or(0)))
}

pub fn now(p: &Props) -> Option<String> {
    network_time(p, boottime()?)
}

/// `NetworkTimezone`: offset (DST included) and DST part, in minutes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Zone {
    pub offset: Option<i32>,
    pub dst: Option<i32>,
}

pub fn zone(p: &Props) -> Zone {
    Zone {
        offset: int(p, "Timezone").map(|s| (s / 60) as i32),
        dst: int(p, "DST").map(|h| (h * 60) as i32),
    }
}

#[derive(Debug, Default)]
pub struct Time {
    pub ofono_path: String,
    pub zone: Zone,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Time")]
impl Time {
    async fn get_network_time(&self, #[zbus(connection)] conn: &Connection) -> Result<String> {
        let p: Props = conn
            .call_method(Some(ofono::SERVICE), self.ofono_path.as_str(), Some(NETWORK_TIME), "GetNetworkTime", &())
            .await?
            .body()
            .deserialize()?;
        now(&p).ok_or_else(|| CoreError::Failed("the network has not sent its time".into()))
    }

    #[zbus(property)]
    async fn network_timezone(&self) -> HashMap<String, OwnedVariant> {
        let mut d = HashMap::new();
        for (k, v) in [("offset", self.zone.offset), ("dst-offset", self.zone.dst)] {
            if let Some(v) = v {
                d.insert(k.to_owned(), OwnedVariant::try_from(Variant::from(v)).unwrap());
            }
        }
        d
    }

    #[zbus(signal)]
    pub async fn network_time_changed(em: &SignalEmitter<'_>, time: &str) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ov(v: impl Into<Variant<'static>>) -> OwnedVariant {
        v.into().try_into().unwrap()
    }

    #[test]
    fn time_moves_on_from_reception() {
        // 2026-10-21 07:00:00 UTC received at boottime 100, in JST.
        let p = Props::from([("UTC".into(), ov(1792566000i64)), ("Received".into(), ov(100i64)), ("Timezone".into(), ov(9 * 3600)), ("DST".into(), ov(0u32))]);
        assert_eq!(network_time(&p, 165).as_deref(), Some("2026-10-21T16:01:05+09:00"));
        assert_eq!(zone(&p), Zone { offset: Some(540), dst: Some(0) });
        assert_eq!(iso8601(0, -3 * 3600 - 1800), "1969-12-31T20:30:00-03:30");
        assert_eq!(network_time(&Props::new(), 0), None, "no NITZ yet");
    }
}
