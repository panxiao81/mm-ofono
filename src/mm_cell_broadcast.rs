//! `org.freedesktop.ModemManager1.Modem.CellBroadcast` and its `Cbm`
//! objects, from oFono's `org.ofono.CellBroadcast`.
//!
//! oFono only reports complete messages and keeps none, so the list holds
//! what arrived since start, every message is `RECEIVED`, and `Update` is
//! always 0: oFono drops the serial number. `Channels` mirrors oFono's
//! `Topics`; ETWS (4352-4356) is always on in oFono on top of those.

use std::sync::atomic::{AtomicU32, Ordering};

use zbus::{Connection, ObjectServer, OwnedObjectPath, Variant, interface, object_server::SignalEmitter};

use crate::error::{CoreError, Result};
use crate::mm::MMCbmState as cbm_state;
use crate::ofono::{self, Props};

pub const CBS: &str = "org.ofono.CellBroadcast";

static NEXT_CBM: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CbmData {
    pub text: String,
    pub channel: u32,
    pub message_code: u32,
}

/// The ETWS message identifier oFono turned into `EmergencyType`
/// (3GPP 23.041 9.4.1.2.2; oFono drops test messages, 4355).
fn etws_channel(emergency_type: &str) -> Option<u32> {
    Some(match emergency_type {
        "Earthquake" => 4352,
        "Tsunami" => 4353,
        "Earthquake+Tsunami" => 4354,
        "Other" => 4356,
        _ => return None,
    })
}

impl CbmData {
    /// `IncomingBroadcast(text, topic)`.
    pub fn text(text: String, topic: u16) -> CbmData {
        CbmData { text, channel: topic.into(), message_code: 0 }
    }

    /// `EmergencyBroadcast(text, info)`. Of the message code oFono passes
    /// on only the alert (bit 9) and popup (bit 8) flags (23.041 9.4.1.2.1).
    pub fn emergency(text: String, info: &Props) -> Option<CbmData> {
        let channel = etws_channel(ofono::str(info, "EmergencyType")?)?;
        let bit = |key, n: u32| if ofono::bool(info, key) == Some(true) { 1u32 << n } else { 0 };
        Some(CbmData { text, channel, message_code: bit("EmergencyAlert", 9) | bit("Popup", 8) })
    }
}

#[derive(Debug)]
pub struct Cbm {
    pub data: CbmData,
}

#[interface(name = "org.freedesktop.ModemManager1.Cbm")]
impl Cbm {
    #[zbus(property)]
    async fn state(&self) -> u32 {
        cbm_state::MM_CBM_STATE_RECEIVED
    }

    #[zbus(property)]
    async fn text(&self) -> &str {
        &self.data.text
    }

    #[zbus(property)]
    async fn channel(&self) -> u32 {
        self.data.channel
    }

    #[zbus(property)]
    async fn message_code(&self) -> u32 {
        self.data.message_code
    }

    #[zbus(property)]
    async fn update(&self) -> u32 {
        0
    }
}

/// oFono's `Topics` ("4370,4383-4400") as channel ranges.
pub fn parse_topics(topics: &str) -> Vec<(u32, u32)> {
    topics
        .split(',')
        .filter_map(|r| {
            let (a, b) = r.trim().split_once('-').unwrap_or((r.trim(), r.trim()));
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .collect()
}

fn topics_string(channels: &[(u32, u32)]) -> String {
    let range = |&(a, b): &(u32, u32)| if a == b { a.to_string() } else { format!("{a}-{b}") };
    channels.iter().map(range).collect::<Vec<_>>().join(",")
}

#[derive(Debug, Default)]
pub struct CellBroadcast {
    pub ofono_path: String,
    pub messages: Vec<OwnedObjectPath>,
    pub channels: Vec<(u32, u32)>,
}

/// Publish a broadcast oFono received on the modem object at `modem`.
pub async fn deliver(conn: &Connection, modem: &OwnedObjectPath, data: CbmData) -> zbus::Result<()> {
    let server = conn.object_server();
    let iface = server.interface::<_, CellBroadcast>(modem).await?;
    let path = OwnedObjectPath::try_from(format!("{}/CBM/{}", crate::MM_PATH, NEXT_CBM.fetch_add(1, Ordering::Relaxed)))?;
    server.at(&path, Cbm { data }).await?;
    let em = iface.signal_emitter().clone();
    iface.get_mut().await.messages.push(path.clone());
    iface.get().await.cell_broadcasts_changed(&em).await?;
    CellBroadcast::added(&em, path).await
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.CellBroadcast")]
impl CellBroadcast {
    async fn list(&self) -> Vec<OwnedObjectPath> {
        self.messages.clone()
    }

    async fn delete(
        &mut self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        path: OwnedObjectPath,
    ) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MESSAGING).await?;
        let Some(i) = self.messages.iter().position(|p| *p == path) else {
            return Err(CoreError::Failed(format!("no cell broadcast message {path}")));
        };
        self.messages.remove(i);
        server.remove::<Cbm, _>(&path).await?;
        self.cell_broadcasts_changed(&em).await?;
        Self::deleted(&em, path).await?;
        Ok(())
    }

    /// Sets oFono's `Topics`; `Channels` follows once oFono confirms it.
    async fn set_channels(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: zbus::message::Header<'_>, channels: Vec<(u32, u32)>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        if channels.iter().any(|&(a, b)| a > b || b > 0xffff) {
            return Err(CoreError::Failed(format!("invalid channel ranges {channels:?}")));
        }
        Ok(ofono::set_property(conn, &self.ofono_path, CBS, "Topics", Variant::from(topics_string(&channels))).await?)
    }

    #[zbus(signal)]
    async fn added(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deleted(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(property)]
    async fn cell_broadcasts(&self) -> Vec<OwnedObjectPath> {
        self.messages.clone()
    }

    #[zbus(property)]
    async fn channels(&self) -> Vec<(u32, u32)> {
        self.channels.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::OwnedVariant;

    fn ov(v: impl Into<Variant<'static>>) -> OwnedVariant {
        v.into().try_into().unwrap()
    }

    #[test]
    fn topics_round_trip() {
        assert_eq!(parse_topics("4370,4383-4400"), [(4370, 4370), (4383, 4400)]);
        assert_eq!(parse_topics(""), []);
        assert_eq!(topics_string(&[(4370, 4370), (4383, 4400)]), "4370,4383-4400");
    }

    #[test]
    fn etws_back_to_its_channel() {
        let info = Props::from([("EmergencyType".into(), ov("Tsunami")), ("EmergencyAlert".into(), ov(true)), ("Popup".into(), ov(false))]);
        let m = CbmData::emergency("津波警報".into(), &info).unwrap();
        assert_eq!((m.channel, m.message_code), (4353, 1 << 9));
        let unknown = Props::from([("EmergencyType".into(), ov("Meteor"))]);
        assert!(CbmData::emergency(String::new(), &unknown).is_none());
    }
}
