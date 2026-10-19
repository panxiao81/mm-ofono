//! mm-ofono against a fake oFono replaying traces recorded on real devices.

mod common;

use std::{collections::HashMap, time::Duration};

use common::{Bus, FakeOfono, trace};
use futures_util::StreamExt;
use mm_ofono::{MM_PATH, MM_SERVICE, mm, ofono};
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, OwnedVariant, Variant, message::Type};

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;
const MODEM: &str = "org.freedesktop.ModemManager1.Modem";

async fn managed_objects(conn: &Connection) -> Objects {
    conn.call_method(Some(MM_SERVICE), MM_PATH, Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &())
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap()
}

async fn get_all(conn: &Connection, path: &str, iface: &str) -> zbus::Result<HashMap<String, OwnedVariant>> {
    conn.call_method(Some(MM_SERVICE), path, Some("org.freedesktop.DBus.Properties"), "GetAll", &(iface,))
        .await?
        .body()
        .deserialize()
}

fn prop<T: TryFrom<OwnedVariant>>(props: &HashMap<String, OwnedVariant>, name: &str) -> T {
    T::try_from(props[name].try_clone().unwrap()).ok().unwrap_or_else(|| panic!("{name}: wrong type"))
}

/// An object path property, e.g. Modem.Sim.
fn path(props: &HashMap<String, OwnedVariant>, name: &str) -> String {
    match &*props[name] {
        Variant::ObjectPath(p) => p.to_string(),
        v => panic!("{name}: not an object path: {v:?}"),
    }
}

/// The modem whose primary port is `port`.
fn modem<'a>(objects: &'a Objects, port: &str) -> Option<(&'a OwnedObjectPath, &'a HashMap<String, OwnedVariant>)> {
    objects
        .iter()
        .map(|(p, i)| (p, &i[MODEM]))
        .find(|(_, m)| prop::<String>(m, "PrimaryPort") == port)
}

/// Start mm-ofono on the bus and watch what clients would see: every
/// InterfacesAdded must point at a SIM that already exists.
async fn start_bridge(bus: &Bus) -> (Connection, tokio::task::JoinHandle<Vec<String>>) {
    let mm = bus.connect().await;
    mm_ofono::serve(&mm).await.unwrap();
    mm.request_name(MM_SERVICE).await.unwrap();
    tokio::spawn(mm_ofono::run(mm));

    let client = bus.connect().await;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.freedesktop.DBus.ObjectManager")
        .member("InterfacesAdded")
        .build()
        .unwrap();
    let mut added = MessageStream::for_match_rule(rule, &client, None).await.unwrap();
    let watcher_conn = client.clone();
    let watcher = tokio::spawn(async move {
        let mut problems = vec![];
        while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(3), added.next()).await {
            let (obj, ifaces): (OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>) =
                msg.body().deserialize().unwrap();
            let sim = path(&ifaces[MODEM], "Sim");
            if sim != "/" && get_all(&watcher_conn, &sim, "org.freedesktop.ModemManager1.Sim").await.is_err() {
                problems.push(format!("{obj} announced with SIM {sim} that doesn't exist"));
            }
        }
        problems
    });
    (client, watcher)
}

