use super::*;
fn keys(key: &str) -> Option<u16> {
    match key {
        "cmd" => Some(55),
        "Tab" => Some(48),
        "Escape" => Some(53),
        "ArrowLeft" => Some(123),
        "ArrowRight" => Some(124),
        _ => None,
    }
}
fn definition(id: &str, inputs: &[&str], program: Program) -> Definition {
    Definition {
        version: 1,
        id: id.into(),
        name: id.into(),
        enabled: true,
        priority: 100,
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        program,
        on_cancel: vec![],
    }
}
fn frame(held: &[&str], pressed: &[&str], released: &[&str]) -> Frame {
    Frame {
        held: held.iter().map(|s| s.to_string()).collect(),
        pressed: pressed.iter().map(|s| s.to_string()).collect(),
        released: released.iter().map(|s| s.to_string()).collect(),
        ..Frame::default()
    }
}
fn install(runtime: &mut Runtime, definition: Definition) {
    runtime.install(Prepared::new(definition, keys).unwrap());
    runtime.dispatch(&Frame::default());
    runtime.drain();
}
fn hold_rule(id: &str, input: &str) -> Definition {
    definition(
        id,
        &[input],
        Program::Rules {
            rules: vec![
                Rule {
                    on: Trigger::Press {
                        input: input.into(),
                    },
                    all_held: BTreeSet::new(),
                    actions: vec![Action::KeyDown { key: "cmd".into() }],
                },
                Rule {
                    on: Trigger::Release {
                        input: input.into(),
                    },
                    all_held: BTreeSet::new(),
                    actions: vec![Action::ReleaseAll],
                },
            ],
        },
    )
}
#[test]
fn rule_mouse_example_consumes_without_duplicate_outputs() {
    let mut runtime = Runtime::new(keys);
    let mut example: Definition = serde_json::from_str(include_str!(
        "../../../apps/companion/behaviors/mouse-buttons.json"
    ))
    .unwrap();
    example.enabled = true;
    install(&mut runtime, example);
    let consumed = runtime.dispatch(&frame(&["x"], &["x"], &[]));
    assert!(consumed.contains("x"));
    assert_eq!(
        runtime.drain(),
        vec![Output::Mouse {
            button: 0,
            down: true
        }]
    );
    runtime.dispatch(&Frame {
        kind: EventKind::Tick,
        held: BTreeSet::from(["x".into()]),
        ..Frame::default()
    });
    assert!(runtime.drain().is_empty());
    runtime.dispatch(&frame(&[], &[], &["x"]));
    assert_eq!(
        runtime.drain(),
        vec![Output::Mouse {
            button: 0,
            down: false
        }]
    );
}
#[test]
fn script_example_switches_and_unload_releases_command() {
    let mut runtime = Runtime::new(keys);
    let mut example: Definition = serde_json::from_str(include_str!(
        "../../../apps/companion/behaviors/app-switcher.json"
    ))
    .unwrap();
    example.enabled = true;
    install(&mut runtime, example);
    runtime.dispatch(&frame(&["l1"], &["l1"], &[]));
    let events = runtime.drain();
    assert!(
        events
            .iter()
            .any(|e| matches!(e,Output::Key{code:48,down:true,held} if held.contains(&55)))
    );
    assert_eq!(runtime.snapshots()[0].status, Status::Active);
    runtime.unload("app-switcher");
    assert!(runtime.drain().iter().any(|e| matches!(
        e,
        Output::Key {
            code: 55,
            down: false,
            ..
        }
    )));
}
#[test]
fn disabling_one_scope_preserves_another_scopes_modifier() {
    let mut runtime = Runtime::new(keys);
    install(&mut runtime, hold_rule("first", "x"));
    install(&mut runtime, hold_rule("second", "y"));
    runtime.dispatch(&frame(&["x", "y"], &["x", "y"], &[]));
    runtime.drain();
    runtime.enable("first", false).unwrap();
    assert!(runtime.drain().is_empty());
    runtime.unload("second");
    assert!(runtime.drain().iter().any(|e| matches!(
        e,
        Output::Key {
            code: 55,
            down: false,
            ..
        }
    )));
}
#[test]
fn entire_invalid_action_batch_is_rejected_and_failing_event_consumed() {
    let mut runtime = Runtime::new(keys);
    let source = r#"fn on_event(e,s) {
        if e.pressed.contains("x") {
            #{handled:true, actions:[#{type:"key_down",key:"cmd"},#{type:"key_tap",key:"bad"}]}
        } else {#{handled:false}}
    }"#;
    install(
        &mut runtime,
        definition(
            "bad",
            &["x"],
            Program::Script {
                source: source.into(),
            },
        ),
    );
    assert!(runtime.dispatch(&frame(&["x"], &["x"], &[])).contains("x"));
    assert!(runtime.drain().is_empty());
    assert!(matches!(runtime.snapshots()[0].status, Status::Faulted(_)));
    assert!(runtime.dispatch(&frame(&[], &[], &["x"])).is_empty());
}
#[test]
fn script_failure_cleans_previous_lease_and_other_rules_continue() {
    let mut runtime = Runtime::new(keys);
    let source = r#"fn on_event(e,s) {
        if e.pressed.contains("x") {#{handled:true,actions:[#{type:"key_down",key:"cmd"}]}}
        else if e.released.contains("x") {throw "failed";}
        else {#{handled:false}}
    }"#;
    install(
        &mut runtime,
        definition(
            "bad",
            &["x"],
            Program::Script {
                source: source.into(),
            },
        ),
    );
    install(&mut runtime, hold_rule("good", "y"));
    runtime.dispatch(&frame(&["x"], &["x"], &[]));
    runtime.drain();
    assert!(runtime.dispatch(&frame(&[], &[], &["x"])).contains("x"));
    assert!(runtime.drain().iter().any(|e| matches!(
        e,
        Output::Key {
            code: 55,
            down: false,
            ..
        }
    )));
    runtime.dispatch(&frame(&["y"], &["y"], &[]));
    assert!(runtime.drain().iter().any(|e| matches!(
        e,
        Output::Key {
            code: 55,
            down: true,
            ..
        }
    )));
}
#[test]
fn non_terminating_script_faults_without_blocking_the_runtime() {
    let mut runtime = Runtime::new(keys);
    let source = "fn on_event(e,s) { loop {} }";
    runtime.install(
        Prepared::new(
            definition(
                "loop",
                &["x"],
                Program::Script {
                    source: source.into(),
                },
            ),
            keys,
        )
        .unwrap(),
    );
    runtime.dispatch(&Frame::default());
    assert!(matches!(runtime.snapshots()[0].status, Status::Faulted(_)));
}
#[test]
fn newly_enabled_behavior_waits_for_neutral() {
    let mut runtime = Runtime::new(keys);
    runtime.install(Prepared::new(hold_rule("hold", "x"), keys).unwrap());
    assert!(runtime.dispatch(&frame(&["x"], &["x"], &[])).contains("x"));
    assert!(runtime.drain().is_empty());
    runtime.dispatch(&Frame::default());
    runtime.dispatch(&frame(&["x"], &["x"], &[]));
    assert!(runtime.drain().iter().any(|e| matches!(
        e,
        Output::Key {
            code: 55,
            down: true,
            ..
        }
    )));
}
#[test]
fn rejected_replacement_leaves_previous_behavior_working() {
    let mut runtime = Runtime::new(keys);
    install(&mut runtime, hold_rule("hold", "x"));
    assert!(
        Prepared::new(
            definition(
                "hold",
                &["x"],
                Program::Script {
                    source: "fn broken".into()
                }
            ),
            keys
        )
        .is_err()
    );
    runtime.dispatch(&frame(&["x"], &["x"], &[]));
    assert!(runtime.drain().iter().any(|e| matches!(
        e,
        Output::Key {
            code: 55,
            down: true,
            ..
        }
    )));
}
#[test]
fn hot_replacement_releases_old_resources_and_waits_for_neutral() {
    let mut runtime = Runtime::new(keys);
    install(&mut runtime, hold_rule("replace", "x"));
    install(&mut runtime, hold_rule("unrelated", "y"));
    runtime.dispatch(&frame(&["x"], &["x"], &[]));
    runtime.drain();
    let mut replacement = hold_rule("replace", "x");
    if let Program::Rules { rules } = &mut replacement.program {
        rules[0].actions = vec![Action::MouseDown {
            button: Button::Right,
        }];
    }
    runtime.install(Prepared::new(replacement, keys).unwrap());
    assert!(runtime.drain().iter().any(|event| matches!(
        event,
        Output::Key {
            code: 55,
            down: false,
            ..
        }
    )));
    // Replacing while physically held must not start the new action.
    runtime.dispatch(&frame(&["x"], &[], &[]));
    assert!(runtime.drain().is_empty());
    runtime.dispatch(&frame(&[], &[], &["x"]));
    runtime.drain();
    runtime.dispatch(&frame(&["x", "y"], &["x", "y"], &[]));
    let events = runtime.drain();
    assert!(events.contains(&Output::Mouse {
        button: 1,
        down: true
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        Output::Key {
            code: 55,
            down: true,
            ..
        }
    )));
    runtime.dispatch(&frame(&["y"], &[], &["x"]));
    let events = runtime.drain();
    assert_eq!(
        events,
        vec![Output::Mouse {
            button: 1,
            down: false
        }]
    );
    // The unrelated owner still holds Command.
    runtime.unload("unrelated");
    assert!(runtime.drain().iter().any(|event| matches!(
        event,
        Output::Key {
            code: 55,
            down: false,
            ..
        }
    )));
}
#[test]
fn default_mapping_and_behavior_share_the_same_key_lease() {
    let mut broker = Broker::default();
    broker.set_keys(LEGACY, BTreeSet::from([55]));
    broker.set_keys("plugin", BTreeSet::from([55]));
    broker.drain();
    broker.release(LEGACY);
    assert!(broker.drain().is_empty());
    broker.release("plugin");
    assert_eq!(broker.drain().len(), 1);
}

#[test]
fn script_repeats_on_ticks_and_stops_repeating_after_release() {
    let mut runtime = Runtime::new(keys);
    let mut example: Definition = serde_json::from_str(include_str!(
        "../../../apps/companion/behaviors/app-switcher.json"
    ))
    .unwrap();
    example.enabled = true;
    install(&mut runtime, example);
    runtime.dispatch(&frame(&["l1"], &["l1"], &[]));
    runtime.dispatch(&frame(&["l1", "dpad_left"], &["dpad_left"], &[]));
    runtime.drain();
    let tick = Frame {
        kind: EventKind::Tick,
        held: BTreeSet::from(["l1".into(), "dpad_left".into()]),
        time_ms: 500,
        dt_ms: 10,
        ..Frame::default()
    };
    runtime.dispatch(&tick);
    assert!(runtime.drain().iter().any(|event| matches!(
        event,
        Output::Key {
            code: 123,
            down: true,
            ..
        }
    )));
    runtime.dispatch(&frame(&[], &[], &["l1", "dpad_left"]));
    runtime.drain();
    runtime.dispatch(&Frame {
        time_ms: 1000,
        ..tick
    });
    assert!(runtime.drain().is_empty());
}
