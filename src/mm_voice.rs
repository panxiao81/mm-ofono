//! `org.freedesktop.ModemManager1.Modem.Voice`, backed by
//! `org.ofono.VoiceCallManager`. Ended calls stay listed until DeleteCall,
//! as in ModemManager.

use std::collections::HashMap;

use zbus::{Connection, ObjectServer, OwnedObjectPath, OwnedVariant, Variant, interface, object_server::SignalEmitter};

use crate::derive::VOICE;
use crate::error::{CoreError, Result};
use crate::mm::{MMCallDirection as dir, MMCallState as st};
use crate::mm_call::{self, Call};
use crate::ofono::{self, SERVICE};
use crate::voice::{CallData, Calls};

pub const CALL_SETTINGS: &str = "org.ofono.CallSettings";

#[derive(Debug, Default)]
pub struct Voice {
    pub ofono_path: String,
    pub calls: Vec<OwnedObjectPath>,
    pub emergency_only: bool,
    pub registry: Calls,
}

impl Voice {
    /// Export a call and announce it.
    pub async fn add_call(&mut self, server: &ObjectServer, em: &SignalEmitter<'_>, data: CallData, ofono_call: Option<String>) -> zbus::Result<OwnedObjectPath> {
        let path = mm_call::next_path();
        let call = Call {
            ofono_path: self.ofono_path.clone(),
            calls: self.registry.clone(),
            data: data.into(),
            ofono_call: ofono_call.clone().into(),
        };
        server.at(&path, call).await?;
        if let Some(o) = ofono_call {
            self.registry.lock().unwrap().by_ofono.insert(o, path.clone());
        }
        self.calls.push(path.clone());
        self.calls_changed(em).await?;
        Self::call_added(em, path.clone()).await?;
        Ok(path)
    }

    async fn manager(&self, conn: &Connection, method: &str) -> Result<()> {
        conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(VOICE), method, &()).await?;
        Ok(())
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Voice")]
impl Voice {
    async fn list_calls(&self) -> Vec<OwnedObjectPath> {
        self.calls.clone()
    }

    /// Deleting a call that is still up hangs it up first.
    async fn delete_call(
        &mut self, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        path: OwnedObjectPath,
    ) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        let Some(i) = self.calls.iter().position(|c| *c == path) else {
            return Err(CoreError::Failed(format!("no call {path}")));
        };
        let call = server.interface::<_, Call>(&path).await?;
        let (live, ofono_call) = {
            let c = call.get().await;
            let live = c.data.lock().unwrap().state != st::MM_CALL_STATE_TERMINATED as i32;
            (live, c.ofono_call.lock().unwrap().clone())
        };
        if let Some(o) = ofono_call {
            if live {
                conn.call_method(Some(SERVICE), o.as_str(), Some(crate::voice::VOICE_CALL), "Hangup", &()).await?;
            }
            self.registry.lock().unwrap().by_ofono.remove(&o);
        }
        server.remove::<Call, _>(&path).await?;
        self.calls.remove(i);
        self.calls_changed(&em).await?;
        Self::call_deleted(&em, path).await?;
        Ok(())
    }

    async fn create_call(
        &mut self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        properties: HashMap<String, OwnedVariant>,
    ) -> Result<OwnedObjectPath> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        let number = match properties.get("number").map(|v| &**v) {
            Some(Variant::Str(n)) if !n.is_empty() => n.to_string(),
            _ => return Err(CoreError::Failed("missing 'number'".into())),
        };
        let data = CallData { direction: dir::MM_CALL_DIRECTION_OUTGOING as i32, number, ..Default::default() };
        Ok(self.add_call(server, &em, data, None).await?)
    }

    async fn hold_and_accept(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.manager(conn, "HoldAndAnswer").await
    }

    async fn hangup_and_accept(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.manager(conn, "ReleaseAndAnswer").await
    }

    async fn hangup_all(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.manager(conn, "HangupAll").await
    }

    async fn transfer(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.manager(conn, "Transfer").await
    }

    async fn call_waiting_setup(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, enable: bool) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        let value = if enable { "enabled" } else { "disabled" };
        Ok(ofono::set_property(conn, &self.ofono_path, CALL_SETTINGS, "VoiceCallWaiting", value.into()).await?)
    }

    async fn call_waiting_query(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<bool> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        let props = ofono::get_properties(conn, &self.ofono_path, CALL_SETTINGS).await?;
        Ok(ofono::str(&props, "VoiceCallWaiting") == Some("enabled"))
    }

    #[zbus(signal)]
    async fn call_added(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_deleted(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(property)]
    async fn calls(&self) -> Vec<OwnedObjectPath> {
        self.calls.clone()
    }

    #[zbus(property)]
    async fn emergency_only(&self) -> bool {
        self.emergency_only
    }
}
