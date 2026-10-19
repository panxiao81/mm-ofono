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

    // Connecting made this SIM the data SIM (none was set).
    let nemo = ofono.conn.object_server().interface::<_, common::NemoModemManager>("/").await.unwrap();
    assert_eq!(nemo.get().await.data_sim_set.len(), 1);

    // Same APN: the same bearer.
    assert_eq!(connect().await, bearer);

    call::<()>(&client, &modem_path, SIMPLE, "Disconnect", &(OwnedObjectPath::try_from("/").unwrap(),)).await.unwrap();
    let b = get_all(&client, bearer.as_str(), BEARER).await.unwrap();
    assert!(!prop::<bool>(&b, "Connected"));
    settle(&client, |o| modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == state::MM_MODEM_STATE_REGISTERED)).await;

    call::<()>(&client, &modem_path, MODEM, "DeleteBearer", &(bearer.clone(),)).await.unwrap();
    assert!(get_all(&client, bearer.as_str(), BEARER).await.is_err());
}

const SIGNAL: &str = "org.freedesktop.ModemManager1.Modem.Signal";

#[tokio::test]
async fn signal_follows_cell_info() {
    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let cell_info = ofono.cell_info("/ril_0").await;
    let (client, modem_path) = registered(&bus, &ofono).await;

    let lte = |client: Connection, path: String| async move {
        let props = get_all(&client, &path, SIGNAL).await.unwrap();
        let lte: HashMap<String, OwnedVariant> = prop(&props, "Lte");
        lte.get("rsrp").map(|v| f64::try_from(v.try_clone().unwrap()).unwrap())
    };
    let wait_for = |want: Option<f64>| {
        let (client, path) = (client.clone(), modem_path.clone());
        async move {
            for _ in 0..100 {
                if lte(client.clone(), path.clone()).await == want {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            panic!("Lte rsrp never became {want:?}, is {:?}", lte(client, path).await);
        }
    };

    // Nothing is measured (or subscribed) until a client asks for a rate.
    assert_eq!(lte(client.clone(), modem_path.clone()).await, None);
    assert!(!cell_info.get().await.subscribed);

    call::<()>(&client, &modem_path, SIGNAL, "Setup", &(5u32,)).await.unwrap();
    wait_for(Some(-67.0)).await;

    ofono.cell_changed("/ril_0/cell_0", "rsrp", 75).await;
    wait_for(Some(-75.0)).await;

    call::<()>(&client, &modem_path, SIGNAL, "Setup", &(0u32,)).await.unwrap();
    wait_for(None).await;
    assert!(!cell_info.get().await.subscribed);
}

const USSD: &str = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd";

#[tokio::test]
async fn ussd_round_trip() {
    use mm::MMModem3gppUssdSessionState as session;

    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem_path) = registered(&bus, &ofono).await;
    let state = || {
        let (client, path) = (client.clone(), modem_path.clone());
        async move { prop::<u32>(&get_all(&client, &path, USSD).await.unwrap(), "State") }
    };
    let wait_state = |want: u32| {
        let state = state.clone();
        async move {
            for _ in 0..100 {
                if state().await == want {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            panic!("USSD state never became {want}");
        }
    };

    wait_state(session::MM_MODEM_3GPP_USSD_SESSION_STATE_IDLE).await;
    let reply: String = call(&client, &modem_path, USSD, "Initiate", &("*100#",)).await.unwrap();
    assert_eq!(reply, "Balance: 100 JPY");

    // A code oFono handles itself still gets an answer.
    let reply: String = call(&client, &modem_path, USSD, "Initiate", &("*#21#",)).await.unwrap();
    assert!(reply.starts_with("CallForwarding: "), "{reply}");

    // Network-initiated request: shown, then answered.
    ofono.ussd_request("/ril_0", "1: Yes 2: No").await;
    wait_state(session::MM_MODEM_3GPP_USSD_SESSION_STATE_USER_RESPONSE).await;
    let props = get_all(&client, &modem_path, USSD).await.unwrap();
    assert_eq!(prop::<String>(&props, "NetworkRequest"), "1: Yes 2: No");
    let reply: String = call(&client, &modem_path, USSD, "Respond", &("1",)).await.unwrap();
    assert_eq!(reply, "You chose 1");
    wait_state(session::MM_MODEM_3GPP_USSD_SESSION_STATE_IDLE).await;
}

const VOICE: &str = "org.freedesktop.ModemManager1.Modem.Voice";
const CALL: &str = "org.freedesktop.ModemManager1.Call";

async fn call_state(client: &Connection, call: &str) -> (i32, i32) {
    let p = get_all(client, call, CALL).await.unwrap();
    (prop(&p, "State"), prop(&p, "StateReason"))
}

async fn wait_call(client: &Connection, call: &str, want: u32) -> (i32, i32) {
    for _ in 0..100 {
        let s = call_state(client, call).await;
        if s.0 == want as i32 {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("{call} never reached state {want}, is {:?}", call_state(client, call).await);
}

#[tokio::test]
async fn voice_outgoing_call() {
    use mm::{MMCallDirection as dir, MMCallState as st, MMCallStateReason as why};

    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem_path) = registered(&bus, &ofono).await;

    let rule = MatchRule::builder().msg_type(Type::Signal).interface(CALL).member("StateChanged").build().unwrap();
    let mut changes = MessageStream::for_match_rule(rule, &client, None).await.unwrap();

    let props = HashMap::from([("number", Variant::from("0901234567"))]);
    let call: OwnedObjectPath = call(&client, &modem_path, VOICE, "CreateCall", &(props,)).await.unwrap();
    let p = get_all(&client, call.as_str(), CALL).await.unwrap();
    assert_eq!(prop::<i32>(&p, "State"), st::MM_CALL_STATE_UNKNOWN as i32);
    assert_eq!(prop::<i32>(&p, "Direction"), dir::MM_CALL_DIRECTION_OUTGOING as i32);
    assert!(ofono.voice("/ril_0").await.get().await.dialled.is_empty(), "CreateCall must not dial");

    super_call(&client, call.as_str(), "Start").await;
    assert_eq!(ofono.voice("/ril_0").await.get().await.dialled, ["0901234567"]);
    wait_call(&client, call.as_str(), st::MM_CALL_STATE_RINGING_OUT).await;
    let listed: Vec<OwnedObjectPath> = self::call(&client, &modem_path, VOICE, "ListCalls", &()).await.unwrap();
    assert_eq!(listed, [call.clone()], "the dialled call is adopted, not duplicated");

    let ofono_call = ofono.voice("/ril_0").await.get().await.calls[0].clone();
    ofono.remote_answer(&ofono_call).await;
    assert_eq!(wait_call(&client, call.as_str(), st::MM_CALL_STATE_ACTIVE).await.1, why::MM_CALL_STATE_REASON_ACCEPTED as i32);

    let _: () = self::call(&client, call.as_str(), CALL, "SendDtmf", &("12#",)).await.unwrap();
    assert_eq!(ofono.voice("/ril_0").await.get().await.tones, ["12#"]);

    super_call(&client, call.as_str(), "Hangup").await;
    assert_eq!(wait_call(&client, call.as_str(), st::MM_CALL_STATE_TERMINATED).await.1, why::MM_CALL_STATE_REASON_TERMINATED as i32);
    let listed: Vec<OwnedObjectPath> = self::call(&client, &modem_path, VOICE, "ListCalls", &()).await.unwrap();
    assert_eq!(listed, [call.clone()], "ended calls stay until deleted");

    let _: () = self::call(&client, &modem_path, VOICE, "DeleteCall", &(call.clone(),)).await.unwrap();
    assert!(get_all(&client, call.as_str(), CALL).await.is_err());

    // StateChanged went through every step, in order.
    let mut seen = vec![];
    while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_millis(200), changes.next()).await {
        let (_, new, _): (i32, i32, u32) = m.body().deserialize().unwrap();
        seen.push(new);
    }
    let want = [st::MM_CALL_STATE_DIALING, st::MM_CALL_STATE_RINGING_OUT, st::MM_CALL_STATE_ACTIVE, st::MM_CALL_STATE_TERMINATED].map(|s| s as i32);
    assert_eq!(seen, want);
}

async fn super_call(client: &Connection, call: &str, method: &str) {
    let _: () = self::call(client, call, CALL, method, &()).await.unwrap();
}

#[tokio::test]
async fn voice_incoming_call() {
    use mm::{MMCallDirection as dir, MMCallState as st, MMCallStateReason as why};

    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem_path) = registered(&bus, &ofono).await;

    let rule = MatchRule::builder().msg_type(Type::Signal).interface(VOICE).member("CallAdded").build().unwrap();
    let mut added = MessageStream::for_match_rule(rule, &client, None).await.unwrap();
    let ofono_call = ofono.incoming_call("/ril_0", "+81900000001").await;
    let msg = tokio::time::timeout(Duration::from_secs(3), added.next()).await.unwrap().unwrap().unwrap();
    let call: OwnedObjectPath = msg.body().deserialize().unwrap();

    let p = get_all(&client, call.as_str(), CALL).await.unwrap();
    assert_eq!(prop::<i32>(&p, "State"), st::MM_CALL_STATE_RINGING_IN as i32);
    assert_eq!(prop::<i32>(&p, "StateReason"), why::MM_CALL_STATE_REASON_INCOMING_NEW as i32);
    assert_eq!(prop::<i32>(&p, "Direction"), dir::MM_CALL_DIRECTION_INCOMING as i32);
    assert_eq!(prop::<String>(&p, "Number"), "+81900000001");

    super_call(&client, call.as_str(), "Accept").await;
    wait_call(&client, call.as_str(), st::MM_CALL_STATE_ACTIVE).await;
    ofono.remote_hangup(&ofono_call).await;
    wait_call(&client, call.as_str(), st::MM_CALL_STATE_TERMINATED).await;
    let listed: Vec<OwnedObjectPath> = self::call(&client, &modem_path, VOICE, "ListCalls", &()).await.unwrap();
    assert_eq!(listed, [call]);
}

/// Pulling and reinserting the SIM keeps the same modem object (clients
/// number SIMs by modem), which goes failed and recovers.
#[tokio::test]
async fn sim_swap_keeps_the_modem() {
    use mm::{MMModemState as state, MMModemStateFailedReason as failed};

    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem_path) = registered(&bus, &ofono).await;
    let full: Vec<String> = [
        "org.ofono.VoiceCallManager", "org.ofono.SimManager", "org.ofono.NetworkRegistration",
        "org.ofono.RadioSettings", "org.ofono.ConnectionManager",
    ]
    .map(String::from)
    .into();
    let sim_only: Vec<String> = vec!["org.ofono.VoiceCallManager".into(), "org.ofono.SimManager".into()];

    // Pulled: oFono drops the network interfaces and reports no card.
    ofono.set("/ril_0", "org.ofono.SimManager", vec![("Present", false.into())]).await;
    ofono.set("/ril_0", "org.ofono.Modem", vec![("Interfaces", sim_only.into())]).await;
    let objects = settle(&client, |o| modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == state::MM_MODEM_STATE_FAILED)).await;
    let (obj, m) = modem(&objects, "ril_0").unwrap();
    assert_eq!(obj.as_str(), modem_path, "same modem object");
    assert_eq!(prop::<u32>(m, "StateFailedReason"), failed::MM_MODEM_STATE_FAILED_REASON_SIM_MISSING);
    assert_eq!(path(m, "Sim"), "/");

    // Reinserted.
    ofono.set("/ril_0", "org.ofono.SimManager", vec![("Present", true.into())]).await;
    ofono.set("/ril_0", "org.ofono.Modem", vec![("Interfaces", full.into())]).await;
    let objects = settle(&client, |o| modem(o, "ril_0").is_some_and(|(_, m)| prop::<i32>(m, "State") == state::MM_MODEM_STATE_REGISTERED)).await;
    let (obj, m) = modem(&objects, "ril_0").unwrap();
    assert_eq!(obj.as_str(), modem_path, "still the same modem object");
    assert_ne!(path(m, "Sim"), "/");
}

