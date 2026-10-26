//! A private D-Bus daemon and a fake oFono that replays recorded traces
//! (see dev/trace2replay.py).

#![allow(dead_code)]

use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

use serde_json::Value as Json;
use zbus::{
    Connection, ObjectServer, connection, interface,
    object_server::{Interface, SignalEmitter},
    {ObjectPath, OwnedObjectPath, OwnedVariant, Variant},
};

/// A throwaway session-style bus; the daemon dies with this value.
pub struct Bus {
    daemon: Child,
    pub address: String,
}

impl Bus {
    pub fn start() -> Bus {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut address).unwrap();
        Bus { daemon, address: address.trim().to_owned() }
    }

    pub async fn connect(&self) -> Connection {
        connection::Builder::address(self.address.as_str()).build().await.unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
    }
}

type Props = HashMap<String, OwnedVariant>;

fn copy(props: &Props) -> Props {
    props.iter().map(|(k, v)| (k.clone(), v.try_clone().unwrap())).collect()
}

/// An oFono interface: a bag of properties with GetProperties, SetProperty
/// and PropertyChanged.
trait Bag: Interface + Default {
    fn props(&mut self) -> &mut Props;
    async fn emit(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;
}

macro_rules! bag {
    ($ty:ident, $name:literal) => {
        #[derive(Default)]
        pub struct $ty {
            props: Props,
        }

        #[interface(name = $name)]
        impl $ty {
            fn get_properties(&self) -> Props {
                copy(&self.props)
            }

            async fn set_property(
                &mut self,
                #[zbus(signal_emitter)] em: SignalEmitter<'_>,
                name: String,
                value: OwnedVariant,
            ) -> zbus::fdo::Result<()> {
                self.props.insert(name.clone(), value.try_clone().map_err(zbus::Error::from)?);
                Self::property_changed(&em, &name, &value).await?;
                Ok(())
            }

            #[zbus(signal)]
            async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;
        }

        impl Bag for $ty {
            fn props(&mut self) -> &mut Props {
                &mut self.props
            }
            async fn emit(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()> {
                Self::property_changed(em, name, value).await
            }
        }
    };
}

bag!(OfonoModem, "org.ofono.Modem");
bag!(SimManager, "org.ofono.SimManager");
bag!(NetworkRegistration, "org.ofono.NetworkRegistration");
bag!(RadioSettings, "org.ofono.RadioSettings");
bag!(CellBroadcast, "org.ofono.CellBroadcast");

bag!(Message, "org.ofono.Message");

/// oFono's MessageManager: incoming messages are pushed by the test;
/// SendMessage creates a message object that goes pending -> sent (or
/// failed when the text is "FAIL").
#[derive(Default)]
pub struct MessageManager {
    pub sent: Vec<(String, String)>,
    count: u32,
}

#[interface(name = "org.ofono.MessageManager")]
impl MessageManager {
    async fn send_message(
        &mut self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        to: String,
        text: String,
    ) -> OwnedObjectPath {
        self.count += 1;
        let path = OwnedObjectPath::try_from(format!("{}/message_{}", em.path(), self.count)).unwrap();
        conn.object_server().at(&path, Message::default()).await.unwrap();
        let outcome = if text == "FAIL" { "failed" } else { "sent" };
        self.sent.push((to, text));
        let (conn, p) = (conn.clone(), path.clone());
        tokio::spawn(async move {
            let em = SignalEmitter::new(&conn, p.clone()).unwrap();
            for state in ["pending", outcome] {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Message::property_changed(&em, "State", &state.into()).await.unwrap();
            }
            conn.object_server().remove::<Message, _>(&p).await.unwrap();
        });
        path
    }

    #[zbus(signal)]
    pub async fn incoming_message(em: &SignalEmitter<'_>, message: &str, info: &Props) -> zbus::Result<()>;
}

/// oFono's ConnectionManager: a property bag plus contexts.
#[derive(Default)]
pub struct ConnectionManager {
    props: Props,
    pub contexts: Vec<OwnedObjectPath>,
}

#[interface(name = "org.ofono.ConnectionManager")]
impl ConnectionManager {
    fn get_properties(&self) -> Props {
        copy(&self.props)
    }

    async fn set_property(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>, name: String, value: OwnedVariant) -> zbus::fdo::Result<()> {
        self.props.insert(name.clone(), value.try_clone().map_err(zbus::Error::from)?);
        Self::property_changed(&em, &name, &value).await?;
        Ok(())
    }

    async fn get_contexts(&self, #[zbus(object_server)] server: &ObjectServer) -> Vec<(OwnedObjectPath, Props)> {
        let mut out = vec![];
        for path in &self.contexts {
            let ctx = server.interface::<_, ConnectionContext>(path).await.unwrap();
            out.push((path.clone(), copy(&ctx.get().await.props)));
        }
        out
    }

    async fn add_context(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        r#type: String,
    ) -> OwnedObjectPath {
        let path = OwnedObjectPath::try_from(format!("{}/context{}", em.path(), self.contexts.len() + 1)).unwrap();
        let ctx = ConnectionContext::new(&r#type, "");
        let props = copy(&ctx.props);
        server.at(&path, ctx).await.unwrap();
        self.contexts.push(path.clone());
        Self::context_added(&em, path.as_ref(), &props).await.unwrap();
        path
    }

    #[zbus(signal)]
    async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;

    async fn remove_context(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        path: OwnedObjectPath,
    ) -> zbus::fdo::Result<()> {
        let i = self.contexts.iter().position(|p| *p == path).ok_or_else(|| zbus::fdo::Error::Failed("no such context".into()))?;
        self.contexts.remove(i);
        server.remove::<ConnectionContext, _>(&path).await?;
        Self::context_removed(&em, path.as_ref()).await?;
        Ok(())
    }

    #[zbus(signal)]
    async fn context_added(em: &SignalEmitter<'_>, path: ObjectPath<'_>, props: &Props) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn context_removed(em: &SignalEmitter<'_>, path: ObjectPath<'_>) -> zbus::Result<()>;
}

impl Bag for ConnectionManager {
    fn props(&mut self) -> &mut Props {
        &mut self.props
    }
    async fn emit(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()> {
        Self::property_changed(em, name, value).await
    }
}

fn ov<'a>(v: impl Into<Variant<'a>>) -> OwnedVariant {
    OwnedVariant::try_from(v.into()).unwrap()
}

/// oFono's own errors, for what the fakes refuse.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.ofono.Error")]
pub enum OfonoError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NotAllowed(String),
}

/// A connection context that comes up like ofono-binder's: rmnet interface,
/// static /30 IPv4 and a /64 IPv6 address. Like the patched oFono, a
/// non-IMS context won't come up while roaming with RoamingAllowed off.
pub struct ConnectionContext {
    pub props: Props,
}

impl ConnectionContext {
    pub fn new(ty: &str, apn: &str) -> ConnectionContext {
        let props = Props::from([
            ("Type".to_owned(), ov(ty)),
            ("AccessPointName".to_owned(), ov(apn)),
            ("Active".to_owned(), ov(false)),
            ("Protocol".to_owned(), ov("dual")),
            ("AuthenticationMethod".to_owned(), ov("any")),
            ("Settings".to_owned(), ov(HashMap::<String, Variant>::new())),
            ("IPv6.Settings".to_owned(), ov(HashMap::<String, Variant>::new())),
        ]);
        ConnectionContext { props }
    }
}

#[interface(name = "org.ofono.ConnectionContext")]
impl ConnectionContext {
    fn get_properties(&self) -> Props {
        copy(&self.props)
    }

    async fn set_property(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        name: String,
        value: OwnedVariant,
    ) -> Result<(), OfonoError> {
        let mut changes = vec![(name.clone(), value.try_clone().map_err(zbus::Error::from)?)];
        if name == "Active" {
            let up = bool::try_from(value.try_clone().map_err(zbus::Error::from)?).unwrap_or(false);
            if up && self.props.get("Type").and_then(|v| String::try_from(v.try_clone().ok()?).ok()).as_deref() != Some("ims") {
                let modem = em.path().as_str().rsplit_once('/').unwrap().0.to_owned();
                let status = match server.interface::<_, NetworkRegistration>(modem.as_str()).await {
                    Ok(n) => n.get().await.props.get("Status").and_then(|v| String::try_from(v.try_clone().ok()?).ok()),
                    Err(_) => None,
                };
                let allowed = match server.interface::<_, ConnectionManager>(modem.as_str()).await {
                    Ok(c) => c.get().await.props.get("RoamingAllowed").and_then(|v| bool::try_from(v.try_clone().ok()?).ok()),
                    Err(_) => None,
                };
                if status.as_deref() == Some("roaming") && allowed != Some(true) {
                    return Err(OfonoError::NotAllowed("Operation is not allowed".into()));
                }
            }
            let (v4, v6): (HashMap<String, Variant>, HashMap<String, Variant>) = if up {
                (
                    HashMap::from([
                        ("Interface".into(), Variant::from("rmnet_data2")),
                        ("Method".into(), "static".into()),
                        ("Address".into(), "10.172.70.13".into()),
                        ("Netmask".into(), "255.255.255.252".into()),
                        ("Gateway".into(), "10.172.70.14".into()),
                        ("DomainNameServers".into(), Variant::from(vec!["1.1.1.1".to_owned()])),
                    ]),
                    HashMap::from([
                        ("Interface".into(), Variant::from("rmnet_data2")),
                        ("Address".into(), "240a:b:48fa:638a::1".into()),
                        ("PrefixLength".into(), 64u8.into()),
                    ]),
                )
            } else {
                (HashMap::new(), HashMap::new())
            };
            changes.push(("Settings".into(), ov(v4)));
            changes.push(("IPv6.Settings".into(), ov(v6)));
        }
        for (k, v) in changes {
            self.props.insert(k.clone(), v.try_clone().map_err(zbus::Error::from)?);
            Self::property_changed(&em, &k, &v).await?;
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;
}

/// oFono's VoiceCallManager. Dial announces the call (CallAdded) *before*
/// returning, the order that makes adopting the dialled call necessary.
#[derive(Default)]
pub struct VoiceCallManager {
    props: Props,
    pub calls: Vec<OwnedObjectPath>,
    pub dialled: Vec<String>,
    pub tones: Vec<String>,
    count: u32,
}

impl VoiceCallManager {
    async fn new_call(&mut self, conn: &Connection, em: &SignalEmitter<'_>, state: &str, number: &str) -> OwnedObjectPath {
        self.count += 1;
        let path = OwnedObjectPath::try_from(format!("{}/voicecall{:02}", em.path(), self.count)).unwrap();
        let call = VoiceCall { props: Props::from([("State".to_owned(), ov(state)), ("LineIdentification".to_owned(), ov(number)), ("Multiparty".to_owned(), ov(false))]) };
        let props = copy(&call.props);
        conn.object_server().at(&path, call).await.unwrap();
        self.calls.push(path.clone());
        Self::call_added(em, path.as_ref(), &props).await.unwrap();
        path
    }
}

#[interface(name = "org.ofono.VoiceCallManager")]
impl VoiceCallManager {
    fn get_properties(&self) -> Props {
        copy(&self.props)
    }

    async fn dial(&mut self, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>, number: String, _hide: String) -> OwnedObjectPath {
        self.dialled.push(number.clone());
        // oFono reports the number internationally formatted.
        let path = self.new_call(conn, &em, "dialing", &format!("+81{}", number.trim_start_matches('0'))).await;
        let (conn, p) = (conn.clone(), path.clone());
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            set_call_state(&conn, &p, "alerting").await;
        });
        path
    }

    async fn get_calls(&self, #[zbus(object_server)] server: &ObjectServer) -> Vec<(OwnedObjectPath, Props)> {
        let mut out = vec![];
        for c in &self.calls {
            out.push((c.clone(), copy(&server.interface::<_, VoiceCall>(c).await.unwrap().get().await.props)));
        }
        out
    }

    fn send_tones(&mut self, tones: String) {
        self.tones.push(tones);
    }

    #[zbus(signal)]
    async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_added(em: &SignalEmitter<'_>, path: ObjectPath<'_>, props: &Props) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_removed(em: &SignalEmitter<'_>, path: ObjectPath<'_>) -> zbus::Result<()>;
}

impl Bag for VoiceCallManager {
    fn props(&mut self) -> &mut Props {
        &mut self.props
    }
    async fn emit(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()> {
        Self::property_changed(em, name, value).await
    }
}

pub struct VoiceCall {
    props: Props,
}

#[interface(name = "org.ofono.VoiceCall")]
impl VoiceCall {
    fn get_properties(&self) -> Props {
        copy(&self.props)
    }

    async fn answer(&self, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) {
        let (conn, p) = (conn.clone(), OwnedObjectPath::from(em.path().to_owned()));
        tokio::spawn(async move { set_call_state(&conn, &p, "active").await });
    }

    async fn hangup(&self, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) {
        let (conn, p) = (conn.clone(), OwnedObjectPath::from(em.path().to_owned()));
        tokio::spawn(async move { end_call(&conn, &p, "local").await });
    }

    #[zbus(signal)]
    async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn disconnect_reason(em: &SignalEmitter<'_>, reason: &str) -> zbus::Result<()>;
}

async fn set_call_state(conn: &Connection, call: &OwnedObjectPath, state: &str) {
    let c = conn.object_server().interface::<_, VoiceCall>(call).await.unwrap();
    c.get_mut().await.props.insert("State".into(), ov(state));
    VoiceCall::property_changed(c.signal_emitter(), "State", &state.into()).await.unwrap();
}

/// What oFono does when a call ends: reason, disconnected, then removal.
async fn end_call(conn: &Connection, call: &OwnedObjectPath, reason: &str) {
    let server = conn.object_server();
    let c = server.interface::<_, VoiceCall>(call).await.unwrap();
    VoiceCall::disconnect_reason(c.signal_emitter(), reason).await.unwrap();
    set_call_state(conn, call, "disconnected").await;
    server.remove::<VoiceCall, _>(call).await.unwrap();
    let modem = call.as_str().rsplit_once('/').unwrap().0.to_owned();
    let vcm = server.interface::<_, VoiceCallManager>(modem.as_str()).await.unwrap();
    vcm.get_mut().await.calls.retain(|p| p != call);
    VoiceCallManager::call_removed(vcm.signal_emitter(), call.as_ref()).await.unwrap();
}

/// oFono's SupplementaryServices: "*100#" is a USSD balance query, "*#21#"
/// a call forwarding query oFono answers itself.
#[derive(Default)]
pub struct SupplementaryServices {
    props: Props,
}

impl SupplementaryServices {
    async fn set_state(&mut self, em: &SignalEmitter<'_>, state: &str) {
        self.props.insert("State".into(), ov(state));
        Self::property_changed(em, "State", &state.into()).await.unwrap();
    }
}

#[interface(name = "org.ofono.SupplementaryServices")]
impl SupplementaryServices {
    fn get_properties(&self) -> Props {
        copy(&self.props)
    }

    async fn initiate(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>, command: String) -> (String, OwnedVariant) {
        self.set_state(&em, "active").await;
        let reply = match command.as_str() {
            "*#21#" => ("CallForwarding".to_owned(), ov(("interrogation", "VoiceUnconditional"))),
            _ => ("USSD".to_owned(), ov("Balance: 100 JPY")),
        };
        self.set_state(&em, "idle").await;
        reply
    }

    async fn respond(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>, reply: String) -> String {
        self.set_state(&em, "idle").await;
        format!("You chose {reply}")
    }

    async fn cancel(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) {
        self.set_state(&em, "idle").await;
    }

    #[zbus(signal)]
    async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn request_received(em: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;
}

impl Bag for SupplementaryServices {
    fn props(&mut self) -> &mut Props {
        &mut self.props
    }
    async fn emit(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()> {
        Self::property_changed(em, name, value).await
    }
}

/// Sailfish CellInfo: subscribing (GetCells) first returns nothing, then
/// announces the cells, like the real one.
#[derive(Default)]
pub struct CellInfo {
    pub subscribed: bool,
    pub cells: Vec<OwnedObjectPath>,
    /// The serving cell's ci when next subscribed (default 0x26F1B0B).
    pub ci: Option<i32>,
}

#[interface(name = "org.nemomobile.ofono.CellInfo")]
impl CellInfo {
    async fn get_cells(&mut self, #[zbus(connection)] conn: &Connection, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> Vec<OwnedObjectPath> {
        if !self.subscribed {
            self.subscribed = true;
            let cell = OwnedObjectPath::try_from(format!("{}/cell_0", em.path())).unwrap();
            conn.object_server()
                .at(&cell, Cell { kind: "lte".into(), registered: true, props: Props::from([
                    ("rsrp".to_owned(), ov(67i32)),
                    ("rsrq".to_owned(), ov(9i32)),
                    ("signalStrength".to_owned(), ov(31i32)),
                    ("mcc".to_owned(), ov(440i32)),
                    ("mnc".to_owned(), ov(51i32)),
                    ("tac".to_owned(), ov(0x964Fi32)),
                    ("ci".to_owned(), ov(self.ci.unwrap_or(0x26F1B0B))),
                ]) })
                .await
                .unwrap();
            self.cells = vec![cell.clone()];
            let em = em.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                CellInfo::cells_added(&em, &[cell]).await.unwrap();
            });
            return vec![];
        }
        self.cells.clone()
    }

    async fn unsubscribe(&mut self, #[zbus(object_server)] server: &ObjectServer, #[zbus(signal_emitter)] em: SignalEmitter<'_>) {
        self.subscribed = false;
        for c in std::mem::take(&mut self.cells) {
            server.remove::<Cell, _>(&c).await.unwrap();
            CellInfo::cells_removed(&em, &[c]).await.unwrap();
        }
    }

    #[zbus(signal)]
    async fn cells_added(em: &SignalEmitter<'_>, paths: &[OwnedObjectPath]) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn cells_removed(em: &SignalEmitter<'_>, paths: &[OwnedObjectPath]) -> zbus::Result<()>;
}

pub struct Cell {
    kind: String,
    registered: bool,
    props: Props,
}

#[interface(name = "org.nemomobile.ofono.Cell")]
impl Cell {
    fn get_all(&self) -> (i32, String, bool, Props) {
        (1, self.kind.clone(), self.registered, copy(&self.props))
    }

    #[zbus(signal)]
    pub async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: &Variant<'_>) -> zbus::Result<()>;
}

/// PolicyKit: allows or refuses everything, and records what was asked.
#[derive(Default)]
pub struct Polkit {
    pub refuse: bool,
    /// (subject bus name, action) per check.
    pub checks: Vec<(String, String)>,
}

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Polkit {
    fn check_authorization(
        &mut self,
        subject: (String, HashMap<String, OwnedVariant>),
        action_id: String,
        _details: HashMap<String, String>,
        _flags: u32,
        _cancellation_id: String,
    ) -> (bool, bool, HashMap<String, String>) {
        let name = String::try_from(subject.1["name"].try_clone().unwrap()).unwrap();
        self.checks.push((name, action_id));
        (!self.refuse, false, HashMap::new())
    }
}

/// oFono's WAP push agent registration: one agent, as in plugins/push-notification.c.
#[derive(Default)]
pub struct PushNotification {
    /// (bus name, object path) of the registered agent.
    pub agent: Option<(String, OwnedObjectPath)>,
}

#[interface(name = "org.ofono.PushNotification")]
impl PushNotification {
    fn register_agent(&mut self, #[zbus(header)] header: zbus::message::Header<'_>, path: OwnedObjectPath) -> zbus::fdo::Result<()> {
        if self.agent.is_some() {
            return Err(zbus::fdo::Error::Failed("busy".into()));
        }
        self.agent = Some((header.sender().unwrap().to_string(), path));
        Ok(())
    }

    fn unregister_agent(&mut self, path: OwnedObjectPath) -> zbus::fdo::Result<()> {
        match &self.agent {
            Some((_, p)) if *p == path => Ok(self.agent = None),
            _ => Err(zbus::fdo::Error::Failed("no such agent".into())),
        }
    }
}

/// Sailfish oFono's data SIM selection.
#[derive(Default)]
pub struct NemoModemManager {
    pub data_modem: String,
    pub data_sim_set: Vec<String>,
}

#[interface(name = "org.nemomobile.ofono.ModemManager")]
impl NemoModemManager {
    /// A string, not an object path, and "" when no modem has data — as on
    /// the real Sailfish oFono.
    fn get_default_data_modem(&self) -> String {
        self.data_modem.clone()
    }

    /// Like the real phone, a SIM without a voice path only registers once it
    /// is the data SIM: the fake's searching /ril_0 then registers.
    async fn set_default_data_sim(&mut self, #[zbus(object_server)] server: &ObjectServer, imsi: String) {
        self.data_sim_set.push(imsi);
        // ponytail: the fake has one SIM-bearing modem
        self.data_modem = "/ril_0".into();
        if let Ok(netreg) = server.interface::<_, NetworkRegistration>("/ril_0").await {
            let searching = netreg.get().await.props.get("Status").and_then(|v| String::try_from(v.try_clone().ok()?).ok()) == Some("searching".into());
            if searching {
                netreg.get_mut().await.props.insert("Status".into(), ov("roaming"));
                NetworkRegistration::property_changed(netreg.signal_emitter(), "Status", &"roaming".into()).await.unwrap();
            }
        }
    }
}

#[derive(Default)]
pub struct Manager {
    modems: Vec<OwnedObjectPath>,
}

#[interface(name = "org.ofono.Manager")]
impl Manager {
    async fn get_modems(&self, #[zbus(object_server)] server: &ObjectServer) -> Vec<(OwnedObjectPath, Props)> {
        let mut out = vec![];
        for path in &self.modems {
            let modem = server.interface::<_, OfonoModem>(path).await.unwrap();
            out.push((path.clone(), copy(&modem.get_mut().await.props)));
        }
        out
    }

    #[zbus(signal)]
    async fn modem_added(em: &SignalEmitter<'_>, path: ObjectPath<'_>, props: &Props) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn modem_removed(em: &SignalEmitter<'_>, path: ObjectPath<'_>) -> zbus::Result<()>;
}

/// busctl's typed JSON (`{"type": "a{sy}", "data": ...}`) as a D-Bus value.
pub fn value(typed: &Json) -> OwnedVariant {
    let data = &typed["data"];
    let v: Variant = match typed["type"].as_str().unwrap() {
        "b" => data.as_bool().unwrap().into(),
        "s" => data.as_str().unwrap().into(),
        "y" => (data.as_u64().unwrap() as u8).into(),
        "q" => (data.as_u64().unwrap() as u16).into(),
        "u" => (data.as_u64().unwrap() as u32).into(),
        "as" => data.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect::<Vec<_>>().into(),
        "a{sy}" => data
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_u64().unwrap() as u8))
            .collect::<HashMap<_, _>>()
            .into(),
        "a(sy)" => data
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e[0].as_str().unwrap().to_owned(), e[1].as_u64().unwrap() as u8))
            .collect::<Vec<_>>()
            .into(),
        t => panic!("replay: unsupported type {t}"),
    };
    v.try_into().unwrap()
}

fn props(obj: &Json) -> Props {
    obj.as_object().unwrap().iter().map(|(k, v)| (k.clone(), value(v))).collect()
}

/// The fake oFono service, driven by a replay file.
pub struct FakeOfono {
    pub conn: Connection,
}

impl FakeOfono {
    pub async fn start(bus: &Bus) -> FakeOfono {
        let conn = bus.connect().await;
        conn.object_server().at("/", Manager::default()).await.unwrap();
        conn.object_server().at("/", NemoModemManager::default()).await.unwrap();
        for modem in ["/ril_0", "/ril_1"] {
            conn.object_server().at(modem, PushNotification::default()).await.unwrap();
        }
        conn.request_name("org.ofono").await.unwrap();
        conn.object_server().at("/org/freedesktop/PolicyKit1/Authority", Polkit::default()).await.unwrap();
        conn.request_name("org.freedesktop.PolicyKit1").await.unwrap();
        FakeOfono { conn }
    }

