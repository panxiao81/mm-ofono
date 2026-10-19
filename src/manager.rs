use std::collections::HashMap;

use zbus::{interface, zvariant::OwnedValue};

use crate::error::{CoreError, Result};

/// `org.freedesktop.ModemManager1` at the root object.
pub struct Manager;

#[interface(name = "org.freedesktop.ModemManager1")]
impl Manager {
    /// oFono discovers modems on its own; there is nothing to rescan.
    async fn scan_devices(&self) {}

    async fn set_logging(&self, _level: &str) -> Result<()> {
        Err(CoreError::Unsupported("use RUST_LOG instead".into()))
    }

    async fn report_kernel_event(&self, _properties: HashMap<String, OwnedValue>) -> Result<()> {
        Err(CoreError::Unsupported("devices come from oFono, not udev".into()))
    }

    async fn inhibit_device(&self, _uid: &str, _inhibit: bool) -> Result<()> {
        Err(CoreError::Unsupported("not implemented".into()))
    }

    /// The ModemManager API level we implement, not this crate's version:
    /// clients compare it against MM releases.
    #[zbus(property)]
    async fn version(&self) -> &str {
        "1.24.0"
    }
}
