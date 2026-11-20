use crate::{desktop, network::ViewState};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    pub key: String,
    pub modifiers: Vec<String>,
}

impl Shortcut {
    pub fn label(&self) -> String {
        let prefix = self
            .modifiers
            .iter()
            .map(|s| match s.as_str() {
                "cmd" => "⌘",
                "ctrl" => "⌃",
                "alt" => "⌥",
                "shift" => "⇧",
                _ => "",
            })
            .collect::<String>();
        let key = match self.key.as_str() {
            "cmd" => "⌘",
            "ctrl" => "⌃",
            "alt" => "⌥",
            "shift" => "⇧",
            "ArrowLeft" => "←",
            "ArrowRight" => "→",
            "ArrowUp" => "↑",
            "ArrowDown" => "↓",
            "Space" => "空格",
            "Enter" => "↩",
            "Escape" => "Esc",
            "Backspace" => "⌫",
            key => key
                .strip_prefix("Key")
                .or_else(|| key.strip_prefix("Digit"))
                .unwrap_or(key),
        };
        format!("{prefix}{key}")
    }
    fn keys(&self) -> Vec<u16> {
        self.modifiers
            .iter()
            .filter_map(|key| key_code(key))
            .chain(key_code(&self.key))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    None,
    Shortcut(Shortcut),
    Hold(Shortcut),
    AppSwitcher,
    MouseHold,
    RightMouseHold,
    Pointer,
    Scroll,
    Faster,
    Slower,
}
impl Action {
    pub fn label(&self) -> String {
        match self {
            Self::None => "未映射".into(),
            Self::Shortcut(s) => s.label(),
            Self::Hold(s) => format!("保持 {}", s.label()),
            Self::AppSwitcher => "按住切换应用".into(),
            Self::MouseHold => "保持鼠标左键".into(),
            Self::RightMouseHold => "保持鼠标右键".into(),
            Self::Pointer => "移动指针".into(),
            Self::Scroll => "滚动".into(),
            Self::Faster => "指针与滚动加速".into(),
            Self::Slower => "精细控制".into(),
        }
    }
}

pub const INPUTS: &[(&str, &str)] = &[
    ("l1", "L1"),
    ("r1", "R1"),
    ("l2", "L2"),
    ("r2", "R2"),
    ("left_stick", "左摇杆 · 推动"),
    ("right_stick", "右摇杆 · 推动"),
    ("l3", "L3 · 按下"),
    ("r3", "R3 · 按下"),
    ("dpad_left", "十字键 ←"),
    ("dpad_right", "十字键 →"),
    ("dpad_up", "十字键 ↑"),
    ("dpad_down", "十字键 ↓"),
    ("a", "A"),
    ("b", "B"),
    ("x", "X"),
    ("y", "Y"),
    ("options", "Options"),
    ("menu", "Menu"),
    ("capture", "Capture"),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub enabled: bool,
    pub bindings: BTreeMap<String, Action>,
    pub deadzone: f64,
    pub speed: f64,
    pub scroll_speed: f64,
    pub reverse_scroll: bool,
}
impl Default for Config {
    fn default() -> Self {
        let mut bindings = BTreeMap::new();
        for (input, action) in [
            ("l1", Action::AppSwitcher),
            ("x", Action::MouseHold),
            ("y", Action::RightMouseHold),
            (
                "a",
                Action::Shortcut(Shortcut {
                    key: "Enter".into(),
                    modifiers: vec![],
                }),
            ),
            (
                "b",
                Action::Shortcut(Shortcut {
                    key: "Escape".into(),
                    modifiers: vec![],
                }),
            ),
            ("left_stick", Action::Pointer),
            ("right_stick", Action::Scroll),
            ("l2", Action::Faster),
            ("r2", Action::Slower),
            (
                "dpad_left",
                Action::Shortcut(Shortcut {
                    key: "ArrowLeft".into(),
                    modifiers: vec!["ctrl".into()],
                }),
            ),
            (
                "dpad_right",
                Action::Shortcut(Shortcut {
                    key: "ArrowRight".into(),
                    modifiers: vec!["ctrl".into()],
                }),
            ),
        ] {
            bindings.insert(input.into(), action);
        }
        Self {
            version: 2,
            enabled: false,
            bindings,
            deadzone: 0.15,
            speed: 900.0,
            scroll_speed: 650.0,
            reverse_scroll: false,
        }
    }
}

fn path() -> PathBuf {
    directories::ProjectDirs::from("dev", "stadiawave", "companion")
        .unwrap()
        .data_dir()
        .join("button-mapping.json")
}
pub fn load() -> (Config, String) {
    match std::fs::read(path()) {
        Ok(data) => match serde_json::from_slice::<Config>(&data).and_then(|mut c| {
            upgrade(&mut c);
            validate(&c).map_err(serde::de::Error::custom)?;
            Ok(c)
        }) {
            Ok(config) => (config, String::new()),
            Err(error) => (
                Config::default(),
                format!("映射配置读取失败，已载入默认预设。保存后替换原配置。{error}"),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (Config::default(), String::new())
        }
        Err(error) => (Config::default(), format!("无法读取映射配置。{error}")),
    }
}
fn upgrade(config: &mut Config) {
    if config.version != 1 {
        return;
    }
    let defaults = Config::default();
    let mut old_bindings = defaults.bindings.clone();
    for key in ["x", "y", "a", "b"] {
        old_bindings.remove(key);
    }
    old_bindings.insert("r1".into(), Action::MouseHold);
    // Upgrade the previous preset as a whole. Custom mappings retain their
    // existing actions, and can opt into the new layout with Restore preset.
    if config.bindings == old_bindings {
        config.bindings = defaults.bindings;
    }
    config.version = 2;
}

fn validate(config: &Config) -> Result<()> {
    ensure!(config.version == 2, "不支持的映射配置版本");
    ensure!(
        config.deadzone.is_finite() && (0.05..=0.5).contains(&config.deadzone),
        "死区应在 0.05 到 0.5 之间"
    );
    ensure!(
        config.speed.is_finite() && (100.0..=3000.0).contains(&config.speed),
        "指针速度应在 100 到 3000 之间"
    );
    ensure!(
        config.scroll_speed.is_finite() && (50.0..=2000.0).contains(&config.scroll_speed),
        "滚动速度应在 50 到 2000 之间"
    );
    for (input, action) in &config.bindings {
        ensure!(INPUTS.iter().any(|(id, _)| id == input), "未知手柄输入");
        match action {
            Action::Shortcut(s) | Action::Hold(s) => {
                ensure!(key_code(&s.key).is_some(), "不支持的键盘按键 {}", s.key);
                ensure!(
                    s.modifiers
                        .iter()
                        .all(|m| ["cmd", "ctrl", "alt", "shift"].contains(&m.as_str())),
                    "无效修饰键"
                );
            }
            Action::Pointer | Action::Scroll => {
                ensure!(input.ends_with("_stick"), "此动作需要摇杆")
            }
            Action::Faster | Action::Slower => {
                ensure!(input == "l2" || input == "r2", "此动作需要扳机")
            }
            _ => ensure!(!input.ends_with("_stick"), "请选择摇杆动作"),
        }
    }
    Ok(())
}
pub fn save(config: Config) -> Result<()> {
    validate(&config)?;
    let path = path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&config)?)?;
    std::fs::rename(temporary, path)?;
    let mut engine = engine().lock().unwrap();
    engine.reset();
    log::info!("MAPPING configuration saved, enabled={}", config.enabled);
    engine.config = config;
    Ok(())
}

