//! `org.freedesktop.ModemManager1.Modem.Modem3gpp`, backed by
//! `org.ofono.NetworkRegistration`.

use std::collections::HashMap;

use zbus::{Connection, OwnedObjectPath, OwnedVariant, interface};

use crate::bearer::{self, CONTEXT, Contexts};
use crate::derive::{self, CONNMAN, NETREG, Status3gpp};
use crate::error::{CoreError, Result};
use crate::ofono::{self, Props, SERVICE};

#[derive(Debug, Default)]
pub struct Modem3gpp {
    pub ofono_path: String,
    pub status: Status3gpp,
    pub contexts: Contexts,
}

fn unsupported<T>(what: &str) -> Result<T> {
    Err(CoreError::Unsupported(format!("{what} is not supported by oFono")))
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp")]
impl Modem3gpp {
    /// "" registers automatically, otherwise with the operator "MCCMNC".
    async fn register(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, operator_id: &str) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        if operator_id.is_empty() {
            conn.call_method(Some(SERVICE), self.ofono_path.as_str(), Some(NETREG), "Register", &()).await?;
            return Ok(());
        }
        let operators: Vec<(OwnedObjectPath, Props)> = conn
            .call_method(Some(SERVICE), self.ofono_path.as_str(), Some(NETREG), "GetOperators", &())
            .await?
            .body()
            .deserialize()?;
        let (op, _) = operators
            .iter()
            .find(|(_, p)| derive::operator_code(Some(p)) == operator_id)
            .ok_or_else(|| CoreError::Failed(format!("operator {operator_id} not found; scan first")))?;
        conn.call_method(Some(SERVICE), op, Some("org.ofono.NetworkOperator"), "Register", &()).await?;
        Ok(())
    }

    async fn scan(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection) -> Result<Vec<HashMap<String, OwnedVariant>>> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let operators: Vec<(OwnedObjectPath, Props)> = conn
            .call_method(Some(SERVICE), self.ofono_path.as_str(), Some(NETREG), "Scan", &())
            .await?
            .body()
            .deserialize()?;
        Ok(operators.iter().map(|(_, p)| derive::scan_result(p)).collect())
    }

    async fn set_eps_ue_mode_operation(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _mode: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        unsupported("SetEpsUeModeOperation")
    }

    /// Writes the attach context (see [`bearer::attach_context`]);
    /// ofono-binder passes it on to the modem. oFono refuses changes while
    /// the context is active, and so does this.
    async fn set_initial_eps_bearer_settings(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, settings: HashMap<String, OwnedVariant>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let (path, changes) = {
            let contexts = self.contexts.lock().unwrap();
            let (path, ctx) = bearer::attach_context(&contexts).ok_or_else(|| CoreError::WrongState("oFono has no internet context".into()))?;
            (path.to_owned(), bearer::attach_context_settings(&settings, ctx))
        };
        for (key, value) in changes {
            ofono::set_property(conn, &path, CONTEXT, key, value.as_str().into()).await?;
        }
        Ok(())
    }

    async fn set_nr5g_registration_settings(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _properties: HashMap<String, OwnedVariant>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        unsupported("SetNr5gRegistrationSettings")
    }

    async fn disable_facility_lock(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _properties: (u32, String)) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        unsupported("DisableFacilityLock")
    }

    async fn set_carrier_lock(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _data: Vec<u8>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        unsupported("SetCarrierLock")
    }

    /// oFono attaches while ConnectionManager is Powered.
    async fn set_packet_service_state(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, state: u32) -> Result<()> {
        use crate::mm::MMModem3gppPacketServiceState as ps;
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let powered = match state {
            ps::MM_MODEM_3GPP_PACKET_SERVICE_STATE_ATTACHED => true,
            ps::MM_MODEM_3GPP_PACKET_SERVICE_STATE_DETACHED => false,
            _ => return Err(CoreError::Failed(format!("invalid packet service state {state}"))),
        };
        Ok(ofono::set_property(conn, &self.ofono_path, CONNMAN, "Powered", powered.into()).await?)
    }

    #[zbus(property)]
    async fn imei(&self) -> &str {
        &self.status.imei
    }

    #[zbus(property)]
    async fn registration_state(&self) -> u32 {
        self.status.registration_state
    }

    #[zbus(property)]
    async fn network_rejection(&self) -> HashMap<String, OwnedVariant> {
        HashMap::new()
    }

    #[zbus(property)]
    async fn operator_code(&self) -> &str {
        &self.status.operator_code
    }

    #[zbus(property)]
    async fn operator_name(&self) -> &str {
        &self.status.operator_name
    }

    #[zbus(property)]
    async fn enabled_facility_locks(&self) -> u32 {
        self.status.enabled_facility_locks
    }

    #[zbus(property)]
    async fn subscription_state(&self) -> u32 {
        crate::mm::MMModem3gppSubscriptionState::MM_MODEM_3GPP_SUBSCRIPTION_STATE_UNKNOWN
    }

    #[zbus(property)]
    async fn eps_ue_mode_operation(&self) -> u32 {
        crate::mm::MMModem3gppEpsUeModeOperation::MM_MODEM_3GPP_EPS_UE_MODE_OPERATION_UNKNOWN
    }

    #[zbus(property)]
    async fn pco(&self) -> Vec<(u32, bool, Vec<u8>)> {
        vec![]
    }

    #[zbus(property)]
    async fn initial_eps_bearer(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/").unwrap()
    }

    #[zbus(property)]
    async fn initial_eps_bearer_settings(&self) -> HashMap<String, OwnedVariant> {
        self.status.initial_eps.as_ref().map(|s| s.to_dict()).unwrap_or_default()
    }

    #[zbus(property)]
    async fn packet_service_state(&self) -> u32 {
        self.status.packet_service_state
    }

    #[zbus(property)]
    async fn nr5g_registration_settings(&self) -> HashMap<String, OwnedVariant> {
        HashMap::new()
    }
}
