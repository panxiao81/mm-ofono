//! ModemManager call state from oFono VoiceCall properties. Pure functions.

use std::collections::HashMap;

use zbus::OwnedObjectPath;

use crate::mm::{MMCallDirection as dir, MMCallState as st, MMCallStateReason as why};
use crate::ofono::{self, Props};

pub const VOICE_CALL: &str = "org.ofono.VoiceCall";

#[derive(Debug, Clone, PartialEq)]
pub struct CallData {
    pub state: i32,
    pub reason: i32,
    pub direction: i32,
    pub number: String,
    pub multiparty: bool,
}

impl Default for CallData {
    fn default() -> CallData {
        CallData {
            state: st::MM_CALL_STATE_UNKNOWN as i32,
            reason: why::MM_CALL_STATE_REASON_UNKNOWN as i32,
            direction: dir::MM_CALL_DIRECTION_UNKNOWN as i32,
            number: String::new(),
            multiparty: false,
        }
    }
}

pub fn state(ofono: &str) -> i32 {
    (match ofono {
        "dialing" => st::MM_CALL_STATE_DIALING,
        "alerting" => st::MM_CALL_STATE_RINGING_OUT,
        "incoming" => st::MM_CALL_STATE_RINGING_IN,
        "waiting" => st::MM_CALL_STATE_WAITING,
        "active" => st::MM_CALL_STATE_ACTIVE,
        "held" => st::MM_CALL_STATE_HELD,
        "disconnected" => st::MM_CALL_STATE_TERMINATED,
        _ => st::MM_CALL_STATE_UNKNOWN,
    }) as i32
}

/// The call as oFono reports it, given what we knew before (`prev`): the
/// direction is fixed by the first state seen, the reason by the transition.
pub fn call_data(props: &Props, prev: Option<&CallData>) -> CallData {
    let s = state(ofono::str(props, "State").unwrap_or_default());
    let unknown = CallData::default();
    let prev = prev.unwrap_or(&unknown);

    let direction = if prev.direction != dir::MM_CALL_DIRECTION_UNKNOWN as i32 {
        prev.direction
    } else if s == st::MM_CALL_STATE_RINGING_IN as i32 || s == st::MM_CALL_STATE_WAITING as i32 {
        dir::MM_CALL_DIRECTION_INCOMING as i32
    } else if s == st::MM_CALL_STATE_DIALING as i32 || s == st::MM_CALL_STATE_RINGING_OUT as i32 {
        dir::MM_CALL_DIRECTION_OUTGOING as i32
    } else {
        dir::MM_CALL_DIRECTION_UNKNOWN as i32
    };

    let reason = if s == prev.state {
        prev.reason
    } else {
        (match s as u32 {
            st::MM_CALL_STATE_RINGING_IN | st::MM_CALL_STATE_WAITING => why::MM_CALL_STATE_REASON_INCOMING_NEW,
            st::MM_CALL_STATE_DIALING | st::MM_CALL_STATE_RINGING_OUT => why::MM_CALL_STATE_REASON_OUTGOING_STARTED,
            st::MM_CALL_STATE_ACTIVE if prev.state != st::MM_CALL_STATE_HELD as i32 => why::MM_CALL_STATE_REASON_ACCEPTED,
            st::MM_CALL_STATE_TERMINATED => why::MM_CALL_STATE_REASON_TERMINATED,
            // Holding, resuming, ...: no reason ModemManager has a name for.
            _ => why::MM_CALL_STATE_REASON_UNKNOWN,
        }) as i32
    };

    let number = ofono::str(props, "LineIdentification").filter(|n| !n.is_empty()).map(str::to_owned);
    CallData {
        state: s,
        reason,
        direction,
        number: number.unwrap_or_else(|| prev.number.clone()),
        multiparty: ofono::bool(props, "Multiparty").unwrap_or(false),
    }
}

/// How a call that oFono dropped ended, from its DisconnectReason.
pub fn ended(prev: &CallData, disconnect_reason: Option<&str>) -> CallData {
    let reason = match disconnect_reason {
        Some("network") => why::MM_CALL_STATE_REASON_ERROR,
        _ => why::MM_CALL_STATE_REASON_TERMINATED,
    };
    CallData { state: st::MM_CALL_STATE_TERMINATED as i32, reason: reason as i32, ..prev.clone() }
}

