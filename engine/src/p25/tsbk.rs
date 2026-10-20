//! Outbound TSBK parsing (TIA-102.AABC-E). Field positions are the bit indexes
//! of the 96-bit block, bit 0 = MSB of byte 0, cross-checked against SDRTrunk's
//! `phase1/message/tsbk/standard/osp/*` and OP25's `tk_p25.py` decode_tsbk.
use serde::Serialize;

pub fn field(bytes: &[u8], start: usize, len: usize) -> u64 {
    (start..start + len).fold(0, |acc, bit| {
        (acc << 1) | ((bytes[bit / 8] >> (7 - bit % 8)) & 1) as u64
    })
}

/// A 16-bit channel field: 4-bit identifier (IDEN table index) and 12-bit channel number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Hash)]
pub struct Channel {
    pub iden: u8,
    pub number: u16,
}
impl Channel {
    pub fn from_bits(bytes: &[u8], start: usize) -> Self {
        Self {
            iden: field(bytes, start, 4) as u8,
            number: field(bytes, start + 4, 12) as u16,
        }
    }
    pub fn raw(&self) -> u16 {
        (self.iden as u16) << 12 | self.number
    }
}

/// Voice service options octet (TIA-102.AABC 2.3.3): emergency, protected
/// (encrypted), duplex, packet mode, priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct ServiceOptions {
    pub emergency: bool,
    pub encrypted: bool,
    pub duplex: bool,
    pub packet: bool,
    pub priority: u8,
}
impl ServiceOptions {
    pub fn from_octet(value: u8) -> Self {
        Self {
            emergency: value & 0x80 != 0,
            encrypted: value & 0x40 != 0,
            duplex: value & 0x20 != 0,
            packet: value & 0x10 != 0,
            priority: value & 7,
        }
    }
}

