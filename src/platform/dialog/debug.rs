//! The debug menu (Ctrl+Shift+D, only in a `--features cheats` build): the debug switches
//! (`platform::debug`) by page, each applied as soon as it changes. It is no modal dialog: the
//! game goes on running beneath it (changes are seen at once), while the window's input goes to
//! it instead of the game (`window.rs`). Drawn over the game's own dialogs (`overlay_image`).

use super::template::{Class, Item, Template};
use super::{Dialog, Kind, Msg, Notify, OV_H, OV_W};
use crate::platform::debug::{self, PAGES, VARS};
use std::sync::Mutex;

/// Our template id (no game dialog has it) and the window handle (no game dialog gets it).
const TEMPLATE: u32 = 0x7d00;
const HWND: u32 = 0x00da_ff00;
const PAGE: u32 = 0x7d01;
/// A switch's control: this + its index; a choice's or number's label: `LABEL` + its index.
const VALUE: u32 = 0x7d10;
const LABEL: u32 = 0x7d80;
const CLOSE: u32 = 2;
const W: i32 = 200;

static PANEL: Mutex<Option<(Dialog, usize)>> = Mutex::new(None);

pub fn open() -> bool {
    PANEL.lock().unwrap().is_some()
}

/// Ctrl+Shift+D: opens or closes it.
pub fn toggle() {
    let mut p = PANEL.lock().unwrap();
    if p.take().is_none() {
        let mut d = Dialog::new(HWND, TEMPLATE, 0, &template());
        init(&mut d);
        let mut page = 0;
        relayout(&mut d, &mut page);
        *p = Some((d, page));
    }
}

fn template() -> Template {
    let mut items = Vec::new();
    let mut add = |class, id, text: &str, style: u32, rect| items.push(Item { class, id, text: text.into(), style: 0x1000_0000 | 0x0001_0000 | style, ex_style: 0, rect });
    add(Class::Static, u32::MAX, "Page", 0, [7, 9, 50, 8]);
    add(Class::ComboBox, PAGE, "", 3, [60, 7, W - 67, 90]);
    for (i, v) in VARS.iter().enumerate() {
        let id = VALUE + i as u32;
        match v.kind {
            debug::Kind::Flag(_) => add(Class::Button, id, v.label, 3, [7, 0, W - 14, 9]),
            debug::Kind::Choice(_) => {
                add(Class::Static, LABEL + i as u32, v.label, 0, [7, 0, 90, 8]);
                add(Class::ComboBox, id, "", 3, [100, 0, W - 107, 90]);
            }
            debug::Kind::Num(..) => {
                add(Class::Static, LABEL + i as u32, v.label, 0, [7, 0, 90, 8]);
                add(Class::Edit, id, "", 0x80, [100, 0, W - 107, 12]);
            }
        }
    }
    add(Class::Button, CLOSE, "Close", 0, [W - 57, 0, 50, 14]);
    Template { style: 0x00c0_0000, title: "Debug (Ctrl+Shift+D)".into(), rect: [0, 0, W, 200], font: None, items }
}

/// The controls from the switches' values.
fn init(d: &mut Dialog) {
    if let Some(c) = d.control(PAGE) {
        if let Kind::Combo { items, sel, .. } = &mut c.kind {
            *items = PAGES.iter().enumerate().map(|(i, p)| (p.to_string(), i as u32)).collect();
            *sel = 0;
        }
        c.sync_combo();
    }
    for (i, v) in VARS.iter().enumerate() {
        let var = debug::ALL[i];
        let Some(c) = d.control(VALUE + i as u32) else { continue };
        match v.kind {
            debug::Kind::Flag(_) => c.check = debug::flag(var) as u32,
            debug::Kind::Choice(opts) => {
                if let Kind::Combo { items, sel, .. } = &mut c.kind {
                    *items = opts.iter().enumerate().map(|(k, o)| (o.1.to_string(), k as u32)).collect();
                    *sel = debug::get(var) as i32;
                }
                c.sync_combo();
            }
            debug::Kind::Num(..) => c.text = format!("{}", debug::num(var)),
        }
    }
}

/// Only the chosen page's rows shown, one below the other, Close below them; the panel at the
/// overlay's right edge (the picture stays visible beside it; the frame rate display is top left).
fn relayout(d: &mut Dialog, page: &mut usize) {
    *page = d.controls.iter().find(|c| c.id == PAGE).and_then(|c| match &c.kind {
        Kind::Combo { sel, .. } => Some(*sel as usize),
        _ => None,
    }).unwrap_or(0).min(PAGES.len() - 1);
    let sy = d.scale.1;
    let top = d.rect[1] + if d.caption { 14 } else { 0 };
    let at = |y: i32| top + (y as f32 * sy).round() as i32;
    let mut y = 24;
    for (i, v) in VARS.iter().enumerate() {
        let shown = v.page == *page;
        for (id, dy) in [(LABEL + i as u32, 2), (VALUE + i as u32, 0)] {
            if let Some(c) = d.control(id) {
                c.visible = shown;
                let h = c.rect[3] - c.rect[1];
                c.rect[1] = at(y + dy);
                c.rect[3] = c.rect[1] + h;
            }
        }
        if shown {
            y += 14;
        }
    }
    if let Some(c) = d.control(CLOSE) {
        let h = c.rect[3] - c.rect[1];
        c.rect[1] = at(y + 4);
        c.rect[3] = c.rect[1] + h;
    }
    d.rect[3] = at(y + 24);
    // at the right edge, from the top
    let (dx, dy) = (OV_W - 8 - d.rect[2], 8 - d.rect[1]);
    d.rect = [d.rect[0] + dx, d.rect[1] + dy, d.rect[2] + dx, (d.rect[3] + dy).min(OV_H - 8)];
    for c in &mut d.controls {
        c.rect = [c.rect[0] + dx, c.rect[1] + dy, c.rect[2] + dx, c.rect[3] + dy];
    }
    if d.focus.is_some_and(|k| !d.controls[k].visible) {
        d.focus = None;
    }
}