    async fn with<B: Bag>(&self, path: &str, f: impl FnOnce(&mut Props)) -> SignalEmitter<'static> {
        let server = self.conn.object_server();
        if server.interface::<_, B>(path).await.is_err() {
            server.at(path, B::default()).await.unwrap();
        }
        let iface = server.interface::<_, B>(path).await.unwrap();
        f(iface.get_mut().await.props());
        iface.signal_emitter().clone()
    }

    async fn update(&self, path: &str, iface: &str, props: Props, emit: bool) {
        macro_rules! dispatch {
            ($($ty:ident),*) => {
                match iface {
                    $(n if n == <$ty as Interface>::name().as_str() => {
                        let changes: Vec<_> = props.iter().map(|(k, v)| (k.clone(), v.try_clone().unwrap())).collect();
                        let em = self.with::<$ty>(path, |p| p.extend(props)).await;
                        if emit {
                            for (k, v) in changes {
                                <$ty as Bag>::emit(&em, &k, &v).await.unwrap();
                            }
                        }
                    })*
                    _ => {} // an interface the bridge doesn't use
                }
            };
        }
        dispatch!(OfonoModem, SimManager, NetworkRegistration, RadioSettings, VoiceCallManager, ConnectionManager, SupplementaryServices, CellBroadcast);
    }

