//! Clean-room AMD64 Zen 2 `#UD` instruction decoder (AMD64 Architecture
//! Programmer's Manual Vol. 3 & Vol. 4).
//!
//! Decodes AMD/Zen 2 user-mode instructions that raise
//! `STATUS_ILLEGAL_INSTRUCTION` on Intel hosts:
//! - `clzero`, `monitorx`, `mwaitx`, `rdpru` (`0F 01 FA..FD`, plus fused
//!   12-byte `clzero` cache-line loops)
//! - `clwb m8` (`66 [REX] 0F AE /6`)
//! - `rdpid r64` (`F3 [REX] 0F C7 /7`, register form)
//! - SSE4a `extrq`, `insertq` (`66/F2 [REX] 0F 78/79`), `movntsd`, `movntss`
//!   (`F2/F3 [REX] 0F 2B /r`)

/// Decoded Zen 2 / SSE4a instruction form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleOnly {
    Clwb,
    Clzero,
    ClzeroLoop { end_reg: u8 },
    Rdpru,
    Monitorx,
    Mwaitx,
    Rdpid(u8),
    Extrq {
        dst: u8,
        src: Option<u8>,
        len: u8,
        idx: u8,
    },
    Insertq {
        dst: u8,
        src: u8,
        reg: bool,
        len: u8,
        idx: u8,
    },
    Movnt {
        double: bool,
        src_xmm: u8,
        prefix_end: usize,
    },
    Ud2,
}

#[inline]
pub fn extrq(val: u64, raw_len: u8, raw_idx: u8) -> u64 {
    let width = match raw_len & 0x3f {
        0 => 64,
        w => u32::from(w),
    };
    let shift = u32::from(raw_idx & 0x3f);
    let shifted = val.checked_shr(shift).unwrap_or(0);
    if width == 64 {
        shifted
    } else {
        shifted & ((1u64 << width) - 1)
    }
}

#[inline]
pub fn insertq(dst: u64, src: u64, raw_len: u8, raw_idx: u8) -> u64 {
    let width = match raw_len & 0x3f {
        0 => 64,
        w => u32::from(w),
    };
    let shift = u32::from(raw_idx & 0x3f);
    let low_mask = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let placed_mask = low_mask.checked_shl(shift).unwrap_or(0);
    let placed_bits = (src & low_mask).checked_shl(shift).unwrap_or(0) & placed_mask;
    (dst & !placed_mask) | placed_bits
}

fn memory_operand_bytes(tail: &[u8]) -> Option<usize> {
    let modrm = *tail.first()?;
    let mode = modrm >> 6;
    let rm = modrm & 7;
    if mode == 3 {
        return Some(1);
    }
    let (mut size, base_low) = if rm == 4 {
        (2usize, *tail.get(1)? & 7)
    } else {
        (1usize, rm)
    };
    size += match mode {
        0 if base_low == 5 => 4,
        0 => 0,
        1 => 1,
        _ => 4,
    };
    (tail.len() >= size).then_some(size)
}