/// Which ModemManager call stands for which oFono call. Shared by the modem
/// task and Call.Start: oFono may announce a dialled call before Dial
/// returns, so Start registers the number first and the task adopts the
/// call instead of creating a second one.
#[derive(Debug, Default)]
pub struct Registry {
    pub by_ofono: HashMap<String, OwnedObjectPath>,
    pub dialling: Vec<(String, OwnedObjectPath)>,
}

pub type Calls = std::sync::Arc<std::sync::Mutex<Registry>>;

impl Registry {
    /// The ModemManager call for oFono call `ofono` with `number`: already
    /// mapped, or adopted from a pending Start.
    pub fn resolve(&mut self, ofono: &str, number: &str, outgoing: bool) -> Option<OwnedObjectPath> {
        if let Some(mm) = self.by_ofono.get(ofono) {
            return Some(mm.clone());
        }
        if !outgoing {
            return None;
        }
        // oFono may reformat the number (e.g. add the country code); with a
        // single call being dialled, that's the one.
        let i = self
            .dialling
            .iter()
            .position(|(n, _)| n == number)
            .or_else(|| (self.dialling.len() == 1).then_some(0))?;
        let (_, mm) = self.dialling.remove(i);
        self.by_ofono.insert(ofono.to_owned(), mm.clone());
        Some(mm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::{OwnedVariant, Variant};

    fn props(state: &str, number: &str) -> Props {
        Props::from([
            ("State".into(), OwnedVariant::try_from(Variant::from(state)).unwrap()),
            ("LineIdentification".into(), OwnedVariant::try_from(Variant::from(number)).unwrap()),
        ])
    }

    #[test]
    fn outgoing_call_lifecycle() {
        let d = call_data(&props("dialing", "+81900000000"), None);
        assert_eq!(d.direction, dir::MM_CALL_DIRECTION_OUTGOING as i32);
        assert_eq!(d.reason, why::MM_CALL_STATE_REASON_OUTGOING_STARTED as i32);
        let d = call_data(&props("alerting", ""), Some(&d));
        assert_eq!(d.state, st::MM_CALL_STATE_RINGING_OUT as i32);
        assert_eq!(d.number, "+81900000000", "keeps the number when oFono sends none");
        let d = call_data(&props("active", ""), Some(&d));
        assert_eq!((d.state, d.reason), (st::MM_CALL_STATE_ACTIVE as i32, why::MM_CALL_STATE_REASON_ACCEPTED as i32));
        let held = call_data(&props("held", ""), Some(&d));
        let resumed = call_data(&props("active", ""), Some(&held));
        assert_ne!(resumed.reason, why::MM_CALL_STATE_REASON_ACCEPTED as i32, "resuming isn't accepting");
        assert_eq!(ended(&d, Some("network")).reason, why::MM_CALL_STATE_REASON_ERROR as i32);
    }

    #[test]
    fn incoming_call() {
        let d = call_data(&props("incoming", "+81900000001"), None);
        assert_eq!(d.direction, dir::MM_CALL_DIRECTION_INCOMING as i32);
        assert_eq!((d.state, d.reason), (st::MM_CALL_STATE_RINGING_IN as i32, why::MM_CALL_STATE_REASON_INCOMING_NEW as i32));
    }

    #[test]
    fn registry_adopts_the_dialled_call() {
        let mut r = Registry::default();
        let mm = OwnedObjectPath::try_from("/org/freedesktop/ModemManager1/Call/0").unwrap();
        r.dialling.push(("0901234567".into(), mm.clone()));
        // oFono reports the number in international format.
        assert_eq!(r.resolve("/ril_0/voicecall01", "+81901234567", true), Some(mm.clone()));
        assert_eq!(r.resolve("/ril_0/voicecall01", "+81901234567", true), Some(mm), "now mapped");
        assert_eq!(r.resolve("/ril_0/voicecall02", "+81900000002", false), None, "incoming: a new call");
    }
}
