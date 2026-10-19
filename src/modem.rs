//! One task per oFono modem. It owns the modem's cached oFono state, applies
//! oFono signals and GetProperties replies strictly one at a time, and after
//! each one brings the ModemManager objects in line with that state.

use std::sync::atomic::{AtomicU32, Ordering};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};
use zbus::{Connection, OwnedObjectPath};

use crate::derive::{self, CONNMAN, NETREG, OfonoModem, RADIO, SIM_MANAGER, VOICE};
use crate::device::{self, PhysicalDevice};
use crate::mm::MMModemStateChangeReason as reason;
use crate::mm_3gpp::Modem3gpp;
use crate::mm_modem::Modem;
use crate::mm_sim::Sim;
use crate::ofono::{self, Change, MODEM, Props};
use crate::{MM_PATH, objects};

/// oFono interfaces whose properties feed the ModemManager objects.
const TRACKED: [&str; 5] = [SIM_MANAGER, NETREG, RADIO, VOICE, CONNMAN];

// ModemManager never reuses object indexes.
static NEXT_MODEM: AtomicU32 = AtomicU32::new(0);
static NEXT_SIM: AtomicU32 = AtomicU32::new(0);

pub enum Event {
    Snapshot(String, Props),
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
}

impl Task {
    async fn run(mut self, props: Props, mut rx: mpsc::UnboundedReceiver<Event>) {
        // Subscribe before reading anything, so no change can slip between.
        let changes = match ofono::property_changes(&self.conn, &self.path).await {
            Ok(c) => c,
            Err(e) => return warn!("{}: can't watch oFono: {e}", self.path),
        };
        let mut changes = std::pin::pin!(changes);

        self.ofono.ifaces.entry(MODEM.into()).or_default().apply_snapshot(props);
        self.fetch(MODEM); // the announced properties may predate the subscription
        self.interfaces_changed();
        self.sync().await;

        loop {
            tokio::select! {
                Some(change) = changes.next() => self.change(change),
                Some(event) = rx.recv() => match event {
                    Event::Snapshot(iface, props) => self.snapshot(&iface, props),
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

    fn change(&mut self, c: Change) {
        let Some(iface) = self.ofono.ifaces.get_mut(&c.iface) else { return };
        let interfaces = c.iface == MODEM && c.key == "Interfaces";
        iface.change(c.key, c.value);
        if interfaces {
            self.interfaces_changed();
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
            } else if !present {
                self.ofono.ifaces.remove(iface);
            }
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

        let status = derive::status(&self.ofono);
        let identity = derive::identity(&self.ofono, &self.path, self.device.as_ref());
        let status_3gpp = derive::status_3gpp(&self.ofono);
        let sim_path = sim.as_ref().and(self.sim.clone());
        match &self.modem {
            None => {
                let path = OwnedObjectPath::try_from(format!("{MM_PATH}/Modem/{}", NEXT_MODEM.fetch_add(1, Ordering::Relaxed)))?;
                info!("{}: exporting {path} (state {})", self.path, status.state);
                let ofono_path = self.path.clone();
                server.at(&path, Modem { ofono_path: ofono_path.clone(), sim: sim_path, status, identity }).await?;
                server.at(&path, Modem3gpp { ofono_path, status: status_3gpp }).await?;
                objects::announce_modem(&self.conn, &path).await?;
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
                if old != new {
                    info!("{}: state {old} -> {new}", self.path);
                    Modem::emit_state_changed(iface.signal_emitter(), old, new, reason::MM_MODEM_STATE_CHANGE_REASON_UNKNOWN).await?;
                }
            }
        }

        // Only now that the modem no longer points at it.
        if sim.is_none() {
            if let Some(path) = self.sim.take() {
                server.remove::<Sim, _>(&path).await?;
            }
        }
        Ok(())
    }

    async fn unexport(&mut self) -> zbus::Result<()> {
        if let Some(path) = self.modem.take() {
            info!("{}: removing {path}", self.path);
            objects::remove_modem(&self.conn, &path).await?;
        }
        if let Some(path) = self.sim.take() {
            self.conn.object_server().remove::<Sim, _>(&path).await?;
        }
        Ok(())
    }
}