/// Like ModemManager: guarded methods check PolicyKit for the caller before
/// touching the modem; queries don't.
#[tokio::test]
async fn unauthorized_callers_are_refused() {
    let bus = Bus::start();
    let ofono = FakeOfono::start(&bus).await;
    let (client, modem_path) = registered(&bus, &ofono).await;
    let polkit = ofono.polkit().await;
    polkit.get_mut().await.refuse = true;
    polkit.get_mut().await.checks.clear();

    let refused = |r: zbus::Result<OwnedObjectPath>, action: &str| match r {
        Err(zbus::Error::MethodError(name, Some(msg), _)) => {
            assert_eq!(name.as_str(), "org.freedesktop.ModemManager1.Error.Core.Unauthorized");
            assert_eq!(msg, format!("PolicyKit authorization failed: not authorized for '{action}'"));
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    };

    let sms = HashMap::from([("number", Variant::from("+10000000002")), ("text", Variant::from("hi"))]);
    refused(call(&client, &modem_path, MESSAGING, "Create", &(sms,)).await, "org.freedesktop.ModemManager1.Messaging");
    let voice = HashMap::from([("number", Variant::from("117"))]);
    refused(call(&client, &modem_path, VOICE, "CreateCall", &(voice,)).await, "org.freedesktop.ModemManager1.Voice");
    let data = HashMap::from([("apn", Variant::from("ctnet"))]);
    refused(call(&client, &modem_path, SIMPLE, "Connect", &(data,)).await, "org.freedesktop.ModemManager1.Device.Control");

    // Nothing reached oFono, and queries still work.
    assert!(ofono.sent_sms("/ril_0").await.is_empty());
    assert!(ofono.voice("/ril_0").await.get().await.dialled.is_empty());
    let listed: Vec<OwnedObjectPath> = call(&client, &modem_path, MESSAGING, "List", &()).await.unwrap();
    assert!(listed.is_empty());

    // Checked for the caller's own bus name.
    let me = client.unique_name().unwrap().to_string();
    let checks = polkit.get().await.checks.clone();
    assert_eq!(checks.len(), 3);
    assert!(checks.iter().all(|(who, _)| *who == me), "{checks:?}");
}
