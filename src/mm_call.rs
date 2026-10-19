//! `org.freedesktop.ModemManager1.Call` for one oFono voice call.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use zbus::{Connection, OwnedObjectPath, OwnedVariant, interface, object_server::SignalEmitter};

use crate::derive::VOICE;
use crate::error::{CoreError, Result};
use crate::mm::{MMCallDirection as dir, MMCallState as st, MMCallStateReason as why};
use crate::ofono::SERVICE;
use crate::voice::{CallData, Calls, VOICE_CALL};

static NEXT_CALL: AtomicU32 = AtomicU32::new(0);

pub fn next_path() -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("{}/Call/{}", crate::MM_PATH, NEXT_CALL.fetch_add(1, Ordering::Relaxed))).unwrap()
}

/// Data behind a mutex: Start and Accept wait on the network and mustn't
/// hold the interface lock against the modem task's updates.
#[derive(Debug, Default)]
pub struct Call {
    /// The oFono modem, e.g. /ril_0.
    pub ofono_path: String,
    pub calls: Calls,
    pub data: Mutex<CallData>,
    /// The oFono VoiceCall once there is one.
    pub ofono_call: Mutex<Option<String>>,
}

impl Call {
    fn get(&self) -> CallData {
        self.data.lock().unwrap().clone()
    }

    fn ofono_call(&self) -> Result<String> {
        self.ofono_call.lock().unwrap().clone().ok_or_else(|| CoreError::Failed("the call hasn't started".into()))
    }

    /// Take new data, announcing changed properties and StateChanged.
    pub async fn apply(&self, em: &SignalEmitter<'_>, new: CallData) -> zbus::Result<()> {
        let old = std::mem::replace(&mut *self.data.lock().unwrap(), new.clone());
        if old.state != new.state {
            self.state_changed(em).await?;
            Self::emit_state_changed(em, old.state, new.state, new.reason as u32).await?;
        }
        if old.reason != new.reason {
            self.state_reason_changed(em).await?;
        }
        if old.direction != new.direction {
            self.direction_changed(em).await?;
        }
        if old.number != new.number {
            self.number_changed(em).await?;
        }
        if old.multiparty != new.multiparty {
            self.multiparty_changed(em).await?;
        }
        Ok(())
    }

    async fn voice_call(&self, conn: &Connection, method: &str, args: &(impl serde::Serialize + zbus::DynamicType)) -> Result<()> {
        conn.call_method(Some(SERVICE), self.ofono_call()?.as_str(), Some(VOICE_CALL), method, args).await?;
        Ok(())
    }

    async fn manager(&self, conn: &Connection, method: &str, args: &(impl serde::Serialize + zbus::DynamicType)) -> Result<()> {
        conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(VOICE), method, args).await?;
        Ok(())
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Call")]
impl Call {
    /// Dial a call made with Voice.CreateCall.
    async fn start(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        let d = self.get();
        if d.state != st::MM_CALL_STATE_UNKNOWN as i32 || d.direction != dir::MM_CALL_DIRECTION_OUTGOING as i32 {
            return Err(CoreError::Failed("only a new outgoing call can be started".into()));
        }
        let me = OwnedObjectPath::from(em.path().to_owned());
        self.calls.lock().unwrap().dialling.push((d.number.clone(), me.clone()));
        let dialled: zbus::Result<OwnedObjectPath> = async {
            conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(VOICE), "Dial", &(d.number.as_str(), ""))
                .await?
                .body()
                .deserialize()
        }
        .await;
        let ofono = {
            let mut calls = self.calls.lock().unwrap();
            calls.dialling.retain(|(_, mm)| *mm != me);
            let ofono = dialled?.to_string();
            calls.by_ofono.insert(ofono.clone(), me);
            ofono
        };
        *self.ofono_call.lock().unwrap() = Some(ofono);
        // The modem task may already have moved it on from oFono's signals.
        if self.get().state == st::MM_CALL_STATE_UNKNOWN as i32 {
            let dialing = CallData {
                state: st::MM_CALL_STATE_DIALING as i32,
                reason: why::MM_CALL_STATE_REASON_OUTGOING_STARTED as i32,
                ..d
            };
            self.apply(&em, dialing).await?;
        }
        Ok(())
    }

    async fn accept(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        match self.get().state as u32 {
            st::MM_CALL_STATE_RINGING_IN => self.voice_call(conn, "Answer", &()).await,
            st::MM_CALL_STATE_WAITING => self.manager(conn, "HoldAndAnswer", &()).await,
            _ => Err(CoreError::Failed("the call isn't ringing".into())),
        }
    }

    async fn deflect(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, number: &str) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.voice_call(conn, "Deflect", &(number,)).await
    }

    async fn join_multiparty(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(VOICE), "CreateMultiparty", &()).await?;
        Ok(())
    }

    async fn leave_multiparty(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        let call = OwnedObjectPath::try_from(self.ofono_call()?)?;
        conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(VOICE), "PrivateChat", &(call,)).await?;
        Ok(())
    }

    async fn hangup(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.voice_call(conn, "Hangup", &()).await
    }

    async fn send_dtmf(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, dtmf: &str) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::VOICE).await?;
        self.manager(conn, "SendTones", &(dtmf,)).await
    }

    /// oFono doesn't report received DTMF; declared for the API.
    #[zbus(signal)]
    async fn dtmf_received(em: &SignalEmitter<'_>, dtmf: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "StateChanged")]
    async fn emit_state_changed(em: &SignalEmitter<'_>, old: i32, new: i32, reason: u32) -> zbus::Result<()>;

    #[zbus(property)]
    async fn state(&self) -> i32 {
        self.get().state
    }

    #[zbus(property)]
    async fn state_reason(&self) -> i32 {
        self.get().reason
    }

    #[zbus(property)]
    async fn direction(&self) -> i32 {
        self.get().direction
    }

    #[zbus(property)]
    async fn number(&self) -> String {
        self.get().number
    }

    #[zbus(property)]
    async fn multiparty(&self) -> bool {
        self.get().multiparty
    }

    /// Call audio is routed by the platform (vendor audio HAL), not here.
    #[zbus(property)]
    async fn audio_port(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    async fn audio_format(&self) -> HashMap<String, OwnedVariant> {
        HashMap::new()
    }
}
