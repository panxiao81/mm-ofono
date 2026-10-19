//! `org.freedesktop.ModemManager1.Modem.Signal`. Cell measurements cost
//! power, so the modem task subscribes to oFono's CellInfo only while a
//! client has asked for a refresh rate (Setup(0) turns it off), as in
//! ModemManager.

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;
use zbus::{OwnedVariant, Variant, interface, object_server::SignalEmitter};

use crate::error::Result;
use crate::modem::Event;
use crate::signal::{Dict, Values};

#[derive(Debug, Default)]
pub struct ModemSignal {
    pub rate: u32,
    pub rssi_threshold: u32,
    pub error_rate_threshold: bool,
    pub values: Values,
    /// To the modem task, which owns the CellInfo subscription.
    pub task: Option<UnboundedSender<Event>>,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Signal")]
impl ModemSignal {
    async fn setup(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>, rate: u32) -> Result<()> {
        if (rate > 0) != (self.rate > 0) {
            if let Some(task) = &self.task {
                let _ = task.send(Event::CellInfo(rate > 0));
            }
        }
        self.rate = rate;
        self.rate_changed(&em).await?;
        Ok(())
    }

    /// Thresholds are recorded; values are announced on every change anyway.
    async fn setup_thresholds(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>, settings: HashMap<String, OwnedVariant>) -> Result<()> {
        if let Some(Variant::U32(t)) = settings.get("rssi-threshold").map(|v| &**v) {
            self.rssi_threshold = *t;
            self.rssi_threshold_changed(&em).await?;
        }
        if let Some(Variant::Bool(b)) = settings.get("error-rate-threshold").map(|v| &**v) {
            self.error_rate_threshold = *b;
            self.error_rate_threshold_changed(&em).await?;
        }
        Ok(())
    }

    #[zbus(property)]
    async fn rate(&self) -> u32 {
        self.rate
    }

    #[zbus(property)]
    async fn rssi_threshold(&self) -> u32 {
        self.rssi_threshold
    }

    #[zbus(property)]
    async fn error_rate_threshold(&self) -> bool {
        self.error_rate_threshold
    }

    #[zbus(property)]
    async fn cdma(&self) -> Dict {
        Dict::new()
    }

    #[zbus(property)]
    async fn evdo(&self) -> Dict {
        Dict::new()
    }

    #[zbus(property)]
    async fn gsm(&self) -> Dict {
        crate::bearer::clone_dict(&self.values.gsm)
    }

    #[zbus(property)]
    async fn umts(&self) -> Dict {
        crate::bearer::clone_dict(&self.values.umts)
    }

    #[zbus(property)]
    async fn lte(&self) -> Dict {
        crate::bearer::clone_dict(&self.values.lte)
    }

    #[zbus(property)]
    async fn nr5g(&self) -> Dict {
        crate::bearer::clone_dict(&self.values.nr5g)
    }
}