/// Poll until `ready` holds for the exported objects.
async fn settle(client: &Connection, ready: impl Fn(&Objects) -> bool) -> Objects {
    for _ in 0..100 {
        let objects = managed_objects(client).await;
        if ready(&objects) {
            return objects;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never settled: {:#?}", managed_objects(client).await);
}

/// China Telecom SIM in slot 1 roaming on KDDI, slot 2 empty; recorded while
/// restarting oFono on a Sony Xperia 5 II (so it is also a boot sequence).
#[tokio::test]
async fn ct_sim_ofono_restart() {
    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, watcher) = start_bridge(&bus).await;

    ofono.replay(&trace("ct-ofono-restart"), async || tokio::time::sleep(Duration::from_millis(2)).await).await;

    let objects = settle(&client, |o| {
        modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == mm::MMModemState::MM_MODEM_STATE_REGISTERED)
    })
    .await;

    // Only modems are managed objects, like real ModemManager.
    assert!(objects.keys().all(|p| p.starts_with(&format!("{MM_PATH}/Modem/"))), "{:?}", objects.keys());

    let (_, ril0) = modem(&objects, "ril_0").unwrap();
    assert_eq!(prop::<u32>(ril0, "AccessTechnologies"), mm::MMModemAccessTechnology::MM_MODEM_ACCESS_TECHNOLOGY_LTE);
    assert_eq!(prop::<(u32, bool)>(ril0, "SignalQuality"), (40, true));
    assert_eq!(prop::<u32>(ril0, "PowerState"), mm::MMModemPowerState::MM_MODEM_POWER_STATE_ON);
    assert_eq!(prop::<u32>(ril0, "UnlockRequired"), mm::MMModemLock::MM_MODEM_LOCK_NONE);
    assert_eq!(prop::<String>(ril0, "Revision"), "sdx55.rmtefs-484436-87");

    let gpp = &objects.iter().find(|(_, i)| prop::<String>(&i[MODEM], "PrimaryPort") == "ril_0").unwrap().1
        ["org.freedesktop.ModemManager1.Modem.Modem3gpp"];
    assert_eq!(
        prop::<u32>(gpp, "RegistrationState"),
        mm::MMModem3gppRegistrationState::MM_MODEM_3GPP_REGISTRATION_STATE_ROAMING
    );
    assert_eq!(prop::<String>(gpp, "OperatorCode"), "44051");
    assert_eq!(prop::<String>(gpp, "OperatorName"), "中国电信 (KDDI)");

    let sim = get_all(&client, &path(ril0, "Sim"), "org.freedesktop.ModemManager1.Sim").await.unwrap();
    assert_eq!(prop::<String>(&sim, "OperatorIdentifier"), "46011");
    assert_eq!(prop::<String>(&sim, "OperatorName"), "中国电信");
    assert!(prop::<String>(&sim, "Imsi").starts_with("46011"));

    let (_, ril1) = modem(&objects, "ril_1").unwrap();
    assert_eq!(prop::<i32>(ril1, "State"), mm::MMModemState::MM_MODEM_STATE_FAILED);
    assert_eq!(
        prop::<u32>(ril1, "StateFailedReason"),
        mm::MMModemStateFailedReason::MM_MODEM_STATE_FAILED_REASON_SIM_MISSING
    );
    assert_eq!(path(ril1, "Sim"), "/");

    let problems = watcher.await.unwrap();
    assert!(problems.is_empty(), "{problems:#?}");
}

const MESSAGING: &str = "org.freedesktop.ModemManager1.Modem.Messaging";
const SMS: &str = "org.freedesktop.ModemManager1.Sms";

async fn call<R: serde::de::DeserializeOwned + zbus::Type>(
    conn: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    args: &(impl serde::Serialize + zbus::DynamicType),
) -> zbus::Result<R> {
    conn.call_method(Some(MM_SERVICE), path, Some(iface), method, args).await?.body().deserialize()
}

/// Bring the CT trace to a registered modem and return the client and the
/// path of the ril_0 modem object.
async fn registered(bus: &Bus, ofono: &FakeOfono) -> (Connection, String) {
    let (client, _watcher) = start_bridge(bus).await;
    ofono.replay(&trace("ct-ofono-restart"), async || tokio::time::sleep(Duration::from_millis(1)).await).await;
    let objects = settle(&client, |o| {
        modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == mm::MMModemState::MM_MODEM_STATE_REGISTERED)
    })
    .await;
    let path = modem(&objects, "ril_0").unwrap().0.to_string();
    (client, path)
}

#[tokio::test]
async fn sms_round_trip() {
    use mm::{MMSmsPduType as pdu, MMSmsState as state};

    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem) = registered(&bus, &ofono).await;

    // Incoming: announced with Added(path, received = true).
    let rule = MatchRule::builder().msg_type(Type::Signal).interface(MESSAGING).member("Added").build().unwrap();
    let mut added = MessageStream::for_match_rule(rule, &client, None).await.unwrap();
    ofono.incoming_sms("/ril_0", "你好 hello", "+10000000001", "2026-10-20T01:00:00+0900").await;
    let msg = tokio::time::timeout(Duration::from_secs(3), added.next()).await.unwrap().unwrap().unwrap();
    let (sms, received): (OwnedObjectPath, bool) = msg.body().deserialize().unwrap();
    assert!(received);
    let props = get_all(&client, sms.as_str(), SMS).await.unwrap();
    assert_eq!(prop::<String>(&props, "Text"), "你好 hello");
    assert_eq!(prop::<String>(&props, "Number"), "+10000000001");
    assert_eq!(prop::<String>(&props, "Timestamp"), "2026-10-20T01:00:00+0900");
    assert_eq!(prop::<u32>(&props, "State"), state::MM_SMS_STATE_RECEIVED);
    assert_eq!(prop::<u32>(&props, "PduType"), pdu::MM_SMS_PDU_TYPE_DELIVER);
    let listed: Vec<OwnedObjectPath> = call(&client, &modem, MESSAGING, "List", &()).await.unwrap();
    assert_eq!(listed, [sms.clone()]);

    // Outgoing: Create composes, Send sends and returns once oFono says "sent".
    let create = |text: &'static str| {
        let (client, modem) = (client.clone(), modem.clone());
        async move {
            let props = HashMap::from([("number", Variant::from("+10000000002")), ("text", Variant::from(text))]);
            call::<OwnedObjectPath>(&client, &modem, MESSAGING, "Create", &(props,)).await.unwrap()
        }
    };
    let out = create("bye").await;
    let props = get_all(&client, out.as_str(), SMS).await.unwrap();
    assert_eq!(prop::<u32>(&props, "State"), state::MM_SMS_STATE_UNKNOWN);
    assert_eq!(prop::<u32>(&props, "PduType"), pdu::MM_SMS_PDU_TYPE_SUBMIT);
    assert!(ofono.sent_sms("/ril_0").await.is_empty(), "Create must not send");

    call::<()>(&client, out.as_str(), SMS, "Send", &()).await.unwrap();
    let props = get_all(&client, out.as_str(), SMS).await.unwrap();
    assert_eq!(prop::<u32>(&props, "State"), state::MM_SMS_STATE_SENT);
    assert_eq!(ofono.sent_sms("/ril_0").await, [("+10000000002".to_owned(), "bye".to_owned())]);

    // A failed send is an error and leaves the message unsent.
    let failing = create("FAIL").await;
    assert!(call::<()>(&client, failing.as_str(), SMS, "Send", &()).await.is_err());
    let props = get_all(&client, failing.as_str(), SMS).await.unwrap();
    assert_eq!(prop::<u32>(&props, "State"), state::MM_SMS_STATE_UNKNOWN);

    // Delete drops it from the list and the bus.
    call::<()>(&client, &modem, MESSAGING, "Delete", &(sms.clone(),)).await.unwrap();
    assert!(get_all(&client, sms.as_str(), SMS).await.is_err());
    let listed: Vec<OwnedObjectPath> = call(&client, &modem, MESSAGING, "List", &()).await.unwrap();
    assert!(!listed.contains(&sms));
}

