//! PTT probes for the direct serial (COM port) method: one CAT status
//! command per radio model plus the rule that marks the response as
//! "transmitting".
//!
//! The commands and response rules were extracted from OmniRig's per-model
//! rig definition files (field-tested against real radios) and the vendors'
//! CAT manuals:
//!
//! - Icom (CI-V binary): `FE FE <addr> E0 1C 00 FD` reads the RX/TX status;
//!   the second-to-last response byte, bit 0, is set while transmitting.
//! - Kenwood / Elecraft K-series (ASCII): `IF;` reads the full status line;
//!   character 28 is `1` while transmitting.
//! - Flex PowerSDR: `IF;` like Kenwood; SmartSDR prefixes the address
//!   (`ZZIF;`, character 30).
//! - Yaesu CI-V (FT-817/847/857/897): `00 00 00 00 F7` (FT-847: `E7`
//!   terminator) reads a status byte, bit 7 encodes RX/TX (polarity differs
//!   between the F7 and E7 generations); newer Yaesu (FT-840/900/920/990/
//!   1000 series) answer a 5-8 byte status block on the `FA` terminator;
//!   the FT-767GX sends an 86-byte block without a terminator.
//! - Yaesu FT-891/900/991 platform, the FTX-1 and the FT-DX10: ASCII `TX;`
//!   reads the transmit state (`0` RX, `1` CAT-keyed, `2` radio transmitting).
//!   The FT-DX10 exposes two virtual COM ports — the Enhanced COM Port is the
//!   CAT one, the Standard COM Port is TX control only.
//! - Ten-Tec Paragon II (CI-V): `FE FE 2C E0 17 FD`, bit 2 of the
//!   second-to-last byte.
//! - Xiegu G90 speaks Icom CI-V (address 0x88); the X-6200 answers the same
//!   `1C 00` command with a shorter 8-byte block.

/// One byte test against the collected response:
/// `(data[idx] & mask) == expect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteTest {
    pub idx: usize,
    pub mask: u8,
    pub expect: u8,
}

/// How the radio's response is read after the command is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadSpec {
    /// Stop when the terminator byte arrives (it stays in the collected
    /// buffer). The cap guards against a radio that never terminates.
    Until(u8, usize),
    /// Stop after exactly `n` bytes (radios that send a fixed-size block
    /// without a terminator).
    Count(usize),
}

impl ReadSpec {
    fn term(&self) -> Option<u8> {
        match self {
            ReadSpec::Until(term, _) => Some(*term),
            ReadSpec::Count(_) => None,
        }
    }
}

/// A PTT probe: the command to send, how to read the reply, and the rules
/// that mark the radio as transmitting.
pub struct Probe {
    /// The exact command bytes to send.
    pub cmd: &'static [u8],
    /// How the response is read.
    pub read: ReadSpec,
    /// How long to wait for the response before declaring it missing.
    pub timeout_ms: u32,
    /// Primary TX rule: all of these byte tests must match.
    pub tx: &'static [ByteTest],
    /// Alternative TX rule (OR with the primary one): for radios whose
    /// OmniRig definition has two pmTx flag forms (different VFO or status
    /// bits that both mean "transmitting").
    pub tx_alt: Option<&'static [ByteTest]>,
    /// Instead of fixed indices, test the byte directly before the trailing
    /// terminator (the 1-byte Yaesu status replies, whose prefix length can
    /// vary).
    pub last_data: Option<ByteTest>,
}

