//! WAP push (MMS notifications) from oFono's `org.ofono.PushNotification`.
//!
//! oFono hands SMS datagrams for the WAP push port (2948) only to a
//! registered agent, never to `IncomingMessage`. ModemManager shows such a
//! message as an `Sms` with `Data` and no text, which is what mmsd-tng
//! reads, so the modem task registers this agent and turns each push into
//! one.

use tokio::sync::mpsc;
use tracing::warn;
use zbus::{Connection, interface, message::Header};

use crate::modem::Event;
use crate::ofono::{self, Props};

pub const PUSH: &str = "org.ofono.PushNotification";

/// The agent object for the oFono modem `ofono_path` (`/ril_0` ->
/// `.../PushAgent/ril_0`).
pub fn agent_path(ofono_path: &str) -> String {
    format!("{}/PushAgent{ofono_path}", crate::MM_PATH)
}

pub struct Agent {
    pub events: mpsc::UnboundedSender<Event>,
}

/// Whether the caller is oFono itself: anyone on the bus could call the
/// agent and plant a fake MMS notification.
async fn from_ofono(conn: &Connection, header: &Header<'_>) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else { return false };
    let Ok(owner) = dbus.get_name_owner(ofono::SERVICE.try_into().unwrap()).await else { return false };
    header.sender().is_some_and(|s| *s == *owner)
}

#[interface(name = "org.ofono.PushNotificationAgent")]
impl Agent {
    async fn receive_notification(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        notification: Vec<u8>,
        info: Props,
    ) -> zbus::fdo::Result<()> {
        if !from_ofono(conn, &header).await {
            return Err(zbus::fdo::Error::AccessDenied("only oFono may deliver pushes".into()));
        }
        drop(self.events.send(Event::Push(notification, info)));
        Ok(())
    }

    /// oFono dropped the agent (modem off, SMS atom gone); no unregister due.
    async fn release(&self, #[zbus(connection)] conn: &Connection, #[zbus(header)] header: Header<'_>) {
        if from_ofono(conn, &header).await {
            drop(self.events.send(Event::PushReleased));
        }
    }
}

/// Export the agent and register it with oFono; returns whether oFono took it.
pub async fn register(conn: &Connection, ofono_path: &str, events: mpsc::UnboundedSender<Event>) -> bool {
    let path = agent_path(ofono_path);
    if let Err(e) = conn.object_server().at(path.as_str(), Agent { events }).await {
        warn!("{ofono_path}: exporting the push agent: {e}");
        return false;
    }
    let path = zbus::ObjectPath::try_from(path.as_str()).unwrap();
    match conn.call_method(Some(ofono::SERVICE), ofono_path, Some(PUSH), "RegisterAgent", &(path,)).await {
        Ok(_) => true,
        Err(e) => {
            warn!("{ofono_path}: PushNotification.RegisterAgent: {e}");
            false
        }
    }
}

/// Unregister (if oFono still has it) and unexport the agent.
pub async fn unregister(conn: &Connection, ofono_path: &str, registered: bool) {
    let path = agent_path(ofono_path);
    if registered {
        let p = zbus::ObjectPath::try_from(path.as_str()).unwrap();
        drop(conn.call_method(Some(ofono::SERVICE), ofono_path, Some(PUSH), "UnregisterAgent", &(p,)).await);
    }
    drop(conn.object_server().remove::<Agent, _>(path.as_str()).await);
}
