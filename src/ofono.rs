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

/// A change on one oFono interface of a modem.
#[derive(Debug)]
pub struct Change {
    pub iface: String,
    pub key: String,
    pub value: OwnedVariant,
}

/// `PropertyChanged` from every interface of one modem, in emission order
/// (one match rule on the path, so interfaces can't overtake each other).
pub async fn property_changes(
    conn: &Connection,
    path: &str,
) -> zbus::Result<impl Stream<Item = Change> + use<>> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .path(path.to_owned())
        .member("PropertyChanged")
        .build()?;
    let stream = MessageStream::for_match_rule(rule, conn, None).await?;
    Ok(stream.filter_map(|msg| async move {
        let msg = msg.ok()?;
        let iface = msg.header().interface()?.to_string();
        if !iface.starts_with("org.ofono.") {
            return None;
        }
        let (key, value): (String, OwnedVariant) = msg.body().deserialize().ok()?;
        Some(Change { iface, key, value })
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
