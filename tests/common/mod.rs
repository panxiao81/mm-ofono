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
bag!(VoiceCallManager, "org.ofono.VoiceCallManager");
bag!(ConnectionManager, "org.ofono.ConnectionManager");

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
        conn.request_name("org.ofono").await.unwrap();
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
        dispatch!(OfonoModem, SimManager, NetworkRegistration, RadioSettings, VoiceCallManager, ConnectionManager);
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
            has!(SimManager, NetworkRegistration, RadioSettings, VoiceCallManager, ConnectionManager)
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

    pub async fn sent_sms(&self, path: &str) -> Vec<(String, String)> {
        self.message_manager(path).await.get().await.sent.clone()
    }
}

pub fn trace(name: &str) -> Vec<Json> {
    let file = format!("{}/tests/traces/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}
