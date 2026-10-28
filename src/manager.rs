use std::collections::HashMap;

use zbus::{interface, OwnedVariant};

use crate::error::{CoreError, Result};

/// `org.freedesktop.ModemManager1` at the root object.
pub struct Manager;

#[interface(name = "org.freedesktop.ModemManager1")]
impl Manager {
    /// oFono discovers modems on its own; there is nothing to rescan.
    async fn scan_devices(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MANAGER_CONTROL).await
    }

    async fn set_logging(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _level: &str) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MANAGER_CONTROL).await?;
        Err(CoreError::Unsupported("use RUST_LOG instead".into()))
    }

    async fn report_kernel_event(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _properties: HashMap<String, OwnedVariant>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MANAGER_CONTROL).await?;
        Err(CoreError::Unsupported("devices come from oFono, not udev".into()))
    }

    async fn inhibit_device(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _uid: &str, _inhibit: bool) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::MANAGER_CONTROL).await?;
        Err(CoreError::Unsupported("not implemented".into()))
    }

    /// The ModemManager release we behave like, not this crate's version:
    /// clients compare it against MM releases. The interfaces are 1.24's,
    /// but cellbroadcastd treats any 1.24.x as lacking SetChannels (true of
    /// MM 1.24, not of us), and upstream's CellBroadcast was complete by the
    /// 1.25.95 release candidate.
    #[zbus(property)]
    async fn version(&self) -> &str {
        "1.25.95"
    }
}
