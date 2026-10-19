//! Mapping between ModemManager bearers and oFono connection contexts.
//! Pure functions; the D-Bus side is in mm_bearer.rs.

use std::collections::HashMap;

use zbus::{OwnedVariant, Variant};

use crate::mm::{MMBearerAllowedAuth as auth, MMBearerApnType as apn_type, MMBearerIpFamily as family, MMBearerIpMethod as method};
use crate::ofono::{self, Props};

pub const CONTEXT: &str = "org.ofono.ConnectionContext";

/// oFono's contexts of one modem, by path. Shared by the modem task (which
/// applies signals key by key) and bearers (which store a fresh
/// GetProperties after Connect), so neither can roll the other back with
/// older data; bearer status is always derived from here.
pub type Contexts = std::sync::Arc<std::sync::Mutex<HashMap<String, Props>>>;

type Dict = HashMap<String, OwnedVariant>;

fn v<'a>(x: impl Into<Variant<'a>>) -> OwnedVariant {
    OwnedVariant::try_from(x.into()).unwrap()
}

/// What a client asked for in CreateBearer / Simple.Connect.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Request {
    pub apn: String,
    pub ip_type: u32,
    pub apn_type: u32,
    pub user: String,
    pub password: String,
    pub allowed_auth: u32,
    pub allow_roaming: bool,
}

impl Request {
    pub fn from_dict(d: &Dict) -> Request {
        let s = |k: &str| match d.get(k).map(|v| &**v) {
            Some(Variant::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let u = |k: &str| d.get(k).and_then(|v| u32::try_from(v.try_clone().ok()?).ok());
        Request {
            apn: s("apn"),
            ip_type: u("ip-type").unwrap_or(family::MM_BEARER_IP_FAMILY_NONE),
            apn_type: u("apn-type").unwrap_or(apn_type::MM_BEARER_APN_TYPE_NONE),
            user: s("user"),
            password: s("password"),
            allowed_auth: u("allowed-auth").unwrap_or(auth::MM_BEARER_ALLOWED_AUTH_UNKNOWN),
            allow_roaming: d.get("allow-roaming").and_then(|v| bool::try_from(v.try_clone().ok()?).ok()).unwrap_or(true),
        }
    }

    /// Bearer.Properties: the settings as requested.
    pub fn to_dict(&self) -> Dict {
        let mut d = Dict::from([("apn".into(), v(self.apn.as_str())), ("allow-roaming".into(), v(self.allow_roaming))]);
        if self.ip_type != family::MM_BEARER_IP_FAMILY_NONE {
            d.insert("ip-type".into(), v(self.ip_type));
        }
        if self.apn_type != apn_type::MM_BEARER_APN_TYPE_NONE {
            d.insert("apn-type".into(), v(self.apn_type));
        }
        if !self.user.is_empty() {
            d.insert("user".into(), v(self.user.as_str()));
        }
        if self.allowed_auth != auth::MM_BEARER_ALLOWED_AUTH_UNKNOWN {
            d.insert("allowed-auth".into(), v(self.allowed_auth));
        }
        d
    }

    /// The oFono context type this bearer needs.
    pub fn context_type(&self) -> &'static str {
        if self.apn_type & apn_type::MM_BEARER_APN_TYPE_MMS != 0 { "mms" } else { "internet" }
    }

    /// oFono `Protocol`; no family requested means dual stack.
    pub fn protocol(&self) -> &'static str {
        match self.ip_type {
            family::MM_BEARER_IP_FAMILY_IPV4 => "ip",
            family::MM_BEARER_IP_FAMILY_IPV6 => "ipv6",
            _ => "dual",
        }
    }

