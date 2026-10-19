//! One task per oFono modem. It owns the modem's cached oFono state, applies
//! oFono signals and GetProperties replies strictly one at a time, and after
//! each one brings the ModemManager objects in line with that state.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, Ordering},
};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};
use zbus::{Connection, OwnedObjectPath};

use crate::derive::{self, CONNMAN, NETREG, OfonoModem, RADIO, SIM_MANAGER, VOICE};
use crate::device::{self, PhysicalDevice};
use crate::bearer::{CONTEXT, Contexts};
use crate::mm::{MMCallDirection, MMModemState as state, MMModemStateChangeReason as reason};
use crate::mm_bearer::{Bearer, Simple};
use crate::mm_3gpp::Modem3gpp;
use crate::mm_messaging::{self, Messaging};
use crate::mm_modem::Modem;
use crate::mm_signal::ModemSignal;
use crate::mm_sms::SmsData;
use crate::mm_call::Call;
use crate::mm_ussd::{SUPPLEMENTARY, Ussd, UssdStatus};
use crate::mm_voice::Voice;
use crate::voice::{self, Calls, VOICE_CALL};
use crate::signal::{self, CELL, CELL_INFO, Cell};
use crate::mm_sim::Sim;
use crate::ofono::{self, MODEM, Props, Signal};
use crate::{MM_PATH, objects};

/// oFono interfaces whose properties feed the ModemManager objects.
const TRACKED: [&str; 6] = [SIM_MANAGER, NETREG, RADIO, VOICE, CONNMAN, SUPPLEMENTARY];

// ModemManager never reuses object indexes.
static NEXT_MODEM: AtomicU32 = AtomicU32::new(0);
static NEXT_SIM: AtomicU32 = AtomicU32::new(0);

pub enum Event {
    Snapshot(String, Props),
    Contexts(Vec<(String, Props)>),
    /// Signal.Setup asks to subscribe to (true) or drop cell info.
    CellInfo(bool),
    Cell(String, Cell),
    Calls(Vec<(String, Props)>),
    Removed,
}

/// Start the task for the oFono modem at `path`, announced with `props`.
/// Send [`Event::Removed`] to stop it; it then unexports its objects.
pub fn spawn(conn: Connection, path: String, props: Props) -> mpsc::UnboundedSender<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    let task = Task {
        conn,
        path,
        ofono: OfonoModem::default(),
        events: tx.clone(),
        modem: None,
        sim: None,
        device: None,
        pending_sms: vec![],
        contexts: Contexts::default(),
        cells: HashMap::new(),
        ussd_notification: String::new(),
        ussd_request: String::new(),
        voice_calls: HashMap::new(),
        disconnect_reasons: HashMap::new(),
        calls: Calls::default(),
    };
    tokio::spawn(task.run(props, rx));
    tx
}

struct Task {
    conn: Connection,
    path: String,
    ofono: OfonoModem,
    /// For GetProperties replies to come back through the same queue.
    events: mpsc::UnboundedSender<Event>,
    modem: Option<OwnedObjectPath>,
    sim: Option<OwnedObjectPath>,
    device: Option<PhysicalDevice>,
    /// Messages that arrived before the modem was exported.
    pending_sms: Vec<SmsData>,
    /// oFono connection contexts by path, shared with the bearers.
    contexts: Contexts,
    /// Cells reported by CellInfo while subscribed.
    cells: HashMap<String, Cell>,
    /// The last network-initiated USSD texts (not oFono properties).
    ussd_notification: String,
    ussd_request: String,
    /// oFono voice calls by path, and the MM call each one maps to.
    voice_calls: HashMap<String, Props>,
    disconnect_reasons: HashMap<String, String>,
    calls: Calls,
}