pub fn decode(bytes: &[u8]) -> Option<(ConsoleOnly, usize)> {
    let mut pos = 0usize;
    let mut has_66 = false;
    let mut last_rep = 0u8;
    let mut rex = 0u8;

    while let Some(&b) = bytes.get(pos) {
        match b {
            0x66 => {
                has_66 = true;
                rex = 0;
            }
            0xf2 | 0xf3 => {
                last_rep = b;
                rex = 0;
            }
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x67 => {
                rex = 0;
            }
            0x40..=0x4f => {
                rex = b;
            }
            _ => break,
        }
        pos += 1;
    }

    if *bytes.get(pos)? != 0x0f {
        return None;
    }
    let op = *bytes.get(pos + 1)?;
    let third = bytes.get(pos + 2).copied();

    match (op, third) {
        (0x01, Some(0xfa)) => Some((ConsoleOnly::Monitorx, pos + 3)),
        (0x01, Some(0xfb)) => Some((ConsoleOnly::Mwaitx, pos + 3)),
        (0x01, Some(0xfd)) => Some((ConsoleOnly::Rdpru, pos + 3)),
        (0x01, Some(0xfc)) => {
            if pos == 0
                && bytes.len() >= 12
                && bytes[3..7] == [0x48, 0x83, 0xc0, 0x40]
                && matches!(bytes[7], 0x48 | 0x4c)
                && bytes[8] == 0x39
                && (bytes[9] & 0xc7) == 0xc0
                && bytes[10..12] == [0x72, 0xf4]
            {
                let hi = u8::from(bytes[7] == 0x4c) << 3;
                let end_reg = ((bytes[9] >> 3) & 7) | hi;
                if end_reg != 0 {
                    return Some((ConsoleOnly::ClzeroLoop { end_reg }, 12));
                }
            }
            Some((ConsoleOnly::Clzero, pos + 3))
        }
        (0x0b, _) => Some((ConsoleOnly::Ud2, pos + 2)),
        (0xae, Some(modrm)) if has_66 && (modrm >> 6) != 3 && ((modrm >> 3) & 7) == 6 => {
            let mem_len = memory_operand_bytes(&bytes[pos + 2..])?;
            Some((ConsoleOnly::Clwb, pos + 2 + mem_len))
        }
        (0xc7, Some(modrm))
            if last_rep == 0xf3 && (modrm >> 6) == 3 && ((modrm >> 3) & 7) == 7 =>
        {
            let reg = (modrm & 7) | ((rex & 1) << 3);
            Some((ConsoleOnly::Rdpid(reg), pos + 3))
        }
        (0x2b, Some(modrm)) if matches!(last_rep, 0xf2 | 0xf3) && (modrm >> 6) != 3 => {
            let mem_len = memory_operand_bytes(&bytes[pos + 2..])?;
            let src_xmm = ((modrm >> 3) & 7) | ((rex & 4) << 1);
            Some((
                ConsoleOnly::Movnt {
                    double: last_rep == 0xf2,
                    src_xmm,
                    prefix_end: pos + 2,
                },
                pos + 2 + mem_len,
            ))
        }
        (0x78 | 0x79, Some(modrm))
            if (modrm >> 6) == 3 && (last_rep == 0xf2 || (last_rep == 0 && has_66)) =>
        {
            let reg_field = ((modrm >> 3) & 7) | ((rex & 4) << 1);
            let rm_field = (modrm & 7) | ((rex & 1) << 3);
            match (op, last_rep == 0xf2) {
                (0x78, false) if ((modrm >> 3) & 7) == 0 => {
                    let len = *bytes.get(pos + 3)?;
                    let idx = *bytes.get(pos + 4)?;
                    Some((
                        ConsoleOnly::Extrq {
                            dst: rm_field,
                            src: None,
                            len,
                            idx,
                        },
                        pos + 5,
                    ))
                }
                (0x79, false) => Some((
                    ConsoleOnly::Extrq {
                        dst: reg_field,
                        src: Some(rm_field),
                        len: 0,
                        idx: 0,
                    },
                    pos + 3,
                )),
                (0x78, true) => {
                    let len = *bytes.get(pos + 3)?;
                    let idx = *bytes.get(pos + 4)?;
                    Some((
                        ConsoleOnly::Insertq {
                            dst: reg_field,
                            src: rm_field,
                            reg: false,
                            len,
                            idx,
                        },
                        pos + 5,
                    ))
                }
                (0x79, true) => Some((
                    ConsoleOnly::Insertq {
                        dst: reg_field,
                        src: rm_field,
                        reg: true,
                        len: 0,
                        idx: 0,
                    },
                    pos + 3,
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse4a_bitfield_semantics() {
        assert_eq!(extrq(0x1234_5678_9abc_def0, 8, 16), 0xbc);
        assert_eq!(extrq(0x1234_5678_9abc_def0, 0, 0), 0x1234_5678_9abc_def0);
        assert_eq!(extrq(0x8000_0000_0000_0000, 1, 63), 1);
        assert_eq!(insertq(u64::MAX, 0xab, 8, 8), 0xffff_ffff_ffff_abff);
        assert_eq!(insertq(0, 0x1ff, 4, 60), 0xf000_0000_0000_0000);
        assert_eq!(
            insertq(0x1111, 0xdead_beef_0000_0000, 0, 0),
            0xdead_beef_0000_0000
        );
    }

    #[test]
    fn decodes_zen2_and_sse4a_forms() {
        assert_eq!(decode(&[0x0f, 0x01, 0xfc]), Some((ConsoleOnly::Clzero, 3)));
        assert_eq!(
            decode(&[
                0x0f, 0x01, 0xfc, 0x48, 0x83, 0xc0, 0x40, 0x4c, 0x39, 0xd0, 0x72, 0xf4
            ]),
            Some((ConsoleOnly::ClzeroLoop { end_reg: 10 }, 12))
        );
        assert_eq!(
            decode(&[
                0x67, 0x0f, 0x01, 0xfc, 0x48, 0x83, 0xc0, 0x40, 0x48, 0x39, 0xd0, 0x72, 0xf4
            ]),
            Some((ConsoleOnly::Clzero, 4))
        );
        assert_eq!(decode(&[0x0f, 0x01, 0xfd]), Some((ConsoleOnly::Rdpru, 3)));
        assert_eq!(
            decode(&[0x0f, 0x01, 0xfa]),
            Some((ConsoleOnly::Monitorx, 3))
        );
        assert_eq!(decode(&[0x0f, 0x01, 0xfb]), Some((ConsoleOnly::Mwaitx, 3)));
        assert_eq!(decode(&[0x0f, 0x0b]), Some((ConsoleOnly::Ud2, 2)));
        assert_eq!(
            decode(&[0x66, 0x0f, 0xae, 0x30]),
            Some((ConsoleOnly::Clwb, 4))
        );
        assert_eq!(
            decode(&[0xf3, 0x41, 0x0f, 0xc7, 0xf9]),
            Some((ConsoleOnly::Rdpid(9), 5))
        );
        assert_eq!(
            decode(&[0xf2, 0x0f, 0x2b, 0x10]),
            Some((
                ConsoleOnly::Movnt {
                    double: true,
                    src_xmm: 2,
                    prefix_end: 3
                },
                4
            ))
        );
        assert_eq!(
            decode(&[0xf3, 0x44, 0x0f, 0x2b, 0x50, 0x04]),
            Some((
                ConsoleOnly::Movnt {
                    double: false,
                    src_xmm: 10,
                    prefix_end: 4
                },
                6
            ))
        );
    }
}
