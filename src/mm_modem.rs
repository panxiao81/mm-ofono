//! `org.freedesktop.ModemManager1.Modem` for one oFono modem.

use std::collections::HashMap;

use zbus::{
    Connection, ObjectServer, interface,
    object_server::SignalEmitter,
    {OwnedObjectPath, OwnedVariant, Variant},
};

use crate::derive::{self, Identity, ModemStatus, RADIO};
use crate::bearer::{Contexts, Request};
use crate::error::{CoreError, Result};
use crate::mm_bearer::{self, Bearer};
use crate::mm::{MMBearerIpFamily as ip, MMModemPowerState as power};
use crate::ofono::{self, MODEM};

#[derive(Debug, Default)]
pub struct Modem {
    pub ofono_path: String,
    pub sim: Option<OwnedObjectPath>,
    pub status: ModemStatus,
    pub identity: Identity,
    pub bearers: Vec<OwnedObjectPath>,
    pub contexts: Contexts,
}

impl Modem {
    async fn set(&self, conn: &Connection, iface: &str, key: &str, value: Variant<'_>) -> Result<()> {
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
    async fn enable(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, enable: bool) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        if enable {
            self.set(conn, MODEM, "Powered", true.into()).await?;
        }
        self.set(conn, MODEM, "Online", enable.into()).await
    }

    async fn list_bearers(&self) -> Vec<OwnedObjectPath> {
        self.bearers.clone()
    }

    async fn create_bearer(
        &mut self, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        properties: HashMap<String, OwnedVariant>,
    ) -> Result<OwnedObjectPath> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let path = mm_bearer::new_bearer(conn, server, &self.ofono_path, &self.contexts, Request::from_dict(&properties)).await?;
        self.bearers.push(path.clone());
        self.bearers_changed(&em).await?;
        Ok(path)
    }

    async fn delete_bearer(
        &mut self, #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        bearer: OwnedObjectPath,
    ) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let Some(i) = self.bearers.iter().position(|b| *b == bearer) else {
            return Err(CoreError::Failed(format!("no bearer {bearer}")));
        };
        let b = server.interface::<_, Bearer>(&bearer).await?;
        b.get().await.disconnect_inner(conn, b.signal_emitter()).await?;
        server.remove::<Bearer, _>(&bearer).await?;
        self.bearers.remove(i);
        self.bearers_changed(&em).await?;
        Ok(())
    }

    async fn reset(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Self::unsupported("Reset")
    }

    async fn factory_reset(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _code: &str) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Self::unsupported("FactoryReset")
    }

    async fn set_power_state(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, state: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
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

    async fn set_current_capabilities(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _capabilities: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Self::unsupported("SetCurrentCapabilities")
    }

    async fn set_current_modes(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] conn: &Connection, modes: (u32, u32)) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let pref = derive::preference_for(&self.status.supported_modes, modes).ok_or_else(|| {
            CoreError::Unsupported(format!("mode combination {modes:?} can't be expressed in oFono"))
        })?;
        self.set(conn, RADIO, "TechnologyPreference", pref.into()).await
    }

    async fn set_current_bands(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _bands: Vec<u32>) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Self::unsupported("SetCurrentBands")
    }

    async fn set_primary_sim_slot(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _sim_slot: u32) -> Result<()> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        Err(CoreError::Unsupported("each SIM slot is its own modem; there is no slot to switch".into()))
    }

    async fn get_cell_info(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<Vec<HashMap<String, OwnedVariant>>> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
        let path = header.path().ok_or_else(|| CoreError::Failed("no path".into()))?.to_owned();
        let subscribed = crate::mm_signal::subscribed(conn, &path).await;
        Ok(crate::signal::query_cells(conn, &self.ofono_path, subscribed).await?)
    }

    async fn command(&self, #[zbus(connection)] conn: &zbus::Connection, #[zbus(header)] header: zbus::message::Header<'_>, _cmd: &str, _timeout: u32) -> Result<String> {
        crate::auth::check(conn, &header, crate::auth::DEVICE_CONTROL).await?;
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
        self.bearers.clone()
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
