use std::collections::BTreeSet;

use omemo::DeviceId;

use crate::xml;

pub fn to_xml(devices: impl IntoIterator<Item = DeviceId>) -> xml::DeviceList {
    xml::DeviceList {
        devices: devices.into_iter().map(|id| xml::Device { id }).collect(),
    }
}

/// Device ids in the list; a repeated id counts once.
pub fn from_xml(list: &xml::DeviceList) -> BTreeSet<DeviceId> {
    list.devices.iter().map(|device| device.id).collect()
}
