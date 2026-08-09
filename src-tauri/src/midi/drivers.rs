//! The controller-driver registry (ADR-0031, carrying issue #30's contract).
//!
//! One entry per supported controller is the whole device-specific surface: a
//! port-name fragment to bind on and the position-sync SysEx sent on a fresh
//! bind (knobs are silent until moved — the query makes the device dump every
//! analog position as ordinary CC traffic). Both Pioneers share the 2-deck
//! byte scheme, so the translator ([`super::translate`]) and LED scheme
//! ([`super::leds`]) are common; a genuinely different controller would grow
//! this struct, not fork the module. Matched in declaration order — first
//! match wins, like the TS registry it replaces.

/// A supported controller.
pub struct Driver {
    /// Stable identifier, e.g. `"flx4"` (the picker shows the raw port name).
    pub id: &'static str,
    /// Matched against the CoreMIDI port name to bind the device.
    pub name_fragment: &'static str,
    /// Position-sync / keep-alive SysEx, sent on every fresh bind.
    pub init_sysex: &'static [u8],
}

/// The FLX4 position query — from the Mixxx FLX4 script (Wireshark-derived);
/// doubles as its keep-alive (docs/midi-ddj-flx4.md).
const FLX4_STATUS_QUERY: [u8; 12] = [
    0xf0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x04, 0x05, 0x00, 0x50, 0x02, 0xf7,
];

/// The DDJ-400 position query — verbatim from the Mixxx DDJ-400 `init`
/// (docs/midi-ddj-400.md); its own bytes, the same role.
const DDJ400_STATUS_QUERY: [u8; 12] = [
    0xf0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x02, 0x06, 0x00, 0x03, 0x01, 0xf7,
];

/// The supported controllers, matched in order (first match wins).
pub const DRIVERS: [Driver; 2] = [
    Driver {
        id: "flx4",
        name_fragment: "DDJ-FLX4",
        init_sysex: &FLX4_STATUS_QUERY,
    },
    Driver {
        id: "ddj400",
        name_fragment: "DDJ-400",
        init_sysex: &DDJ400_STATUS_QUERY,
    },
];

/// Normalize the display punctuation ALSA/CoreMIDI/WinMM may add around a USB
/// product name while retaining only identity-bearing alphanumerics. The raw
/// name is still used to open and persist the exact port.
fn identity(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

/// The first registry driver whose normalized fragment occurs in the normalized
/// port name, or `None` for a non-controller port. ALSA commonly reports names
/// such as `DDJ-FLX4:DDJ-FLX4 MIDI 1 24:0`; punctuation/case must not make that
/// class-compliant controller disappear.
pub fn driver_for_name(name: &str) -> Option<&'static Driver> {
    let name = identity(name);
    DRIVERS
        .iter()
        .find(|driver| name.contains(&identity(driver.name_fragment)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ports_by_fragment_in_registry_order() {
        assert_eq!(driver_for_name("DDJ-FLX4").map(|d| d.id), Some("flx4"));
        assert_eq!(driver_for_name("Pioneer DDJ-FLX4 MIDI 1").map(|d| d.id), Some("flx4"));
        assert_eq!(
            driver_for_name("ddj-flx4:ddj-flx4 midi 1 24:0").map(|d| d.id),
            Some("flx4")
        );
        assert_eq!(
            driver_for_name("AlphaTheta  DDJ FLX4  MIDI 1").map(|d| d.id),
            Some("flx4")
        );
        assert_eq!(driver_for_name("DDJ-400").map(|d| d.id), Some("ddj400"));
        assert_eq!(driver_for_name("IAC Driver Bus 1").map(|d| d.id), None);
        assert_eq!(driver_for_name("KeyLab 61").map(|d| d.id), None);
    }
}
