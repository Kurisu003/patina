//! Scripted UI input for automated testing (`PINTA_SCRIPT=file`).
//! Each line is one command, executed one per frame:
//!   wait N                 skip N frames
//!   move X Y               pointer to screen point
//!   down [left|right|middle] / up [...]
//!   drag X0 Y0 X1 Y1 [N]   press at X0,Y0, move in N steps, release
//!   click X Y [right]
//!   key [ctrl+][shift+][alt+]NAME   (NAME = egui key name, e.g. Z, Enter)
//!   text STRING            type text
//!   scroll DY [ctrl]
//!   shot PATH.png          save a screenshot of the next frame
//!   log MESSAGE            print to stdout
//!   quit

use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2};
use std::collections::VecDeque;

pub struct Script {
    lines: VecDeque<String>,
    wait: u32,
    pos: Pos2,
    pending: VecDeque<Vec<Event>>,
}

fn button(s: Option<&str>) -> PointerButton {
    match s {
        Some("right") => PointerButton::Secondary,
        Some("middle") => PointerButton::Middle,
        _ => PointerButton::Primary,
    }
}

impl Script {
    pub fn load(path: &str) -> Option<Script> {
        let text = std::fs::read_to_string(path).ok()?;
        let lines = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        Some(Script { lines, wait: 10, pos: Pos2::new(1.0, 1.0), pending: VecDeque::new() })
    }

    pub fn step(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        ctx.request_repaint();
        if let Some(evs) = self.pending.pop_front() {
            raw.events.extend(evs);
            return;
        }
        if self.wait > 0 {
            self.wait -= 1;
            return;
        }
        let Some(line) = self.lines.pop_front() else { return };
        let mut it = line.splitn(2, ' ');
        let cmd = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("").trim();
        let nums: Vec<f32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        let words: Vec<&str> = rest.split_whitespace().collect();
        let mods_of = |w: &[&str]| {
            let mut m = Modifiers::NONE;
            if w.contains(&"ctrl") {
                m = m | Modifiers::COMMAND;
            }
            if w.contains(&"shift") {
                m = m | Modifiers::SHIFT;
            }
            m
        };
        match cmd {
            "wait" => self.wait = nums.first().copied().unwrap_or(1.0) as u32,
            "move" => {
                self.pos = Pos2::new(nums[0], nums[1]);
                raw.events.push(Event::PointerMoved(self.pos));
            }
            "down" | "up" => {
                raw.events.push(Event::PointerButton { pos: self.pos, button: button(words.first().copied()), pressed: cmd == "down", modifiers: mods_of(&words) });
            }
            "click" => {
                let p = Pos2::new(nums[0], nums[1]);
                let b = button(words.get(2).copied());
                let m = mods_of(&words);
                self.pos = p;
                raw.events.push(Event::PointerMoved(p));
                self.pending.push_back(vec![Event::PointerButton { pos: p, button: b, pressed: true, modifiers: m }]);
                self.pending.push_back(vec![Event::PointerButton { pos: p, button: b, pressed: false, modifiers: m }]);
            }
            "drag" => {
                let (x0, y0, x1, y1) = (nums[0], nums[1], nums[2], nums[3]);
                let n = nums.get(4).copied().unwrap_or(10.0).max(1.0) as usize;
                let b = button(words.iter().copied().find(|w| *w == "right" || *w == "middle"));
                let m = mods_of(&words);
                let p0 = Pos2::new(x0, y0);
                raw.events.push(Event::ModifiersChanged(m));
                raw.events.push(Event::PointerMoved(p0));
                self.pending.push_back(vec![Event::PointerButton { pos: p0, button: b, pressed: true, modifiers: m }]);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let p = Pos2::new(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                    self.pending.push_back(vec![Event::PointerMoved(p)]);
                }
                let p1 = Pos2::new(x1, y1);
                self.pending.push_back(vec![Event::PointerButton { pos: p1, button: b, pressed: false, modifiers: m }]);
                self.pending.push_back(vec![Event::ModifiersChanged(Modifiers::NONE)]);
                self.pos = p1;
            }
            "key" => {
                let parts: Vec<&str> = rest.split('+').collect();
                let name = parts.last().copied().unwrap_or("");
                let mut m = Modifiers::NONE;
                for p in &parts[..parts.len().saturating_sub(1)] {
                    match *p {
                        "ctrl" => m = m | Modifiers::COMMAND,
                        "shift" => m = m | Modifiers::SHIFT,
                        "alt" => m = m | Modifiers::ALT,
                        _ => {}
                    }
                }
                raw.events.push(Event::ModifiersChanged(m));
                if let Some(key) = Key::from_name(name) {
                    raw.events.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m });
                    self.pending.push_back(vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m }, Event::ModifiersChanged(Modifiers::NONE)]);
                } else {
                    println!("script: unknown key {name}");
                }
            }
            "text" => raw.events.push(Event::Text(rest.to_string())),
            "scroll" => {
                let m = mods_of(&words);
                raw.events.push(Event::ModifiersChanged(m));
                raw.events.push(Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, nums[0]), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: m });
                self.pending.push_back(vec![Event::ModifiersChanged(Modifiers::NONE)]);
            }
            "shot" => {
                let path = rest.to_string();
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let mut buf = Vec::with_capacity(w * h * 4);
                    for c in &img.pixels {
                        buf.extend_from_slice(&c.to_array());
                    }
                    if let Some(im) = image::RgbaImage::from_raw(w as u32, h as u32, buf) {
                        let _ = im.save(&path);
                        println!("script: saved {path}");
                    }
                });
                self.wait = 5;
            }
            "log" => println!("script: {rest}"),
            "quit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => println!("script: unknown command {line}"),
        }
    }
}