    /// Plays `events` in order. An interface that appears gets the props of
    /// its first recorded snapshot as its initial state (real oFono doesn't
    /// signal those, they only show up when someone calls GetProperties),
    /// minus keys signalled in between.
    pub async fn replay(&self, events: &[Json], mut after_each: impl AsyncFnMut()) {
        let server = self.conn.object_server();
        let manager = server.interface::<_, Manager>("/").await.unwrap();

        for (i, e) in events.iter().enumerate() {
            if let Some(path) = e["added"].as_str() {
                self.update(path, "org.ofono.Modem", props(&e["props"]), false).await;
                let modem = server.interface::<_, OfonoModem>(path).await.unwrap();
                let announced = copy(&modem.get_mut().await.props);
                let path = OwnedObjectPath::try_from(path).unwrap();
                manager.get_mut().await.modems.push(path.clone());
                Manager::modem_added(manager.signal_emitter(), path.as_ref(), &announced).await.unwrap();
            } else if let Some(path) = e["removed"].as_str() {
                let known = manager.get_mut().await.modems.iter().position(|p| p.as_str() == path);
                if let Some(idx) = known {
                    manager.get_mut().await.modems.remove(idx);
                    Manager::modem_removed(manager.signal_emitter(), path.try_into().unwrap()).await.unwrap();
                }
            } else {
                let (path, iface) = (e["path"].as_str().unwrap(), e["iface"].as_str().unwrap());
                if let Some(snapshot) = e.get("snapshot") {
                    self.update(path, iface, props(snapshot), false).await;
                } else {
                    let (key, val) = (e["changed"][0].as_str().unwrap(), &e["changed"][1]);
                    self.update(path, iface, Props::from([(key.to_owned(), value(val))]), true).await;
                    if iface == "org.ofono.Modem" && key == "Interfaces" {
                        for listed in val["data"].as_array().unwrap() {
                            self.seed(path, listed.as_str().unwrap(), &events[i + 1..]).await;
                        }
                    }
                }
            }
            after_each().await;
        }
    }

