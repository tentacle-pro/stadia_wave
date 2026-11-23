use crate::{Action, KeyResolver};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Key {
        code: u16,
        down: bool,
        held: BTreeSet<u16>,
    },
    Mouse {
        button: usize,
        down: bool,
    },
    Move {
        x: f64,
        y: f64,
        buttons: [bool; 2],
    },
    Scroll {
        x: i32,
        y: i32,
    },
}
#[derive(Default)]
struct Lease {
    keys: BTreeSet<u16>,
    mouse: [bool; 2],
}
#[derive(Default)]
pub struct Broker {
    owners: BTreeMap<String, Lease>,
    outputs: Vec<Output>,
    scroll: BTreeMap<String, [f64; 2]>,
}
impl Broker {
    fn keys(&self) -> BTreeSet<u16> {
        self.owners
            .values()
            .flat_map(|lease| lease.keys.iter().copied())
            .collect()
    }
    pub fn buttons(&self) -> [bool; 2] {
        let mut buttons = [false; 2];
        for lease in self.owners.values() {
            for button in 0..2 {
                buttons[button] |= lease.mouse[button];
            }
        }
        buttons
    }
    fn change(&mut self, owner: &str, f: impl FnOnce(&mut Lease)) {
        let before = self.keys();
        let mouse = self.buttons();
        f(self.owners.entry(owner.into()).or_default());
        let after = self.keys();
        // Release ordinary keys before modifiers, and press modifiers first.
        let modifier = |key: &u16| matches!(*key, 55 | 56 | 58 | 59);
        let mut released: Vec<_> = before.difference(&after).copied().collect();
        released.sort_by_key(modifier);
        for code in released {
            self.outputs.push(Output::Key {
                code,
                down: false,
                held: after.clone(),
            });
        }
        let mut pressed: Vec<_> = after.difference(&before).copied().collect();
        pressed.sort_by_key(|key| !modifier(key));
        for code in pressed {
            self.outputs.push(Output::Key {
                code,
                down: true,
                held: after.clone(),
            });
        }
        let next = self.buttons();
        for button in 0..2 {
            if mouse[button] != next[button] {
                self.outputs.push(Output::Mouse {
                    button,
                    down: next[button],
                });
            }
        }
    }
    pub fn set_keys(&mut self, owner: &str, keys: BTreeSet<u16>) {
        self.change(owner, |lease| lease.keys = keys);
    }
    pub fn set_mouse(&mut self, owner: &str, buttons: [bool; 2]) {
        self.change(owner, |lease| lease.mouse = buttons);
    }
    pub fn tap(&mut self, owner: &str, key: u16, modifiers: BTreeSet<u16>) {
        let previous = self
            .owners
            .get(owner)
            .map(|lease| lease.keys.clone())
            .unwrap_or_default();
        let mut with_modifiers = previous.clone();
        with_modifiers.extend(modifiers);
        self.set_keys(owner, with_modifiers);
        let held = self.keys();
        self.outputs.push(Output::Key {
            code: key,
            down: true,
            held: held.clone(),
        });
        if !held.contains(&key) {
            self.outputs.push(Output::Key {
                code: key,
                down: false,
                held,
            });
        }
        self.set_keys(owner, previous);
    }
    pub fn release(&mut self, owner: &str) {
        self.change(owner, |lease| *lease = Lease::default());
        self.owners.remove(owner);
        self.scroll.remove(owner);
    }
    pub fn clear(&mut self) {
        let owners: Vec<_> = self.owners.keys().cloned().collect();
        for owner in owners {
            self.release(&owner);
        }
    }
    pub fn owns(&self, owner: &str) -> bool {
        self.owners
            .get(owner)
            .is_some_and(|lease| !lease.keys.is_empty() || lease.mouse.iter().any(|held| *held))
    }
    pub(crate) fn apply(&mut self, owner: &str, action: Action, keys: KeyResolver) {
        match action {
            Action::KeyDown { key } => self.change(owner, |lease| {
                lease.keys.insert(keys(&key).unwrap());
            }),
            Action::KeyUp { key } => self.change(owner, |lease| {
                lease.keys.remove(&keys(&key).unwrap());
            }),
            Action::KeyTap { key, modifiers } => self.tap(
                owner,
                keys(&key).unwrap(),
                modifiers.iter().filter_map(|key| keys(key)).collect(),
            ),
            Action::MouseDown { button } => {
                self.change(owner, |lease| lease.mouse[button.index()] = true)
            }
            Action::MouseUp { button } => {
                self.change(owner, |lease| lease.mouse[button.index()] = false)
            }
            Action::ReleaseAll => self.release(owner),
            Action::Move { x, y } => {
                let buttons = self.buttons();
                self.outputs.push(Output::Move { x, y, buttons });
            }
            Action::Scroll { x, y } => {
                let remainder = self.scroll.entry(owner.into()).or_default();
                remainder[0] += x;
                remainder[1] += y;
                let x = remainder[0].trunc() as i32;
                let y = remainder[1].trunc() as i32;
                remainder[0] -= x as f64;
                remainder[1] -= y as f64;
                if x != 0 || y != 0 {
                    self.outputs.push(Output::Scroll { x, y });
                }
            }
        }
    }
    pub fn drain(&mut self) -> Vec<Output> {
        std::mem::take(&mut self.outputs)
    }
}
