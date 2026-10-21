//! `org.freedesktop.ModemManager1.Modem.Location`, 3GPP LAC/CI only: GPS on
//! Droidian goes from the Android HAL to geoclue directly, not through the
//! modem. oFono's NetworkRegistration has no tracking area code on LTE, so
//! the location comes from the serving cell (see [`crate::signal::query_cells`]).

use std::collections::HashMap;

use zbus::{Connection, OwnedVariant, Variant, interface};

use crate::error::{CoreError, Result};
use crate::mm::{MMModemLocationAssistanceDataType as assist, MMModemLocationSource as source};
use crate::signal::Dict;

const LAC_CI: u32 = source::MM_MODEM_LOCATION_SOURCE_3GPP_LAC_CI;

#[derive(Debug)]
pub struct Location {
    pub ofono_path: String,
    pub enabled: u32,
}

impl Default for Location {
    /// MM gathers 3GPP LAC/CI by default.
    fn default() -> Location {
        Location { ofono_path: String::new(), enabled: LAC_CI }
    }
}

fn s(d: &Dict, k: &str) -> Option<String> {
    String::try_from(d.get(k)?.try_clone().ok()?).ok()
}

/// MM's "MCC,MNC,LAC,CI,TAC" (hex codes, 0 when unknown) from a serving
/// cell of [`crate::signal::cell_info`].
pub fn lac_ci(cell: &Dict) -> Option<String> {
    let op = s(cell, "operator-id")?;
    let (mcc, mnc) = op.split_at(3);
    let hex = |k| s(cell, k).unwrap_or_else(|| "0".into());
    let (lac, tac) = if cell.contains_key("tac") { ("0".into(), hex("tac")) } else { (hex("lac"), "0".into()) };
    Some(format!("{mcc},{mnc},{lac},{},{tac}", s(cell, "ci")?))
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Location")]
impl Location {
    async fn setup(&mut self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, sources: u32, signal_location: bool) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        if sources & !LAC_CI != 0 {
            return Err(CoreError::Unsupported(format!("location sources {sources:#x}: only 3GPP LAC/CI is available")));
        }
        if signal_location {
            return Err(CoreError::Unsupported("location signalling is not implemented".into()));
        }
        self.enabled = sources;
        Ok(())
    }

    async fn get_location(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<HashMap<u32, OwnedVariant>> {
        crate::auth::check(conn, &header, crate::auth::LOCATION).await?;
        if self.enabled & LAC_CI == 0 {
            return Ok(HashMap::new());
        }
        let path = header.path().ok_or_else(|| CoreError::Failed("no path".into()))?.to_owned();
        let subscribed = crate::mm_signal::subscribed(conn, &path).await;
        let cells = crate::signal::query_cells(conn, &self.ofono_path, subscribed).await?;
        let serving = cells.iter().filter(|c| bool::try_from(c["serving"].try_clone().unwrap()).unwrap_or(false));
        let loc = serving.filter_map(lac_ci).next().ok_or_else(|| CoreError::WrongState("no serving cell".into()))?;
        Ok(HashMap::from([(LAC_CI, OwnedVariant::try_from(Variant::from(loc)).unwrap())]))
    }

    async fn set_supl_server(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, _supl: &str) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Err(CoreError::Unsupported("no GPS on the modem".into()))
    }

    async fn inject_assistance_data(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, _data: Vec<u8>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Err(CoreError::Unsupported("no GPS on the modem".into()))
    }

    async fn set_gps_refresh_rate(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, _rate: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Err(CoreError::Unsupported("no GPS on the modem".into()))
    }

    #[zbus(property)]
    async fn capabilities(&self) -> u32 {
        LAC_CI
    }

    #[zbus(property)]
    async fn supported_assistance_data(&self) -> u32 {
        assist::MM_MODEM_LOCATION_ASSISTANCE_DATA_TYPE_NONE
    }

    #[zbus(property)]
    async fn enabled(&self) -> u32 {
        self.enabled
    }

    #[zbus(property)]
    async fn signals_location(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn location(&self) -> HashMap<u32, OwnedVariant> {
        HashMap::new()
    }

    #[zbus(property)]
    async fn supl_server(&self) -> &str {
        ""
    }

    #[zbus(property)]
    async fn assistance_data_servers(&self) -> Vec<String> {
        vec![]
    }

    #[zbus(property)]
    async fn gps_refresh_rate(&self) -> u32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(kv: &[(&str, &str)]) -> Dict {
        kv.iter().map(|(k, v)| (k.to_string(), OwnedVariant::try_from(Variant::from(*v)).unwrap())).collect()
    }

    #[test]
    fn lte_serving_cell_as_mm_string() {
        let lte = dict(&[("operator-id", "44050"), ("tac", "964F"), ("ci", "26F1B0B")]);
        assert_eq!(lac_ci(&lte).as_deref(), Some("440,50,0,26F1B0B,964F"));
        let gsm = dict(&[("operator-id", "310005"), ("lac", "1A"), ("ci", "2B")]);
        assert_eq!(lac_ci(&gsm).as_deref(), Some("310,005,1A,2B,0"));
        assert!(lac_ci(&dict(&[("ci", "1")])).is_none(), "no PLMN");
    }
}
