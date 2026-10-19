//! `org.freedesktop.ModemManager1.Modem.Modem3gpp`, backed by
//! `org.ofono.NetworkRegistration`.

use std::collections::HashMap;

use zbus::{Connection, OwnedObjectPath, OwnedVariant, interface};

use crate::derive::{self, NETREG, Status3gpp};
use crate::error::{CoreError, Result};
use crate::ofono::{Props, SERVICE};

#[derive(Debug, Default)]
pub struct Modem3gpp {
    pub ofono_path: String,
    pub status: Status3gpp,
}

fn unsupported<T>(what: &str) -> Result<T> {
    Err(CoreError::Unsupported(format!("{what} is not supported by oFono")))
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp")]
impl Modem3gpp {
    /// "" registers automatically, otherwise with the operator "MCCMNC".
    async fn register(&self, #[zbus(connection)] conn: &Connection, operator_id: &str) -> Result<()> {
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

    async fn scan(&self, #[zbus(connection)] conn: &Connection) -> Result<Vec<HashMap<String, OwnedVariant>>> {
        let operators: Vec<(OwnedObjectPath, Props)> = conn
            .call_method(Some(SERVICE), self.ofono_path.as_str(), Some(NETREG), "Scan", &())
            .await?
            .body()
            .deserialize()?;
        Ok(operators.iter().map(|(_, p)| derive::scan_result(p)).collect())
    }

    async fn set_eps_ue_mode_operation(&self, _mode: u32) -> Result<()> {
        unsupported("SetEpsUeModeOperation")
    }

    async fn set_initial_eps_bearer_settings(&self, _settings: HashMap<String, OwnedVariant>) -> Result<()> {
        unsupported("SetInitialEpsBearerSettings")
    }

    async fn set_nr5g_registration_settings(&self, _properties: HashMap<String, OwnedVariant>) -> Result<()> {
        unsupported("SetNr5gRegistrationSettings")
    }

    async fn disable_facility_lock(&self, _properties: (u32, String)) -> Result<()> {
        unsupported("DisableFacilityLock")
    }

    async fn set_carrier_lock(&self, _data: Vec<u8>) -> Result<()> {
        unsupported("SetCarrierLock")
    }

    async fn set_packet_service_state(&self, _state: u32) -> Result<()> {
        unsupported("SetPacketServiceState")
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
        HashMap::new()
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
