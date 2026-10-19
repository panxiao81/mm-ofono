//! `org.freedesktop.ModemManager1.Bearer` and `...Modem.Simple`, backed by
//! oFono connection contexts (see bearer.rs for the mapping).

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use zbus::{Connection, ObjectServer, OwnedObjectPath, OwnedVariant, interface, object_server::SignalEmitter};

use crate::bearer::{self, CONTEXT, Contexts, Request, Status};
use crate::derive::{CONNMAN, NETREG, SIM_MANAGER};
use crate::error::{CoreError, Result};
use crate::mm::MMBearerType as bearer_type;
use crate::mm_3gpp::Modem3gpp;
use crate::mm_modem::Modem;
use crate::ofono::{self, Props, SERVICE};

static NEXT_BEARER: AtomicU32 = AtomicU32::new(0);

type Dict = HashMap<String, OwnedVariant>;

#[derive(Debug, Default)]
pub struct Bearer {
    /// The oFono modem, e.g. /ril_0, and the context this bearer drives.
    pub ofono_path: String,
    pub context: String,
    pub request: Request,
    pub contexts: Contexts,
    /// Behind a mutex so a slow Connect doesn't hold the interface lock.
    pub status: Mutex<Status>,
}

impl Bearer {
    /// Re-derive the status from the shared context cache and announce what changed.
    pub async fn refresh(&self, em: &SignalEmitter<'_>) -> zbus::Result<()> {
        let new = self.contexts.lock().unwrap().get(&self.context).map(bearer::status).unwrap_or_else(Status::disconnected);
        let old = std::mem::replace(&mut *self.status.lock().unwrap(), new.clone());
        if old.connected != new.connected {
            self.connected_changed(em).await?;
        }
        if old.interface != new.interface {
            self.interface_changed(em).await?;
        }
        if old.ip4 != new.ip4 {
            self.ip4_config_changed(em).await?;
        }
        if old.ip6 != new.ip6 {
            self.ip6_config_changed(em).await?;
        }
        Ok(())
    }

    /// Store a fresh GetProperties of the context, then refresh.
    async fn reload(&self, conn: &Connection, em: &SignalEmitter<'_>) -> Result<()> {
        let props = self.context_props(conn).await?;
        self.contexts.lock().unwrap().insert(self.context.clone(), props);
        Ok(self.refresh(em).await?)
    }

    pub fn is_connected(&self) -> bool {
        self.status.lock().unwrap().connected
    }

    async fn context_props(&self, conn: &Connection) -> Result<Props> {
        Ok(ofono::get_properties(conn, &self.context, CONTEXT).await?)
    }