impl Probe {
    /// Evaluate a collected response: `Some(0)` = RX, `Some(1)` = TX,
    /// `None` = no usable PTT information (truncated / malformed / no
    /// terminator). The pmRx rule of every surveyed radio is the complement
    /// of its pmTx rules, so "no rule matches" means RX.
    pub fn parse(&self, data: &[u8]) -> Option<u8> {
        let term = self.read.term();
        let has_term = term.is_some_and(|t| data.last() == Some(&t));
        if let Some(t) = self.last_data {
            let i = if has_term {
                if data.len() < 2 {
                    return None;
                }
                data.len() - 2
            } else {
                data.len().checked_sub(1)?
            };
            return Some(u8::from((data[i] & t.mask) == t.expect));
        }
        if term.is_some() && !has_term {
            return None;
        }
        // A rule may only test data bytes: in a terminated read the final
        // byte is the terminator, so a rule whose index lands on it means
        // the response is a truncated command echo, not a status.
        let end = data.len() - usize::from(has_term);
        // At least one rule must be fully evaluable; otherwise the
        // response is too short to mean anything.
        let evaluable = Self::rule_ok(self.tx, end)
            || self
                .tx_alt
                .is_some_and(|alt| Self::rule_ok(alt, end));
        if !evaluable {
            return None;
        }
        let tx = Self::rule_matches(self.tx, data)
            || self
                .tx_alt
                .is_some_and(|alt| Self::rule_matches(alt, data));
        Some(u8::from(tx))
    }

    fn rule_ok(rule: &[ByteTest], len: usize) -> bool {
        rule.iter().all(|t| t.idx < len)
    }

    fn rule_matches(rule: &[ByteTest], data: &[u8]) -> bool {
        rule.iter().all(|t| (data[t.idx] & t.mask) == t.expect)
    }
}

/// A supported radio: the dropdown name plus its probe.
pub struct RadioDef {
    pub name: &'static str,
    pub probe: Probe,
}

/// Look up a radio by its exact dropdown name.
pub fn radio_by_name(name: &str) -> Option<&'static RadioDef> {
    RADIOS.iter().find(|r| r.name == name)
}

/// Icom CI-V `1C 00` status probe for the 15-byte responses (all Icoms and
/// the Icom-compatible Xiegu G90).
macro_rules! icom_probe {
    ($addr:literal) => {
        Probe {
            cmd: &[0xFE, 0xFE, $addr, 0xE0, 0x1C, 0x00, 0xFD],
            read: ReadSpec::Until(0xFD, 24),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 13, mask: 0x01, expect: 0x01 }],
            tx_alt: None,
            last_data: None,
        }
    };
}

/// Kenwood-style ASCII `IF;` status probe (Kenwood TS series and the
/// Elecraft K-series, which reuse the protocol).
const IF_PROBE: Probe = Probe {
    cmd: b"IF;",
    read: ReadSpec::Until(b';', 64),
    timeout_ms: 150,
    tx: &[ByteTest { idx: 28, mask: 0xFF, expect: b'1' }],
    tx_alt: None,
    last_data: None,
};

/// SmartSDR: same status line, but the radio echoes the `ZZ` address.
const SMARTSDR_PROBE: Probe = Probe {
    cmd: b"ZZIF;",
    read: ReadSpec::Until(b';', 64),
    timeout_ms: 150,
    tx: &[ByteTest { idx: 30, mask: 0xFF, expect: b'1' }],
    tx_alt: None,
    last_data: None,
};

/// Yaesu FT-891/900/991 platform + FTX-1 + FT-DX10: ASCII `TX;` read,
/// `TX0;` RX / `TX1;` CAT keyed / `TX2;` radio transmitting. Both keyed
/// states count as TX (the denoiser must bypass whenever the radio is or is
/// about to be keyed). The FT-DX10's `TX` read is confirmed from its CAT manual.
const YAESU_TX_PROBE: Probe = Probe {
    cmd: b"TX;",
    read: ReadSpec::Until(b';', 16),
    timeout_ms: 150,
    tx: &[ByteTest { idx: 2, mask: 0xFF, expect: b'2' }],
    tx_alt: Some(&[ByteTest { idx: 2, mask: 0xFF, expect: b'1' }]),
    last_data: None,
};