impl Task {
    async fn run(mut self, props: Props, mut rx: mpsc::UnboundedReceiver<Event>) {
        // Subscribe before reading anything, so no change can slip between.
        let signals = match ofono::signals(&self.conn, &self.path).await {
            Ok(s) => s,
            Err(e) => return warn!("{}: can't watch oFono: {e}", self.path),
        };
        let mut signals = std::pin::pin!(signals);

        self.ofono.ifaces.entry(MODEM.into()).or_default().apply_snapshot(props);
        self.fetch(MODEM); // the announced properties may predate the subscription
        self.interfaces_changed();
        self.sync().await;

        loop {
            tokio::select! {
                Some(signal) = signals.next() => self.signal(signal).await,
                Some(event) = rx.recv() => match event {
                    Event::Snapshot(iface, props) => self.snapshot(&iface, props),
                    Event::Contexts(list) => self.contexts.lock().unwrap().extend(list),
                    Event::CellInfo(on) => self.cell_info(on).await,
                    Event::Cell(path, cell) => drop(self.cells.insert(path, cell)),
                    Event::Calls(list) => self.voice_calls.extend(list),
                    Event::Removed => break,
                },
                else => break,
            }
            self.sync().await;
        }
        if let Err(e) = self.unexport().await {
            warn!("{}: unexport: {e}", self.path);
        }
    }

    fn fetch(&self, iface: &str) {
        let (conn, path, iface, events) = (self.conn.clone(), self.path.clone(), iface.to_owned(), self.events.clone());
        tokio::spawn(async move {
            let props = ofono::get_properties(&conn, &path, &iface).await.unwrap_or_else(|e| {
                // Most likely the interface went away before the reply (its removal
                // comes as a change too). Either way, carry on with what we know
                // rather than holding the modem back forever.
                warn!("{path}: GetProperties {iface}: {e}");
                Props::new()
            });
            let _ = events.send(Event::Snapshot(iface, props));
        });
    }

    fn fetch_contexts(&self) {
        let (conn, path, events) = (self.conn.clone(), self.path.clone(), self.events.clone());
        tokio::spawn(async move {
            let reply = conn
                .call_method(Some(ofono::SERVICE), path.as_str(), Some(CONNMAN), "GetContexts", &())
                .await
                .and_then(|r| r.body().deserialize::<Vec<(OwnedObjectPath, Props)>>());
            match reply {
                Ok(list) => drop(events.send(Event::Contexts(list.into_iter().map(|(p, c)| (p.to_string(), c)).collect()))),
                Err(e) => warn!("{path}: GetContexts: {e}"),
            }
        });
    }

    /// Calls already up when we start (e.g. mm-ofono restarted mid-call).
    fn fetch_calls(&self) {
        let (conn, path, events) = (self.conn.clone(), self.path.clone(), self.events.clone());
        tokio::spawn(async move {
            let reply = conn.call_method(Some(ofono::SERVICE), path.as_str(), Some(VOICE), "GetCalls", &()).await;
            match reply.and_then(|r| r.body().deserialize::<Vec<(OwnedObjectPath, Props)>>()) {
                Ok(list) => drop(events.send(Event::Calls(list.into_iter().map(|(p, c)| (p.to_string(), c)).collect()))),
                Err(e) => warn!("{path}: GetCalls: {e}"),
            }
        });
    }

    /// oFono dropped a call: the MM call ends and stays listed until deleted.
    async fn call_removed(&mut self, ofono_call: &str) {
        self.voice_calls.remove(ofono_call);
        let reason = self.disconnect_reasons.remove(ofono_call);
        let Some(mm) = self.calls.lock().unwrap().by_ofono.remove(ofono_call) else { return };
        let server = self.conn.object_server();
        if let Ok(call) = server.interface::<_, Call>(&mm).await {
            let c = call.get().await;
            let ended = voice::ended(&c.data.lock().unwrap().clone(), reason.as_deref());
            if let Err(e) = c.apply(call.signal_emitter(), ended).await {
                warn!("{}: ending {mm}: {e}", self.path);
            }
        }
    }

