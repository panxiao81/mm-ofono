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

    #[test]
    fn nr_and_out_of_range_asu() {
        let cells = [cell("nr", true, &[("ssRsrp", 90), ("ssRsrq", 11), ("ssSinr", 25)]), cell("gsm", true, &[("signalStrength", 99)])];
        let v = values(&cells);
        assert_eq!(get(&v.nr5g, "rsrp"), -90.0);
        assert_eq!(get(&v.nr5g, "snr"), 25.0);
        assert!(v.gsm.is_empty(), "ASU 99 = unknown");
    }
}
