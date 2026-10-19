//! `org.freedesktop.ModemManager1.Modem` for one oFono modem.

use std::collections::HashMap;

use zbus::{
    Connection, interface,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::derive::{self, Identity, ModemStatus, RADIO};
use crate::error::{CoreError, Result};
use crate::mm::{MMBearerIpFamily as ip, MMModemPowerState as power};
use crate::ofono::{self, MODEM};

#[derive(Debug, Default)]
pub struct Modem {
    pub ofono_path: String,
    pub sim: Option<OwnedObjectPath>,
    pub status: ModemStatus,
    pub identity: Identity,
}

impl Modem {
    async fn set(&self, conn: &Connection, iface: &str, key: &str, value: Value<'_>) -> Result<()> {
        Ok(ofono::set_property(conn, &self.ofono_path, iface, key, value).await?)
    }

    fn unsupported<T>(what: &str) -> Result<T> {
        Err(CoreError::Unsupported(format!("{what} is not supported by oFono")))
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Modem")]
impl Modem {
    /// Enabled = radio on. Disabling keeps the modem powered with the radio
    /// off (low power), as ModemManager does.
    async fn enable(&self, #[zbus(connection)] conn: &Connection, enable: bool) -> Result<()> {
        if enable {
            self.set(conn, MODEM, "Powered", true.into()).await?;
        }
        self.set(conn, MODEM, "Online", enable.into()).await
    }

    async fn list_bearers(&self) -> Vec<OwnedObjectPath> {
        vec![]
    }

    async fn create_bearer(&self, _properties: HashMap<String, OwnedValue>) -> Result<OwnedObjectPath> {
        Err(CoreError::Unsupported("bearers are not implemented yet".into()))
    }

    async fn delete_bearer(&self, _bearer: OwnedObjectPath) -> Result<()> {
        Err(CoreError::Unsupported("bearers are not implemented yet".into()))
    }

    async fn reset(&self) -> Result<()> {
        Self::unsupported("Reset")
    }

    async fn factory_reset(&self, _code: &str) -> Result<()> {
        Self::unsupported("FactoryReset")
    }

    async fn set_power_state(&self, #[zbus(connection)] conn: &Connection, state: u32) -> Result<()> {
        match state {
            power::MM_MODEM_POWER_STATE_ON => {
                self.set(conn, MODEM, "Powered", true.into()).await?;
                self.set(conn, MODEM, "Online", true.into()).await
            }
            power::MM_MODEM_POWER_STATE_LOW => self.set(conn, MODEM, "Online", false.into()).await,
            power::MM_MODEM_POWER_STATE_OFF => self.set(conn, MODEM, "Powered", false.into()).await,
            _ => Err(CoreError::Failed(format!("invalid power state {state}"))),
        }
    }

    async fn set_current_capabilities(&self, _capabilities: u32) -> Result<()> {
        Self::unsupported("SetCurrentCapabilities")
    }

    async fn set_current_modes(&self, #[zbus(connection)] conn: &Connection, modes: (u32, u32)) -> Result<()> {
        let pref = derive::preference_for(&self.status.supported_modes, modes).ok_or_else(|| {
            CoreError::Unsupported(format!("mode combination {modes:?} can't be expressed in oFono"))
        })?;
        self.set(conn, RADIO, "TechnologyPreference", pref.into()).await
    }

    async fn set_current_bands(&self, _bands: Vec<u32>) -> Result<()> {
        Self::unsupported("SetCurrentBands")
    }

    async fn set_primary_sim_slot(&self, _sim_slot: u32) -> Result<()> {
        Err(CoreError::Unsupported("SIM slot switching is not implemented yet".into()))
    }

    async fn get_cell_info(&self) -> Result<Vec<HashMap<String, OwnedValue>>> {
        Err(CoreError::Unsupported("cell info is not implemented yet".into()))
    }

    async fn command(&self, _cmd: &str, _timeout: u32) -> Result<String> {
        Self::unsupported("Command")
    }

    #[zbus(signal, name = "StateChanged")]
    pub async fn emit_state_changed(emitter: &SignalEmitter<'_>, old: i32, new: i32, reason: u32) -> zbus::Result<()>;

    #[zbus(property)]
    async fn sim(&self) -> OwnedObjectPath {
        self.sim.clone().unwrap_or_else(|| OwnedObjectPath::try_from("/").unwrap())
    }

    #[zbus(property)]
    async fn sim_slots(&self) -> Vec<OwnedObjectPath> {
        vec![]
    }

    #[zbus(property)]
    async fn primary_sim_slot(&self) -> u32 {
        0
    }

    #[zbus(property)]
    async fn bearers(&self) -> Vec<OwnedObjectPath> {
        vec![]
    }

    #[zbus(property)]
    async fn supported_capabilities(&self) -> Vec<u32> {
        vec![self.status.capabilities]
    }

    #[zbus(property)]
    async fn current_capabilities(&self) -> u32 {
        self.status.capabilities
    }

    #[zbus(property)]
    async fn max_bearers(&self) -> u32 {
        1
    }

    #[zbus(property)]
    async fn max_active_bearers(&self) -> u32 {
        1
    }

    #[zbus(property)]
    async fn max_active_multiplexed_bearers(&self) -> u32 {
        0
    }

    #[zbus(property)]
    async fn manufacturer(&self) -> &str {
        &self.identity.manufacturer
    }

    #[zbus(property)]
    async fn model(&self) -> &str {
        &self.identity.model
    }

    #[zbus(property)]
    async fn revision(&self) -> &str {
        &self.status.revision
    }

    #[zbus(property)]
    async fn carrier_configuration(&self) -> &str {
        ""
    }

    #[zbus(property)]
    async fn carrier_configuration_revision(&self) -> &str {
        ""
    }

    #[zbus(property)]
    async fn hardware_revision(&self) -> &str {
        &self.identity.hardware_revision
    }

    #[zbus(property)]
    async fn device_identifier(&self) -> &str {
        &self.identity.device_identifier
    }

    #[zbus(property)]
    async fn device(&self) -> &str {
        &self.identity.device
    }

    #[zbus(property)]
    async fn physdev(&self) -> &str {
        &self.identity.device
    }

    #[zbus(property)]
    async fn drivers(&self) -> Vec<String> {
        self.identity.drivers.clone()
    }

    #[zbus(property)]
    async fn plugin(&self) -> &str {
        "mm-ofono"
    }

    #[zbus(property)]
    async fn primary_port(&self) -> &str {
        &self.identity.primary_port
    }

    #[zbus(property)]
    async fn ports(&self) -> Vec<(String, u32)> {
        self.identity.ports.clone()
    }

    #[zbus(property)]
    async fn equipment_identifier(&self) -> &str {
        &self.status.equipment_identifier
    }

    #[zbus(property)]
    async fn unlock_required(&self) -> u32 {
        self.status.unlock_required
    }

    #[zbus(property)]
    async fn unlock_retries(&self) -> HashMap<u32, u32> {
        self.status.unlock_retries.clone()
    }

    #[zbus(property)]
    async fn state(&self) -> i32 {
        self.status.state
    }

    #[zbus(property)]
    async fn state_failed_reason(&self) -> u32 {
        self.status.failed_reason
    }

    #[zbus(property)]
    async fn access_technologies(&self) -> u32 {
        self.status.access_technologies
    }

    #[zbus(property)]
    async fn signal_quality(&self) -> (u32, bool) {
        self.status.signal_quality
    }

    #[zbus(property)]
    async fn own_numbers(&self) -> Vec<String> {
        self.status.own_numbers.clone()
    }

    #[zbus(property)]
    async fn power_state(&self) -> u32 {
        self.status.power_state
    }

    #[zbus(property)]
    async fn supported_modes(&self) -> Vec<(u32, u32)> {
        self.status.supported_modes.clone()
    }

    #[zbus(property)]
    async fn current_modes(&self) -> (u32, u32) {
        self.status.current_modes
    }

    #[zbus(property)]
    async fn supported_bands(&self) -> Vec<u32> {
        vec![]
    }

    #[zbus(property)]
    async fn current_bands(&self) -> Vec<u32> {
        vec![]
    }

    #[zbus(property)]
    async fn supported_ip_families(&self) -> u32 {
        ip::MM_BEARER_IP_FAMILY_IPV4 | ip::MM_BEARER_IP_FAMILY_IPV6 | ip::MM_BEARER_IP_FAMILY_IPV4V6
    }
}
