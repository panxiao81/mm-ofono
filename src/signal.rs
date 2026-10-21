//! ModemManager's Signal values from Sailfish oFono's CellInfo. Cell values
//! follow Android RIL conventions: RSRP/RSRQ are reported negated, RSSI as
//! ASU, SNR in tenths of a dB, and INT_MAX means "not available".

use std::collections::HashMap;

use zbus::{OwnedVariant, Variant};

use crate::ofono::{self, Props};

pub const CELL_INFO: &str = "org.nemomobile.ofono.CellInfo";
pub const CELL: &str = "org.nemomobile.ofono.Cell";

pub type Dict = HashMap<String, OwnedVariant>;

/// One cell as `org.nemomobile.ofono.Cell.GetAll` reports it.
#[derive(Debug, Default)]
pub struct Cell {
    pub kind: String,
    pub registered: bool,
    pub props: Props,
}

/// The per-technology dictionaries of `Modem.Signal`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Values {
    pub gsm: Dict,
    pub umts: Dict,
    pub lte: Dict,
    pub nr5g: Dict,
}

fn int(props: &Props, key: &str) -> Option<i32> {
    match ofono::get(props, key)? {
        Variant::I32(v) if *v != i32::MAX => Some(*v),
        _ => None,
    }
}

fn rssi_from_asu(asu: i32) -> Option<f64> {
    (0..=31).contains(&asu).then(|| f64::from(-113 + 2 * asu))
}

fn dict(entries: impl IntoIterator<Item = (&'static str, Option<f64>)>) -> Dict {
    entries
        .into_iter()
        .filter_map(|(k, v)| Some((k.to_owned(), OwnedVariant::try_from(Variant::from(v?)).unwrap())))
        .collect()
}

/// Values from the serving (registered) cells.
pub fn values<'a>(cells: impl IntoIterator<Item = &'a Cell>) -> Values {
    let mut out = Values::default();
    for c in cells.into_iter().filter(|c| c.registered) {
        let p = &c.props;
        let neg = |k| int(p, k).map(|v| -f64::from(v));
        let asu = int(p, "signalStrength").and_then(rssi_from_asu);
        match c.kind.as_str() {
            "gsm" => out.gsm = dict([("rssi", asu)]),
            "wcdma" => out.umts = dict([("rssi", asu)]),
            "lte" => {
                let snr = int(p, "rssnr").map(|v| f64::from(v) / 10.0);
                out.lte = dict([("rssi", asu), ("rsrp", neg("rsrp")), ("rsrq", neg("rsrq")), ("snr", snr)]);
            }
            "nr" => {
                let snr = int(p, "ssSinr").map(f64::from);
                out.nr5g = dict([("rsrp", neg("ssRsrp")), ("rsrq", neg("ssRsrq")), ("snr", snr)]);
            }
            _ => {}
        }
    }
    out
}

/// `Modem.GetCellInfo` entries, serving and neighbouring cells. Cells
/// report the MNC as a number, so `mnc_len` (the serving network's, 2 or
/// 3) gives it back its leading zeros.
pub fn cell_info<'a>(cells: impl IntoIterator<Item = &'a Cell>, mnc_len: usize) -> Vec<Dict> {
    use crate::mm::MMCellType as ty;
    let mut out = vec![];
    for c in cells {
        let p = &c.props;
        let cell_type = match c.kind.as_str() {
            "gsm" => ty::MM_CELL_TYPE_GSM,
            "wcdma" => ty::MM_CELL_TYPE_UMTS,
            "tdscdma" => ty::MM_CELL_TYPE_TDSCDMA,
            "lte" => ty::MM_CELL_TYPE_LTE,
            "nr" => ty::MM_CELL_TYPE_5GNR,
            _ => continue,
        };
        let mut d = Dict::from([
            ("cell-type".to_owned(), OwnedVariant::try_from(Variant::from(cell_type)).unwrap()),
            ("serving".to_owned(), OwnedVariant::try_from(Variant::from(c.registered)).unwrap()),
        ]);
        let mut put = |k: &str, v: Option<Variant<'static>>| {
            if let Some(v) = v {
                d.insert(k.to_owned(), OwnedVariant::try_from(v).unwrap());
            }
        };
        let hex = |k| int(p, k).map(|v| Variant::from(format!("{v:X}")));
        let uint = |k| int(p, k).and_then(|v| u32::try_from(v).ok()).map(Variant::from);
        let neg = |k| int(p, k).map(|v| Variant::from(-f64::from(v)));
        if let (Some(mcc), Some(mnc)) = (int(p, "mcc"), int(p, "mnc")) {
            put("operator-id", Some(format!("{mcc:03}{mnc:0w$}", w = mnc_len.max(2)).into()));
        }
        match c.kind.as_str() {
            "gsm" => {
                put("lac", hex("lac"));
                put("ci", hex("cid"));
            }
            "wcdma" | "tdscdma" => {
                put("lac", hex("lac"));
                put("ci", hex("cid"));
                put("uarfcn", uint("uarfcn"));
                put("psc", uint("psc"));
            }
            "lte" => {
                put("tac", hex("tac"));
                put("ci", hex("ci"));
                put("physical-ci", hex("pci"));
                put("earfcn", uint("earfcn"));
                put("rsrp", neg("rsrp"));
                put("rsrq", neg("rsrq"));
                put("timing-advance", uint("timingAdvance"));
            }
            _ => {
                // NR: Sailfish names follow Android's CellIdentityNr.
                put("tac", hex("tac"));
                put("ci", int(p, "nci").map(|v| Variant::from(format!("{v:X}"))));
                put("physical-ci", hex("pci"));
                put("nrarfcn", uint("nrarfcn"));
                put("rsrp", neg("ssRsrp"));
                put("rsrq", neg("ssRsrq"));
                put("sinr", int(p, "ssSinr").map(|v| Variant::from(f64::from(v))));
            }
        }
        out.push(d);
    }
    out
}