/// All supported radios. The dropdown lists them in this order.
pub const RADIOS: &[RadioDef] = &[
    // Icom (CI-V 1C 00, 15-byte reply, bit 0 of the penultimate byte).
    RadioDef { name: "Icom IC-7000", probe: icom_probe!(0x70) },
    RadioDef { name: "Icom IC-705", probe: icom_probe!(0xA4) },
    RadioDef { name: "Icom IC-7100", probe: icom_probe!(0x88) },
    RadioDef { name: "Icom IC-7200", probe: icom_probe!(0x76) },
    RadioDef { name: "Icom IC-7300", probe: icom_probe!(0x94) },
    RadioDef { name: "Icom IC-7300 Mk II", probe: icom_probe!(0xB6) },
    RadioDef { name: "Icom IC-7410", probe: icom_probe!(0x80) },
    RadioDef { name: "Icom IC-746", probe: icom_probe!(0x56) },
    RadioDef { name: "Icom IC-746 Pro", probe: icom_probe!(0x66) },
    RadioDef { name: "Icom IC-756", probe: icom_probe!(0x50) },
    RadioDef { name: "Icom IC-756 Pro", probe: icom_probe!(0x5C) },
    RadioDef { name: "Icom IC-756 Pro II", probe: icom_probe!(0x64) },
    RadioDef { name: "Icom IC-756 Pro III", probe: icom_probe!(0x6E) },
    RadioDef { name: "Icom IC-7600", probe: icom_probe!(0x7A) },
    RadioDef { name: "Icom IC-7610", probe: icom_probe!(0x98) },
    RadioDef { name: "Icom IC-7700", probe: icom_probe!(0x74) },
    RadioDef { name: "Icom IC-7760", probe: icom_probe!(0xB2) },
    RadioDef { name: "Icom IC-7800", probe: icom_probe!(0x6A) },
    RadioDef { name: "Icom IC-7850", probe: icom_probe!(0x8E) },
    RadioDef { name: "Icom IC-7851", probe: icom_probe!(0x8E) },
    RadioDef { name: "Icom IC-910", probe: icom_probe!(0x60) },
    RadioDef { name: "Icom IC-9100", probe: icom_probe!(0x7C) },
    RadioDef { name: "Icom IC-9700", probe: icom_probe!(0xA2) },
    RadioDef { name: "Icom IC-970D", probe: icom_probe!(0x2E) },
    RadioDef { name: "Icom ID-5100A", probe: icom_probe!(0x8C) },
    // Icom-compatible.
    RadioDef { name: "Xiegu G90", probe: icom_probe!(0x88) },
    RadioDef {
        name: "Xiegu X-6200",
        probe: Probe {
            cmd: &[0xFE, 0xFE, 0xA4, 0xE0, 0x1C, 0x00, 0xFD],
            read: ReadSpec::Until(0xFD, 16),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 6, mask: 0x01, expect: 0x01 }],
            tx_alt: None,
            last_data: None,
        },
    },
    // Kenwood (ASCII IF;).
    RadioDef { name: "Kenwood TS-440", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-480", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-590", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-690", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-850", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-870", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-930", probe: IF_PROBE },
    RadioDef { name: "Kenwood TS-2000", probe: IF_PROBE },
    // Elecraft K-series (Kenwood-style ASCII).
    RadioDef { name: "Elecraft K2", probe: IF_PROBE },
    RadioDef { name: "Elecraft K3", probe: IF_PROBE },
    RadioDef { name: "Elecraft K4", probe: IF_PROBE },
    RadioDef { name: "Elecraft KX2", probe: IF_PROBE },
    RadioDef { name: "Elecraft KX3", probe: IF_PROBE },
    // Flex.
    RadioDef { name: "Flex (PowerSDR)", probe: IF_PROBE },
    RadioDef { name: "Flex (SmartSDR)", probe: SMARTSDR_PROBE },
    // Yaesu.
    RadioDef {
        name: "Yaesu FT-767GX",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0x0B],
            read: ReadSpec::Count(86),
            timeout_ms: 400,
            tx: &[ByteTest { idx: 85, mask: 0x01, expect: 0x01 }],
            tx_alt: None,
            last_data: None,
        },
    },
    // FT-817/857/897: 1-byte status, TX while bit 7 is clear.
    RadioDef {
        name: "Yaesu FT-817",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xF7],
            read: ReadSpec::Until(0xF7, 8),
            timeout_ms: 150,
            tx: &[],
            tx_alt: None,
            last_data: Some(ByteTest { idx: 0, mask: 0x80, expect: 0x00 }),
        },
    },
    RadioDef {
        name: "Yaesu FT-857",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xF7],
            read: ReadSpec::Until(0xF7, 8),
            timeout_ms: 150,
            tx: &[],
            tx_alt: None,
            last_data: Some(ByteTest { idx: 0, mask: 0x80, expect: 0x00 }),
        },
    },
    RadioDef {
        name: "Yaesu FT-897",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xF7],
            read: ReadSpec::Until(0xF7, 8),
            timeout_ms: 150,
            tx: &[],
            tx_alt: None,
            last_data: Some(ByteTest { idx: 0, mask: 0x80, expect: 0x00 }),
        },
    },
    // FT-847: 1-byte status on the E7 terminator, polarity inverted
    // (TX while bit 7 is set).
    RadioDef {
        name: "Yaesu FT-847",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xE7],
            read: ReadSpec::Until(0xE7, 8),
            timeout_ms: 150,
            tx: &[],
            tx_alt: None,
            last_data: Some(ByteTest { idx: 0, mask: 0x80, expect: 0x80 }),
        },
    },
    // FT-840/900: 5-byte block, TX when byte 1 bit 0 is set with byte 2
    // bit 7 clear, or byte 1 bit 0 clear with byte 2 bit 7 set.
    RadioDef {
        name: "Yaesu FT-840",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[
                ByteTest { idx: 1, mask: 0x01, expect: 0x01 },
                ByteTest { idx: 2, mask: 0x80, expect: 0x00 },
            ],
            tx_alt: Some(&[
                ByteTest { idx: 1, mask: 0x01, expect: 0x00 },
                ByteTest { idx: 2, mask: 0x80, expect: 0x80 },
            ]),
            last_data: None,
        },
    },
    RadioDef {
        name: "Yaesu FT-900",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x01, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[
                ByteTest { idx: 1, mask: 0x01, expect: 0x01 },
                ByteTest { idx: 2, mask: 0x80, expect: 0x00 },
            ],
            tx_alt: Some(&[
                ByteTest { idx: 1, mask: 0x01, expect: 0x00 },
                ByteTest { idx: 2, mask: 0x80, expect: 0x80 },
            ]),
            last_data: None,
        },
    },
    // FT-920/1000MP/100D: TX when byte 0 bit 7 is set or byte 3 bit 4 is
    // set.
    RadioDef {
        name: "Yaesu FT-920",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x01, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 0, mask: 0x80, expect: 0x80 }],
            tx_alt: Some(&[ByteTest { idx: 3, mask: 0x10, expect: 0x10 }]),
            last_data: None,
        },
    },
    RadioDef {
        name: "Yaesu FT-1000MP",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x01, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 0, mask: 0x80, expect: 0x80 }],
            tx_alt: Some(&[ByteTest { idx: 3, mask: 0x10, expect: 0x10 }]),
            last_data: None,
        },
    },
    RadioDef {
        name: "Yaesu FT-100D",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x01, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 0, mask: 0x80, expect: 0x80 }],
            tx_alt: Some(&[ByteTest { idx: 3, mask: 0x10, expect: 0x10 }]),
            last_data: None,
        },
    },
    // FT-990/1000: TX when byte 0 bit 7 is set or byte 2 bit 0 is set.
    RadioDef {
        name: "Yaesu FT-990",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 0, mask: 0x80, expect: 0x80 }],
            tx_alt: Some(&[ByteTest { idx: 2, mask: 0x01, expect: 0x01 }]),
            last_data: None,
        },
    },
    RadioDef {
        name: "Yaesu FT-1000",
        probe: Probe {
            cmd: &[0x00, 0x00, 0x00, 0x00, 0xFA],
            read: ReadSpec::Until(0xFA, 16),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 0, mask: 0x80, expect: 0x80 }],
            tx_alt: Some(&[ByteTest { idx: 2, mask: 0x01, expect: 0x01 }]),
            last_data: None,
        },
    },
    // FT-891/991 platform + FTX-1 + FT-DX10 (ASCII TX;).
    RadioDef { name: "Yaesu FT-891", probe: YAESU_TX_PROBE },
    RadioDef { name: "Yaesu FT-991", probe: YAESU_TX_PROBE },
    RadioDef { name: "Yaesu FT-DX10", probe: YAESU_TX_PROBE },
    RadioDef { name: "Yaesu FTX-1", probe: YAESU_TX_PROBE },
    // Ten-Tec.
    RadioDef {
        name: "Ten-Tec Paragon II",
        probe: Probe {
            cmd: &[0xFE, 0xFE, 0x2C, 0xE0, 0x17, 0xFD],
            read: ReadSpec::Until(0xFD, 24),
            timeout_ms: 150,
            tx: &[ByteTest { idx: 11, mask: 0x04, expect: 0x04 }],
            tx_alt: None,
            last_data: None,
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn icom_reply(addr: u8, status: u8) -> Vec<u8> {
        let mut v = vec![0xFE, 0xFE, addr, 0xE0, 0x1C, 0x00];
        v.extend([0u8; 7]);
        v.push(status);
        v.push(0xFD);
        v
    }

    #[test]
    fn icom_tx_bit() {
        let r = radio_by_name("Icom IC-7300").unwrap();
        assert_eq!(r.probe.parse(&icom_reply(0x94, 0x01)), Some(1));
        assert_eq!(r.probe.parse(&icom_reply(0x94, 0x00)), Some(0));
        // Other bits in the status byte don't matter.
        assert_eq!(r.probe.parse(&icom_reply(0x94, 0x03)), Some(1));
        assert_eq!(r.probe.parse(&icom_reply(0x94, 0x02)), Some(0));
    }

    #[test]
    fn icom_missing_terminator_is_unusable() {
        let r = radio_by_name("Icom IC-7300").unwrap();
        let mut v = icom_reply(0x94, 0x01);
        v.pop();
        assert_eq!(r.probe.parse(&v), None);
        assert_eq!(r.probe.parse(&[]), None);
    }

    #[test]
    fn xiegu_6200_short_reply() {
        let r = radio_by_name("Xiegu X-6200").unwrap();
        let tx = [0xFE, 0xFE, 0xA4, 0xE0, 0x1C, 0x00, 0x01, 0xFD];
        let rx = [0xFE, 0xFE, 0xA4, 0xE0, 0x1C, 0x00, 0x00, 0xFD];
        assert_eq!(r.probe.parse(&tx), Some(1));
        assert_eq!(r.probe.parse(&rx), Some(0));
    }

    fn kenwood_line(tx: bool) -> Vec<u8> {
        let mut s = String::from("IF");
        for _ in 0..36 {
            s.push('0');
        }
        if tx {
            s.replace_range(28..29, "1");
        }
        s.push(';');
        s.into_bytes()
    }

    #[test]
    fn kenwood_if_status() {
        let r = radio_by_name("Kenwood TS-590").unwrap();
        assert_eq!(r.probe.parse(&kenwood_line(true)), Some(1));
        assert_eq!(r.probe.parse(&kenwood_line(false)), Some(0));
        let kx = radio_by_name("Elecraft K3").unwrap();
        assert_eq!(kx.probe.parse(&kenwood_line(true)), Some(1));
    }

    #[test]
    fn smart_sdr_address_echo() {
        let r = radio_by_name("Flex (SmartSDR)").unwrap();
        let mut line = kenwood_line(true);
        // The radio echoes the two address bytes in front of IF.
        line.splice(0..0, b"ZZ".iter().copied());
        assert_eq!(r.probe.parse(&line), Some(1));
    }

    #[test]
    fn yaesu_tx_ascii() {
        let r = radio_by_name("Yaesu FT-991").unwrap();
        assert_eq!(r.probe.parse(b"TX0;"), Some(0));
        assert_eq!(r.probe.parse(b"TX1;"), Some(1));
        assert_eq!(r.probe.parse(b"TX2;"), Some(1));
        assert_eq!(r.probe.parse(b"TV;"), None);
        // FT-DX10: same `TX;` read, confirmed by its CAT manual (0 = both off,
        // 1 = CAT-keyed, 2 = radio transmitting).
        let dx10 = radio_by_name("Yaesu FT-DX10").unwrap();
        assert_eq!(dx10.probe.cmd, b"TX;");
        assert_eq!(dx10.probe.parse(b"TX1;"), Some(1));
        assert_eq!(dx10.probe.parse(b"TX2;"), Some(1));
    }

    #[test]
    fn yaesu_f7_status_byte() {
        let r = radio_by_name("Yaesu FT-857").unwrap();
        // 1 byte + terminator; bit 7 clear = TX.
        assert_eq!(r.probe.parse(&[0x00, 0xF7]), Some(1));
        assert_eq!(r.probe.parse(&[0x80, 0xF7]), Some(0));
        // With an echo prefix the tested byte is still the one before F7.
        assert_eq!(r.probe.parse(&[0x00, 0x00, 0x00, 0xF7]), Some(1));
        assert_eq!(r.probe.parse(&[0x00, 0x00, 0x80, 0xF7]), Some(0));
        // No terminator at all: the single byte is still usable.
        assert_eq!(r.probe.parse(&[0x00]), Some(1));
    }

    #[test]
    fn yaesu_e7_inverted_polarity() {
        let r = radio_by_name("Yaesu FT-847").unwrap();
        assert_eq!(r.probe.parse(&[0x80, 0xE7]), Some(1));
        assert_eq!(r.probe.parse(&[0x00, 0xE7]), Some(0));
    }

    #[test]
    fn yaesu_5_byte_block() {
        let r = radio_by_name("Yaesu FT-1000").unwrap();
        // Byte 0 bit 7 set = TX.
        assert_eq!(r.probe.parse(&[0x80, 0, 0, 0, 0, 0xFA]), Some(1));
        // Byte 2 bit 0 set = TX.
        assert_eq!(r.probe.parse(&[0x00, 0, 0x01, 0, 0, 0xFA]), Some(1));
        // Neither = RX.
        assert_eq!(r.probe.parse(&[0x00, 0, 0x00, 0, 0, 0xFA]), Some(0));
    }

    #[test]
    fn yaesu_ft840_combined_rules() {
        let r = radio_by_name("Yaesu FT-840").unwrap();
        // Byte 1 bit 0 set AND byte 2 bit 7 clear = TX.
        assert_eq!(r.probe.parse(&[0, 0x01, 0x00, 0, 0, 0xFA]), Some(1));
        // Byte 1 bit 0 clear AND byte 2 bit 7 set = TX.
        assert_eq!(r.probe.parse(&[0, 0x00, 0x80, 0, 0, 0xFA]), Some(1));
        // Neither combination = RX.
        assert_eq!(r.probe.parse(&[0, 0x00, 0x00, 0, 0, 0xFA]), Some(0));
        assert_eq!(r.probe.parse(&[0, 0x01, 0x80, 0, 0, 0xFA]), Some(0));
    }

    #[test]
    fn ten_tec_paragon() {
        let r = radio_by_name("Ten-Tec Paragon II").unwrap();
        let mut v = vec![0u8; 13];
        v[0] = 0xFE;
        v[1] = 0xFE;
        v[2] = 0x2C;
        v[3] = 0xE0;
        v[4] = 0x17;
        v[11] = 0x04; // TX bit 2
        v[12] = 0xFD;
        assert_eq!(r.probe.parse(&v), Some(1));
        v[11] = 0x00;
        assert_eq!(r.probe.parse(&v), Some(0));
    }

    #[test]
    fn ft767gx_86_byte_block() {
        let r = radio_by_name("Yaesu FT-767GX").unwrap();
        let mut v = vec![0u8; 86];
        v[85] = 0x01;
        assert_eq!(r.probe.parse(&v), Some(1));
        v[85] = 0x00;
        assert_eq!(r.probe.parse(&v), Some(0));
        // Too short = unusable.
        assert_eq!(r.probe.parse(&v[..85]), None);
    }

    #[test]
    fn all_radios_have_unique_names_and_nonempty_commands() {
        let mut seen = std::collections::HashSet::new();
        for r in RADIOS {
            assert!(seen.insert(r.name), "duplicate radio name {}", r.name);
            assert!(!r.probe.cmd.is_empty());
            assert!(!r.probe.tx.is_empty() || r.probe.last_data.is_some());
        }
        assert_eq!(RADIOS.len(), 59);
    }
}