/// The switches from the controls (after every input).
fn apply(d: &Dialog) {
    for (i, v) in VARS.iter().enumerate() {
        let var = debug::ALL[i];
        let Some(k) = d.find(VALUE + i as u32) else { continue };
        let c = &d.controls[k];
        let value = match (&v.kind, &c.kind) {
            (debug::Kind::Flag(_), _) => Some(c.check),
            (debug::Kind::Choice(_), Kind::Combo { sel, .. }) => Some(*sel as u32),
            (debug::Kind::Num(..), _) => debug::num_from(var, &c.text),
            _ => None,
        };
        if let Some(x) = value.filter(|&x| x != debug::get(var)) {
            debug::set(var, x);
            crate::platform::log!("debug: {} = {}", v.env, describe(var));
        }
    }
}

fn describe(v: debug::V) -> String {
    match VARS[v as usize].kind {
        debug::Kind::Flag(_) => (debug::flag(v) as u32).to_string(),
        debug::Kind::Choice(_) => format!("{:?}", debug::choice(v)),
        debug::Kind::Num(..) => debug::num(v).to_string(),
    }
}

/// A window message (`msg`, `wp`, `lp` as the game would get it) for the panel. False when it
/// is closed (the message is the game's).
pub fn input(msg: u32, wp: u32, lp: u32) -> bool {
    let mut p = PANEL.lock().unwrap();
    let Some((d, page)) = p.as_mut() else { return false };
    let mut out: Vec<Notify> = Vec::new();
    d.input(&Msg { hwnd: HWND, msg, wp, lp }, &mut out);
    apply(d);
    let was = *page;
    relayout(d, page);
    if *page != was {
        d.focus = None;
    }
    // Esc or Close end it (as a dialog without a procedure, `result`); Enter only applies
    if msg == 0x100 && wp == 0x0d {
        d.result = None;
    }
    if d.result.is_some() || out.iter().any(|n| n.msg == super::WM_COMMAND && n.wp & 0xffff == CLOSE) {
        *p = None;
    }
    true
}

/// What the overlay shows: the game's dialogs and over them the panel (None: nothing).
pub fn overlay_image(dialogs: &[Dialog]) -> Option<Vec<u32>> {
    let p = PANEL.lock().unwrap();
    match p.as_ref() {
        None if dialogs.is_empty() => None,
        None => Some(super::draw::render(dialogs)),
        Some((d, _)) => {
            let mut all = dialogs.to_vec();
            all.push(d.clone());
            Some(super::draw::render(&all))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every page laid out: its rows shown, none overlapping, all inside the overlay;
    /// `FCOP_DLG_PNG=<dir>` writes each as a picture.
    #[test]
    fn pages() {
        for p in 0..PAGES.len() {
            let mut d = Dialog::new(HWND, TEMPLATE, 0, &template());
            init(&mut d);
            if let Some(c) = d.control(PAGE)
                && let Kind::Combo { sel, .. } = &mut c.kind
            {
                *sel = p as i32;
            }
            d.control(PAGE).unwrap().sync_combo();
            let mut page = 0;
            relayout(&mut d, &mut page);
            assert_eq!(page, p);
            let shown = VARS.iter().enumerate().filter(|(_, v)| v.page == p).count();
            assert!(shown > 0);
            let vis: Vec<_> = d
                .controls
                .iter()
                .filter(|c| c.visible)
                .map(|c| {
                    let mut r = c.rect;
                    if matches!(c.kind, Kind::Combo { .. }) {
                        r[3] = r[3].min(r[1] + 14);
                    }
                    (c.id, r)
                })
                .collect();
            for (k, (a, ra)) in vis.iter().enumerate() {
                assert!(ra[0] >= d.rect[0] && ra[2] <= d.rect[2] && ra[3] <= d.rect[3], "page {p}: {a:#x} {ra:?} outside {:?}", d.rect);
                for (b, rb) in &vis[k + 1..] {
                    let overlap = ra[0] < rb[2] && rb[0] < ra[2] && ra[1] < rb[3] && rb[1] < ra[3];
                    assert!(!overlap, "page {p}: {a:#x} {ra:?} overlaps {b:#x} {rb:?}");
                }
            }
            assert!(d.rect[3] <= OV_H);
            if let Ok(dir) = std::env::var("FCOP_DLG_PNG") {
                let px = super::super::draw::render(&[d]);
                let rgba: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
                let f = std::fs::File::create(format!("{dir}/debug_{p}.png")).unwrap();
                let mut e = png::Encoder::new(std::io::BufWriter::new(f), 640, 480);
                e.set_color(png::ColorType::Rgba);
                e.write_header().unwrap().write_image_data(&rgba).unwrap();
            }
        }
    }
}
