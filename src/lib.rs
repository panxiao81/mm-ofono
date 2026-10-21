pub mod auth;
pub mod bearer;
pub mod derive;
pub mod device;
pub mod error;
pub mod manager;
pub mod mm;
pub mod mm_3gpp;
pub mod mm_bearer;
pub mod mm_cell_broadcast;
pub mod mm_location;
pub mod mm_call;
pub mod mm_messaging;
pub mod mm_modem;
pub mod mm_signal;
pub mod mm_sim;
pub mod mm_sms;
pub mod mm_time;
pub mod mm_ussd;
pub mod mm_voice;
pub mod modem;
pub mod objects;
pub mod ofono;
pub mod signal;
pub mod voice;

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::info;
use zbus::{Connection, MatchRule, MessageStream, fdo::DBusProxy, message::Type, OwnedObjectPath};

pub const MM_SERVICE: &str = "org.freedesktop.ModemManager1";
pub const MM_PATH: &str = "/org/freedesktop/ModemManager1";

/// Export the root objects on `conn` (before claiming the bus name).
pub async fn serve(conn: &Connection) -> zbus::Result<()> {
    let server = conn.object_server();
    server.at(MM_PATH, objects::ModemObjects::default()).await?;
    server.at(MM_PATH, manager::Manager).await?;
    Ok(())
}

async fn manager_signals(conn: &Connection, member: &'static str) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(ofono::SERVICE)
        .interface("org.ofono.Manager")
        .member(member)
        .path("/")
        .build()?;
    MessageStream::for_match_rule(rule, conn, None).await
}

/// The oFono modems we follow, each with the oFono instance (unique bus
/// name) that announced it.
///
/// Modems come and go with ModemAdded/ModemRemoved alone. An oFono that
/// exits can't say ModemRemoved, so its disappearing from the bus drops
/// *its* modems and nothing else; an oFono appearing needs no handling, as
/// it announces its modems itself. The two kinds of events stay independent,
/// so their relative order (they come on separate streams) doesn't matter.
#[derive(Debug, Default)]
struct Modems {
    by_path: HashMap<String, String>,
}

impl Modems {
    /// True if `path` is new and should be followed.
    fn added(&mut self, path: &str, owner: &str) -> bool {
        if self.by_path.contains_key(path) {
            return false;
        }
        self.by_path.insert(path.to_owned(), owner.to_owned());
        true
    }

    fn removed(&mut self, path: &str) -> bool {
        self.by_path.remove(path).is_some()
    }

    /// oFono instance `owner` left the bus: the modems it announced.
    fn instance_gone(&mut self, owner: &str) -> Vec<String> {
        let gone: Vec<String> = self.by_path.iter().filter(|(_, o)| *o == owner).map(|(p, _)| p.clone()).collect();
        for p in &gone {
            self.by_path.remove(p);
        }
        gone
    }
}

/// Follow oFono's modems for as long as the connection lives.
pub async fn run(conn: Connection) -> zbus::Result<()> {
    let dbus = DBusProxy::new(&conn).await?;
    // Subscribe first, then pick up modems that already exist (duplicates
    // are ignored by path).
    let mut owner_changes = dbus.receive_name_owner_changed_with_args(&[(0, ofono::SERVICE)]).await?;
    let mut added = manager_signals(&conn, "ModemAdded").await?;
    let mut removed = manager_signals(&conn, "ModemRemoved").await?;
    let mut modems = Modems::default();
    let mut tasks: HashMap<String, mpsc::UnboundedSender<modem::Event>> = HashMap::new();
    let order = modem::SlotOrder::new();

    let follow = |modems: &mut Modems, tasks: &mut HashMap<_, _>, path: String, owner: &str, props| {
        if modems.added(&path, owner) {
            tasks.insert(path.clone(), modem::spawn(conn.clone(), path, props, order.clone()));
        }
    };
    let stop = |tasks: &mut HashMap<String, mpsc::UnboundedSender<modem::Event>>, path: &str| {
        if let Some(tx) = tasks.remove(path) {
            let _ = tx.send(modem::Event::Removed);
        }
    };

    if let Ok(owner) = dbus.get_name_owner(ofono::SERVICE.try_into()?).await {
        for (path, props) in ofono::get_modems(&conn).await? {
            follow(&mut modems, &mut tasks, path.to_string(), owner.as_str(), props);
        }
    }

    loop {
        tokio::select! {
            Some(Ok(msg)) = added.next() => {
                let sender = msg.header().sender().map(|s| s.to_string()).unwrap_or_default();
                if let Ok((path, props)) = msg.body().deserialize::<(OwnedObjectPath, ofono::Props)>() {
                    follow(&mut modems, &mut tasks, path.to_string(), &sender, props);
                }
            }
            Some(Ok(msg)) = removed.next() => {
                if let Ok(path) = msg.body().deserialize::<OwnedObjectPath>() {
                    if modems.removed(path.as_str()) {
                        stop(&mut tasks, path.as_str());
                    }
                }
            }
            Some(change) = owner_changes.next() => {
                if let Some(old) = change.args()?.old_owner().as_ref() {
                    info!("oFono instance {old} left the bus");
                    for path in modems.instance_gone(old.as_str()) {
                        stop(&mut tasks, &path);
                    }
                }
            }
            else => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the phone: mm-ofono starts first, and the new oFono's ModemAdded
    /// may be handled before or after the bus says it appeared. Either way
    /// the modem stays.
    #[test]
    fn appearing_owner_changes_nothing() {
        let mut m = Modems::default();
        assert!(m.added("/ril_1", ":1.20"));
        assert!(m.instance_gone(":1.3").is_empty(), "some other instance leaving doesn't touch it");
        assert!(!m.added("/ril_1", ":1.20"), "followed once");
    }

    #[test]
    fn exiting_instance_takes_its_modems() {
        let mut m = Modems::default();
        m.added("/ril_0", ":1.5");
        m.added("/ril_1", ":1.5");
        // The restarted oFono's modem, seen before the old one's exit.
        assert!(!m.added("/ril_0", ":1.30"), "path still held by the old instance");
        let mut gone = m.instance_gone(":1.5");
        gone.sort();
        assert_eq!(gone, ["/ril_0", "/ril_1"]);
        assert!(m.added("/ril_0", ":1.30"), "re-announced by the new instance: followed again");
    }
}
