//! ModemManager's view of one oFono modem, computed from the cached oFono
//! properties. Pure functions: no D-Bus here, so every rule is unit-testable.

use std::collections::HashMap;

use zbus::Variant;

use crate::mm::{
    MMModemAccessTechnology as tech, MMModemCapability as cap, MMModemLock as lock,
    MMModemMode as mode, MMModemPowerState as power, MMModemState as state,
    MMModemStateFailedReason as failed,
};
use crate::device::PhysicalDevice;
use crate::ofono::{Iface, MODEM};

pub const SIM_MANAGER: &str = "org.ofono.SimManager";
pub const NETREG: &str = "org.ofono.NetworkRegistration";
pub const RADIO: &str = "org.ofono.RadioSettings";
pub const VOICE: &str = "org.ofono.VoiceCallManager";

/// Cached properties of every interface of one oFono modem; an interface
/// is present iff oFono lists it in `org.ofono.Modem.Interfaces`.
#[derive(Debug, Default)]
pub struct OfonoModem {
    pub ifaces: HashMap<String, Iface>,
}

impl OfonoModem {
    fn get(&self, iface: &str, key: &str) -> Option<&Variant<'static>> {
        self.ifaces.get(iface)?.props.get(key).map(|v| &**v)
    }

    fn str(&self, iface: &str, key: &str) -> Option<&str> {
        match self.get(iface, key)? {
            Variant::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn bool(&self, iface: &str, key: &str) -> Option<bool> {
        match self.get(iface, key)? {
            Variant::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn strings(&self, iface: &str, key: &str) -> Vec<String> {
        match self.get(iface, key) {
            Some(Variant::Array(a)) => a
                .iter()
                .filter_map(|v| match v {
                    Variant::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    pub fn has(&self, iface: &str) -> bool {
        self.ifaces.contains_key(iface)
    }

    /// Enough is known to show this modem: power state and, once oFono has
    /// the SIM interface, whether a card is inserted. Exporting earlier made
    /// clients see a SIM-less modem at boot.
    pub fn ready(&self) -> bool {
        self.bool(MODEM, "Powered").is_some() && self.bool(SIM_MANAGER, "Present").is_some()
    }

    pub fn sim_present(&self) -> bool {
        self.bool(SIM_MANAGER, "Present") == Some(true)
    }
}

/// ModemManager properties that follow from oFono state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModemStatus {
    pub state: state::Type,
    pub failed_reason: failed::Type,
    pub power_state: power::Type,
    pub unlock_required: lock::Type,
    pub unlock_retries: HashMap<u32, u32>,
    pub access_technologies: tech::Type,
    pub signal_quality: (u32, bool),
    pub own_numbers: Vec<String>,
    pub capabilities: cap::Type,
    pub supported_modes: Vec<(u32, u32)>,
    pub current_modes: (u32, u32),
    pub revision: String,
    pub equipment_identifier: String,
}

/// oFono `PinRequired` / `Retries` keys and the MM lock they correspond to.
const LOCKS: [(&str, lock::Type); 16] = [
    ("pin", lock::MM_MODEM_LOCK_SIM_PIN),
    ("pin2", lock::MM_MODEM_LOCK_SIM_PIN2),
    ("puk", lock::MM_MODEM_LOCK_SIM_PUK),
    ("puk2", lock::MM_MODEM_LOCK_SIM_PUK2),
    ("service", lock::MM_MODEM_LOCK_PH_SP_PIN),
    ("servicepuk", lock::MM_MODEM_LOCK_PH_SP_PUK),
    ("network", lock::MM_MODEM_LOCK_PH_NET_PIN),
    ("networkpuk", lock::MM_MODEM_LOCK_PH_NET_PUK),
    ("phone", lock::MM_MODEM_LOCK_PH_SIM_PIN),
    ("corp", lock::MM_MODEM_LOCK_PH_CORP_PIN),
    ("corppuk", lock::MM_MODEM_LOCK_PH_CORP_PUK),
    ("firstphone", lock::MM_MODEM_LOCK_PH_FSIM_PIN),
    ("firstphonepuk", lock::MM_MODEM_LOCK_PH_FSIM_PUK),
    ("netsub", lock::MM_MODEM_LOCK_PH_NETSUB_PIN),
    ("netsubpuk", lock::MM_MODEM_LOCK_PH_NETSUB_PUK),
    ("none", lock::MM_MODEM_LOCK_NONE),
];

fn lock_for(ofono: &str) -> Option<lock::Type> {
    LOCKS.iter().find(|(k, _)| *k == ofono).map(|(_, l)| *l)
}

/// oFono radio technologies, lowest first, with the MM mode bit and capability.
const RATS: [(&str, mode::Type, cap::Type); 4] = [
    ("gsm", mode::MM_MODEM_MODE_2G, cap::MM_MODEM_CAPABILITY_GSM_UMTS),
    ("umts", mode::MM_MODEM_MODE_3G, cap::MM_MODEM_CAPABILITY_GSM_UMTS),
    ("lte", mode::MM_MODEM_MODE_4G, cap::MM_MODEM_CAPABILITY_LTE),
    ("nr", mode::MM_MODEM_MODE_5G, cap::MM_MODEM_CAPABILITY_5GNR),
];

fn access_technology(ofono: &str) -> tech::Type {
    match ofono {
        "gsm" => tech::MM_MODEM_ACCESS_TECHNOLOGY_GSM,
        "gprs" => tech::MM_MODEM_ACCESS_TECHNOLOGY_GPRS,
        "edge" => tech::MM_MODEM_ACCESS_TECHNOLOGY_EDGE,
        "umts" => tech::MM_MODEM_ACCESS_TECHNOLOGY_UMTS,
        "hsdpa" => tech::MM_MODEM_ACCESS_TECHNOLOGY_HSDPA,
        "hsupa" => tech::MM_MODEM_ACCESS_TECHNOLOGY_HSUPA,
        "hspa" => tech::MM_MODEM_ACCESS_TECHNOLOGY_HSPA,
        "lte" => tech::MM_MODEM_ACCESS_TECHNOLOGY_LTE,
        "nr" => tech::MM_MODEM_ACCESS_TECHNOLOGY_5GNR,
        _ => tech::MM_MODEM_ACCESS_TECHNOLOGY_UNKNOWN,
    }
}

/// An oFono preference for technology X means "X and everything below it,
/// preferring X". Returns MM's supported (allowed, preferred) combinations,
/// in the order of the available technologies.
pub fn supported_modes(available: &[String]) -> Vec<(u32, u32)> {
    let mut allowed = mode::MM_MODEM_MODE_NONE;
    RATS.iter()
        .filter(|(rat, _, _)| available.iter().any(|a| a == rat))
        .enumerate()
        .map(|(i, (_, bit, _))| {
            allowed |= bit;
            let preferred = if i == 0 { mode::MM_MODEM_MODE_NONE } else { *bit };
            (allowed, preferred)
        })
        .collect()
}

/// The supported combination for oFono's preference: the one whose top
/// mode is the preferred technology ("any" = the widest).
pub fn current_modes(supported: &[(u32, u32)], preference: Option<&str>) -> (u32, u32) {
    let none = (mode::MM_MODEM_MODE_NONE, mode::MM_MODEM_MODE_NONE);
    let Some(pref) = preference else { return none };
    if pref == "any" {
        return supported.last().copied().unwrap_or(none);
    }
    let Some((_, bit, _)) = RATS.iter().find(|r| r.0 == pref) else { return none };
    supported
        .iter()
        .copied()
        .find(|&(allowed, _)| allowed & !(bit - 1) == *bit)
        .unwrap_or(none)
}

/// The oFono technology preference for an MM (allowed, preferred) pair from
/// `supported`. oFono can't express "no preference", so a NONE preference is
/// taken as preferring the highest allowed mode.
pub fn preference_for(supported: &[(u32, u32)], (allowed, preferred): (u32, u32)) -> Option<&'static str> {
    let (allowed, _) = supported
        .iter()
        .find(|&&(a, p)| a == allowed && (p == preferred || preferred == mode::MM_MODEM_MODE_NONE))?;
    let top = 1 << (u32::BITS - 1 - allowed.leading_zeros());
    RATS.iter().find(|r| r.1 == top).map(|r| r.0)
}

pub fn status(m: &OfonoModem) -> ModemStatus {
    let powered = m.bool(MODEM, "Powered") == Some(true);
    let online = m.bool(MODEM, "Online") == Some(true);
    let pin = m.str(SIM_MANAGER, "PinRequired").unwrap_or("none");
    let reg = m.str(NETREG, "Status");

    let (st, reason) = if !powered {
        (state::MM_MODEM_STATE_DISABLED, failed::MM_MODEM_STATE_FAILED_REASON_NONE)
    } else if !m.sim_present() {
        (state::MM_MODEM_STATE_FAILED, failed::MM_MODEM_STATE_FAILED_REASON_SIM_MISSING)
    } else if pin != "none" {
        (state::MM_MODEM_STATE_LOCKED, failed::MM_MODEM_STATE_FAILED_REASON_NONE)
    } else if !online {
        (state::MM_MODEM_STATE_DISABLED, failed::MM_MODEM_STATE_FAILED_REASON_NONE)
    } else {
        let st = match reg {
            Some("registered" | "roaming") => state::MM_MODEM_STATE_REGISTERED,
            Some("searching") => state::MM_MODEM_STATE_SEARCHING,
            _ => state::MM_MODEM_STATE_ENABLED,
        };
        (st, failed::MM_MODEM_STATE_FAILED_REASON_NONE)
    };
    let registered = st == state::MM_MODEM_STATE_REGISTERED;

    let retries = match m.get(SIM_MANAGER, "Retries") {
        Some(Variant::Dict(d)) => d
            .iter()
            .filter_map(|(k, v)| match (k, v) {
                (Variant::Str(k), Variant::U8(n)) => Some((lock_for(k)?, u32::from(*n))),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    };

    let strength = match m.get(NETREG, "Strength") {
        Some(Variant::U8(s)) => u32::from(*s),
        _ => 0,
    };

    let available = m.strings(RADIO, "AvailableTechnologies");
    let supported = supported_modes(&available);
    let capabilities = match RATS.iter().filter(|r| available.contains(&r.0.to_owned())).fold(0, |c, r| c | r.2) {
        // ponytail: before RadioSettings shows up, assume a GSM/LTE phone so
        // NetworkManager treats it as 3GPP from the start.
        0 => cap::MM_MODEM_CAPABILITY_GSM_UMTS | cap::MM_MODEM_CAPABILITY_LTE,
        c => c,
    };
    let current = current_modes(&supported, m.str(RADIO, "TechnologyPreference"));

    ModemStatus {
        state: st,
        failed_reason: reason,
        power_state: if !powered {
            power::MM_MODEM_POWER_STATE_OFF
        } else if !online {
            power::MM_MODEM_POWER_STATE_LOW
        } else {
            power::MM_MODEM_POWER_STATE_ON
        },
        unlock_required: if m.sim_present() {
            lock_for(pin).unwrap_or(lock::MM_MODEM_LOCK_UNKNOWN)
        } else {
            lock::MM_MODEM_LOCK_UNKNOWN
        },
        unlock_retries: retries,
        access_technologies: if registered {
            access_technology(m.str(NETREG, "Technology").unwrap_or(""))
        } else {
            tech::MM_MODEM_ACCESS_TECHNOLOGY_UNKNOWN
        },
        signal_quality: if registered { (strength, true) } else { (0, false) },
        own_numbers: m.strings(SIM_MANAGER, "SubscriberNumbers"),
        capabilities,
        supported_modes: supported,
        current_modes: current,
        revision: m.str(MODEM, "Revision").unwrap_or_default().to_owned(),
        equipment_identifier: m.str(MODEM, "Serial").unwrap_or_default().to_owned(),
    }
}

/// ModemManager's identity properties for one modem.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Identity {
    pub manufacturer: String,
    pub model: String,
    pub hardware_revision: String,
    pub device: String,
    pub drivers: Vec<String>,
    pub primary_port: String,
    pub ports: Vec<(String, u32)>,
    pub device_identifier: String,
}

/// oFono's own values win; the hardware found by device discovery fills the
/// rest. The control "port" is the oFono modem (a vendor RIL slot), named
/// like a port: `/ril_0` -> `ril_0`.
pub fn identity(m: &OfonoModem, path: &str, dev: Option<&PhysicalDevice>) -> Identity {
    use sha1::{Digest, Sha1};
    use crate::mm::MMModemPortType as port;

    let pick = |ofono_key, hw: Option<&String>| {
        m.str(MODEM, ofono_key).map(str::to_owned).or_else(|| hw.cloned()).unwrap_or_default()
    };
    let manufacturer = pick("Manufacturer", dev.and_then(|d| d.manufacturer.as_ref()));
    let model = pick("Model", dev.and_then(|d| d.model.as_ref()));
    let revision = m.str(MODEM, "Revision").unwrap_or_default();

    let primary_port = path.trim_start_matches('/').to_owned();
    let mut ports = vec![(primary_port.clone(), port::MM_MODEM_PORT_TYPE_UNKNOWN)];
    if let Some(d) = dev {
        ports.push((d.net_port.clone(), port::MM_MODEM_PORT_TYPE_NET));
    }

    // Like ModemManager: a hash of what identifies the hardware and firmware,
    // stable across reboots and SIM changes, shared by identical devices.
    let ids = dev.and_then(|d| d.ids.as_deref()).unwrap_or_default();
    let digest = Sha1::digest([ids, revision, &manufacturer, &model].join("\n"));
    let device_identifier = digest.iter().map(|b| format!("{b:02x}")).collect();

    Identity {
        hardware_revision: dev.and_then(|d| d.hw_revision.clone()).unwrap_or_default(),
        device: dev.map(|d| d.sysfs.clone()).unwrap_or_default(),
        drivers: dev.map(|d| d.drivers.clone()).unwrap_or_default(),
        manufacturer,
        model,
        primary_port,
        ports,
        device_identifier,
    }
}

/// ModemManager SIM properties; `None` while no card is inserted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SimStatus {
    pub identifier: String,
    pub imsi: String,
    pub operator_identifier: String,
    pub operator_name: String,
    pub emergency_numbers: Vec<String>,
}

pub fn sim(m: &OfonoModem) -> Option<SimStatus> {
    if !m.sim_present() {
        return None;
    }
    let s = |k| m.str(SIM_MANAGER, k).unwrap_or_default().to_owned();
    Some(SimStatus {
        identifier: s("CardIdentifier"),
        imsi: s("SubscriberIdentity"),
        operator_identifier: format!("{}{}", s("MobileCountryCode"), s("MobileNetworkCode")),
        operator_name: s("ServiceProviderName"),
        emergency_numbers: m.strings(VOICE, "EmergencyNumbers"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::OwnedVariant;

    fn modem(props: &[(&str, &str, Variant<'static>)]) -> OfonoModem {
        let mut m = OfonoModem::default();
        for (iface, key, value) in props {
            m.ifaces
                .entry(iface.to_string())
                .or_default()
                .props
                .insert(key.to_string(), OwnedVariant::try_from(value.clone()).unwrap());
        }
        m
    }

    fn strs(v: &[&str]) -> Variant<'static> {
        Variant::from(v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn not_ready_until_sim_presence_is_known() {
        let m = modem(&[(MODEM, "Powered", true.into())]);
        assert!(!m.ready());
        let m = modem(&[(MODEM, "Powered", true.into()), (SIM_MANAGER, "Present", false.into())]);
        assert!(m.ready());
    }

    #[test]
    fn empty_slot_is_failed_sim_missing() {
        let s = status(&modem(&[(MODEM, "Powered", true.into()), (SIM_MANAGER, "Present", false.into())]));
        assert_eq!((s.state, s.failed_reason), (state::MM_MODEM_STATE_FAILED, failed::MM_MODEM_STATE_FAILED_REASON_SIM_MISSING));
        assert_eq!(s.unlock_required, lock::MM_MODEM_LOCK_UNKNOWN);
    }

    #[test]
    fn pin_locked() {
        let s = status(&modem(&[
            (MODEM, "Powered", true.into()),
            (SIM_MANAGER, "Present", true.into()),
            (SIM_MANAGER, "PinRequired", "puk".into()),
        ]));
        assert_eq!(s.state, state::MM_MODEM_STATE_LOCKED);
        assert_eq!(s.unlock_required, lock::MM_MODEM_LOCK_SIM_PUK);
    }

    #[test]
    fn radio_off_is_disabled_low_power() {
        let s = status(&modem(&[
            (MODEM, "Powered", true.into()),
            (MODEM, "Online", false.into()),
            (SIM_MANAGER, "Present", true.into()),
        ]));
        assert_eq!((s.state, s.power_state), (state::MM_MODEM_STATE_DISABLED, power::MM_MODEM_POWER_STATE_LOW));
    }

    /// The CT SIM roaming on KDDI, as in tests/traces/ct-ofono-restart.json.
    #[test]
    fn roaming_on_lte() {
        let mut retries = HashMap::new();
        retries.insert("pin".to_string(), 3u8);
        retries.insert("puk".to_string(), 10u8);
        let s = status(&modem(&[
            (MODEM, "Powered", true.into()),
            (MODEM, "Online", true.into()),
            (MODEM, "Revision", "sdx55.rmtefs-484436-87".into()),
            (SIM_MANAGER, "Present", true.into()),
            (SIM_MANAGER, "PinRequired", "none".into()),
            (SIM_MANAGER, "Retries", Variant::from(retries)),
            (NETREG, "Status", "roaming".into()),
            (NETREG, "Technology", "lte".into()),
            (NETREG, "Strength", 40u8.into()),
            (RADIO, "AvailableTechnologies", strs(&["gsm", "umts", "lte", "nr"])),
            (RADIO, "TechnologyPreference", "lte".into()),
        ]));
        assert_eq!(s.state, state::MM_MODEM_STATE_REGISTERED);
        assert_eq!(s.power_state, power::MM_MODEM_POWER_STATE_ON);
        assert_eq!(s.access_technologies, tech::MM_MODEM_ACCESS_TECHNOLOGY_LTE);
        assert_eq!(s.signal_quality, (40, true));
        assert_eq!(s.unlock_required, lock::MM_MODEM_LOCK_NONE);
        assert_eq!(s.unlock_retries[&lock::MM_MODEM_LOCK_SIM_PUK], 10);
        assert_eq!(
            s.capabilities,
            cap::MM_MODEM_CAPABILITY_GSM_UMTS | cap::MM_MODEM_CAPABILITY_LTE | cap::MM_MODEM_CAPABILITY_5GNR
        );
        let (g2, g3, g4) = (mode::MM_MODEM_MODE_2G, mode::MM_MODEM_MODE_3G, mode::MM_MODEM_MODE_4G);
        assert_eq!(s.current_modes, (g2 | g3 | g4, g4));
        assert_eq!(s.revision, "sdx55.rmtefs-484436-87");
    }

    #[test]
    fn modes_follow_available_technologies() {
        let (g2, g3, g4, g5) =
            (mode::MM_MODEM_MODE_2G, mode::MM_MODEM_MODE_3G, mode::MM_MODEM_MODE_4G, mode::MM_MODEM_MODE_5G);
        let all = ["gsm", "umts", "lte", "nr"].map(String::from);
        assert_eq!(
            supported_modes(&all),
            [(g2, 0), (g2 | g3, g3), (g2 | g3 | g4, g4), (g2 | g3 | g4 | g5, g5)]
        );
        // A modem without 2G/3G (oFono order isn't guaranteed either).
        let supported = supported_modes(&["nr".into(), "lte".into()]);
        assert_eq!(supported, [(g4, 0), (g4 | g5, g5)]);
        assert_eq!(current_modes(&supported, Some("lte")), (g4, 0));
        assert_eq!(current_modes(&supported, Some("nr")), (g4 | g5, g5));
        assert_eq!(current_modes(&supported, Some("any")), (g4 | g5, g5));
        assert_eq!(current_modes(&supported, Some("umts")), (0, 0));
        assert_eq!(current_modes(&supported, None), (0, 0));

        let supported = supported_modes(&all);
        assert_eq!(preference_for(&supported, (g2 | g3 | g4, g4)), Some("lte"));
        assert_eq!(preference_for(&supported, (g2 | g3 | g4, 0)), Some("lte"));
        assert_eq!(preference_for(&supported, (g2, 0)), Some("gsm"));
        assert_eq!(preference_for(&supported, (g2 | g3 | g4, g3)), None); // oFono can't prefer a lower RAT
        assert_eq!(preference_for(&supported, (g4, 0)), None); // 4G-only isn't expressible either
    }

    #[test]
    fn identity_prefers_ofono_then_hardware() {
        let dev = PhysicalDevice {
            sysfs: "/sys/devices/platform/soc/1c10000.qcom,pcie/pci0002:00/0002:00:00.0/0002:01:00.0".into(),
            drivers: vec!["mhi".into(), "mhi_netdev".into()],
            net_port: "rmnet_mhi0".into(),
            ids: Some("17cb:0306:17cb:010c".into()),
            hw_revision: Some("0x00".into()),
            manufacturer: Some("Qualcomm Technologies, Inc".into()),
            model: Some("SDX55 [Snapdragon X55 5G]".into()),
        };
        let m = modem(&[(MODEM, "Revision", "sdx55.rmtefs-484436-87".into())]);
        let id = identity(&m, "/ril_0", Some(&dev));
        assert_eq!(id.manufacturer, "Qualcomm Technologies, Inc");
        assert_eq!(id.model, "SDX55 [Snapdragon X55 5G]");
        assert_eq!(id.device, dev.sysfs);
        assert_eq!(id.primary_port, "ril_0");
        assert_eq!(id.ports, [("ril_0".into(), 1), ("rmnet_mhi0".into(), 2)]);
        assert_eq!(id.device_identifier.len(), 40);
        // Both SIM slots of one modem are the same device.
        assert_eq!(identity(&m, "/ril_1", Some(&dev)).device_identifier, id.device_identifier);

        let m = modem(&[(MODEM, "Manufacturer", "Acme".into())]);
        assert_eq!(identity(&m, "/ril_0", Some(&dev)).manufacturer, "Acme");
        assert_eq!(identity(&m, "/ril_0", None).model, "");
    }

    #[test]
    fn sim_only_when_present() {
        assert_eq!(sim(&modem(&[(SIM_MANAGER, "Present", false.into())])), None);
        let s = sim(&modem(&[
            (SIM_MANAGER, "Present", true.into()),
            (SIM_MANAGER, "MobileCountryCode", "460".into()),
            (SIM_MANAGER, "MobileNetworkCode", "11".into()),
            (VOICE, "EmergencyNumbers", strs(&["112", "110"])),
        ]))
        .unwrap();
        assert_eq!(s.operator_identifier, "46011");
        assert_eq!(s.emergency_numbers, ["112", "110"]);
    }
}