pub fn key_code(key: &str) -> Option<u16> {
    let code = match key {
        "a" | "KeyA" => 0,
        "s" | "KeyS" => 1,
        "d" | "KeyD" => 2,
        "f" | "KeyF" => 3,
        "h" | "KeyH" => 4,
        "g" | "KeyG" => 5,
        "z" | "KeyZ" => 6,
        "x" | "KeyX" => 7,
        "c" | "KeyC" => 8,
        "v" | "KeyV" => 9,
        "b" | "KeyB" => 11,
        "q" | "KeyQ" => 12,
        "w" | "KeyW" => 13,
        "e" | "KeyE" => 14,
        "r" | "KeyR" => 15,
        "y" | "KeyY" => 16,
        "t" | "KeyT" => 17,
        "Digit1" | "1" => 18,
        "Digit2" | "2" => 19,
        "Digit3" | "3" => 20,
        "Digit4" | "4" => 21,
        "Digit6" | "6" => 22,
        "Digit5" | "5" => 23,
        "Equal" => 24,
        "Digit9" | "9" => 25,
        "Digit7" | "7" => 26,
        "Minus" => 27,
        "Digit8" | "8" => 28,
        "Digit0" | "0" => 29,
        "BracketRight" => 30,
        "o" | "KeyO" => 31,
        "u" | "KeyU" => 32,
        "BracketLeft" => 33,
        "i" | "KeyI" => 34,
        "p" | "KeyP" => 35,
        "Enter" => 36,
        "l" | "KeyL" => 37,
        "j" | "KeyJ" => 38,
        "Quote" => 39,
        "k" | "KeyK" => 40,
        "Semicolon" => 41,
        "Backslash" => 42,
        "Comma" => 43,
        "Slash" => 44,
        "n" | "KeyN" => 45,
        "m" | "KeyM" => 46,
        "Period" => 47,
        "Tab" => 48,
        "Space" | " " => 49,
        "Backquote" => 50,
        "Backspace" => 51,
        "Escape" => 53,
        "cmd" => 55,
        "shift" => 56,
        "alt" => 58,
        "ctrl" => 59,
        "NumpadDecimal" => 65,
        "NumpadMultiply" => 67,
        "NumpadAdd" => 69,
        "NumLock" | "Clear" => 71,
        "NumpadDivide" => 75,
        "NumpadEnter" => 76,
        "NumpadSubtract" => 78,
        "NumpadEqual" => 81,
        "Numpad0" => 82,
        "Numpad1" => 83,
        "Numpad2" => 84,
        "Numpad3" => 85,
        "Numpad4" => 86,
        "Numpad5" => 87,
        "Numpad6" => 88,
        "Numpad7" => 89,
        "Numpad8" => 91,
        "Numpad9" => 92,
        "F13" => 105,
        "F14" => 107,
        "F15" => 113,
        "F16" => 106,
        "F17" => 64,
        "F18" => 79,
        "F19" => 80,
        "F20" => 90,
        "IntlBackslash" => 10,
        "IntlYen" => 93,
        "IntlRo" => 94,
        "F1" => 122,
        "F2" => 120,
        "F3" => 99,
        "F4" => 118,
        "F5" => 96,
        "F6" => 97,
        "F7" => 98,
        "F8" => 100,
        "F9" => 101,
        "F10" => 109,
        "F11" => 103,
        "F12" => 111,
        "ArrowLeft" => 123,
        "ArrowRight" => 124,
        "ArrowDown" => 125,
        "ArrowUp" => 126,
        "Delete" => 117,
        "Home" => 115,
        "End" => 119,
        "PageUp" => 116,
        "PageDown" => 121,
        _ => return None,
    };
    Some(code)
}