    /// oFono only brings up data on the default data SIM; connecting on a
    /// modem makes its SIM that one, then waits for packet attach.
    async fn make_data_modem(&self, conn: &Connection) -> Result<()> {
        // Returns the path as a string, "" when no modem has data.
        let current: std::result::Result<String, _> = async {
            conn.call_method(Some(SERVICE), "/", Some("org.nemomobile.ofono.ModemManager"), "GetDefaultDataModem", &())
                .await?
                .body()
                .deserialize::<String>()
        }
        .await;
        let current = match current {
            Ok(c) => c,
            // Only a missing interface means "not Sailfish oFono, nothing to choose".
            Err(zbus::Error::MethodError(name, ..)) if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
                || name.as_str() == "org.freedesktop.DBus.Error.UnknownInterface" => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if current != self.ofono_path {
            let sim = ofono::get_properties(conn, &self.ofono_path, SIM_MANAGER).await?;
            let imsi = ofono::str(&sim, "SubscriberIdentity").unwrap_or_default().to_owned();
            conn.call_method(Some(SERVICE), "/", Some("org.nemomobile.ofono.ModemManager"), "SetDefaultDataSim", &(imsi,))
                .await?;
        }
        for _ in 0..60 {
            if ofono::bool(&ofono::get_properties(conn, &self.ofono_path, CONNMAN).await?, "Attached") == Some(true) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(CoreError::Failed("no packet service attach".into()))
    }

    pub async fn connect_inner(&self, conn: &Connection, em: &SignalEmitter<'_>) -> Result<()> {
        let ctx = self.context_props(conn).await?;
        if ofono::bool(&ctx, "Active") == Some(true) {
            return self.reload(conn, em).await;
        }

        let netreg = ofono::get_properties(conn, &self.ofono_path, NETREG).await.unwrap_or_default();
        if !matches!(ofono::str(&netreg, "Status"), Some("registered" | "roaming")) {
            return Err(CoreError::WrongState("not registered with a network".into()));
        }
        if ofono::str(&netreg, "Status") == Some("roaming") {
            if !self.request.allow_roaming {
                return Err(CoreError::Failed("roaming and the bearer doesn't allow it".into()));
            }
            let connman = ofono::get_properties(conn, &self.ofono_path, CONNMAN).await?;
            if ofono::bool(&connman, "RoamingAllowed") == Some(false) {
                ofono::set_property(conn, &self.ofono_path, CONNMAN, "RoamingAllowed", true.into()).await?;
            }
        }
        self.make_data_modem(conn).await?;

        for (key, value) in self.request.context_settings(&ctx) {
            ofono::set_property(conn, &self.context, CONTEXT, key, value.as_str().into()).await?;
        }
        // oFono replies once the context is up (or failed to come up).
        ofono::set_property(conn, &self.context, CONTEXT, "Active", true.into()).await?;
        // Clients read Interface/Ip*Config right after Connect returns.
        self.reload(conn, em).await
    }

    pub async fn disconnect_inner(&self, conn: &Connection, em: &SignalEmitter<'_>) -> Result<()> {
        if self.is_connected() {
            ofono::set_property(conn, &self.context, CONTEXT, "Active", false.into()).await?;
        }
        self.reload(conn, em).await
    }
}

/// What Simple.Connect does first, as in ModemManager: a disabled modem is
/// enabled, then registration is awaited (clients such as NetworkManager
/// rely on this instead of calling Enable themselves).
async fn bring_up(conn: &Connection, server: &ObjectServer, modem: &zbus::ObjectPath<'_>) -> Result<()> {
    use crate::mm::MMModemState as st;
    let iface = server.interface::<_, Modem>(modem).await?;
    let (state, ofono_path) = {
        let m = iface.get().await;
        (m.status.state, m.ofono_path.clone())
    };
    match state {
        st::MM_MODEM_STATE_LOCKED => return Err(CoreError::WrongState("the SIM is locked".into())),
        st::MM_MODEM_STATE_FAILED => return Err(CoreError::WrongState("the modem has failed (no SIM?)".into())),
        st::MM_MODEM_STATE_DISABLED => {
            ofono::set_property(conn, &ofono_path, crate::ofono::MODEM, "Powered", true.into()).await?;
            ofono::set_property(conn, &ofono_path, crate::ofono::MODEM, "Online", true.into()).await?;
        }
        _ => {}
    }
    for _ in 0..120 {
        if iface.get().await.status.state >= st::MM_MODEM_STATE_REGISTERED {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(CoreError::WrongState("not registered with a network".into()))
}

/// Export a bearer for `req` on the oFono modem `ofono_path`, on a fitting
/// context (added if none fits). The caller lists it on the modem.
pub async fn new_bearer(conn: &Connection, server: &ObjectServer, ofono_path: &str, contexts: &Contexts, req: Request) -> Result<OwnedObjectPath> {
    let listed: Vec<(OwnedObjectPath, Props)> = match conn.call_method(Some(SERVICE), ofono_path, Some(CONNMAN), "GetContexts", &()).await {
        Ok(reply) => reply.body().deserialize()?,
        // oFono only offers ConnectionManager once the radio is up and packet service exists.
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
                || name.as_str() == "org.freedesktop.DBus.Error.UnknownInterface" =>
        {
            return Err(CoreError::WrongState("no packet data service yet".into()));
        }
        Err(e) => return Err(e.into()),
    };
    let listed: Vec<(String, Props)> = listed.into_iter().map(|(p, props)| (p.to_string(), props)).collect();
    let context = match bearer::choose_context(&req, &listed) {
        Some(c) => c.to_owned(),
        None => conn
            .call_method(Some(SERVICE), ofono_path, Some(CONNMAN), "AddContext", &(req.context_type(),))
            .await?
            .body()
            .deserialize::<OwnedObjectPath>()?
            .to_string(),
    };

    let path = OwnedObjectPath::try_from(format!("{}/Bearer/{}", crate::MM_PATH, NEXT_BEARER.fetch_add(1, Ordering::Relaxed)))?;
    server
        .at(
            &path,
            Bearer {
                ofono_path: ofono_path.to_owned(),
                context,
                request: req,
                contexts: contexts.clone(),
                status: Default::default(),
            },
        )
        .await?;
    let iface = server.interface::<_, Bearer>(&path).await?;
    iface.get().await.reload(conn, iface.signal_emitter()).await?;
    Ok(path)
}

#[interface(name = "org.freedesktop.ModemManager1.Bearer")]
impl Bearer {
    async fn connect(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        self.connect_inner(conn, &em).await
    }

    async fn disconnect(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        self.disconnect_inner(conn, &em).await
    }

    #[zbus(property)]
    async fn interface(&self) -> String {
        self.status.lock().unwrap().interface.clone()
    }

    #[zbus(property)]
    async fn connected(&self) -> bool {
        self.is_connected()
    }

    #[zbus(property)]
    async fn connection_error(&self) -> (String, String) {
        (String::new(), String::new())
    }

    #[zbus(property)]
    async fn suspended(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn multiplexed(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn ip4_config(&self) -> Dict {
        bearer::clone_dict(&self.status.lock().unwrap().ip4)
    }

    #[zbus(property)]
    async fn ip6_config(&self) -> Dict {
        bearer::clone_dict(&self.status.lock().unwrap().ip6)
    }

    #[zbus(property)]
    async fn stats(&self) -> Dict {
        Dict::new()
    }

    #[zbus(property)]
    async fn reload_stats_supported(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn ip_timeout(&self) -> u32 {
        20
    }

    #[zbus(property)]
    async fn bearer_type(&self) -> u32 {
        bearer_type::MM_BEARER_TYPE_DEFAULT
    }

    /// MM_3GPP_PROFILE_ID_UNKNOWN (a #define, so not in the generated enums).
    #[zbus(property)]
    async fn profile_id(&self) -> i32 {
        -1
    }

    #[zbus(property)]
    async fn properties(&self) -> Dict {
        self.request.to_dict()
    }
}

/// `org.freedesktop.ModemManager1.Modem.Simple`.
#[derive(Debug, Default)]
pub struct Simple {
    pub ofono_path: String,
    pub contexts: Contexts,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Simple")]
impl Simple {
    /// Reuse the modem's bearer for the same APN and type, or create one,
    /// and connect it.
    async fn connect(
        &self, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        properties: Dict,
    ) -> Result<OwnedObjectPath> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let req = Request::from_dict(&properties);
        bring_up(conn, server, em.path()).await?;
        let modem = server.interface::<_, Modem>(em.path()).await?;
        let existing = modem.get().await.bearers.clone();
        let mut found = None;
        for path in existing {
            let b = server.interface::<_, Bearer>(&path).await?;
            let b = b.get().await;
            if b.request.apn == req.apn && b.request.context_type() == req.context_type() {
                found = Some(path);
                break;
            }
        }
        let path = match found {
            Some(p) => p,
            None => {
                let p = new_bearer(conn, server, &self.ofono_path, &self.contexts, req).await?;
                let mut m = modem.get_mut().await;
                m.bearers.push(p.clone());
                m.bearers_changed(modem.signal_emitter()).await?;
                p
            }
        };
        let b = server.interface::<_, Bearer>(&path).await?;
        b.get().await.connect_inner(conn, b.signal_emitter()).await?;
        Ok(path)
    }

    /// "/" disconnects every bearer of the modem.
    async fn disconnect(
        &self, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        bearer: OwnedObjectPath,
    ) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let all = server.interface::<_, Modem>(em.path()).await?.get().await.bearers.clone();
        let targets: Vec<_> = if bearer.as_str() == "/" { all } else { all.into_iter().filter(|p| *p == bearer).collect() };
        if targets.is_empty() {
            return Err(CoreError::Failed(format!("no bearer {bearer}")));
        }
        for path in targets {
            let b = server.interface::<_, Bearer>(&path).await?;
            b.get().await.disconnect_inner(conn, b.signal_emitter()).await?;
        }
        Ok(())
    }

    async fn get_status(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> Result<Dict> {
        let v = |x: zbus::Variant<'_>| OwnedVariant::try_from(x).unwrap();
        let m = server.interface::<_, Modem>(em.path()).await?;
        let m = m.get().await;
        let g = server.interface::<_, Modem3gpp>(em.path()).await?;
        let g = g.get().await;
        Ok(Dict::from([
            ("state".into(), v((m.status.state as u32).into())),
            ("signal-quality".into(), v(zbus::Variant::from(m.status.signal_quality))),
            ("current-bands".into(), v(zbus::Variant::from(Vec::<u32>::new()))),
            ("access-technologies".into(), v(m.status.access_technologies.into())),
            ("m3gpp-registration-state".into(), v(g.status.registration_state.into())),
            ("m3gpp-operator-code".into(), v(g.status.operator_code.as_str().into())),
            ("m3gpp-operator-name".into(), v(g.status.operator_name.as_str().into())),
        ]))
    }
}
