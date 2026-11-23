//! The PlayStation version's menu music, which the PC version left out. The player's PS1 disc
//! holds it in `CW/FE.MIS` (the front end), a mission file in the PC's chunk format: a `SWVR`
//! chunk names the track (`MenuTune1_sw14`), then `VAGM` chunks carry it, each: 8 bytes,
//! `VAGM` (reversed, as every tag), u16 block count, u16 block number, u32 bytes per channel,
//! then the left channel's bytes and the right's. They are PS-ADPCM (the PlayStation sound
//! chip's format): frames of 16 bytes, 28 samples each. It plays at 14260 Hz (the PC's mission
//! music's rate; checked by ear).
//!
//! `setup` copies FE.MIS from a PS1 disc image (or the disc's files) into `game_files/PS1`.

/// The music's sample rate.
pub const RATE: u32 = 14260;

/// The file `setup` keeps, in `game_files/PS1`.
pub const FILE: &str = "FE.MIS";

/// The prediction filters (coefficients / 64).
const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// One channel's decoder: the last two samples carry from frame to frame (and block to block).
#[derive(Default)]
struct Adpcm {
    s1: i32,
    s2: i32,
}

impl Adpcm {
    /// Decode whole 16-byte frames into `out`.
    fn decode(&mut self, data: &[u8], out: &mut Vec<i16>) {
        for f in data.chunks_exact(16) {
            let shift = (f[0] & 15) as u32;
            let (a, b) = FILTERS[((f[0] >> 4) as usize).min(4)];
            for i in 0..28 {
                let nibble = ((f[2 + i / 2] >> (i % 2 * 4)) & 15) as i32;
                // the nibble is the top of a 16-bit value, shifted down
                let raw = ((nibble << 28) >> 16) >> shift.min(12);
                let v = (raw + ((self.s1 * a + self.s2 * b + 32) >> 6)).clamp(-32768, 32767);
                self.s2 = self.s1;
                self.s1 = v;
                out.push(v as i16);
            }
        }
    }
}

fn u16_at(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

/// The menu music in FE.MIS: stereo frames (left, right), or None when it holds none.
pub fn menu_music(file: &[u8]) -> Option<Vec<[i16; 2]>> {
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut l, mut r) = (Adpcm::default(), Adpcm::default());
    let mut next = 0;
    let mut i = 0;
    while i + 8 <= file.len() {
        let size = u32_at(file, i + 4) as usize;
        if size < 8 || i + size > file.len() {
            break;
        }
        let c = &file[i..i + size];
        // tags are stored reversed
        if &c[..4] == b"MGAV" && size >= 28 {
            let (block, per) = (u16_at(c, 22), u32_at(c, 24) as usize);
            let data = &c[28..];
            // a second track or a gap: only the first, whole
            if block != next || data.len() < 2 * per {
                break;
            }
            next += 1;
            l.decode(&data[..per], &mut left);
            r.decode(&data[per..2 * per], &mut right);
        }
        i += size;
    }
    (!left.is_empty()).then(|| left.into_iter().zip(right).map(|(a, b)| [a, b]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adpcm_frame() {
        // filter 1, shift 12: nibbles are their own value; then 60/64 of the last sample is added
        let mut f = [0u8; 16];
        f[0] = 0x1c;
        f[2] = 0x71; // 1, then 7
        f[3] = 0x0f; // -1, then 0
        let mut out = Vec::new();
        Adpcm::default().decode(&f, &mut out);
        assert_eq!(out.len(), 28);
        // 1; 7 + 60/64 * 1; -1 + 60/64 * 8; 0 + 60/64 * 7 (rounded)
        assert_eq!(&out[..4], &[1, 8, 7, 7]);
    }

    #[test]
    fn blocks_in_order() {
        // two blocks of one frame a channel, block numbers 0 and 1
        let mut file = Vec::new();
        for n in 0..2u16 {
            let mut c = b"MGAV".to_vec();
            c.extend_from_slice(&(28u32 + 32).to_le_bytes());
            c.extend_from_slice(b"\0\0\0\0\0\0\0\0MGAV");
            c.extend_from_slice(&2u16.to_le_bytes());
            c.extend_from_slice(&n.to_le_bytes());
            c.extend_from_slice(&16u32.to_le_bytes());
            let mut frame = [0u8; 16];
            frame[0] = 0x0c;
            frame[2] = 1 + n as u8;
            c.extend_from_slice(&frame);
            frame[2] = 0x0f;
            c.extend_from_slice(&frame);
            file.extend_from_slice(&c);
        }
        let m = menu_music(&file).unwrap();
        assert_eq!(m.len(), 56);
        assert_eq!(m[0], [1, -1]);
        assert_eq!(m[28], [2, -1]);
    }
}