    async fn seed(&self, path: &str, iface: &str, later: &[Json]) {
        let same = |e: &&Json| e["path"] == path && e["iface"] == iface;
        let Some(first) = later.iter().filter(same).find_map(|e| e.get("snapshot")) else { return };
        let mut initial = props(first);
        for e in later.iter().filter(same).take_while(|e| e.get("snapshot").is_none()) {
            initial.remove(e["changed"][0].as_str().unwrap());
        }
        // Only for interfaces that don't exist yet.
        let exists = {
            let server = self.conn.object_server();
            let n = iface;
            macro_rules! has { ($($ty:ident),*) => { false $(|| (n == <$ty as Interface>::name().as_str() && server.interface::<_, $ty>(path).await.is_ok()))* } }
            has!(SimManager, NetworkRegistration, RadioSettings, VoiceCallManager, ConnectionManager, SupplementaryServices, CellBroadcast)
        };
        if !exists {
            self.update(path, iface, initial, false).await;
        }
    }
}

impl FakeOfono {
    async fn message_manager(&self, path: &str) -> zbus::object_server::InterfaceRef<MessageManager> {
        let server = self.conn.object_server();
        if server.interface::<_, MessageManager>(path).await.is_err() {
            server.at(path, MessageManager::default()).await.unwrap();
        }
        server.interface::<_, MessageManager>(path).await.unwrap()
    }

