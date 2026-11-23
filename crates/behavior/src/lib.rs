//! Deterministic controller behaviors. All native effects are returned to the host.
mod broker;
mod script;

use anyhow::{Result, bail, ensure};
pub use broker::{Broker, Output};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type KeyResolver = fn(&str) -> Option<u16>;
pub const LEGACY: &str = "@default";
const MAX_ACTIONS: usize = 64;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Frame {
    pub kind: EventKind,
    pub pressed: BTreeSet<String>,
    pub released: BTreeSet<String>,
    pub held: BTreeSet<String>,
    pub axes: [i32; 4],
    pub triggers: [u8; 2],
    pub time_ms: u64,
    pub dt_ms: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    #[default]
    Input,
    Tick,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    pub inputs: BTreeSet<String>,
    pub program: Program,
    #[serde(default)]
    pub on_cancel: Vec<Action>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Program {
    Rules { rules: Vec<Rule> },
    Script { source: String },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub on: Trigger,
    #[serde(default)]
    pub all_held: BTreeSet<String>,
    pub actions: Vec<Action>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Press { input: String },
    Release { input: String },
    Tick,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
    KeyTap {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    MouseDown {
        button: Button,
    },
    MouseUp {
        button: Button,
    },
    ReleaseAll,
    Move {
        x: f64,
        y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
}
impl Button {
    pub fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    #[serde(default)]
    pub handled: bool,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub state: serde_json::Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Disabled,
    Active,
    Faulted(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub status: Status,
}

pub struct Prepared {
    definition: Definition,
    script: Option<script::Script>,
}
impl Prepared {
    pub fn definition(&self) -> &Definition {
        &self.definition
    }
    pub fn new(definition: Definition, keys: KeyResolver) -> Result<Self> {
        ensure!(definition.version == 1, "Unsupported behavior version");
        ensure!(
            !definition.id.is_empty()
                && definition.id.len() <= 80
                && definition
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "Invalid behavior ID"
        );
        ensure!(
            !definition.name.trim().is_empty() && definition.name.len() <= 200,
            "Invalid behavior name"
        );
        ensure!(
            !definition.inputs.is_empty() && definition.inputs.len() <= 32,
            "Declare behavior inputs"
        );
        let known = [
            "a",
            "b",
            "x",
            "y",
            "l1",
            "r1",
            "l2",
            "r2",
            "l3",
            "r3",
            "dpad_left",
            "dpad_right",
            "dpad_up",
            "dpad_down",
            "left_stick",
            "right_stick",
            "menu",
            "options",
            "capture",
        ];
        ensure!(
            definition
                .inputs
                .iter()
                .all(|name| known.contains(&name.as_str())),
            "Unknown or reserved behavior input"
        );
        validate_actions(&definition.on_cancel, keys)?;
        ensure!(
            definition.on_cancel.iter().all(|action| match action {
                Action::ReleaseAll | Action::KeyUp { .. } | Action::MouseUp { .. } => true,
                Action::KeyTap { key, modifiers } => key == "Escape" && modifiers.is_empty(),
                _ => false,
            }),
            "Cancellation supports releases and Escape only"
        );
        let compiled = match &definition.program {
            Program::Rules { rules } => {
                ensure!(rules.len() <= 128, "Too many rules");
                for rule in rules {
                    match &rule.on {
                        Trigger::Press { input } | Trigger::Release { input } => ensure!(
                            definition.inputs.contains(input),
                            "Rule uses an undeclared input"
                        ),
                        Trigger::Tick => {}
                    }
                    ensure!(
                        rule.all_held.is_subset(&definition.inputs),
                        "Condition uses undeclared inputs"
                    );
                    validate_actions(&rule.actions, keys)?;
                }
                None
            }
            Program::Script { source } => Some(script::Script::compile(source)?),
        };
        Ok(Self {
            definition,
            script: compiled,
        })
    }
}

struct Plugin {
    prepared: Prepared,
    status: Status,
    state: serde_json::Value,
    armed: bool,
}
pub struct Runtime {
    plugins: BTreeMap<String, Plugin>,
    broker: Broker,
    keys: KeyResolver,
}
impl Runtime {
    pub fn new(keys: KeyResolver) -> Self {
        Self {
            plugins: BTreeMap::new(),
            broker: Broker::default(),
            keys,
        }
    }
    pub fn install(&mut self, prepared: Prepared) {
        let id = prepared.definition.id.clone();
        if let Some(previous) = self.plugins.get(&id) {
            dispose(&mut self.broker, previous, self.keys);
        }
        let status = if prepared.definition.enabled {
            Status::Active
        } else {
            Status::Disabled
        };
        self.plugins.insert(
            id,
            Plugin {
                prepared,
                status,
                state: serde_json::Value::Null,
                armed: false,
            },
        );
    }
    pub fn unload(&mut self, id: &str) {
        if let Some(plugin) = self.plugins.get(id) {
            dispose(&mut self.broker, plugin, self.keys);
        }
        self.plugins.remove(id);
    }
    pub fn enable(&mut self, id: &str, enabled: bool) -> Result<()> {
        if let Some(plugin) = self.plugins.get(id) {
            dispose(&mut self.broker, plugin, self.keys);
        }
        let plugin = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Unknown behavior"))?;
        self.broker.release(id);
        plugin.state = serde_json::Value::Null;
        plugin.armed = false;
        plugin.status = if enabled {
            Status::Active
        } else {
            Status::Disabled
        };
        plugin.prepared.definition.enabled = enabled;
        Ok(())
    }
    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.plugins
            .values()
            .map(|p| Snapshot {
                id: p.prepared.definition.id.clone(),
                name: p.prepared.definition.name.clone(),
                status: p.status.clone(),
            })
            .collect()
    }
    pub fn definition(&self, id: &str) -> Option<&Definition> {
        self.plugins.get(id).map(|p| &p.prepared.definition)
    }
    pub fn reset(&mut self) {
        for plugin in self.plugins.values() {
            dispose(&mut self.broker, plugin, self.keys);
        }
        self.broker.clear();
        for plugin in self.plugins.values_mut() {
            plugin.state = serde_json::Value::Null;
            plugin.armed = false;
        }
    }
    pub fn dispatch(&mut self, frame: &Frame) -> BTreeSet<String> {
        let mut order: Vec<_> = self
            .plugins
            .iter()
            .map(|(id, p)| {
                (
                    std::cmp::Reverse(p.prepared.definition.priority),
                    id.clone(),
                )
            })
            .collect();
        order.sort();
        let mut consumed = BTreeSet::new();
        for (_, id) in order {
            let plugin = self.plugins.get_mut(&id).unwrap();
            if plugin.status != Status::Active {
                continue;
            }
            let inputs = &plugin.prepared.definition.inputs;
            if !inputs.is_disjoint(&consumed) {
                dispose(&mut self.broker, plugin, self.keys);
                plugin.state = serde_json::Value::Null;
                plugin.armed = false;
                continue;
            }
            if !plugin.armed {
                plugin.armed = inputs.iter().all(|input| !input_active(frame, input));
                if !plugin.armed {
                    consumed.extend(inputs.iter().cloned());
                    continue;
                }
            }
            let decision = match &mut plugin.prepared.script {
                Some(script) => script.call(frame, &plugin.state),
                None => rules(&plugin.prepared.definition.program, frame),
            };
            match decision.and_then(|decision| {
                validate_actions(&decision.actions, self.keys)?;
                ensure!(
                    serde_json::to_vec(&decision.state)?.len() <= 16 * 1024,
                    "Behavior state too large"
                );
                ensure!(
                    decision.handled || decision.actions.is_empty(),
                    "Actions require handled=true"
                );
                Ok(decision)
            }) {
                Ok(decision) => {
                    if decision.handled {
                        consumed.extend(inputs.iter().cloned());
                    }
                    for action in decision.actions {
                        self.broker.apply(&id, action, self.keys);
                    }
                    plugin.state = decision.state;
                    if self.broker.owns(&id) {
                        consumed.extend(inputs.iter().cloned());
                    }
                }
                Err(error) => {
                    // The failing event is consumed, even when preparation failed
                    // before effects. This gives one predictable failure contract.
                    consumed.extend(inputs.iter().cloned());
                    dispose(&mut self.broker, plugin, self.keys);
                    plugin.state = serde_json::Value::Null;
                    plugin.status = Status::Faulted(error.to_string());
                }
            }
        }
        consumed
    }
    pub fn broker(&mut self) -> &mut Broker {
        &mut self.broker
    }
    pub fn drain(&mut self) -> Vec<Output> {
        self.broker.drain()
    }
}

fn dispose(broker: &mut Broker, plugin: &Plugin, keys: KeyResolver) {
    let id = &plugin.prepared.definition.id;
    if broker.owns(id) {
        for action in &plugin.prepared.definition.on_cancel {
            broker.apply(id, action.clone(), keys);
        }
    }
    broker.release(id);
}

pub fn input_active(frame: &Frame, input: &str) -> bool {
    match input {
        "left_stick" => frame.axes[0..2].iter().any(|value| value.abs() > 5000),
        "right_stick" => frame.axes[2..4].iter().any(|value| value.abs() > 5000),
        "l2" => frame.triggers[0] > 20 || frame.held.contains(input),
        "r2" => frame.triggers[1] > 20 || frame.held.contains(input),
        _ => frame.held.contains(input),
    }
}

fn rules(program: &Program, frame: &Frame) -> Result<Decision> {
    let Program::Rules { rules } = program else {
        bail!("Script not compiled")
    };
    let mut result = Decision::default();
    for rule in rules {
        let matches = match &rule.on {
            Trigger::Press { input } => {
                frame.kind == EventKind::Input && frame.pressed.contains(input)
            }
            Trigger::Release { input } => {
                frame.kind == EventKind::Input && frame.released.contains(input)
            }
            Trigger::Tick => frame.kind == EventKind::Tick,
        };
        if matches && rule.all_held.is_subset(&frame.held) {
            result.handled = true;
            result.actions.extend(rule.actions.clone());
        }
    }
    Ok(result)
}
fn validate_actions(actions: &[Action], keys: KeyResolver) -> Result<()> {
    ensure!(actions.len() <= MAX_ACTIONS, "Too many output actions");
    for action in actions {
        match action {
            Action::KeyDown { key } | Action::KeyUp { key } => {
                ensure!(keys(key).is_some(), "Unsupported key {key}")
            }
            Action::KeyTap { key, modifiers } => {
                ensure!(keys(key).is_some(), "Unsupported key {key}");
                ensure!(
                    modifiers.len() <= 4
                        && modifiers
                            .iter()
                            .all(|key| ["cmd", "ctrl", "alt", "shift"].contains(&key.as_str())),
                    "Invalid shortcut modifiers"
                );
            }
            Action::Move { x, y } | Action::Scroll { x, y } => {
                ensure!(
                    x.is_finite() && y.is_finite() && x.abs() <= 1000.0 && y.abs() <= 1000.0,
                    "Invalid motion amount"
                );
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