    /// oFono `AuthenticationMethod`; several allowed methods means any.
    pub fn auth_method(&self) -> &'static str {
        match self.allowed_auth {
            auth::MM_BEARER_ALLOWED_AUTH_NONE => "none",
            auth::MM_BEARER_ALLOWED_AUTH_PAP => "pap",
            auth::MM_BEARER_ALLOWED_AUTH_CHAP => "chap",
            _ => "any",
        }
    }

    /// The oFono context properties to set before activating, if they differ.
    pub fn context_settings(&self, ctx: &Props) -> Vec<(&'static str, String)> {
        let mut want = vec![
            ("AccessPointName", self.apn.clone()),
            ("Protocol", self.protocol().to_owned()),
            ("AuthenticationMethod", self.auth_method().to_owned()),
        ];
        if !self.user.is_empty() {
            want.push(("Username", self.user.clone()));
            want.push(("Password", self.password.clone()));
        }
        want.retain(|(k, val)| ofono::str(ctx, k) != Some(val.as_str()));
        want
    }
}

/// Which existing context a new bearer uses: the one with this APN and
/// type, else an inactive one of the type (its APN will be set); `None`
/// means oFono must add one.
pub fn choose_context<'a>(req: &Request, contexts: &'a [(String, Props)]) -> Option<&'a str> {
    let of_type = || contexts.iter().filter(|(_, p)| ofono::str(p, "Type") == Some(req.context_type()));
    of_type()
        .find(|(_, p)| ofono::str(p, "AccessPointName") == Some(req.apn.as_str()))
        .or_else(|| of_type().find(|(_, p)| ofono::bool(p, "Active") != Some(true)))
        .map(|(path, _)| path.as_str())
}

/// The bearer's live state, from its context.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub connected: bool,
    pub interface: String,
    pub ip4: Dict,
    pub ip6: Dict,
}

pub fn clone_dict(d: &Dict) -> Dict {
    d.iter().map(|(k, val)| (k.clone(), val.try_clone().unwrap())).collect()
}

fn prefix_from_netmask(mask: &str) -> Option<u32> {
    mask.parse::<std::net::Ipv4Addr>().ok().map(|m| u32::from(m).count_ones())
}

/// ModemManager IP config from an oFono `Settings` / `IPv6.Settings` dict.
fn ip_config(settings: &Props, v6: bool) -> Dict {
    let Some(address) = ofono::str(settings, "Address") else { return Dict::new() };
    let dhcp = ofono::str(settings, "Method") == Some("dhcp");
    let mut d = Dict::from([
        ("method".into(), v(if dhcp { method::MM_BEARER_IP_METHOD_DHCP } else { method::MM_BEARER_IP_METHOD_STATIC })),
        ("address".into(), v(address)),
    ]);
    let prefix = if v6 {
        match ofono::get(settings, "PrefixLength") {
            Some(Variant::U8(p)) => Some(u32::from(*p)),
            _ => None,
        }
    } else {
        ofono::str(settings, "Netmask").and_then(prefix_from_netmask)
    };
    if let Some(p) = prefix {
        d.insert("prefix".into(), v(p));
    }
    if let Some(gw) = ofono::str(settings, "Gateway") {
        d.insert("gateway".into(), v(gw));
    }
    for (i, dns) in ofono::strings(settings, "DomainNameServers").iter().take(3).enumerate() {
        d.insert(format!("dns{}", i + 1), v(dns.as_str()));
    }
    d
}

fn dict(ctx: &Props, key: &str) -> Props {
    match ofono::get(ctx, key) {
        Some(Variant::Dict(d)) => d
            .iter()
            .filter_map(|(k, val)| match (k, val) {
                (Variant::Str(k), Variant::Variant(inner)) => Some((k.to_string(), OwnedVariant::try_from((**inner).try_clone().ok()?).ok()?)),
                (Variant::Str(k), other) => Some((k.to_string(), OwnedVariant::try_from(other.try_clone().ok()?).ok()?)),
                _ => None,
            })
            .collect(),
        _ => Props::new(),
    }
}