    /// Bring MM calls in line with oFono's: update mapped ones, adopt the
    /// one being dialled through Call.Start, create the rest (incoming, or
    /// dialled by another oFono client).
    async fn sync_calls(&self, modem: &OwnedObjectPath) -> zbus::Result<()> {
        let server = self.conn.object_server();
        let voice = server.interface::<_, Voice>(modem).await?;
        for (ofono_call, props) in &self.voice_calls {
            let fresh = voice::call_data(props, None);
            let outgoing = fresh.direction == MMCallDirection::MM_CALL_DIRECTION_OUTGOING as i32;
            let mm = self.calls.lock().unwrap().resolve(ofono_call, &fresh.number, outgoing);
            match mm {
                Some(mm) => {
                    let call = server.interface::<_, Call>(&mm).await?;
                    let c = call.get().await;
                    c.ofono_call.lock().unwrap().get_or_insert_with(|| ofono_call.clone());
                    let prev = c.data.lock().unwrap().clone();
                    c.apply(call.signal_emitter(), voice::call_data(props, Some(&prev))).await?;
                }
                None => {
                    let em = voice.signal_emitter().clone();
                    voice.get_mut().await.add_call(&server, &em, fresh, Some(ofono_call.clone())).await?;
                }
            }
        }
        Ok(())
    }

    /// Subscribe to CellInfo (GetCells does that, per connection) or drop it.
    async fn cell_info(&mut self, on: bool) {
        let method = if on { "GetCells" } else { "Unsubscribe" };
        let reply = self.conn.call_method(Some(ofono::SERVICE), self.path.as_str(), Some(CELL_INFO), method, &()).await;
        match reply {
            Ok(r) if on => {
                // Cells that already exist (another subscriber) aren't announced again.
                for path in r.body().deserialize::<Vec<OwnedObjectPath>>().unwrap_or_default() {
                    self.fetch_cell(path.as_str());
                }
            }
            Ok(_) => self.cells.clear(),
            Err(e) => warn!("{}: CellInfo.{method}: {e}", self.path),
        }
    }

    fn fetch_cell(&self, path: &str) {
        let (conn, path, events) = (self.conn.clone(), path.to_owned(), self.events.clone());
        tokio::spawn(async move {
            let reply = conn.call_method(Some(ofono::SERVICE), path.as_str(), Some(CELL), "GetAll", &()).await;
            match reply.and_then(|r| r.body().deserialize::<(i32, String, bool, Props)>()) {
                Ok((_, kind, registered, props)) => drop(events.send(Event::Cell(path, Cell { kind, registered, props }))),
                Err(e) => warn!("{path}: Cell.GetAll: {e}"),
            }
        });
    }

    async fn signal(&mut self, signal: Signal) {
        match signal {
            Signal::Property { path, iface, key, value } if path == self.path => {
                let Some(i) = self.ofono.ifaces.get_mut(&iface) else { return };
                let interfaces = iface == MODEM && key == "Interfaces";
                i.change(key, value);
                if interfaces {
                    self.interfaces_changed();
                }
            }
            Signal::Property { path, iface, key, value } if iface == CONTEXT => {
                if let Some(ctx) = self.contexts.lock().unwrap().get_mut(&path) {
                    ctx.insert(key, value);
                }
            }
            Signal::Property { path, iface, key, value } if iface == CELL => {
                if let Some(cell) = self.cells.get_mut(&path) {
                    cell.props.insert(key, value);
                }
            }
            Signal::Property { path, iface, key, value } if iface == VOICE_CALL => {
                if let Some(call) = self.voice_calls.get_mut(&path) {
                    call.insert(key, value);
                }
            }
            Signal::Property { .. } => {}
            Signal::Sms { text, info, immediate } => self.received_sms(text, info, immediate).await,
            Signal::CallAdded { path, props } => drop(self.voice_calls.insert(path, props)),
            Signal::DisconnectReason { path, reason } => drop(self.disconnect_reasons.insert(path, reason)),
            Signal::CallRemoved { path } => self.call_removed(&path).await,
            Signal::UssdNotification(text) => self.ussd_notification = text,
            Signal::UssdRequest(text) => self.ussd_request = text,
            Signal::CellsAdded(paths) => paths.iter().for_each(|p| self.fetch_cell(p)),
            Signal::CellsRemoved(paths) => paths.iter().for_each(|p| drop(self.cells.remove(p))),
            Signal::ContextAdded { path, props } => drop(self.contexts.lock().unwrap().insert(path, props)),
            Signal::ContextRemoved { path } => drop(self.contexts.lock().unwrap().remove(&path)),
        }
    }

