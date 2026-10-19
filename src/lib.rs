pub mod derive;
pub mod device;
pub mod error;
pub mod manager;
pub mod mm;
pub mod mm_3gpp;
pub mod mm_messaging;
pub mod mm_modem;
pub mod mm_sim;
pub mod mm_sms;
pub mod modem;
pub mod objects;
pub mod ofono;

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
        .interface("org.ofono.Manager")
        .member(member)
        .path("/")
        .build()?;
    MessageStream::for_match_rule(rule, conn, None).await
}

/// Follow oFono's modems for as long as the connection lives, including
/// oFono restarting.
pub async fn run(conn: Connection) -> zbus::Result<()> {
    let dbus = DBusProxy::new(&conn).await?;
    let mut owner = dbus.receive_name_owner_changed_with_args(&[(0, ofono::SERVICE)]).await?;
    let mut added = manager_signals(&conn, "ModemAdded").await?;
    let mut removed = manager_signals(&conn, "ModemRemoved").await?;
    let mut modems: HashMap<String, mpsc::UnboundedSender<modem::Event>> = HashMap::new();

    let add = |modems: &mut HashMap<_, _>, path: String, props| {
        modems.entry(path.clone()).or_insert_with(|| modem::spawn(conn.clone(), path, props));
    };
    let remove_all = |modems: &mut HashMap<String, mpsc::UnboundedSender<modem::Event>>| {
        for (_, tx) in modems.drain() {
            let _ = tx.send(modem::Event::Removed);
        }
    };

    if dbus.name_has_owner(ofono::SERVICE.try_into()?).await? {
        for (path, props) in ofono::get_modems(&conn).await? {
            add(&mut modems, path.to_string(), props);
        }
    }

    loop {
        tokio::select! {
            Some(Ok(msg)) = added.next() => {
                if let Ok((path, props)) = msg.body().deserialize::<(OwnedObjectPath, ofono::Props)>() {
                    add(&mut modems, path.to_string(), props);
                }
            }
            Some(Ok(msg)) = removed.next() => {
                if let Ok(path) = msg.body().deserialize::<OwnedObjectPath>() {
                    if let Some(tx) = modems.remove(path.as_str()) {
                        let _ = tx.send(modem::Event::Removed);
                    }
                }
            }
            Some(change) = owner.next() => {
                // A new oFono instance starts from scratch; so do we.
                remove_all(&mut modems);
                if change.args()?.new_owner().is_some() {
                    info!("oFono (re)started");
                    for (path, props) in ofono::get_modems(&conn).await? {
                        add(&mut modems, path.to_string(), props);
                    }
                } else {
                    info!("oFono went away");
                }
            }
            else => return Ok(()),
        }
    }
}