struct Engine {
    config: Config,
    paused: bool,
    armed: bool,
    owner: Option<(String, String)>,
    held: BTreeSet<String>,
    outputs: BTreeSet<u16>,
    mouse: [bool; 2],
    switcher: Option<String>,
    cancelled: bool,
    axes: [i32; 4],
    triggers: [u8; 2],
    last_tick: Instant,
    scroll: [f64; 2],
    repeat: Option<(String, Instant)>,
}
static STARTED: AtomicBool = AtomicBool::new(false);

fn engine() -> &'static Mutex<Engine> {
    static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();
    ENGINE.get_or_init(|| Mutex::new(Engine::new(load().0)))
}

pub fn pause(paused: bool) {
    let mut engine = engine().lock().unwrap();
    if engine.paused != paused {
        engine.reset();
        engine.paused = paused;
    }
}
pub fn release() {
    engine().lock().unwrap().reset();
}
pub fn input(
    session: &str,
    controller: &str,
    held: &[String],
    axes: Option<[i32; 4]>,
    triggers: Option<[u8; 2]>,
    voice: bool,
) {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let mut engine = engine().lock().unwrap();
    let owner = (session.to_owned(), controller.to_owned());
    if engine.owner.as_ref() != Some(&owner) {
        engine.reset();
        engine.owner = Some(owner);
    }
    if let Some(axes) = axes {
        engine.axes = axes;
    }
    if let Some(triggers) = triggers {
        engine.triggers = triggers;
    }
    engine.input(held, voice, desktop::authorized());
}

