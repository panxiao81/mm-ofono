//! PolicyKit authorization, as ModemManager does it: every method that
//! changes something or reaches the network checks one of ModemManager's
//! own actions (org.freedesktop.ModemManager1.policy, shipped with the
//! modemmanager package) for the calling bus name, before doing anything.
//!
//! zbus_polkit would do, but only exists for zbus 5; this is the one call.

use std::collections::HashMap;

use zbus::{Connection, Variant, message::Header, proxy};

use crate::error::{CoreError, Result};

pub const MANAGER_CONTROL: &str = "org.freedesktop.ModemManager1.Control";
pub const DEVICE_CONTROL: &str = "org.freedesktop.ModemManager1.Device.Control";
pub const MESSAGING: &str = "org.freedesktop.ModemManager1.Messaging";
pub const VOICE: &str = "org.freedesktop.ModemManager1.Voice";
pub const USSD: &str = "org.freedesktop.ModemManager1.USSD";
pub const LOCATION: &str = "org.freedesktop.ModemManager1.Location";

/// POLKIT_CHECK_AUTHORIZATION_FLAGS_ALLOW_USER_INTERACTION
const ALLOW_USER_INTERACTION: u32 = 1;

#[proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Variant<'_>>),
        action_id: &str,
        details: &HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

/// Fails unless the caller is authorized for `action`. Messages match
/// ModemManager's, as clients may show them.
pub async fn check(conn: &Connection, header: &Header<'_>, action: &str) -> Result<()> {
    let sender = header.sender().ok_or_else(|| CoreError::Failed("PolicyKit authorization failed: no sender".into()))?;
    let subject = ("system-bus-name", HashMap::from([("name", Variant::from(sender.as_str()))]));
    let reply = async {
        AuthorityProxy::new(conn)
            .await?
            .check_authorization(&subject, action, &HashMap::new(), ALLOW_USER_INTERACTION, "")
            .await
    }
    .await;
    match reply {
        Err(e) => Err(CoreError::Failed(format!("PolicyKit authorization failed: '{e}'"))),
        Ok((true, _, _)) => Ok(()),
        Ok((false, true, _)) => Err(CoreError::Unauthorized(format!("PolicyKit authorization failed: challenge needed for '{action}'"))),
        Ok(_) => Err(CoreError::Unauthorized(format!("PolicyKit authorization failed: not authorized for '{action}'"))),
    }
}