pub fn status(ctx: &Props) -> Status {
    let connected = ofono::bool(ctx, "Active") == Some(true);
    if !connected {
        return Status::default();
    }
    let (v4, v6) = (dict(ctx, "Settings"), dict(ctx, "IPv6.Settings"));
    let interface = ofono::str(&v4, "Interface").or_else(|| ofono::str(&v6, "Interface")).unwrap_or_default().to_owned();
    Status { connected, interface, ip4: ip_config(&v4, false), ip6: ip_config(&v6, true) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(kv: Vec<(&str, Variant<'static>)>) -> Props {
        kv.into_iter().map(|(k, val)| (k.to_owned(), OwnedVariant::try_from(val).unwrap())).collect()
    }

    fn ctx(ty: &str, apn: &str, active: bool) -> Props {
        props(vec![("Type", ty.to_owned().into()), ("AccessPointName", apn.to_owned().into()), ("Active", active.into())])
    }

    #[test]
    fn picks_context_by_apn_then_free_one() {
        let contexts = vec![
            ("/ril_0/context1".to_owned(), ctx("internet", "ctnet", true)),
            ("/ril_0/context2".to_owned(), ctx("mms", "ctwap", false)),
            ("/ril_0/context3".to_owned(), ctx("internet", "other", false)),
        ];
        let req = |apn: &str, t| Request { apn: apn.into(), apn_type: t, ..Default::default() };
        assert_eq!(choose_context(&req("ctnet", 0), &contexts), Some("/ril_0/context1"));
        assert_eq!(choose_context(&req("new", 0), &contexts), Some("/ril_0/context3"));
        assert_eq!(choose_context(&req("ctwap", apn_type::MM_BEARER_APN_TYPE_MMS), &contexts), Some("/ril_0/context2"));
        assert_eq!(choose_context(&req("x", 0), &contexts[..1]), None); // the only one is busy
    }

    #[test]
    fn context_settings_only_what_differs() {
        let r = Request { apn: "ctnet".into(), ip_type: family::MM_BEARER_IP_FAMILY_IPV4, ..Default::default() };
        let c = props(vec![("AccessPointName", "ctnet".into()), ("Protocol", "dual".into()), ("AuthenticationMethod", "any".into())]);
        assert_eq!(r.context_settings(&c), [("Protocol", "ip".to_owned())]);
    }

    #[test]
    fn dual_stack_status() {
        let v4: HashMap<String, Variant> = HashMap::from([
            ("Interface".into(), Variant::from("rmnet_data2")),
            ("Method".into(), "static".into()),
            ("Address".into(), "10.172.70.13".into()),
            ("Netmask".into(), "255.255.255.252".into()),
            ("Gateway".into(), "10.172.70.14".into()),
            ("DomainNameServers".into(), Variant::from(vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()])),
        ]);
        let v6: HashMap<String, Variant> = HashMap::from([
            ("Interface".into(), Variant::from("rmnet_data2")),
            ("Address".into(), "240a:b:48fa:638a::1".into()),
            ("PrefixLength".into(), 64u8.into()),
        ]);
        let c = props(vec![("Active", true.into()), ("Settings", Variant::from(v4)), ("IPv6.Settings", Variant::from(v6))]);
        let s = status(&c);
        assert!(s.connected);
        assert_eq!(s.interface, "rmnet_data2");
        assert_eq!(u32::try_from(s.ip4["prefix"].try_clone().unwrap()).unwrap(), 30);
        assert_eq!(String::try_from(s.ip4["dns2"].try_clone().unwrap()).unwrap(), "8.8.8.8");
        assert_eq!(u32::try_from(s.ip4["method"].try_clone().unwrap()).unwrap(), method::MM_BEARER_IP_METHOD_STATIC);
        assert_eq!(u32::try_from(s.ip6["prefix"].try_clone().unwrap()).unwrap(), 64);

        assert_eq!(status(&props(vec![("Active", false.into())])), Status::default());
    }
}
