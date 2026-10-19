//! The ObjectManager at /org/freedesktop/ModemManager1, and announcing
//! property changes.
//!
//! Like ModemManager, the ObjectManager lists only modem objects. SIMs (and
//! later bearers, SMS, calls) live below the same path but are reached
//! through modem properties; zbus's own ObjectManager would list them too,
//! and libmm-glib takes those for broken modems.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use zbus::{
    Connection, ObjectServer, fdo, interface,
    object_server::{Interface, InterfaceRef, SignalEmitter},
    {ObjectPath, OwnedObjectPath, OwnedVariant, Variant},
};

use crate::{MM_PATH, mm_3gpp::Modem3gpp, mm_bearer::Simple, mm_messaging::Messaging, mm_modem::Modem, mm_signal::ModemSignal, mm_ussd::Ussd};

pub type IfaceProps = HashMap<String, HashMap<String, OwnedVariant>>;

#[derive(Debug, Default)]
pub struct ModemObjects {
    modems: HashSet<OwnedObjectPath>,
}

async fn props_of<I: Interface>(server: &ObjectServer, conn: &Connection, path: &ObjectPath<'_>) -> fdo::Result<Option<HashMap<String, OwnedVariant>>> {
    let Ok(iface) = server.interface::<_, I>(path).await else { return Ok(None) };
    let props = iface.get().await.get_all(server, conn, None, iface.signal_emitter()).await?;
    Ok(Some(props))
}

/// Calls `$f::<I>()` for every ModemManager interface a modem object can have.
macro_rules! for_modem_interfaces {
    ($f:ident ( $($arg:expr),* )) => {{
        $f::<Modem>($($arg),*).await?;
        $f::<Modem3gpp>($($arg),*).await?;
        $f::<Messaging>($($arg),*).await?;
        $f::<Simple>($($arg),*).await?;
        $f::<ModemSignal>($($arg),*).await?;
        $f::<Ussd>($($arg),*).await?;
    }};
}

/// Every ModemManager interface exported on a modem object, with properties.
async fn modem_interfaces(server: &ObjectServer, conn: &Connection, path: &ObjectPath<'_>) -> fdo::Result<IfaceProps> {
    let mut out = IfaceProps::new();
    async fn add<I: Interface>(out: &mut IfaceProps, server: &ObjectServer, conn: &Connection, path: &ObjectPath<'_>) -> fdo::Result<()> {
        if let Some(p) = props_of::<I>(server, conn, path).await? {
            out.insert(I::name().to_string(), p);
        }
        Ok(())
    }
    for_modem_interfaces!(add(&mut out, server, conn, path));
    Ok(out)
}

#[interface(name = "org.freedesktop.DBus.ObjectManager")]
impl ModemObjects {
    async fn get_managed_objects(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<HashMap<OwnedObjectPath, IfaceProps>> {
        let mut out = HashMap::new();
        for path in &self.modems {
            out.insert(path.clone(), modem_interfaces(server, conn, path).await?);
        }
        Ok(out)
    }

    #[zbus(signal)]
    async fn interfaces_added(emitter: &SignalEmitter<'_>, object_path: ObjectPath<'_>, interfaces_and_properties: &IfaceProps) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn interfaces_removed(emitter: &SignalEmitter<'_>, object_path: ObjectPath<'_>, interfaces: &[&str]) -> zbus::Result<()>;
}

/// List a modem object, whose interfaces the caller has already exported,
/// and announce it.
pub async fn announce_modem(conn: &Connection, path: &OwnedObjectPath) -> zbus::Result<()> {
    let server = conn.object_server();
    let om = server.interface::<_, ModemObjects>(MM_PATH).await?;
    om.get_mut().await.modems.insert(path.clone());
    let ifaces = modem_interfaces(&server, conn, path).await?;
    ModemObjects::interfaces_added(om.signal_emitter(), path.as_ref(), &ifaces).await
}

/// Unexport every interface of a modem object and announce its removal.
pub async fn remove_modem(conn: &Connection, path: &OwnedObjectPath) -> zbus::Result<()> {
    let server = conn.object_server();
    let om = server.interface::<_, ModemObjects>(MM_PATH).await?;
    om.get_mut().await.modems.remove(path);

    let mut removed = vec![];
    async fn remove<I: Interface>(removed: &mut Vec<String>, server: &ObjectServer, path: &OwnedObjectPath) -> zbus::Result<()> {
        if server.remove::<I, _>(path).await.is_ok() {
            removed.push(I::name().to_string());
        }
        Ok(())
    }
    for_modem_interfaces!(remove(&mut removed, &server, path));
    let removed: Vec<&str> = removed.iter().map(String::as_str).collect();
    ModemObjects::interfaces_removed(om.signal_emitter(), path.as_ref(), &removed).await
}

/// Run `f` on an exported interface, then announce whichever properties
/// changed. Comparing `get_all` before and after uses the property getters
/// themselves, so no property can be forgotten.
pub async fn update<I: Interface>(conn: &Connection, iface: &InterfaceRef<I>, f: impl FnOnce(&mut I)) -> zbus::Result<()> {
    let server = conn.object_server();
    let emitter = iface.signal_emitter();
    let mut guard = iface.get_mut().await;
    let before = guard.get_all(&server, conn, None, emitter).await?;
    f(&mut guard);
    let after = guard.get_all(&server, conn, None, emitter).await?;
    drop(guard);

    let mut changed = HashMap::new();
    for (name, value) in &after {
        if before.get(name) != Some(value) {
            changed.insert(name.as_str(), Variant::from(value.try_clone()?));
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    fdo::Properties::properties_changed(emitter, I::name(), changed, Cow::Borrowed(&[])).await
}