    fn snapshot(&mut self, iface: &str, props: Props) {
        if let Some(i) = self.ofono.ifaces.get_mut(iface) {
            i.apply_snapshot(props);
        }
    }

    /// Track interfaces oFono adds, forget the ones it drops.
    fn interfaces_changed(&mut self) {
        let listed = self.ofono.strings(MODEM, "Interfaces");
        for iface in TRACKED {
            let present = listed.iter().any(|i| i == iface);
            if present && !self.ofono.has(iface) {
                self.ofono.ifaces.insert(iface.into(), Default::default());
                self.fetch(iface);
                if iface == CONNMAN {
                    self.fetch_contexts();
                }
                if iface == VOICE {
                    self.fetch_calls();
                }
            } else if !present {
                self.ofono.ifaces.remove(iface);
                if iface == CONNMAN {
                    self.contexts.lock().unwrap().clear();
                }
            }
        }
    }

    async fn received_sms(&mut self, text: String, info: Props, immediate: bool) {
        let sms = SmsData::received(text, &info, immediate);
        // Like ModemManager, no personal data (numbers, text) in the log.
        info!("{}: SMS received", self.path);
        match &self.modem {
            Some(path) => {
                if let Err(e) = mm_messaging::deliver(&self.conn, path, sms).await {
                    warn!("{}: delivering SMS: {e}", self.path);
                }
            }
            None => self.pending_sms.push(sms),
        }
    }

    async fn sync(&mut self) {
        if let Err(e) = self.try_sync().await {
            warn!("{}: {e}", self.path);
        }
    }