const SIMPLE: &str = "org.freedesktop.ModemManager1.Modem.Simple";
const BEARER: &str = "org.freedesktop.ModemManager1.Bearer";

#[tokio::test]
async fn data_round_trip() {
    use mm::{MMBearerIpFamily as family, MMBearerIpMethod as method, MMModemState as state};

    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem_path) = registered(&bus, &ofono).await;
    let ctx = ofono.add_context("/ril_0", "internet", "ctnet").await;

    let connect = || {
        let (client, modem) = (client.clone(), modem_path.clone());
        async move {
            let props = HashMap::from([("apn", Variant::from("ctnet")), ("ip-type", Variant::from(family::MM_BEARER_IP_FAMILY_IPV4V6))]);
            call::<OwnedObjectPath>(&client, &modem, SIMPLE, "Connect", &(props,)).await.unwrap()
        }
    };
    let bearer = connect().await;

    // Everything NetworkManager reads right after Connect returns.
    let b = get_all(&client, bearer.as_str(), BEARER).await.unwrap();
    assert!(prop::<bool>(&b, "Connected"));
    assert_eq!(prop::<String>(&b, "Interface"), "rmnet_data2");
    let ip4: HashMap<String, OwnedVariant> = prop(&b, "Ip4Config");
    assert_eq!(prop::<u32>(&ip4, "method"), method::MM_BEARER_IP_METHOD_STATIC);
    assert_eq!(prop::<String>(&ip4, "address"), "10.172.70.13");
    assert_eq!(prop::<u32>(&ip4, "prefix"), 30);
    assert_eq!(prop::<String>(&ip4, "dns1"), "1.1.1.1");
    let ip6: HashMap<String, OwnedVariant> = prop(&b, "Ip6Config");
    assert_eq!(prop::<u32>(&ip6, "prefix"), 64);
    assert_eq!(ofono::str(&ofono.context(&ctx).await, "Protocol"), Some("dual"));

    let objects = settle(&client, |o| modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == state::MM_MODEM_STATE_CONNECTED)).await;
    let (_, m) = modem(&objects, "ril_0").unwrap();
    let bearers: Vec<OwnedObjectPath> = prop(m, "Bearers");
    assert_eq!(bearers, [bearer.clone()]);

    // Same APN: the same bearer.
    assert_eq!(connect().await, bearer);

    call::<()>(&client, &modem_path, SIMPLE, "Disconnect", &(OwnedObjectPath::try_from("/").unwrap(),)).await.unwrap();
    let b = get_all(&client, bearer.as_str(), BEARER).await.unwrap();
    assert!(!prop::<bool>(&b, "Connected"));
    settle(&client, |o| modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == state::MM_MODEM_STATE_REGISTERED)).await;

    call::<()>(&client, &modem_path, MODEM, "DeleteBearer", &(bearer.clone(),)).await.unwrap();
    assert!(get_all(&client, bearer.as_str(), BEARER).await.is_err());
}
