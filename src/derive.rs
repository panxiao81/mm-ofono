//! ModemManager's view of one oFono modem, computed from the cached oFono
//! properties. Pure functions: no D-Bus here, so every rule is unit-testable.

use std::collections::HashMap;

use zbus::{OwnedVariant, Variant};

use crate::mm::{
    MMModemAccessTechnology as tech, MMModemCapability as cap, MMModemLock as lock,
    MMModemMode as mode, MMModemPowerState as power, MMModemState as state,
    MMModemStateFailedReason as failed,
};
use crate::device::PhysicalDevice;
use crate::ofono::{self, Iface, MODEM, Props};

pub const SIM_MANAGER: &str = "org.ofono.SimManager";
pub const NETREG: &str = "org.ofono.NetworkRegistration";
pub const RADIO: &str = "org.ofono.RadioSettings";
pub const VOICE: &str = "org.ofono.VoiceCallManager";
pub const CONNMAN: &str = "org.ofono.ConnectionManager";
/// Not a property interface: the cached reply of its
/// GetServingCellInformation, refreshed whenever Strength changes.
pub const NETMON: &str = "org.ofono.NetworkMonitor";

/// Cached properties of every interface of one oFono modem; an interface
/// is present iff oFono lists it in `org.ofono.Modem.Interfaces`.
#[derive(Debug, Default)]
pub struct OfonoModem {
    pub ifaces: HashMap<String, Iface>,
}

impl OfonoModem {
    pub fn props(&self, iface: &str) -> Option<&Props> {
        self.ifaces.get(iface).map(|i| &i.props)
    }