    async fn try_sync(&mut self) -> zbus::Result<()> {
        if !self.ofono.ready() {
            return self.unexport().await;
        }
        if self.device.is_none() {
            self.device = device::discover().await;
        }
        let server = self.conn.object_server();

        // The SIM goes first: clients read it as soon as they see the modem.
        let sim = derive::sim(&self.ofono);
        match (&self.sim, &sim) {
            (None, Some(status)) => {
                let path = OwnedObjectPath::try_from(format!("{MM_PATH}/SIM/{}", NEXT_SIM.fetch_add(1, Ordering::Relaxed)))?;
                server.at(&path, Sim { ofono_path: self.path.clone(), status: status.clone() }).await?;
                self.sim = Some(path);
            }
            (Some(path), Some(status)) => {
                let iface = server.interface::<_, Sim>(path).await?;
                objects::update(&self.conn, &iface, |s| s.status = status.clone()).await?;
            }
            _ => {}
        }

        let mut status = derive::status(&self.ofono);
        if status.state == state::MM_MODEM_STATE_REGISTERED && self.bearer_connected().await {
            status.state = state::MM_MODEM_STATE_CONNECTED;
        }
        let identity = derive::identity(&self.ofono, &self.path, self.device.as_ref());
        let status_3gpp = derive::status_3gpp(&self.ofono);
        let status_locked = status.state == state::MM_MODEM_STATE_LOCKED;
        let sim_path = sim.as_ref().and(self.sim.clone());
        match &self.modem {
            None => {
                let path = OwnedObjectPath::try_from(format!("{MM_PATH}/Modem/{}", NEXT_MODEM.fetch_add(1, Ordering::Relaxed)))?;
                info!("{}: exporting {path} (state {})", self.path, status.state);
                let ofono_path = self.path.clone();
                let contexts = self.contexts.clone();
                server
                    .at(&path, Modem { ofono_path: ofono_path.clone(), sim: sim_path, status, identity, bearers: vec![], contexts: contexts.clone() })
                    .await?;
                server.at(&path, Modem3gpp { ofono_path: ofono_path.clone(), status: status_3gpp }).await?;
                server.at(&path, Messaging { ofono_path: ofono_path.clone(), messages: vec![] }).await?;
                server.at(&path, Simple { ofono_path, contexts }).await?;
                server.at(&path, ModemSignal { task: Some(self.events.clone()), ..Default::default() }).await?;
                server.at(&path, Ussd { ofono_path: self.path.clone(), ..Default::default() }).await?;
                server.at(&path, Voice { ofono_path: self.path.clone(), registry: self.calls.clone(), ..Default::default() }).await?;
                objects::announce_modem(&self.conn, &path).await?;
                for sms in std::mem::take(&mut self.pending_sms) {
                    mm_messaging::deliver(&self.conn, &path, sms).await?;
                }
                self.modem = Some(path);
            }
            Some(path) => {
                let iface = server.interface::<_, Modem>(path).await?;
                let (old, new) = (iface.get().await.status.state, status.state);
                objects::update(&self.conn, &iface, |m| {
                    m.sim = sim_path;
                    m.status = status;
                    m.identity = identity;
                })
                .await?;
                let iface_3gpp = server.interface::<_, Modem3gpp>(path).await?;
                objects::update(&self.conn, &iface_3gpp, |m| m.status = status_3gpp).await?;
                for bearer in iface.get().await.bearers.clone() {
                    let b = server.interface::<_, Bearer>(&bearer).await?;
                    b.get().await.refresh(b.signal_emitter()).await?;
                }
                let ussd = UssdStatus {
                    state: derive::ussd_state(&self.ofono),
                    network_notification: self.ussd_notification.clone(),
                    network_request: self.ussd_request.clone(),
                };
                let iface_ussd = server.interface::<_, Ussd>(path).await?;
                iface_ussd.get().await.apply(iface_ussd.signal_emitter(), ussd).await?;
                let emergency_only = !self.ofono.sim_present() || status_locked;
                let iface_voice = server.interface::<_, Voice>(path).await?;
                objects::update(&self.conn, &iface_voice, |v| v.emergency_only = emergency_only).await?;
                let values = signal::values(self.cells.values());
                let iface_signal = server.interface::<_, ModemSignal>(path).await?;
                objects::update(&self.conn, &iface_signal, |s| s.values = values).await?;
                if old != new {
                    info!("{}: state {old} -> {new}", self.path);
                    Modem::emit_state_changed(iface.signal_emitter(), old, new, reason::MM_MODEM_STATE_CHANGE_REASON_UNKNOWN).await?;
                }
            }
        }

        if let Some(modem) = self.modem.clone() {
            self.sync_calls(&modem).await?;
        }

        // Only now that the modem no longer points at it.
        if sim.is_none() {
            if let Some(path) = self.sim.take() {
                server.remove::<Sim, _>(&path).await?;
            }
        }
        Ok(())
    }

    /// Whether a bearer of this modem has its context active.
    async fn bearer_connected(&self) -> bool {
        let Some(path) = &self.modem else { return false };
        let server = self.conn.object_server();
        let Ok(modem) = server.interface::<_, Modem>(path).await else { return false };
        for bearer in modem.get().await.bearers.clone() {
            let Ok(b) = server.interface::<_, Bearer>(&bearer).await else { continue };
            let context = b.get().await.context.clone();
            if self.contexts.lock().unwrap().get(&context).and_then(|c| ofono::bool(c, "Active")) == Some(true) {
                return true;
            }
        }
        false
    }

    async fn unexport(&mut self) -> zbus::Result<()> {
        if let Some(path) = self.modem.take() {
            info!("{}: removing {path}", self.path);
            let server = self.conn.object_server();
            if let Ok(modem) = server.interface::<_, Modem>(&path).await {
                for bearer in modem.get().await.bearers.clone() {
                    server.remove::<Bearer, _>(&bearer).await?;
                }
            }
            if let Ok(voice) = server.interface::<_, Voice>(&path).await {
                for call in voice.get().await.calls.clone() {
                    server.remove::<Call, _>(&call).await?;
                }
            }
            *self.calls.lock().unwrap() = Default::default();
            objects::remove_modem(&self.conn, &path).await?;
        }
        if let Some(path) = self.sim.take() {
            self.conn.object_server().remove::<Sim, _>(&path).await?;
        }
        Ok(())
    }
}
