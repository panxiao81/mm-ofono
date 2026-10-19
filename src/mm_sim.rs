//! `org.freedesktop.ModemManager1.Sim`, backed by `org.ofono.SimManager`.

use zbus::{Connection, interface};

use crate::derive::{SIM_MANAGER, SimStatus};
use crate::error::{CoreError, Result};
use crate::mm::{MMSimEsimStatus as esim, MMSimRemovability as removability, MMSimType as sim_type};

#[derive(Debug, Default)]
pub struct Sim {
    pub ofono_path: String,
    pub status: SimStatus,
}

impl Sim {
    async fn call(&self, conn: &Connection, method: &str, args: &(impl serde::Serialize + zbus::DynamicType)) -> Result<()> {
        conn.call_method(Some(crate::ofono::SERVICE), self.ofono_path.as_str(), Some(SIM_MANAGER), method, args)
            .await?;
        Ok(())
    }
}

#[interface(name = "org.freedesktop.ModemManager1.Sim")]
impl Sim {
    async fn send_pin(&self, #[zbus(connection)] conn: &Connection, pin: &str) -> Result<()> {
        self.call(conn, "EnterPin", &("pin", pin)).await
    }

    async fn send_puk(&self, #[zbus(connection)] conn: &Connection, puk: &str, pin: &str) -> Result<()> {
        self.call(conn, "ResetPin", &("puk", puk, pin)).await
    }

    async fn enable_pin(&self, #[zbus(connection)] conn: &Connection, pin: &str, enabled: bool) -> Result<()> {
        let method = if enabled { "LockPin" } else { "UnlockPin" };
        self.call(conn, method, &("pin", pin)).await
    }

    async fn change_pin(&self, #[zbus(connection)] conn: &Connection, old_pin: &str, new_pin: &str) -> Result<()> {
        self.call(conn, "ChangePin", &("pin", old_pin, new_pin)).await
    }

    async fn set_preferred_networks(&self, _preferred_networks: Vec<(String, u32)>) -> Result<()> {
        Err(CoreError::Unsupported("oFono does not manage the preferred network list".into()))
    }

    #[zbus(property)]
    async fn active(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn sim_identifier(&self) -> &str {
        &self.status.identifier
    }

    #[zbus(property)]
    async fn imsi(&self) -> &str {
        &self.status.imsi
    }

    #[zbus(property)]
    async fn eid(&self) -> &str {
        ""
    }

    #[zbus(property)]
    async fn operator_identifier(&self) -> &str {
        &self.status.operator_identifier
    }

    #[zbus(property)]
    async fn operator_name(&self) -> &str {
        &self.status.operator_name
    }

    #[zbus(property)]
    async fn emergency_numbers(&self) -> Vec<String> {
        self.status.emergency_numbers.clone()
    }

    #[zbus(property)]
    async fn preferred_networks(&self) -> Vec<(String, u32)> {
        vec![]
    }

    #[zbus(property)]
    async fn gid1(&self) -> Vec<u8> {
        vec![]
    }

    #[zbus(property)]
    async fn gid2(&self) -> Vec<u8> {
        vec![]
    }

    #[zbus(property)]
    async fn sim_type(&self) -> u32 {
        sim_type::MM_SIM_TYPE_PHYSICAL
    }

    #[zbus(property)]
    async fn esim_status(&self) -> u32 {
        esim::MM_SIM_ESIM_STATUS_UNKNOWN
    }

    #[zbus(property)]
    async fn removability(&self) -> u32 {
        removability::MM_SIM_REMOVABILITY_REMOVABLE
    }
}
