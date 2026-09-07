//! Constants used only when provisioning a blank sensor.
//!
//! Vendor-signed data lifted from the Windows driver: a certificate
//! template and the signatures over the two known flash layouts. They
//! cannot be synthesised, only replayed.

/// Certificate template stored in pairing-record block 5.
pub const CRT_HARDCODED: &str = "170000000001000001000000fcffffffffffffffffffffff00000000000000000000000001000000ffffffff0000000000000000000000000000000000000000000000000000000000000000000000004b60d2273e3cce3bf6b053ccb0061d65bc86987655bdebb3e7933aaad835c65a00000000000000000000000000000000000000000000000000000000000000000000000096c298d84539a1f4a033eb2d817d0377f240a463e5e6bcf847422ce1f2d1176b000000000000000000000000000000000000000000000000000000000000000000000000f551bf376840b6cbce5e316b5733ce2b169e0f7c4aebe78e9b7f1afee242e34f000000000000000000000000000000000000000000000000000000000000000000000000512563fcc2cab9f3849e17a7adfae6bcffffffffffffffff00000000ffffffff000000000000000000000000000000000000000000000000000000000000000000000000ffffffffffffffffffffffff00000000000000000000000001000000ffffffff000000000000000000000000000000000000000000000000000000000000000000000000";

/// Signature over the standard partition layout.
pub const PARTITION_SIGNATURE: &str = "1db02a886b007e2b47263bb8fe30bd64a1f58bea7b25f1e1ba9ae09add7ecff36333f8198339cdd713f043633710a17bc7b3f418f1d8ff435a1bf47f065dffca727109152217fce73bf2bf8e01a1641f6a24b0c492a6a3f10114057275846842b1c8b66bd6700738524d4471bca3315ba23bb832743220ad195b60558aa79a3edeb2604834e2bb62e890b0ce405b3b8ef2fec2aab3e22bff23f89a58ff0dc015fece5d3ed3f5496ace879a92980aec9d85eb7e9df245eae03a41acfd4e7d1cb1dbd0df42d534904de00b6389f68867646e9d7c3d0b1dffd74070b2d0f2049b9f1dc7b0c9651c59be3ea891674725e1f2f7a484a941615b80211105978369cf71";

/// Signature over the layout used by 138a:0090, whose database is smaller.
pub const PARTITION_SIGNATURE_0090: &str = "e44f7a80d6137794d330b5d026c328a73c907f3f653d411255b7c2f8b425d870a8a53c6630ca864b84590e3c6786f0d69be4bbab5736388f8527237a0a86bbce7ced9450c4964709e89ac535aa00787158e0a8d9b1fb75f0f7ae53d4bd11abfcf5ee67a5a71e248a426b3aff4567048fa93de65939ccfbe3f31149a82c64fbfd6a2a6cf748e1d9bd8562cf39b1a4b307b37be223317b1b817e364f2877d29d123731314aa627cbf234e0ea69a406a4735a03a45495023ef706bdb542c949d243ac2c08c00abf43faa5528a0a8e49b02c507b01b6f1c9abffc669d8c84d7e4a714da32aade7928eca9698b82bee6b72c642c9add80bbd7ccc4121b80220d52b8a";

/// A partition table entry: id, type, access level, offset, size.
pub struct PartitionSpec {
    pub id: u8,
    pub kind: u8,
    pub access_lvl: u16,
    pub offset: u32,
    pub size: u32,
}

/// Standard layout: cert store, firmware, calibration, template database.
pub static FLASH_LAYOUT: &[PartitionSpec] = &[
    PartitionSpec { id: 0x01, kind: 0x04, access_lvl: 0x0007, offset: 0x00001000, size: 0x00001000 },
    PartitionSpec { id: 0x02, kind: 0x01, access_lvl: 0x0002, offset: 0x00002000, size: 0x0003e000 },
    PartitionSpec { id: 0x05, kind: 0x05, access_lvl: 0x0003, offset: 0x00040000, size: 0x00008000 },
    PartitionSpec { id: 0x06, kind: 0x06, access_lvl: 0x0003, offset: 0x00048000, size: 0x00008000 },
    PartitionSpec { id: 0x04, kind: 0x03, access_lvl: 0x0005, offset: 0x00050000, size: 0x00080000 },
];

/// Layout for 138a:0090, which allots less space to the database.
pub static FLASH_LAYOUT_0090: &[PartitionSpec] = &[
    PartitionSpec { id: 0x01, kind: 0x04, access_lvl: 0x0007, offset: 0x00001000, size: 0x00001000 },
    PartitionSpec { id: 0x02, kind: 0x01, access_lvl: 0x0002, offset: 0x00002000, size: 0x0003e000 },
    PartitionSpec { id: 0x05, kind: 0x05, access_lvl: 0x0003, offset: 0x00040000, size: 0x00008000 },
    PartitionSpec { id: 0x06, kind: 0x06, access_lvl: 0x0003, offset: 0x00048000, size: 0x00008000 },
    PartitionSpec { id: 0x04, kind: 0x03, access_lvl: 0x0005, offset: 0x00050000, size: 0x00030000 },
];