impl Engine {
    fn new(config: Config) -> Self {
        Self {
            config,
            paused: false,
            armed: false,
            owner: None,
            held: BTreeSet::new(),
            outputs: BTreeSet::new(),
            mouse: [false; 2],
            switcher: None,
            cancelled: false,
            axes: [0; 4],
            triggers: [0; 2],
            last_tick: Instant::now(),
            scroll: [0.0; 2],
            repeat: None,
        }
    }

    fn reset(&mut self) {
        if self.switcher.is_some() {
            self.tap(53);
        }
        for key in std::mem::take(&mut self.outputs) {
            output::mapping_key(key, false, &BTreeSet::new());
        }
        for (button, held) in self.mouse.into_iter().enumerate() {
            if held {
                output::mapping_mouse_button(button, false);
            }
        }
        self.mouse = [false; 2];
        self.held.clear();
        self.switcher = None;
        self.cancelled = false;
        self.armed = false;
        self.axes = [0; 4];
        self.triggers = [0; 2];
        self.scroll = [0.0; 2];
        self.repeat = None;
        self.last_tick = Instant::now();
    }
    fn tap(&self, code: u16) {
        output::mapping_key(code, true, &self.outputs);
        if !self.outputs.contains(&code) {
            output::mapping_key(code, false, &self.outputs);
        }
    }
    fn sync_keys(&mut self, wanted: BTreeSet<u16>) {
        for key in self.outputs.difference(&wanted) {
            output::mapping_key(*key, false, &wanted);
        }
        for key in wanted.difference(&self.outputs) {
            output::mapping_key(*key, true, &wanted);
        }
        self.outputs = wanted;
    }
    fn input(&mut self, held: &[String], voice: bool, authorized: bool) {
        let mut next: BTreeSet<String> = held.iter().map(|s| s.to_ascii_lowercase()).collect();
        for (index, key) in ["l2", "r2"].iter().enumerate() {
            if self.triggers[index] > 128 {
                next.insert((*key).into());
            }
        }
        if !self.config.enabled || self.paused || voice || next.contains("assistant") || !authorized
        {
            self.reset();
            return;
        }
        if !self.armed {
            self.armed = next.is_empty()
                && self.axes.iter().all(|v| v.abs() < 5000)
                && self.triggers.iter().all(|v| *v < 20);
            self.held = next;
            return;
        }
        let pressed: Vec<_> = next.difference(&self.held).cloned().collect();
        if self
            .switcher
            .as_ref()
            .is_some_and(|key| !next.contains(key))
        {
            self.switcher = None;
            self.cancelled = false;
        }
        let actions = self.config.bindings.clone();
        let entering = pressed
            .iter()
            .find(|key| actions.get(*key) == Some(&Action::AppSwitcher))
            .cloned();
        if let Some(key) = entering {
            self.switcher = Some(key);
            self.cancelled = false;
        }
        let mut wanted = BTreeSet::new();
        let mut mouse = [false; 2];
        for key in &next {
            match actions.get(key) {
                Some(Action::Hold(shortcut)) => wanted.extend(shortcut.keys()),
                Some(Action::MouseHold) => mouse[0] = true,
                Some(Action::RightMouseHold) => mouse[1] = true,
                _ => {}
            }
        }
        if self.switcher.is_some() && !self.cancelled {
            wanted.insert(55);
        }
        self.sync_keys(wanted);
        for button in 0..2 {
            if mouse[button] != self.mouse[button] {
                output::mapping_mouse_button(button, mouse[button]);
            }
        }
        self.mouse = mouse;
        if pressed
            .iter()
            .any(|key| actions.get(key) == Some(&Action::AppSwitcher))
        {
            self.tap(48);
        }
        for key in &pressed {
            if self.switcher.is_some() && key.starts_with("dpad_") {
                if self.cancelled {
                    continue;
                }
                match key.as_str() {
                    "dpad_down" => {
                        self.tap(53);
                        self.cancelled = true;
                        let held_outputs = next
                            .iter()
                            .filter_map(|input| match actions.get(input) {
                                Some(Action::Hold(shortcut)) => Some(shortcut.keys()),
                                _ => None,
                            })
                            .flatten()
                            .collect();
                        self.sync_keys(held_outputs);
                    }
                    "dpad_left" => {
                        self.tap(123);
                        self.repeat =
                            Some((key.clone(), Instant::now() + Duration::from_millis(450)));
                    }
                    "dpad_right" => {
                        self.tap(124);
                        self.repeat =
                            Some((key.clone(), Instant::now() + Duration::from_millis(450)));
                    }
                    _ => {}
                }
            } else if let Some(Action::Shortcut(shortcut)) = actions.get(key) {
                let before = self.outputs.clone();
                let mut wanted = before.clone();
                wanted.extend(shortcut.modifiers.iter().filter_map(|s| key_code(s)));
                self.sync_keys(wanted);
                if let Some(code) = key_code(&shortcut.key) {
                    self.tap(code);
                }
                self.sync_keys(before);
            }
        }
        if self
            .repeat
            .as_ref()
            .is_some_and(|(key, _)| !next.contains(key))
            || self.switcher.is_none()
            || self.cancelled
        {
            self.repeat = None;
        }
        self.held = next;
    }
    fn tick(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        if dt > 0.25 {
            self.reset();
            return;
        }
        if !self.armed || self.paused || !self.config.enabled {
            return;
        }
        if let Some((key, deadline)) = self.repeat.clone() {
            if now >= deadline {
                self.tap(if key == "dpad_left" { 123 } else { 124 });
                self.repeat = Some((key, now + Duration::from_millis(100)));
            }
        }
        let multiplier = speed_multiplier(&self.config, self.triggers);
        for (index, name) in [(0, "left_stick"), (2, "right_stick")] {
            let (x, y) = velocity(self.axes[index], self.axes[index + 1], self.config.deadzone);
            match self.config.bindings.get(name) {
                Some(Action::Pointer) => output::mapping_move(
                    x * self.config.speed * dt * multiplier,
                    y * self.config.speed * dt * multiplier,
                    self.mouse,
                ),
                Some(Action::Scroll) => {
                    let sign = if self.config.reverse_scroll {
                        1.0
                    } else {
                        -1.0
                    };
                    self.scroll[0] += x * self.config.scroll_speed * dt * multiplier * sign;
                    self.scroll[1] += y * self.config.scroll_speed * dt * multiplier * sign;
                    let dx = self.scroll[0].trunc() as i32;
                    let dy = self.scroll[1].trunc() as i32;
                    self.scroll[0] -= dx as f64;
                    self.scroll[1] -= dy as f64;
                    output::mapping_scroll(dx, dy);
                }
                _ => {}
            }
        }
    }
}
pub fn speed_multiplier(config: &Config, triggers: [u8; 2]) -> f64 {
    let mut multiplier: f64 = 1.0;
    for (index, name) in ["l2", "r2"].iter().enumerate() {
        let amount = triggers[index] as f64 / 255.0;
        if config.bindings.get(*name) == Some(&Action::Faster) {
            multiplier *= 1.0 + amount * 2.0;
        }
    }
    for (index, name) in ["l2", "r2"].iter().enumerate() {
        let amount = triggers[index] as f64 / 255.0;
        if config.bindings.get(*name) == Some(&Action::Slower) && amount > 0.02 {
            multiplier = 1.0 - amount * 0.8;
        }
    }
    multiplier
}