    fn get(&self, iface: &str, key: &str) -> Option<&Variant<'static>> {
        ofono::get(self.props(iface)?, key)
    }

    fn str(&self, iface: &str, key: &str) -> Option<&str> {
        ofono::str(self.props(iface)?, key)
    }

    fn bool(&self, iface: &str, key: &str) -> Option<bool> {
        ofono::bool(self.props(iface)?, key)
    }

    pub fn strings(&self, iface: &str, key: &str) -> Vec<String> {
        self.props(iface).map(|p| ofono::strings(p, key)).unwrap_or_default()
    }

    pub fn has(&self, iface: &str) -> bool {
        self.ifaces.contains_key(iface)
    }

    /// Enough is known to show this modem: power state, whether a SIM is
    /// inserted, and the initial properties of every interface oFono lists.
    /// Exporting earlier made clients see a SIM-less modem at boot, or a
    /// state that changes a moment later.
    pub fn ready(&self) -> bool {
        self.bool(MODEM, "Powered").is_some()
            && self.bool(SIM_MANAGER, "Present").is_some()
            && self.ifaces.values().all(|i| i.fetched)
    }

    /// What [`OfonoModem::ready`] still waits for, for the log.
    pub fn not_ready(&self) -> String {
        let mut missing: Vec<String> = self.ifaces.iter().filter(|(_, i)| !i.fetched).map(|(n, _)| n.clone()).collect();
        if self.bool(MODEM, "Powered").is_none() {
            missing.push("Modem.Powered".into());
        }
        if self.bool(SIM_MANAGER, "Present").is_none() {
            missing.push("SimManager.Present".into());
        }
        missing.join(", ")
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

/// MM_RSSI_TO_QUALITY: -113..-51 dBm scaled to 0..100, with C's truncation.
fn rssi_quality(dbm: i32) -> u32 {
    (100 - (dbm.clamp(-113, -51) + 51) * 100 / (-113 + 51)) as u32
}

pub fn status(m: &OfonoModem) -> ModemStatus {
    status_on(m, &[])
}

/// Technologies assumed when no slot has shown its RadioSettings yet.
// ponytail: a GSM/LTE phone, so NetworkManager treats it as 3GPP from the start.
const FALLBACK_TECHNOLOGIES: [&str; 3] = ["gsm", "umts", "lte"];

/// [`status`], where `hardware` is what another SIM slot of the same oFono
/// reports. RadioSettings only exists while a SIM is in, but capabilities
/// and modes describe the modem itself (like ModemManager, which reads them
/// from the hardware), so an empty slot borrows them.
pub fn status_on(m: &OfonoModem, hardware: &[String]) -> ModemStatus {
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

    let strength = match (m.get(NETMON, "Strength"), m.get(NETREG, "Strength")) {
        // The serving cell's RSSI in ASU, rated like MM's QMI plugin does.
        (Some(Variant::U8(asu @ 0..=31)), _) => rssi_quality(-113 + 2 * i32::from(*asu)),
        // oFono's own rating is the worst of RSSI/RSRP/RSRQ/SNR, well below MM's.
        (_, Some(Variant::U8(s))) => u32::from(*s),
        _ => 0,
    };

    let mut available = m.strings(RADIO, "AvailableTechnologies");
    if available.is_empty() {
        available = if hardware.is_empty() { FALLBACK_TECHNOLOGIES.map(String::from).to_vec() } else { hardware.to_vec() };
    }
    let supported = supported_modes(&available);
    let capabilities = RATS.iter().filter(|r| available.contains(&r.0.to_owned())).fold(0, |c, r| c | r.2);
    // Without RadioSettings nothing restricts the modes: the widest.
    let current = current_modes(&supported, Some(m.str(RADIO, "TechnologyPreference").unwrap_or("any")));

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
            match access_technology(m.str(NETREG, "Technology").unwrap_or("")) {
                // ofono-binder reports an LTE registration with EN-DC as "nr"
                // too. oFono's serving cell is then still the LTE anchor (it
                // never lists NR cells); on SA there is none. MM says lte|5gnr
                // for NSA.
                tech::MM_MODEM_ACCESS_TECHNOLOGY_5GNR if m.str(NETMON, "Technology") == Some("lte") => {
                    tech::MM_MODEM_ACCESS_TECHNOLOGY_LTE | tech::MM_MODEM_ACCESS_TECHNOLOGY_5GNR
                }
                t => t,
            }
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

/// `org.freedesktop.ModemManager1.Modem.Modem3gpp` properties.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status3gpp {
    pub imei: String,
    pub registration_state: u32,
    pub operator_code: String,
    pub operator_name: String,
    pub enabled_facility_locks: u32,
    pub packet_service_state: u32,
    /// Filled in by the modem task, from the shared contexts.
    pub initial_eps: Option<crate::bearer::AttachSettings>,
}

pub fn status_3gpp(m: &OfonoModem) -> Status3gpp {
    use crate::mm::{
        MMModem3gppFacility as fac, MMModem3gppPacketServiceState as ps, MMModem3gppRegistrationState as reg,
    };

    let registration_state = match m.str(NETREG, "Status") {
        Some("unregistered") => reg::MM_MODEM_3GPP_REGISTRATION_STATE_IDLE,
        Some("registered") => reg::MM_MODEM_3GPP_REGISTRATION_STATE_HOME,
        Some("searching") => reg::MM_MODEM_3GPP_REGISTRATION_STATE_SEARCHING,
        Some("denied") => reg::MM_MODEM_3GPP_REGISTRATION_STATE_DENIED,
        Some("roaming") => reg::MM_MODEM_3GPP_REGISTRATION_STATE_ROAMING,
        _ => reg::MM_MODEM_3GPP_REGISTRATION_STATE_UNKNOWN,
    };
    let registered = matches!(m.str(NETREG, "Status"), Some("registered" | "roaming"));

    let mut locks = m
        .strings(SIM_MANAGER, "LockedPins")
        .iter()
        .filter_map(|pin| match pin.as_str() {
            "pin" => Some(fac::MM_MODEM_3GPP_FACILITY_SIM),
            "phone" => Some(fac::MM_MODEM_3GPP_FACILITY_PH_SIM),
            "firstphone" => Some(fac::MM_MODEM_3GPP_FACILITY_PH_FSIM),
            "network" => Some(fac::MM_MODEM_3GPP_FACILITY_NET_PERS),
            "netsub" => Some(fac::MM_MODEM_3GPP_FACILITY_NET_SUB_PERS),
            "service" => Some(fac::MM_MODEM_3GPP_FACILITY_PROVIDER_PERS),
            "corp" => Some(fac::MM_MODEM_3GPP_FACILITY_CORP_PERS),
            _ => None,
        })
        .fold(fac::MM_MODEM_3GPP_FACILITY_NONE, |a, b| a | b);
    if m.bool(SIM_MANAGER, "FixedDialing") == Some(true) {
        locks |= fac::MM_MODEM_3GPP_FACILITY_FIXED_DIALING;
    }

    Status3gpp {
        imei: m.str(MODEM, "Serial").unwrap_or_default().to_owned(),
        registration_state,
        operator_code: if registered { operator_code(m.props(NETREG)) } else { String::new() },
        operator_name: if registered { m.str(NETREG, "Name").unwrap_or_default().to_owned() } else { String::new() },
        enabled_facility_locks: locks,
        packet_service_state: match m.bool(CONNMAN, "Attached") {
            Some(true) => ps::MM_MODEM_3GPP_PACKET_SERVICE_STATE_ATTACHED,
            Some(false) => ps::MM_MODEM_3GPP_PACKET_SERVICE_STATE_DETACHED,
            None => ps::MM_MODEM_3GPP_PACKET_SERVICE_STATE_UNKNOWN,
        },
        initial_eps: None,
    }
}

/// "MCCMNC" from an oFono NetworkRegistration or NetworkOperator.
pub fn operator_code(props: Option<&Props>) -> String {
    let Some(p) = props else { return String::new() };
    format!(
        "{}{}",
        ofono::str(p, "MobileCountryCode").unwrap_or_default(),
        ofono::str(p, "MobileNetworkCode").unwrap_or_default()
    )
}

/// One entry of Modem3gpp.Scan from an oFono NetworkOperator.
pub fn scan_result(op: &Props) -> HashMap<String, OwnedVariant> {
    use crate::mm::MMModem3gppNetworkAvailability as avail;

    let status = match ofono::str(op, "Status") {
        Some("available") => avail::MM_MODEM_3GPP_NETWORK_AVAILABILITY_AVAILABLE,
        Some("current") => avail::MM_MODEM_3GPP_NETWORK_AVAILABILITY_CURRENT,
        Some("forbidden") => avail::MM_MODEM_3GPP_NETWORK_AVAILABILITY_FORBIDDEN,
        _ => avail::MM_MODEM_3GPP_NETWORK_AVAILABILITY_UNKNOWN,
    };
    // The best technology the operator offers.
    let tech = ofono::strings(op, "Technologies")
        .iter()
        .map(|t| access_technology(t))
        .max()
        .unwrap_or(tech::MM_MODEM_ACCESS_TECHNOLOGY_UNKNOWN);
    let name = ofono::str(op, "Name").unwrap_or_default();

    let v = |x: Variant<'_>| OwnedVariant::try_from(x).unwrap();
    HashMap::from([
        ("status".into(), v(status.into())),
        ("operator-long".into(), v(name.into())),
        ("operator-short".into(), v(name.into())),
        ("operator-code".into(), v(operator_code(Some(op)).as_str().into())),
        ("access-technology".into(), v(tech.into())),
    ])
}

/// MM USSD session state from SupplementaryServices.State.
pub fn ussd_state(m: &OfonoModem) -> u32 {
    use crate::mm::MMModem3gppUssdSessionState as ussd;
    match m.str(crate::mm_ussd::SUPPLEMENTARY, "State") {
        Some("idle") => ussd::MM_MODEM_3GPP_USSD_SESSION_STATE_IDLE,
        Some("active") => ussd::MM_MODEM_3GPP_USSD_SESSION_STATE_ACTIVE,
        Some("user-response") => ussd::MM_MODEM_3GPP_USSD_SESSION_STATE_USER_RESPONSE,
        _ => ussd::MM_MODEM_3GPP_USSD_SESSION_STATE_UNKNOWN,
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

/// The SIM as MM shows it, once its ICCID is known: like MM, which exports a
/// SIM only after loading its identifiers. NetworkManager reads the SIM once,
/// when Modem.Sim changes, and matches profiles' gsm.sim-id against it; a SIM
/// shown before oFono read the ICCID (as after an oFono restart) left every
/// profile usable on every modem.
pub fn sim(m: &OfonoModem) -> Option<SimStatus> {
    if !m.sim_present() || m.str(SIM_MANAGER, "CardIdentifier").unwrap_or_default().is_empty() {
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
            let iface = m.ifaces.entry(iface.to_string()).or_default();
            iface.fetched = true;
            iface.props.insert(key.to_string(), OwnedVariant::try_from(value.clone()).unwrap());
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
        let mut m = modem(&[(MODEM, "Powered", true.into()), (SIM_MANAGER, "Present", false.into())]);
        assert!(m.ready());
        // NetworkRegistration just appeared and its GetProperties is in flight.
        m.ifaces.insert(NETREG.into(), Default::default());
        assert!(!m.ready());
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
    fn quality_from_serving_cell_rssi() {
        let base = [
            (MODEM, "Powered", true.into()),
            (MODEM, "Online", true.into()),
            (SIM_MANAGER, "Present", true.into()),
            (SIM_MANAGER, "PinRequired", "none".into()),
            (NETREG, "Status", "registered".into()),
            (NETREG, "Strength", 35u8.into()),
        ];
        let rated = |asu: u8| {
            let mut props = base.to_vec();
            props.push((NETMON, "Strength", asu.into()));
            status(&modem(&props)).signal_quality
        };
        assert_eq!(rated(30), (97, true), "-53 dBm, as MM rates it");
        assert_eq!(rated(31), (100, true));
        assert_eq!(rated(0), (0, true));
        assert_eq!(rated(99), (35, true), "unknown: oFono's rating");
        assert_eq!(status(&modem(&base)).signal_quality, (35, true), "no NetworkMonitor");
    }

    /// An empty slot has no RadioSettings; gnome-control-center asserts on
    /// allowed modes of 0.
    #[test]
    fn empty_slot_borrows_the_modems_technologies() {
        let empty = modem(&[(MODEM, "Powered", true.into()), (SIM_MANAGER, "Present", false.into())]);
        let (g2, g3, g4, g5) =
            (mode::MM_MODEM_MODE_2G, mode::MM_MODEM_MODE_3G, mode::MM_MODEM_MODE_4G, mode::MM_MODEM_MODE_5G);

        let s = status_on(&empty, &["gsm", "umts", "lte", "nr"].map(String::from));
        assert_eq!(s.state, state::MM_MODEM_STATE_FAILED);
        assert_eq!(
            s.capabilities,
            cap::MM_MODEM_CAPABILITY_GSM_UMTS | cap::MM_MODEM_CAPABILITY_LTE | cap::MM_MODEM_CAPABILITY_5GNR
        );
        assert_eq!(s.supported_modes.len(), 4);
        assert_eq!(s.current_modes, (g2 | g3 | g4 | g5, g5));

        let s = status(&empty); // no slot knows yet
        assert_eq!(s.capabilities, cap::MM_MODEM_CAPABILITY_GSM_UMTS | cap::MM_MODEM_CAPABILITY_LTE);
        assert_eq!(s.current_modes, (g2 | g3 | g4, g4));
    }

    #[test]
    fn nr_is_nsa_while_the_serving_cell_is_lte() {
        let base = [
            (MODEM, "Powered", true.into()),
            (MODEM, "Online", true.into()),
            (SIM_MANAGER, "Present", true.into()),
            (SIM_MANAGER, "PinRequired", "none".into()),
            (NETREG, "Status", "registered".into()),
            (NETREG, "Technology", "nr".into()),
        ];
        let (lte, nr) = (tech::MM_MODEM_ACCESS_TECHNOLOGY_LTE, tech::MM_MODEM_ACCESS_TECHNOLOGY_5GNR);
        assert_eq!(status(&modem(&base)).access_technologies, nr, "SA: no LTE serving cell");
        let mut nsa = base.to_vec();
        nsa.push((NETMON, "Technology", "lte".into()));
        assert_eq!(status(&modem(&nsa)).access_technologies, lte | nr);
    }

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
    fn three_gpp_roaming_with_pin_lock() {
        use crate::mm::{MMModem3gppFacility as fac, MMModem3gppRegistrationState as reg};
        let s = status_3gpp(&modem(&[
            (MODEM, "Serial", "350000000000017".into()),
            (NETREG, "Status", "roaming".into()),
            (NETREG, "Name", "中国电信 (KDDI)".into()),
            (NETREG, "MobileCountryCode", "440".into()),
            (NETREG, "MobileNetworkCode", "51".into()),
            (SIM_MANAGER, "LockedPins", strs(&["pin"])),
            (SIM_MANAGER, "FixedDialing", true.into()),
            (CONNMAN, "Attached", true.into()),
        ]));
        assert_eq!(s.registration_state, reg::MM_MODEM_3GPP_REGISTRATION_STATE_ROAMING);
        assert_eq!(s.operator_code, "44051");
        assert_eq!(s.operator_name, "中国电信 (KDDI)");
        assert_eq!(s.enabled_facility_locks, fac::MM_MODEM_3GPP_FACILITY_SIM | fac::MM_MODEM_3GPP_FACILITY_FIXED_DIALING);
        assert_eq!(s.packet_service_state, crate::mm::MMModem3gppPacketServiceState::MM_MODEM_3GPP_PACKET_SERVICE_STATE_ATTACHED);

        // Not registered: no operator, like ModemManager.
        let s = status_3gpp(&modem(&[(NETREG, "Status", "searching".into()), (NETREG, "Name", "x".into())]));
        assert_eq!((s.operator_code.as_str(), s.operator_name.as_str()), ("", ""));
    }

    #[test]
    fn scan_result_from_ofono_operator() {
        use crate::mm::MMModem3gppNetworkAvailability as avail;
        let mut op = Props::new();
        for (k, v) in [
            ("Name", Variant::from("NTT DOCOMO")),
            ("Status", "forbidden".into()),
            ("MobileCountryCode", "440".into()),
            ("MobileNetworkCode", "10".into()),
            ("Technologies", strs(&["umts", "lte"])),
        ] {
            op.insert(k.into(), v.try_into().unwrap());
        }
        let r = scan_result(&op);
        assert_eq!(u32::try_from(r["status"].try_clone().unwrap()).unwrap(), avail::MM_MODEM_3GPP_NETWORK_AVAILABILITY_FORBIDDEN);
        assert_eq!(String::try_from(r["operator-code"].try_clone().unwrap()).unwrap(), "44010");
        assert_eq!(u32::try_from(r["access-technology"].try_clone().unwrap()).unwrap(), tech::MM_MODEM_ACCESS_TECHNOLOGY_LTE);
    }

    #[test]
    fn sim_only_when_present_and_identified() {
        assert_eq!(sim(&modem(&[(SIM_MANAGER, "Present", false.into())])), None);
        assert_eq!(sim(&modem(&[(SIM_MANAGER, "Present", true.into())])), None, "ICCID not read yet");
        let s = sim(&modem(&[
            (SIM_MANAGER, "Present", true.into()),
            (SIM_MANAGER, "CardIdentifier", "89860312345678901234".into()),
            (SIM_MANAGER, "MobileCountryCode", "460".into()),
            (SIM_MANAGER, "MobileNetworkCode", "11".into()),
            (VOICE, "EmergencyNumbers", strs(&["112", "110"])),
        ]))
        .unwrap();
        assert_eq!(s.operator_identifier, "46011");
        assert_eq!(s.emergency_numbers, ["112", "110"]);
    }
}
