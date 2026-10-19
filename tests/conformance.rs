//! Every interface we export must match ModemManager's own introspection XML
//! (from modemmanager-dev) member for member: same methods, argument types,
//! signals, property types and access.

use std::collections::BTreeSet;

use mm_ofono::{manager::Manager, mm_3gpp::Modem3gpp, mm_bearer::{Bearer, Simple}, mm_messaging::Messaging, mm_modem::Modem, mm_signal::ModemSignal, mm_sim::Sim, mm_sms::Sms, mm_ussd::Ussd, mm_voice::Voice, mm_call::Call};
use zbus::object_server::Interface;
use zbus_xml::{Arg, ArgDirection, Node};

const XML_DIR: &str = "/usr/share/dbus-1/interfaces";

/// One line per member, e.g. `method SetLogging(s) -> ()`, so two
/// definitions of the same interface can be diffed as sets.
fn members(xml: &str, iface_name: &str) -> BTreeSet<String> {
    let node = Node::from_reader(xml.as_bytes()).expect("valid introspection XML");
    let iface = node
        .interfaces()
        .iter()
        .find(|i| i.name() == iface_name)
        .unwrap_or_else(|| panic!("{iface_name} not in XML"));

    let sig = |args: &[Arg], dir: Option<ArgDirection>| -> String {
        args.iter()
            .filter(|a| dir.is_none() || a.direction().unwrap_or(ArgDirection::In) == dir.unwrap())
            .map(|a| a.ty().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };

    let mut out = BTreeSet::new();
    for m in iface.methods() {
        out.insert(format!(
            "method {}({}) -> ({})",
            m.name(),
            sig(m.args(), Some(ArgDirection::In)),
            sig(m.args(), Some(ArgDirection::Out))
        ));
    }
    for s in iface.signals() {
        out.insert(format!("signal {}({})", s.name(), sig(s.args(), None)));
    }
    for p in iface.properties() {
        out.insert(format!("property {}: {} {:?}", p.name(), p.ty(), p.access()));
    }
    out
}

fn assert_conforms<I: Interface>(iface: &I) {
    let name = I::name().to_string();

    let spec = std::fs::read_to_string(format!("{XML_DIR}/{name}.xml"))
        .unwrap_or_else(|e| panic!("{name}.xml: {e} (is modemmanager-dev installed?)"));

    let mut ours = String::from("<node>");
    iface.introspect_to_writer(&mut ours, 0);
    ours.push_str("</node>");

    let (spec, ours) = (members(&spec, &name), members(&ours, &name));
    let missing: Vec<_> = spec.difference(&ours).collect();
    let extra: Vec<_> = ours.difference(&spec).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{name} differs from ModemManager\n  missing: {missing:#?}\n  extra: {extra:#?}"
    );
}

#[test]
fn manager() {
    assert_conforms(&Manager);
}

#[test]
fn sim() {
    assert_conforms(&Sim::default());
}

#[test]
fn modem() {
    assert_conforms(&Modem::default());
}

#[test]
fn modem_3gpp() {
    assert_conforms(&Modem3gpp::default());
}

#[test]
fn messaging() {
    assert_conforms(&Messaging::default());
}

#[test]
fn sms() {
    assert_conforms(&Sms::default());
}

#[test]
fn bearer() {
    assert_conforms(&Bearer::default());
}

#[test]
fn simple() {
    assert_conforms(&Simple::default());
}

#[test]
fn signal() {
    assert_conforms(&ModemSignal::default());
}

#[test]
fn ussd() {
    assert_conforms(&Ussd::default());
}

#[test]
fn voice() {
    assert_conforms(&Voice::default());
}

#[test]
fn call() {
    assert_conforms(&Call::default());
}