pub fn velocity(x: i32, y: i32, deadzone: f64) -> (f64, f64) {
    let x = x as f64 / 32768.0;
    let y = -(y as f64) / 32768.0;
    let length = x.hypot(y);
    if length <= deadzone {
        return (0.0, 0.0);
    }
    let speed = ((length.min(1.0) - deadzone) / (1.0 - deadzone)).powf(1.8);
    (x / length * speed, y / length * speed)
}

pub fn start(state: Arc<Mutex<ViewState>>, dictation: crate::dictation::Dictation) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let state = state.lock().unwrap().clone();
            let voice = dictation.mapping_blocked() || state.recording.is_some();
            let mut engine = engine().lock().unwrap();
            let valid_owner = engine.owner.as_ref().is_some_and(|(session, controller)| {
                state.session_id.as_ref() == Some(session)
                    && state
                        .hardware
                        .controllers
                        .iter()
                        .any(|c| &c.id == controller && c.ready)
            });
            if !state.connected || !valid_owner || voice || !desktop::authorized() {
                engine.reset();
            } else {
                engine.tick(Instant::now());
            }
        }
    });
}

#[cfg(not(test))]
mod output {
    pub use crate::desktop::{mapping_key, mapping_mouse_button, mapping_move, mapping_scroll};
}
#[cfg(test)]
mod output {
    use std::{cell::RefCell, collections::BTreeSet};
    thread_local! {
        pub static KEYS: RefCell<Vec<(u16,bool,BTreeSet<u16>)>> = const { RefCell::new(Vec::new()) };
        pub static MOUSE: RefCell<Vec<(usize,bool)>> = const { RefCell::new(Vec::new()) };
        pub static SCROLL: RefCell<Vec<(i32,i32)>> = const { RefCell::new(Vec::new()) };
    }
    pub fn mapping_key(key: u16, down: bool, flags: &BTreeSet<u16>) {
        KEYS.with_borrow_mut(|events| events.push((key, down, flags.clone())));
    }
    pub fn mapping_mouse_button(button: usize, down: bool) {
        MOUSE.with_borrow_mut(|events| events.push((button, down)));
    }
    pub fn mapping_move(_: f64, _: f64, _: [bool; 2]) {}
    pub fn mapping_scroll(x: i32, y: i32) {
        SCROLL.with_borrow_mut(|events| events.push((x, y)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn active() -> Engine {
        let mut config = Config::default();
        config.enabled = true;
        let mut engine = Engine::new(config);
        engine.input(&[], false, true);
        output::KEYS.with_borrow_mut(Vec::clear);
        output::MOUSE.with_borrow_mut(Vec::clear);
        output::SCROLL.with_borrow_mut(Vec::clear);
        engine
    }
    fn send(engine: &mut Engine, keys: &[&str]) {
        engine.input(
            &keys.iter().map(|key| key.to_string()).collect::<Vec<_>>(),
            false,
            true,
        );
    }
    #[test]
    fn switcher_holds_command_until_release_and_cancels_safely() {
        let mut engine = active();
        send(&mut engine, &["l1"]);
        assert!(engine.outputs.contains(&55));
        output::KEYS.with_borrow(|events| {
            assert!(
                events
                    .iter()
                    .any(|(key, down, flags)| *key == 48 && *down && flags.contains(&55))
            );
            assert!(!events.iter().any(|(key, down, _)| *key == 55 && !down));
        });
        send(&mut engine, &["l1", "dpad_left"]);
        assert!(engine.repeat.is_some());
        send(&mut engine, &[]);
        assert!(engine.outputs.is_empty());
        assert!(engine.repeat.is_none());
        send(&mut engine, &["l1"]);
        send(&mut engine, &["l1", "dpad_down"]);
        assert!(engine.outputs.is_empty());
        assert!(engine.cancelled);
    }
    #[test]
    fn reset_releases_keys_and_requires_neutral_before_resuming() {
        let mut engine = active();
        send(&mut engine, &["l1", "x"]);
        assert!(engine.mouse[0]);
        engine.reset();
        assert_eq!(engine.mouse, [false; 2]);
        assert!(engine.outputs.is_empty());
        send(&mut engine, &["l1"]);
        assert!(engine.outputs.is_empty());
        send(&mut engine, &[]);
        send(&mut engine, &["l1"]);
        assert!(engine.outputs.contains(&55));
        engine.input(&["l1".into()], true, true);
        assert!(engine.outputs.is_empty() && !engine.armed);
    }
    #[test]
    fn shared_modifier_stays_held_when_one_binding_releases() {
        let mut engine = active();
        let hold = Action::Hold(Shortcut {
            key: "cmd".into(),
            modifiers: vec![],
        });
        engine.config.bindings.insert("x".into(), hold.clone());
        engine.config.bindings.insert("y".into(), hold);
        send(&mut engine, &["x", "y"]);
        send(&mut engine, &["y"]);
        assert!(engine.outputs.contains(&55));
        send(&mut engine, &[]);
        assert!(engine.outputs.is_empty());
    }
    #[test]
    fn mouse_buttons_release_independently_and_on_voice() {
        let mut engine = active();
        send(&mut engine, &["x", "y"]);
        assert_eq!(engine.mouse, [true, true]);
        send(&mut engine, &["y"]);
        assert_eq!(engine.mouse, [false, true]);
        engine.input(&["y".into()], true, true);
        assert_eq!(engine.mouse, [false, false]);
        output::MOUSE.with_borrow(|events| {
            assert_eq!(*events, vec![(0, true), (1, true), (0, false), (1, false)])
        });
    }
    #[test]
    fn voice_confirmation_cannot_leak_into_enter_or_escape() {
        for (input, code) in [("a", 36), ("b", 53)] {
            let mut engine = active();
            engine.input(&[input.into()], true, true);
            send(&mut engine, &[input]);
            output::KEYS.with_borrow(|events| assert!(events.is_empty()));
            send(&mut engine, &[]);
            send(&mut engine, &[input]);
            output::KEYS.with_borrow(|events| {
                assert!(events.iter().any(|(key, down, _)| *key == code && *down))
            });
        }
    }
    #[test]
    fn previous_default_layout_migrates_but_custom_bindings_survive() {
        let mut config = Config::default();
        config.version = 1;
        for key in ["x", "y", "a", "b"] {
            config.bindings.remove(key);
        }
        config.bindings.insert("r1".into(), Action::MouseHold);
        config.enabled = true;
        let mut custom = config.clone();
        custom.bindings.insert(
            "x".into(),
            Action::Shortcut(Shortcut {
                key: "KeyC".into(),
                modifiers: vec!["cmd".into()],
            }),
        );
        let previous = custom.bindings.clone();
        upgrade(&mut config);
        assert_eq!(config.bindings, Config::default().bindings);
        assert!(config.enabled);
        upgrade(&mut custom);
        assert_eq!(custom.bindings, previous);
        assert_eq!(custom.version, 2);
    }
    #[test]
    fn triggers_scale_right_stick_scrolling_with_precision_priority() {
        fn scroll_for(triggers: [u8; 2]) -> i32 {
            let mut engine = active();
            engine.axes[2] = 32767;
            engine.triggers = triggers;
            let now = Instant::now();
            engine.last_tick = now - Duration::from_millis(100);
            engine.tick(now);
            output::SCROLL.with_borrow(|events| events[0].0.abs())
        }
        let normal = scroll_for([0, 0]);
        let fast = scroll_for([255, 0]);
        let slow = scroll_for([0, 255]);
        assert!(normal > 0 && fast > normal * 2);
        assert!(slow > 0 && slow < normal / 2);
        assert_eq!(scroll_for([255, 255]), slow);
    }
    #[test]
    fn precision_trigger_overrides_acceleration() {
        let config = Config::default();
        assert_eq!(speed_multiplier(&config, [0, 0]), 1.0);
        assert_eq!(speed_multiplier(&config, [255, 0]), 3.0);
        assert!((speed_multiplier(&config, [0, 255]) - 0.2).abs() < 0.0001);
        assert!((speed_multiplier(&config, [255, 255]) - 0.2).abs() < 0.0001);
    }
    #[test]
    fn radial_deadzone_and_normalized_diagonal() {
        assert_eq!(velocity(100, -100, 0.15), (0.0, 0.0));
        let (x, y) = velocity(32767, 32767, 0.15);
        assert!((x.hypot(y) - 1.0).abs() < 0.0001);
        assert!(x > 0.0 && y < 0.0);
    }
    #[test]
    fn defaults_round_trip_and_reject_invalid_inputs() {
        let config: Config =
            serde_json::from_slice(&serde_json::to_vec(&Config::default()).unwrap()).unwrap();
        assert!(validate(&config).is_ok());
        let mut invalid = config;
        invalid
            .bindings
            .insert("assistant".into(), Action::AppSwitcher);
        assert!(validate(&invalid).is_err());
    }
}
