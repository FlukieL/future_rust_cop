//! Controller binding glyphs from the supplied Xbox and PlayStation atlases.
use crate::platform::advapi32;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

static STYLE: AtomicU8 = AtomicU8::new(255);

/// 0: automatic, 1: Xbox, 2: PlayStation.
pub fn style() -> u32 {
    let current = STYLE.load(Ordering::Relaxed);
    if current != 255 {
        return current as u32;
    }
    let value = advapi32::value("", "ControllerGlyphs")
        .and_then(|(_, data)| Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?)))
        .filter(|&v| v <= 2)
        .unwrap_or(0);
    STYLE.store(value as u8, Ordering::Relaxed);
    value
}

pub fn set_style(value: u32) {
    let value = value.min(2);
    STYLE.store(value as u8, Ordering::Relaxed);
    advapi32::set_value("", "ControllerGlyphs", 4, &value.to_le_bytes());
}

fn decode(bytes: &'static [u8]) -> Option<Vec<u8>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    if info.width != 512 || info.height != 640 || info.color_type != png::ColorType::Rgba {
        return None;
    }
    pixels.truncate(info.buffer_size());
    Some(pixels)
}

pub fn atlas(playstation: bool) -> Option<&'static [u8]> {
    static XBOX: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    static PLAYSTATION: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    if playstation {
        PLAYSTATION.get_or_init(|| decode(include_bytes!("../../../data/controller_glyphs/atlases/playstation_atlas.png"))).as_deref()
    } else {
        XBOX.get_or_init(|| decode(include_bytes!("../../../data/controller_glyphs/atlases/xbox_atlas.png"))).as_deref()
    }
}

/// Atlas cell for a binding label. Legacy and user-defined labels remain plain text.
pub fn cell(text: &str) -> Option<usize> {
    let (device, name) = text.split_once(':')?;
    if !device.trim().eq_ignore_ascii_case("Joystick") {
        return None;
    }
    let name = name.trim();
    Some(match name {
        "Pad A" => 0, "Pad B" => 1, "Pad X" => 2, "Pad Y" => 3,
        "Pad LB" => 4, "Pad RB" => 5, "Pad LT" => 6, "Pad RT" => 7,
        "Pad Up" => 8, "Pad Down" => 9, "Pad Left" => 10, "Pad Right" => 11,
        "Left" | "Right" | "Up" | "Down" => 12,
        "RStick L" | "RStick R" | "RStick U" | "RStick D" => 13,
        "Pad L3" => 14, "Pad R3" => 15, "Pad Back" => 16,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn atlases_and_binding_cells() {
        assert_eq!(super::atlas(false).unwrap().len(), 512 * 640 * 4);
        assert_eq!(super::atlas(true).unwrap().len(), 512 * 640 * 4);
        assert_eq!(super::cell("Joystick: Pad A"), Some(0));
        assert_eq!(super::cell("Joystick: Pad RT"), Some(7));
        assert_eq!(super::cell("Keyboard: A"), None);
    }
}
