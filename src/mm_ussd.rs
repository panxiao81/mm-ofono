//! `org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd`, backed by
//! `org.ofono.SupplementaryServices`.

use std::sync::Mutex;

use zbus::{Connection, OwnedVariant, Variant, interface, object_server::SignalEmitter};

use crate::error::Result;
use crate::ofono::SERVICE;

pub const SUPPLEMENTARY: &str = "org.ofono.SupplementaryServices";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UssdStatus {
    pub state: u32,
    pub network_notification: String,
    pub network_request: String,
}

/// Behind a mutex: a network round trip can take seconds and mustn't hold
/// the interface lock against the modem task's updates.
#[derive(Debug, Default)]
pub struct Ussd {
    pub ofono_path: String,
    pub status: Mutex<UssdStatus>,
}

impl Ussd {
    pub async fn apply(&self, em: &SignalEmitter<'_>, new: UssdStatus) -> zbus::Result<()> {
        let old = std::mem::replace(&mut *self.status.lock().unwrap(), new.clone());
        if old.state != new.state {
            self.state_changed(em).await?;
        }
        if old.network_notification != new.network_notification {
            self.network_notification_changed(em).await?;
        }
        if old.network_request != new.network_request {
            self.network_request_changed(em).await?;
        }
        Ok(())
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd")]
impl Ussd {
    async fn initiate(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, command: &str) -> Result<String> {
        crate::auth::check(conn, &header, crate::auth::USSD).await?;
        let (kind, value): (String, OwnedVariant) = conn
            .call_method(Some(SERVICE), self.ofono_path.as_str(), Some(SUPPLEMENTARY), "Initiate", &(command,))
            .await?
            .body()
            .deserialize()?;
        Ok(match &*value {
            Variant::Str(s) if kind == "USSD" => s.to_string(),
            // oFono handled a supplementary service code itself (e.g. *#21#);
            // show what it found rather than nothing.
            other => format!("{kind}: {other}"),
        })
    }

    async fn respond(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, response: &str) -> Result<String> {
        crate::auth::check(conn, &header, crate::auth::USSD).await?;
        Ok(conn
            .call_method(Some(SERVICE), self.ofono_path.as_str(), Some(SUPPLEMENTARY), "Respond", &(response,))
            .await?
            .body()
            .deserialize()?)
    }

    async fn cancel(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::USSD).await?;
        conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(SUPPLEMENTARY), "Cancel", &()).await?;
        Ok(())
    }

    #[zbus(property)]
    async fn state(&self) -> u32 {
        self.status.lock().unwrap().state
    }

    #[zbus(property)]
    async fn network_notification(&self) -> String {
        self.status.lock().unwrap().network_notification.clone()
    }

    #[zbus(property)]
    async fn network_request(&self) -> String {
        self.status.lock().unwrap().network_request.clone()
    }
}
