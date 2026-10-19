//! `org.freedesktop.ModemManager1.Modem.Messaging`. oFono doesn't store
//! messages, so the list holds what arrived or was created since start.

use std::collections::HashMap;

use zbus::{Connection, ObjectServer, OwnedObjectPath, OwnedVariant, Variant, interface, object_server::SignalEmitter};

use crate::error::{CoreError, Result};
use crate::mm::{MMSmsPduType as pdu, MMSmsState as state, MMSmsStorage as storage};
use crate::mm_sms::{self, Sms, SmsData};

#[derive(Debug, Default)]
pub struct Messaging {
    pub ofono_path: String,
    pub messages: Vec<OwnedObjectPath>,
}

impl Messaging {
    async fn add(&mut self, server: &ObjectServer, em: &SignalEmitter<'_>, data: SmsData, received: bool) -> zbus::Result<OwnedObjectPath> {
        let path = mm_sms::next_path();
        server.at(&path, Sms::new(&self.ofono_path, data)).await?;
        self.messages.push(path.clone());
        self.messages_changed(em).await?;
        Self::added(em, path.clone(), received).await?;
        Ok(path)
    }
}

/// Publish a message oFono received on the modem object at `modem`.
pub async fn deliver(conn: &Connection, modem: &OwnedObjectPath, data: SmsData) -> zbus::Result<()> {
    let server = conn.object_server();
    let iface = server.interface::<_, Messaging>(modem).await?;
    let em = iface.signal_emitter().clone();
    iface.get_mut().await.add(&server, &em, data, true).await?;
    Ok(())
}

fn str_prop(props: &HashMap<String, OwnedVariant>, key: &str) -> Option<String> {
    match &**props.get(key)? {
        Variant::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Messaging")]
impl Messaging {
    async fn list(&self) -> Vec<OwnedObjectPath> {
        self.messages.clone()
    }

    async fn set_default_storage(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _storage: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MESSAGING).await?;
        Err(CoreError::Unsupported("oFono keeps no message storage".into()))
    }

    async fn delete(
        &mut self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        path: OwnedObjectPath,
    ) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MESSAGING).await?;
        let Some(i) = self.messages.iter().position(|p| *p == path) else {
            return Err(CoreError::Failed(format!("no message {path}")));
        };
        self.messages.remove(i);
        server.remove::<Sms, _>(&path).await?;
        self.messages_changed(&em).await?;
        Self::deleted(&em, path).await?;
        Ok(())
    }

    /// Composes a message; Sms.Send sends it.
    async fn create(
        &mut self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        properties: HashMap<String, OwnedVariant>,
    ) -> Result<OwnedObjectPath> {
        crate::auth::check(conn, &header, crate::auth::MESSAGING).await?;
        let number = str_prop(&properties, "number").ok_or_else(|| CoreError::Failed("missing 'number'".into()))?;
        let text = str_prop(&properties, "text").unwrap_or_default();
        let data = match properties.get("data").map(|v| &**v) {
            Some(Variant::Array(a)) => a.iter().filter_map(|b| if let Variant::U8(b) = b { Some(*b) } else { None }).collect(),
            _ => vec![],
        };
        if text.is_empty() && data.is_empty() {
            return Err(CoreError::Failed("missing 'text' or 'data'".into()));
        }
        let sms = SmsData {
            state: state::MM_SMS_STATE_UNKNOWN,
            pdu_type: pdu::MM_SMS_PDU_TYPE_SUBMIT,
            number,
            text,
            data,
            smsc: str_prop(&properties, "smsc").unwrap_or_default(),
            class: properties.get("class").and_then(|v| i32::try_from(v.try_clone().ok()?).ok()).unwrap_or(-1),
            delivery_report_request: properties
                .get("delivery-report-request")
                .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(false),
            timestamp: String::new(),
        };
        Ok(self.add(server, &em, sms, false).await?)
    }

    #[zbus(signal)]
    async fn added(em: &SignalEmitter<'_>, path: OwnedObjectPath, received: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deleted(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(property)]
    async fn messages(&self) -> Vec<OwnedObjectPath> {
        self.messages.clone()
    }

    #[zbus(property)]
    async fn supported_storages(&self) -> Vec<u32> {
        vec![]
    }

    #[zbus(property)]
    async fn default_storage(&self) -> u32 {
        storage::MM_SMS_STORAGE_UNKNOWN
    }
}