    pub async fn incoming_sms(&self, path: &str, text: &str, sender: &str, sent_time: &str) {
        let mm = self.message_manager(path).await;
        let info = Props::from([
            ("Sender".to_owned(), OwnedVariant::try_from(Variant::from(sender)).unwrap()),
            ("SentTime".to_owned(), OwnedVariant::try_from(Variant::from(sent_time)).unwrap()),
        ]);
        MessageManager::incoming_message(mm.signal_emitter(), text, &info).await.unwrap();
    }

    /// Add a data context to the modem at `path` and mark it packet-attached.
    pub async fn add_context(&self, modem: &str, ty: &str, apn: &str) -> String {
        let server = self.conn.object_server();
        self.update(modem, "org.ofono.ConnectionManager", Props::from([("Attached".to_owned(), ov(true))]), true).await;
        let cm = server.interface::<_, ConnectionManager>(modem).await.unwrap();
        let path = OwnedObjectPath::try_from(format!("{modem}/context{}", cm.get().await.contexts.len() + 1)).unwrap();
        let ctx = ConnectionContext::new(ty, apn);
        let props = copy(&ctx.props);
        server.at(&path, ctx).await.unwrap();
        cm.get_mut().await.contexts.push(path.clone());
        ConnectionManager::context_added(cm.signal_emitter(), path.as_ref(), &props).await.unwrap();
        path.to_string()
    }

