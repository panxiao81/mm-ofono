/// Errors in ModemManager's own namespace, so libmm-glib clients map them
/// to the right `MMCoreError` codes.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.ModemManager1.Error.Core")]
pub enum CoreError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Unsupported(String),
    Unauthorized(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