/// Channel identifier table entry from IDEN_UP / IDEN_UP_VU / IDEN_UP_TDMA.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Iden {
    pub iden: u8,
    pub base_hz: u64,
    pub spacing_hz: u64,
    /// Signed transmit (uplink) offset in Hz; None when the field says "no offset".
    pub tx_offset_hz: Option<i64>,
    pub bandwidth_hz: u32,
    /// TDMA carriers carry 2 (or 4) logical channels per frequency.
    pub tdma: bool,
    pub slots: u8,
}
impl Iden {
    /// Downlink frequency for a channel number. For TDMA identifiers the
    /// channel number counts timeslots: carrier = number / slots (SDRTrunk
    /// FrequencyBandUpdateTDMA.getDownlinkFrequency; OP25 tk_p25 channel_id_to_frequency).
    pub fn downlink_hz(&self, number: u16) -> u64 {
        let carrier = number as u64 / self.slots.max(1) as u64;
        self.base_hz + self.spacing_hz * carrier
    }
    pub fn slot(&self, number: u16) -> Option<u8> {
        self.tdma.then(|| (number % self.slots.max(1) as u16) as u8)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tsbk {
    GroupVoiceGrant {
        options: ServiceOptions,
        channel: Channel,
        group: u16,
        source: u32,
    },
    GroupVoiceGrantUpdate {
        updates: Vec<(Channel, u16)>,
    },
    GroupVoiceGrantUpdateExplicit {
        options: ServiceOptions,
        channel: Channel,
        uplink: Channel,
        group: u16,
    },
    UnitVoiceGrant {
        channel: Channel,
        target: u32,
        source: u32,
    },
    UnitVoiceGrantUpdate {
        channel: Channel,
        target: u32,
        source: u32,
    },
    TelephoneGrant {
        options: ServiceOptions,
        channel: Channel,
        source: u32,
    },
    /// Motorola patch / supergroup grant (MFID 0x90 opcode 0x02).
    PatchGrant {
        options: ServiceOptions,
        channel: Channel,
        group: u16,
        source: u32,
    },
    /// Motorola patch grant update (MFID 0x90 opcode 0x03).
    PatchGrantUpdate {
        updates: Vec<(Channel, u16)>,
    },
    /// Motorola patch add (MFID 0x90 opcode 0x00): supergroup and member groups.
    PatchAdd {
        supergroup: u16,
        groups: Vec<u16>,
    },
    PatchDelete {
        supergroup: u16,
        groups: Vec<u16>,
    },
    IdenUp(Iden),
    NetworkStatus {
        lra: u8,
        wacn: u32,
        system: u16,
        channel: Channel,
        service_class: u8,
    },
    RfssStatus {
        lra: u8,
        system: u16,
        rfss: u8,
        site: u8,
        channel: Channel,
        service_class: u8,
    },
    AdjacentStatus {
        lra: u8,
        system: u16,
        rfss: u8,
        site: u8,
        channel: Channel,
        service_class: u8,
    },
    SecondaryControl {
        rfss: u8,
        site: u8,
        channels: Vec<Channel>,
    },
    SystemService {
        available: u32,
        supported: u32,
    },
    /// Motorola control channel base station ID (MFID 0x90 opcode 0x0B), CW callsign.
    BaseStationId {
        callsign: String,
        channel: Option<Channel>,
    },
    Other {
        mfid: u8,
        opcode: u8,
    },
}

impl Tsbk {
    /// Parse a CRC-checked 12-byte outbound TSBK.
    pub fn parse(b: &[u8; 12]) -> Self {
        let opcode = b[0] & 0x3f;
        let mfid = b[1];
        let ch = |start| Channel::from_bits(b, start);
        let f = |start, len| field(b, start, len);
        let opts = ServiceOptions::from_octet(b[2]);
        let pair_updates = || {
            let mut updates = vec![(ch(16), f(32, 16) as u16)];
            let (second, group) = (ch(48), f(64, 16) as u16);
            if group != 0 && (second, group) != updates[0] {
                updates.push((second, group));
            }
            updates
        };
        match (mfid, opcode) {
            (0x00 | 0x01, 0x00) => Tsbk::GroupVoiceGrant {
                options: opts,
                channel: ch(24),
                group: f(40, 16) as u16,
                source: f(56, 24) as u32,
            },
            (0x00 | 0x01, 0x02) => Tsbk::GroupVoiceGrantUpdate {
                updates: pair_updates(),
            },
            (0x00 | 0x01, 0x03) => Tsbk::GroupVoiceGrantUpdateExplicit {
                options: opts,
                channel: ch(32),
                uplink: ch(48),
                group: f(64, 16) as u16,
            },
            (0x00 | 0x01, 0x04) => Tsbk::UnitVoiceGrant {
                channel: ch(16),
                target: f(32, 24) as u32,
                source: f(56, 24) as u32,
            },
            (0x00 | 0x01, 0x06) => Tsbk::UnitVoiceGrantUpdate {
                channel: ch(16),
                target: f(32, 24) as u32,
                source: f(56, 24) as u32,
            },
            (0x00 | 0x01, 0x08) => Tsbk::TelephoneGrant {
                options: opts,
                channel: ch(24),
                source: f(56, 24) as u32,
            },
            (0x00 | 0x01, 0x3d) => {
                // IDEN_UP: bandwidth 9 bits x125 Hz, offset sign + 8 bits x250 kHz.
                let sign = f(29, 1) == 1;
                let magnitude = f(30, 8) as i64 * 250_000;
                Tsbk::IdenUp(Iden {
                    iden: f(16, 4) as u8,
                    bandwidth_hz: f(20, 9) as u32 * 125,
                    tx_offset_hz: (magnitude != 0)
                        .then_some(if sign { magnitude } else { -magnitude }),
                    spacing_hz: f(38, 10) * 125,
                    base_hz: f(48, 32) * 5,
                    tdma: false,
                    slots: 1,
                })
            }
            (0x00 | 0x01, 0x34) => {
                // IDEN_UP_VU: bandwidth code, offset sign + 13 bits x spacing.
                let spacing = f(38, 10) * 125;
                let sign = f(24, 1) == 1;
                let magnitude = (f(25, 13) * spacing) as i64;
                Tsbk::IdenUp(Iden {
                    iden: f(16, 4) as u8,
                    bandwidth_hz: match f(20, 4) {
                        4 => 6250,
                        5 => 12500,
                        _ => 0,
                    },
                    tx_offset_hz: (magnitude != 0)
                        .then_some(if sign { magnitude } else { -magnitude }),
                    spacing_hz: spacing,
                    base_hz: f(48, 32) * 5,
                    tdma: false,
                    slots: 1,
                })
            }
            (0x00 | 0x01, 0x33) => {
                // IDEN_UP_TDMA: channel type selects access type and slots per carrier.
                let spacing = f(38, 10) * 125;
                let sign = f(24, 1) == 1;
                let magnitude = (f(25, 13) * spacing) as i64;
                let (tdma, slots, bandwidth) = match f(20, 4) {
                    0 | 1 => (false, 1, 12500),
                    2 => (false, 1, 6250),
                    3 | 5 => (true, 2, 12500),
                    4 => (true, 4, 25000),
                    _ => (false, 1, 0),
                };
                Tsbk::IdenUp(Iden {
                    iden: f(16, 4) as u8,
                    bandwidth_hz: bandwidth,
                    tx_offset_hz: (magnitude != 0)
                        .then_some(if sign { magnitude } else { -magnitude }),
                    spacing_hz: spacing,
                    base_hz: f(48, 32) * 5,
                    tdma,
                    slots,
                })
            }
            (0x00 | 0x01, 0x3b) => Tsbk::NetworkStatus {
                lra: f(16, 8) as u8,
                wacn: f(24, 20) as u32,
                system: f(44, 12) as u16,
                channel: ch(56),
                service_class: f(72, 8) as u8,
            },
            (0x00 | 0x01, 0x3a) | (0x00 | 0x01, 0x3c) => {
                let (lra, system, rfss, site, channel, service_class) = (
                    f(16, 8) as u8,
                    f(28, 12) as u16,
                    f(40, 8) as u8,
                    f(48, 8) as u8,
                    ch(56),
                    f(72, 8) as u8,
                );
                if opcode == 0x3a {
                    Tsbk::RfssStatus {
                        lra,
                        system,
                        rfss,
                        site,
                        channel,
                        service_class,
                    }
                } else {
                    Tsbk::AdjacentStatus {
                        lra,
                        system,
                        rfss,
                        site,
                        channel,
                        service_class,
                    }
                }
            }
            (0x00 | 0x01, 0x39) => {
                let mut channels = vec![ch(32)];
                if f(72, 8) != 0 && ch(56) != channels[0] {
                    channels.push(ch(56));
                }
                Tsbk::SecondaryControl {
                    rfss: f(16, 8) as u8,
                    site: f(24, 8) as u8,
                    channels,
                }
            }
            (0x00 | 0x01, 0x38) => Tsbk::SystemService {
                available: f(24, 24) as u32,
                supported: f(48, 24) as u32,
            },
            (0x90, 0x00) | (0x90, 0x01) => {
                let groups: Vec<u16> = [32, 48, 64]
                    .iter()
                    .map(|s| f(*s, 16) as u16)
                    .filter(|g| *g != 0)
                    .collect();
                let supergroup = f(16, 16) as u16;
                if opcode == 0 {
                    Tsbk::PatchAdd { supergroup, groups }
                } else {
                    Tsbk::PatchDelete { supergroup, groups }
                }
            }
            (0x90, 0x02) => Tsbk::PatchGrant {
                options: opts,
                channel: ch(24),
                group: f(40, 16) as u16,
                source: f(56, 24) as u32,
            },
            (0x90, 0x03) => Tsbk::PatchGrantUpdate {
                updates: pair_updates(),
            },
            (0x90, 0x0b) => {
                // Eight 6-bit characters, value + 43 is ASCII (SDRTrunk MotorolaBaseStationId).
                let callsign: String = (0..8)
                    .map(|i| f(16 + 6 * i, 6) as u8)
                    .filter(|c| *c != 0)
                    .map(|c| (c + 43) as char)
                    .collect();
                let channel = ch(64);
                Tsbk::BaseStationId {
                    callsign,
                    channel: (channel.number != 0).then_some(channel),
                }
            }
            _ => Tsbk::Other { mfid, opcode },
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Tsbk::GroupVoiceGrant { .. } => "GRP_V_CH_GRANT",
            Tsbk::GroupVoiceGrantUpdate { .. } => "GRP_V_CH_GRANT_UPDT",
            Tsbk::GroupVoiceGrantUpdateExplicit { .. } => "GRP_V_CH_GRANT_UPDT_EXP",
            Tsbk::UnitVoiceGrant { .. } => "UU_V_CH_GRANT",
            Tsbk::UnitVoiceGrantUpdate { .. } => "UU_V_CH_GRANT_UPDT",
            Tsbk::TelephoneGrant { .. } => "TELE_INT_CH_GRANT",
            Tsbk::PatchGrant { .. } => "MOT_GRG_CH_GRANT",
            Tsbk::PatchGrantUpdate { .. } => "MOT_GRG_CH_GRANT_UPDT",
            Tsbk::PatchAdd { .. } => "MOT_GRG_ADD_CMD",
            Tsbk::PatchDelete { .. } => "MOT_GRG_DEL_CMD",
            Tsbk::IdenUp(_) => "IDEN_UP",
            Tsbk::NetworkStatus { .. } => "NET_STS_BCST",
            Tsbk::RfssStatus { .. } => "RFSS_STS_BCST",
            Tsbk::AdjacentStatus { .. } => "ADJ_STS_BCST",
            Tsbk::SecondaryControl { .. } => "SCCB",
            Tsbk::SystemService { .. } => "SYS_SRV_BCST",
            Tsbk::BaseStationId { .. } => "MOT_BSI",
            Tsbk::Other { .. } => "OTHER",
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::p25::fec::{crc_ccitt_fill, crc_ccitt_ok};

    /// Build a TSBK from opcode, MFID and 64 argument bits, with valid CRC.
    pub fn build(last: bool, opcode: u8, mfid: u8, args: u64) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0] = (last as u8) << 7 | opcode;
        b[1] = mfid;
        b[2..10].copy_from_slice(&args.to_be_bytes());
        crc_ccitt_fill(&mut b);
        assert!(crc_ccitt_ok(&b));
        b
    }

    #[test]
    fn iden_up_800mhz_matches_hand_math() {
        // 800 MHz rebanded plan: base 851.006250 MHz, 6.25 kHz spacing, -45 MHz offset,
        // 12.5 kHz bandwidth. Base is in 5 Hz units, spacing in 125 Hz units.
        let iden: u64 = 1;
        let bandwidth = 12500 / 125; // 100
        let offset = 45_000_000 / 250_000; // 180 = 0xB4; sign bit 0 = negative
        let spacing = 6250 / 125; // 50
        let base = 851_006_250 / 5;
        let args = iden << 60 | bandwidth << 51 | offset << 42 | spacing << 32 | base;
        let tsbk = Tsbk::parse(&build(true, 0x3d, 0, args));
        let Tsbk::IdenUp(table) = tsbk else {
            panic!("{tsbk:?}")
        };
        assert_eq!(table.iden, 1);
        assert_eq!(table.base_hz, 851_006_250);
        assert_eq!(table.spacing_hz, 6250);
        assert_eq!(table.bandwidth_hz, 12500);
        assert_eq!(table.tx_offset_hz, Some(-45_000_000));
        // Channel 0x4D (77) of this table is 851.48750 MHz.
        assert_eq!(table.downlink_hz(77), 851_487_500);
    }
    #[test]
    fn iden_up_tdma_counts_timeslots() {
        // Channel type 3: TDMA 12.5 kHz, 2 slots. Offset magnitude counts spacings.
        let spacing = 6250 / 125;
        let offset_units = 45_000_000 / 6250; // 7200, negative (sign 0)
        let args = 2u64 << 60 | 3 << 56 | offset_units << 42 | spacing << 32 | 851_006_250 / 5;
        let Tsbk::IdenUp(table) = Tsbk::parse(&build(true, 0x33, 0, args)) else {
            panic!()
        };
        assert!(table.tdma);
        assert_eq!(table.slots, 2);
        assert_eq!(table.tx_offset_hz, Some(-45_000_000));
        assert_eq!(table.downlink_hz(0x0a1), 851_006_250 + 6250 * 80);
        assert_eq!(table.slot(0x0a1), Some(1));
    }
    #[test]
    fn group_voice_grant_fields() {
        // Encrypted + emergency, channel 1-0x04D, talkgroup 0x1234, source 0xABCDEF.
        let args = 0xC0u64 << 56 | 0x104D << 40 | 0x1234 << 24 | 0xABCDEF;
        let tsbk = Tsbk::parse(&build(true, 0x00, 0, args));
        assert_eq!(
            tsbk,
            Tsbk::GroupVoiceGrant {
                options: ServiceOptions {
                    emergency: true,
                    encrypted: true,
                    duplex: false,
                    packet: false,
                    priority: 0
                },
                channel: Channel {
                    iden: 1,
                    number: 0x4d
                },
                group: 0x1234,
                source: 0xabcdef
            }
        );
    }
    #[test]
    fn grant_update_dedups_repeated_pair() {
        let args = 0x104Du64 << 48 | 0x0101 << 32 | 0x104D << 16 | 0x0101;
        assert_eq!(
            Tsbk::parse(&build(true, 0x02, 0, args)),
            Tsbk::GroupVoiceGrantUpdate {
                updates: vec![(
                    Channel {
                        iden: 1,
                        number: 0x4d
                    },
                    0x101
                )]
            }
        );
    }
    #[test]
    fn network_and_rfss_status() {
        let args = 0x01u64 << 56 | 0xBEE00 << 36 | 0x3AB << 24 | 0x104D << 8 | 0x70;
        assert_eq!(
            Tsbk::parse(&build(true, 0x3b, 0, args)),
            Tsbk::NetworkStatus {
                lra: 1,
                wacn: 0xbee00,
                system: 0x3ab,
                channel: Channel {
                    iden: 1,
                    number: 0x4d
                },
                service_class: 0x70
            }
        );
        let args = 0x01u64 << 56 | 0x3AB << 40 | 0x01 << 32 | 0x0E << 24 | 0x104D << 8 | 0x70;
        let Tsbk::RfssStatus { system, rfss, site, .. } = Tsbk::parse(&build(true, 0x3a, 0, args))
        else {
            panic!()
        };
        assert_eq!((system, rfss, site), (0x3ab, 1, 14));
    }
}