/// Modem.GetCellInfo. Cells exist only while the connection is subscribed
/// (GetCells subscribes, and lists them right away only if it already
/// was), so without Signal's subscription this subscribes, gives the modem
/// a moment, and unsubscribes again.
pub async fn query_cells(conn: &zbus::Connection, modem: &str, signal_subscribed: bool) -> zbus::Result<Vec<Dict>> {
    use zbus::OwnedObjectPath;
    let get_cells = || async {
        conn.call_method(Some(ofono::SERVICE), modem, Some(CELL_INFO), "GetCells", &()).await?.body().deserialize::<Vec<OwnedObjectPath>>()
    };
    let mut paths = get_cells().await?;
    if paths.is_empty() {
        // ponytail: fixed wait; waiting for CellsAdded would answer sooner.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        paths = get_cells().await?;
    }
    let mut cells = vec![];
    for p in &paths {
        let reply = conn.call_method(Some(ofono::SERVICE), p.as_str(), Some(CELL), "GetAll", &()).await;
        if let Ok((_, kind, registered, props)) = reply.and_then(|r| r.body().deserialize::<(i32, String, bool, Props)>()) {
            cells.push(Cell { kind, registered, props });
        }
    }
    // ponytail: a Signal.Setup arriving during the wait loses its
    // subscription here; it comes back with the client's next Setup.
    if !signal_subscribed {
        conn.call_method(Some(ofono::SERVICE), modem, Some(CELL_INFO), "Unsubscribe", &()).await?;
    }
    let netreg = ofono::get_properties(conn, modem, crate::derive::NETREG).await.unwrap_or_default();
    let mnc_len = ofono::str(&netreg, "MobileNetworkCode").map_or(2, str::len);
    Ok(cell_info(&cells, mnc_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(kind: &str, registered: bool, kv: &[(&str, i32)]) -> Cell {
        let props = kv.iter().map(|(k, v)| (k.to_string(), OwnedVariant::try_from(Variant::from(*v)).unwrap())).collect();
        Cell { kind: kind.into(), registered, props }
    }

    fn get(d: &Dict, k: &str) -> f64 {
        f64::try_from(d[k].try_clone().unwrap()).unwrap()
    }

    /// The serving cell recorded on the Xperia 5 II (docomo, LTE anchor).
    #[test]
    fn lte_serving_cell() {
        let cells = [
            cell("lte", true, &[("signalStrength", 31), ("rsrp", 67), ("rsrq", 9), ("rssnr", i32::MAX)]),
            cell("lte", false, &[("signalStrength", 31), ("rsrp", 71), ("rsrq", 10)]), // neighbour: ignored
        ];
        let v = values(&cells);
        assert_eq!(get(&v.lte, "rssi"), -51.0);
        assert_eq!(get(&v.lte, "rsrp"), -67.0);
        assert_eq!(get(&v.lte, "rsrq"), -9.0);
        assert!(!v.lte.contains_key("snr"), "INT_MAX means unavailable");
        assert!(v.nr5g.is_empty() && v.gsm.is_empty() && v.umts.is_empty());
    }

    /// Cells read on the Xperia 5 II (CT roaming on KDDI, 440-50).
    #[test]
    fn cell_info_from_recorded_cells() {
        let cells = [
            cell("lte", true, &[("mcc", 440), ("mnc", 50), ("ci", 40835851), ("pci", 33), ("tac", 38479), ("earfcn", 1300), ("rsrp", 77), ("rsrq", 7), ("timingAdvance", i32::MAX)]),
            cell("lte", false, &[("pci", 0), ("earfcn", 100), ("rsrp", 104), ("rsrq", 19)]),
        ];
        let info = cell_info(&cells, 2);
        let s = |d: &Dict, k: &str| String::try_from(d[k].try_clone().unwrap()).unwrap();
        assert_eq!(s(&info[0], "operator-id"), "44050");
        assert_eq!(s(&info[0], "tac"), "964F");
        assert_eq!(s(&info[0], "ci"), "26F1B0B");
        assert_eq!(s(&info[0], "physical-ci"), "21");
        assert_eq!(get(&info[0], "rsrp"), -77.0);
        assert!(!info[0].contains_key("timing-advance"), "INT_MAX means unavailable");
        assert!(bool::try_from(info[0]["serving"].try_clone().unwrap()).unwrap());
        assert!(!info[1].contains_key("operator-id"), "neighbours report no PLMN");
        assert_eq!(cell_info(&[cell("gsm", true, &[("mcc", 310), ("mnc", 5)])], 3)[0].get("operator-id").map(|v| String::try_from(v.try_clone().unwrap()).unwrap()), Some("310005".into()));
    }

    #[test]
    fn nr_and_out_of_range_asu() {
        let cells = [cell("nr", true, &[("ssRsrp", 90), ("ssRsrq", 11), ("ssSinr", 25)]), cell("gsm", true, &[("signalStrength", 99)])];
        let v = values(&cells);
        assert_eq!(get(&v.nr5g, "rsrp"), -90.0);
        assert_eq!(get(&v.nr5g, "snr"), 25.0);
        assert!(v.gsm.is_empty(), "ASU 99 = unknown");
    }
}