    pub async fn context(&self, path: &str) -> Props {
        let ctx = self.conn.object_server().interface::<_, ConnectionContext>(path).await.unwrap();
        copy(&ctx.get().await.props)
    }

    /// Set properties on an oFono interface of `modem`, announcing each.
    pub async fn set(&self, modem: &str, iface: &str, props: Vec<(&str, Variant<'static>)>) {
        let props = props.into_iter().map(|(k, v)| (k.to_owned(), ov(v))).collect();
        self.update(modem, iface, props, true).await;
    }

    pub async fn cell_info(&self, modem: &str) -> zbus::object_server::InterfaceRef<CellInfo> {
        let server = self.conn.object_server();
        if server.interface::<_, CellInfo>(modem).await.is_err() {
            server.at(modem, CellInfo::default()).await.unwrap();
        }
        server.interface::<_, CellInfo>(modem).await.unwrap()
    }

    /// Change a measurement of a subscribed cell.
    pub async fn cell_changed(&self, cell: &str, name: &str, value: i32) {
        let c = self.conn.object_server().interface::<_, Cell>(cell).await.unwrap();
        c.get_mut().await.props.insert(name.to_owned(), ov(value));
        Cell::property_changed(c.signal_emitter(), name, &value.into()).await.unwrap();
    }

    /// The network asks the user something (session waits for Respond).
    pub async fn ussd_request(&self, modem: &str, text: &str) {
        let ss = self.conn.object_server().interface::<_, SupplementaryServices>(modem).await.unwrap();
        let em = ss.signal_emitter().clone();
        ss.get_mut().await.set_state(&em, "user-response").await;
        SupplementaryServices::request_received(&em, text).await.unwrap();
    }

    pub async fn voice(&self, modem: &str) -> zbus::object_server::InterfaceRef<VoiceCallManager> {
        self.conn.object_server().interface::<_, VoiceCallManager>(modem).await.unwrap()
    }

    pub async fn incoming_call(&self, modem: &str, number: &str) -> OwnedObjectPath {
        let vcm = self.voice(modem).await;
        let em = vcm.signal_emitter().clone();
        vcm.get_mut().await.new_call(&self.conn, &em, "incoming", number).await
    }

    pub async fn remote_answer(&self, call: &OwnedObjectPath) {
        set_call_state(&self.conn, call, "active").await;
    }

    pub async fn remote_hangup(&self, call: &OwnedObjectPath) {
        end_call(&self.conn, call, "remote").await;
    }

    /// A boolean property of the oFono Modem interface at `path`.
    pub async fn modem_bool(&self, path: &str, key: &str) -> Option<bool> {
        let m = self.conn.object_server().interface::<_, OfonoModem>(path).await.ok()?;
        let v = m.get().await.props.get(key)?.try_clone().ok()?;
        bool::try_from(v).ok()
    }

    pub async fn connman_bool(&self, path: &str, key: &str) -> Option<bool> {
        let m = self.conn.object_server().interface::<_, ConnectionManager>(path).await.ok()?;
        let v = m.get().await.props.get(key)?.try_clone().ok()?;
        bool::try_from(v).ok()
    }

    pub async fn polkit(&self) -> zbus::object_server::InterfaceRef<Polkit> {
        self.conn.object_server().interface::<_, Polkit>("/org/freedesktop/PolicyKit1/Authority").await.unwrap()
    }

    /// oFono's CellBroadcast.Topics on `modem`.
    pub async fn cb_topics(&self, modem: &str) -> String {
        let cb = self.conn.object_server().interface::<_, CellBroadcast>(modem).await.unwrap();
        let v = cb.get_mut().await.props().get("Topics").unwrap().try_clone().unwrap();
        String::try_from(v).unwrap()
    }

    /// A CellBroadcast signal from `conn` (oFono's own connection, or an impostor).
    pub async fn broadcast<B: serde::Serialize + zbus::DynamicType>(conn: &Connection, modem: &str, member: &str, body: &B) {
        conn.emit_signal(None::<&str>, modem, "org.ofono.CellBroadcast", member, body).await.unwrap();
    }

    /// The PushNotification agent registered on the modem at `path`, once there is one.
    pub async fn push_agent(&self, path: &str) -> Option<(String, OwnedObjectPath)> {
        let server = self.conn.object_server();
        if server.interface::<_, PushNotification>(path).await.is_err() {
            server.at(path, PushNotification::default()).await.unwrap();
        }
        server.interface::<_, PushNotification>(path).await.unwrap().get().await.agent.clone()
    }

    /// Deliver a WAP push to the agent from `conn` (oFono's own connection, or an impostor).
    pub async fn push(conn: &Connection, agent: &(String, OwnedObjectPath), data: &[u8], sender: &str, sent_time: &str) -> zbus::Result<()> {
        let info = Props::from([
            ("Sender".to_owned(), OwnedVariant::try_from(Variant::from(sender)).unwrap()),
            ("SentTime".to_owned(), OwnedVariant::try_from(Variant::from(sent_time)).unwrap()),
            ("LocalSentTime".to_owned(), OwnedVariant::try_from(Variant::from(sent_time)).unwrap()),
        ]);
        conn.call_method(Some(agent.0.as_str()), &agent.1, Some("org.ofono.PushNotificationAgent"), "ReceiveNotification", &(data, info))
            .await
            .map(drop)
    }

    pub async fn sent_sms(&self, path: &str) -> Vec<(String, String)> {
        self.message_manager(path).await.get().await.sent.clone()
    }
}

pub fn trace(name: &str) -> Vec<Json> {
    let file = format!("{}/tests/traces/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}
