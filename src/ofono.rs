//! Thin oFono client. Every oFono interface follows one pattern
//! (GetProperties / SetProperty / PropertyChanged), so a few generic calls
//! cover them all instead of a typed proxy per interface.

use std::collections::{HashMap, HashSet};

use futures_util::{Stream, StreamExt};
use zbus::{
    Connection, MatchRule, MessageStream,
    message::Type,
    {OwnedObjectPath, OwnedVariant, Variant},
};

pub const SERVICE: &str = "org.ofono";
pub const MODEM: &str = "org.ofono.Modem";

pub type Props = HashMap<String, OwnedVariant>;

pub async fn get_modems(conn: &Connection) -> zbus::Result<Vec<(OwnedObjectPath, Props)>> {
    conn.call_method(Some(SERVICE), "/", Some("org.ofono.Manager"), "GetModems", &())
        .await?
        .body()
        .deserialize()
}

pub async fn get_properties(conn: &Connection, path: &str, iface: &str) -> zbus::Result<Props> {
    conn.call_method(Some(SERVICE), path, Some(iface), "GetProperties", &())
        .await?
        .body()
        .deserialize()
}

pub async fn set_property(
    conn: &Connection,
    path: &str,
    iface: &str,
    key: &str,
    value: Variant<'_>,
) -> zbus::Result<()> {
    conn.call_method(Some(SERVICE), path, Some(iface), "SetProperty", &(key, value))
        .await?;
    Ok(())
}

pub fn get<'a>(props: &'a Props, key: &str) -> Option<&'a Variant<'static>> {
    props.get(key).map(|v| &**v)
}

pub fn str<'a>(props: &'a Props, key: &str) -> Option<&'a str> {
    match get(props, key)? {
        Variant::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

pub fn bool(props: &Props, key: &str) -> Option<bool> {
    match get(props, key)? {
        Variant::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn strings(props: &Props, key: &str) -> Vec<String> {
    match get(props, key) {
        Some(Variant::Array(a)) => a
            .iter()
            .filter_map(|v| match v {
                Variant::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// A signal from oFono about one modem or its child objects.
#[derive(Debug)]
pub enum Signal {
    /// `PropertyChanged` on `path` (the modem or e.g. a data context).
    Property { path: String, iface: String, key: String, value: OwnedVariant },
    /// `MessageManager.IncomingMessage`, or `ImmediateMessage` (class 0) when `immediate`.
    Sms { text: String, info: Props, immediate: bool },
    ContextAdded { path: String, props: Props },
    ContextRemoved { path: String },
    /// SupplementaryServices: network-initiated USSD, informational or
    /// expecting a Respond.
    UssdNotification(String),
    UssdRequest(String),
    /// VoiceCallManager.CallAdded / CallRemoved, and a call's
    /// DisconnectReason (which comes before its removal).
    CallAdded { path: String, props: Props },
    CallRemoved { path: String },
    DisconnectReason { path: String, reason: String },
    /// Sailfish CellInfo: cells appear while someone is subscribed.
    CellsAdded(Vec<String>),
    CellsRemoved(Vec<String>),
}

/// Every oFono signal on the modem's path and below, in emission order
/// (one match rule, so interfaces and objects can't overtake each other).
pub async fn signals(conn: &Connection, path: &str) -> zbus::Result<impl Stream<Item = Signal> + use<>> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .path_namespace(path.to_owned())
        .build()?;
    let stream = MessageStream::for_match_rule(rule, conn, None).await?;
    Ok(stream.filter_map(|msg| async move {
        let msg = msg.ok()?;
        let header = msg.header();
        let iface = header.interface()?.to_string();
        if !iface.starts_with("org.ofono.") && !iface.starts_with("org.nemomobile.ofono.Cell") {
            return None;
        }
        match header.member()?.as_str() {
            "PropertyChanged" => {
                let (key, value): (String, OwnedVariant) = msg.body().deserialize().ok()?;
                Some(Signal::Property { path: header.path()?.to_string(), iface, key, value })
            }
            m @ ("IncomingMessage" | "ImmediateMessage") => {
                let (text, info): (String, Props) = msg.body().deserialize().ok()?;
                Some(Signal::Sms { text, info, immediate: m == "ImmediateMessage" })
            }
            "ContextAdded" => {
                let (path, props): (OwnedObjectPath, Props) = msg.body().deserialize().ok()?;
                Some(Signal::ContextAdded { path: path.to_string(), props })
            }
            "ContextRemoved" => {
                let path: OwnedObjectPath = msg.body().deserialize().ok()?;
                Some(Signal::ContextRemoved { path: path.to_string() })
            }
            m @ ("NotificationReceived" | "RequestReceived") => {
                let text: String = msg.body().deserialize().ok()?;
                Some(if m == "NotificationReceived" { Signal::UssdNotification(text) } else { Signal::UssdRequest(text) })
            }
            "CallAdded" => {
                let (path, props): (OwnedObjectPath, Props) = msg.body().deserialize().ok()?;
                Some(Signal::CallAdded { path: path.to_string(), props })
            }
            "CallRemoved" => {
                let path: OwnedObjectPath = msg.body().deserialize().ok()?;
                Some(Signal::CallRemoved { path: path.to_string() })
            }
            "DisconnectReason" => {
                let reason: String = msg.body().deserialize().ok()?;
                Some(Signal::DisconnectReason { path: header.path()?.to_string(), reason })
            }
            m @ ("CellsAdded" | "CellsRemoved") => {
                let paths: Vec<OwnedObjectPath> = msg.body().deserialize().ok()?;
                let paths = paths.into_iter().map(|p| p.to_string()).collect();
                Some(if m == "CellsAdded" { Signal::CellsAdded(paths) } else { Signal::CellsRemoved(paths) })
            }
            _ => None,
        }
    }))
}

/// Cached properties of one oFono interface.
///
/// Interfaces don't announce their initial properties, so they are fetched
/// with GetProperties when the interface appears. Signals may arrive while
/// that call is in flight and are newer than its reply for the keys they
/// touch, so a snapshot only fills keys no signal has set since the fetch
/// started.
#[derive(Debug, Default)]
pub struct Iface {
    pub props: Props,
    changed_since_fetch: HashSet<String>,
    /// A GetProperties reply has been applied at least once.
    pub fetched: bool,
}

impl Iface {
    pub fn change(&mut self, key: String, value: OwnedVariant) {
        self.changed_since_fetch.insert(key.clone());
        self.props.insert(key, value);
    }

    pub fn apply_snapshot(&mut self, snapshot: Props) {
        for (key, value) in snapshot {
            if !self.changed_since_fetch.contains(&key) {
                self.props.insert(key, value);
            }
        }
        self.changed_since_fetch.clear();
        self.fetched = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> OwnedVariant {
        Variant::from(s).try_into().unwrap()
    }

    #[test]
    fn snapshot_does_not_undo_newer_signals() {
        let mut iface = Iface::default();
        // GetProperties is sent; while it is in flight the status moves on.
        iface.change("Status".into(), v("registered"));
        // The reply was built before that change.
        iface.apply_snapshot(Props::from([
            ("Status".into(), v("searching")),
            ("Name".into(), v("KDDI")),
        ]));
        assert_eq!(iface.props["Status"], v("registered"));
        assert_eq!(iface.props["Name"], v("KDDI"));

        // After the snapshot, later snapshots apply normally again.
        iface.apply_snapshot(Props::from([("Status".into(), v("roaming"))]));
        assert_eq!(iface.props["Status"], v("roaming"));
    }
}
