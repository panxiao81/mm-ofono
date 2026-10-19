//! Finds the physical modem behind oFono, for ModemManager's identity
//! properties (Device, Physdev, Drivers, Manufacturer, Model, ...).
//!
//! oFono only talks to the vendor RIL, so the hardware is found from the data
//! path instead: the vendor data daemon (netmgrd) stacks rmnet links on the
//! modem's network device, e.g. `rmnet_data0@rmnet_mhi0`.

use std::io;

use futures_util::TryStreamExt;
use rtnetlink::packet_route::link::{LinkAttribute, LinkInfo, LinkMessage};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhysicalDevice {
    /// sysfs path of the bus device (e.g. the PCIe function of an external modem).
    pub sysfs: String,
    /// Bus device driver first, then the network driver.
    pub drivers: Vec<String>,
    /// Network device carrying the data links.
    pub net_port: String,
    /// "vendor:device:subsystem_vendor:subsystem_device" when the bus has IDs.
    pub ids: Option<String>,
    pub hw_revision: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
}

/// The parts of an rtnetlink link we look at.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub index: u32,
    pub name: String,
    pub kind: Option<String>,
    /// IFLA_LINK: the device a stacked link sits on.
    pub lower: Option<u32>,
}

impl From<LinkMessage> for Link {
    fn from(msg: LinkMessage) -> Self {
        let mut link = Link { index: msg.header.index, name: String::new(), kind: None, lower: None };
        for attr in msg.attributes {
            match attr {
                LinkAttribute::IfName(name) => link.name = name,
                LinkAttribute::Link(lower) => link.lower = Some(lower),
                LinkAttribute::LinkInfo(infos) => {
                    for info in infos {
                        if let LinkInfo::Kind(kind) = info {
                            link.kind = Some(kind.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        link
    }
}

pub async fn links() -> io::Result<Vec<Link>> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::spawn(conn);
    handle
        .link()
        .get()
        .execute()
        .map_ok(Link::from)
        .try_collect()
        .await
        .map_err(io::Error::other)
}

/// The device the first rmnet link is stacked on.
pub fn data_port(links: &[Link]) -> Option<&Link> {
    let lower = links
        .iter()
        .find(|l| l.kind.as_deref() == Some("rmnet"))?
        .lower?;
    links.iter().find(|l| l.index == lower)
}

/// Hardware details of the device behind network interface `net_port`.
pub fn describe(net_port: &str) -> Option<PhysicalDevice> {
    let net = udev::Device::from_subsystem_sysname("net".into(), net_port.into()).ok()?;
    // e.g. the MHI channel device bound to mhi_netdev, or the IPA platform device
    let net_parent = net.parent()?;
    // The bus device is the nearest ancestor with vendor IDs (PCI); SoC-internal
    // modems have none, and then the network device's parent stands in.
    let bus = std::iter::successors(Some(net_parent.clone()), |d| d.parent())
        .find(|d| d.attribute_value("vendor").is_some())
        .unwrap_or_else(|| net_parent.clone());

    let text = |v: Option<&std::ffi::OsStr>| v.map(|s| s.to_string_lossy().trim().to_owned());
    let attr = |name| text(bus.attribute_value(name));

    let mut drivers: Vec<String> = [text(bus.driver()), text(net_parent.driver())]
        .into_iter()
        .flatten()
        .collect();
    drivers.dedup();

    let ids = ["vendor", "device", "subsystem_vendor", "subsystem_device"]
        .map(|a| attr(a).map(|v| v.trim_start_matches("0x").to_owned()));
    let ids = ids[0].is_some().then(|| ids.into_iter().flatten().collect::<Vec<_>>().join(":"));

    Some(PhysicalDevice {
        sysfs: bus.syspath().to_string_lossy().into_owned(),
        drivers,
        net_port: net_port.to_owned(),
        ids,
        hw_revision: attr("revision"),
        // udev's hwdb names, e.g. "Qualcomm Technologies, Inc" / "SDX55 [Snapdragon X55 5G]"
        manufacturer: text(bus.property_value("ID_VENDOR_FROM_DATABASE")),
        model: text(bus.property_value("ID_MODEL_FROM_DATABASE")),
    })
}

pub async fn discover() -> Option<PhysicalDevice> {
    let links = links().await.ok()?;
    describe(&data_port(&links)?.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(index: u32, name: &str, kind: Option<&str>, lower: Option<u32>) -> Link {
        Link { index, name: name.into(), kind: kind.map(Into::into), lower }
    }

    /// Links on a Sony Xperia 5 II: SDX55 over PCIe, data via mhi_netdev.
    #[test]
    fn data_port_is_below_rmnet_links() {
        let links = [
            link(1, "lo", None, None),
            link(12, "rmnet_ipa0", None, None), // unused IPA netdev
            link(15, "rmnet_mhi0", None, None),
            link(16, "rmnet_data0", Some("rmnet"), Some(15)),
            link(30, "veth0944ae59", Some("veth"), Some(12)), // container veth
        ];
        assert_eq!(data_port(&links).unwrap().name, "rmnet_mhi0");
    }

    #[test]
    fn no_data_port_without_rmnet_links() {
        let links = [link(1, "lo", None, None), link(30, "veth0", Some("veth"), Some(1))];
        assert_eq!(data_port(&links), None);
    }
}
