use crate::{
    mapping::{self, Action, Config, Shortcut},
    network::ViewState,
    pages,
};
use base64::Engine;
use nsh_base::{button, switch};
use nsh_core::{
    element::{ImageSrc, Painter, canvas, image, input},
    keyboard::{KeyCapture, KeyEvent},
    prelude::*,
};

const HOTSPOTS: &[(&str, f32, f32)] = &[
    ("l1", 117.0, 21.0),
    ("r1", 352.0, 21.0),
    ("l2", 62.0, 21.0),
    ("r2", 408.0, 21.0),
    ("dpad_up", 132.0, 112.0),
    ("dpad_down", 132.0, 151.0),
    ("dpad_left", 111.0, 132.0),
    ("dpad_right", 154.0, 132.0),
    ("a", 345.0, 155.0),
    ("b", 369.0, 130.0),
    ("x", 319.0, 130.0),
    ("y", 344.0, 105.0),
    ("left_stick", 179.0, 181.0),
    ("right_stick", 295.0, 181.0),
    ("options", 195.0, 104.0),
    ("menu", 280.0, 104.0),
    ("capture", 263.0, 131.0),
];

fn small_button(label: &'static str, action: impl Fn() + 'static) -> impl IntoElement {
    button()
        .on_press(action)
        .child(pages::control().child(text(label).text_size(px(12.0))))
}

fn selected_action(config: &Config, selected: &str) -> Action {
    config
        .bindings
        .get(selected)
        .cloned()
        .unwrap_or(Action::None)
}
fn set_action(draft: Signal<Config>, selected: Signal<String>, action: Action) {
    let mut config = draft.get();
    config.bindings.insert(selected.get(), action);
    draft.set(config);
}

pub fn page(
    model: Signal<ViewState>,
    dirty_state: Signal<bool>,
    notice: Signal<String>,
) -> impl IntoElement {
    let (config, warning) = mapping::load();
    let saved = signal(config.clone());
    let draft = signal(config);
    let selected = signal("l1".to_owned());
    notice.set(warning);
    let recording = signal(false);
    let recorder = element_ref();
    let binding_list = element_ref();
    effect(move || {
        let _ = selected.get();
        binding_list.scroll_to(Point::new(px(0.0), px(0.0)), false);
    });
    let testing = signal(false);
    let preview = signal((110.0f32, 45.0f32));
    let diagram_width = signal(470.0f32);
    let speed = signal(draft.get().speed.to_string());
    let deadzone = signal(draft.get().deadzone.to_string());
    let scroll_speed = signal(draft.get().scroll_speed.to_string());
    let dirty = move || {
        draft.get() != saved.get()
            || speed.get() != saved.get().speed.to_string()
            || deadzone.get() != saved.get().deadzone.to_string()
            || scroll_speed.get() != saved.get().scroll_speed.to_string()
    };
    effect(move || dirty_state.set(dirty()));
    effect(move || mapping::pause(recording.get() || testing.get() || dirty()));
    cleanup(move || {
        mapping::pause(false);
        dirty_state.set(false);
    });
    set_interval(std::time::Duration::from_millis(16), move || {
        if testing.get() {
            let state = model.get();
            let zone = deadzone
                .get()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && (0.05..=0.5).contains(value))
                .unwrap_or(draft.get().deadzone);
            let (dx, dy) = mapping::velocity(state.axes[0], state.axes[1], zone);
            let test_speed = speed
                .get()
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite() && (100.0..=3000.0).contains(value))
                .unwrap_or(900.0);
            let multiplier = mapping::speed_multiplier(&draft.get(), state.triggers) as f32;
            let (x, y) = preview.get();
            preview.set((
                (x + dx as f32 * test_speed * multiplier * 0.016).clamp(5.0, 215.0),
                (y + dy as f32 * test_speed * multiplier * 0.016).clamp(5.0, 85.0),
            ));
        }
    });
    let uri = format!(
        "data:image/webp;base64,{}",
        base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("../assets/controller.webp"))
    );
    view()
        .element_ref(&recorder)
        .focusable(true)
        .on_blur(move || recording.set(false))
        .gap_4()
        .text_color(rgb(pages::INK))
        .capture_keys(move || {
            if recording.get() {
                KeyCapture::All
            } else {
                KeyCapture::None
            }
        })
        .on_key_down(move |event: &KeyEvent| {
            if !recording.get() || event.repeat {
                return;
            }
            let code = event.code.to_string();
            if mapping::key_code(&code).is_none() {
                notice.set("请按一个普通键，可同时按住 ⌘ ⌃ ⌥ ⇧。".into());
                return;
            }
            let mut modifiers = Vec::new();
            if event.modifiers.cmd {
                modifiers.push("cmd".into());
            }
            if event.modifiers.ctrl {
                modifiers.push("ctrl".into());
            }
            if event.modifiers.alt {
                modifiers.push("alt".into());
            }
            if event.modifiers.shift {
                modifiers.push("shift".into());
            }
            let shortcut = Shortcut {
                key: code,
                modifiers,
            };
            let action = if matches!(
                selected_action(&draft.get(), &selected.get()),
                Action::Hold(_)
            ) {
                Action::Hold(shortcut)
            } else {
                Action::Shortcut(shortcut)
            };
            set_action(draft, selected, action);
            recording.set(false);
            notice.set("快捷键已录入，保存后生效。".into());
        })
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text("按键映射")
                        .text_size(px(33.0))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    view()
                        .flex_row()
                        .gap_2()
                        .child(small_button("恢复预设", move || {
                            draft.set(Config::default());
                            speed.set("900".into());
                            deadzone.set("0.15".into());
                            scroll_speed.set("650".into());
                            notice.set("已载入默认预设，保存后生效。".into());
                        }))
                        .child(small_button("放弃修改", move || {
                            let config = saved.get();
                            speed.set(config.speed.to_string());
                            deadzone.set(config.deadzone.to_string());
                            scroll_speed.set(config.scroll_speed.to_string());
                            draft.set(config);
                            recording.set(false);
                            testing.set(false);
                            notice.set(String::new());
                        }))
                        .child(small_button("保存", move || {
                            let mut config = draft.get();
                            let (Ok(speed_value), Ok(deadzone_value), Ok(scroll_value)) = (
                                speed.get().parse(),
                                deadzone.get().parse(),
                                scroll_speed.get().parse(),
                            ) else {
                                notice.set("请填写有效的速度和死区数值。".into());
                                return;
                            };
                            config.speed = speed_value;
                            config.deadzone = deadzone_value;
                            config.scroll_speed = scroll_value;
                            match mapping::save(config.clone()) {
                                Ok(()) => {
                                    speed.set(config.speed.to_string());
                                    deadzone.set(config.deadzone.to_string());
                                    scroll_speed.set(config.scroll_speed.to_string());
                                    saved.set(config.clone());
                                    draft.set(config);
                                    notice.set("已保存，手柄回中后生效。".into());
                                }
                                Err(error) => notice.set(error.to_string()),
                            }
                        })),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    switch()
                        .checked(move || saved.get().enabled)
                        .on_change(move |enabled: &bool| {
                            let mut config = saved.get();
                            config.enabled = *enabled;
                            match mapping::save(config.clone()) {
                                Ok(()) => {
                                    saved.set(config);
                                    let mut pending = draft.get();
                                    pending.enabled = *enabled;
                                    draft.set(pending);
                                    notice.set(if dirty() {
                                        "开关已保存，保存映射修改后恢复桌面控制。".into()
                                    } else {
                                        String::new()
                                    });
                                }
                                Err(error) => notice.set(error.to_string()),
                            }
                        })
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .rounded_lg()
                        .bg(move || {
                            rgb(if saved.get().enabled {
                                0xe0f0ed
                            } else {
                                0xe8ecea
                            })
                        })
                        .track_style(|style| {
                            style
                                .w(px(52.0))
                                .p(px(3.0))
                                .rounded_full()
                                .bg(rgb(0x889b94))
                                .checked(|style| style.bg(rgb(pages::MINT)))
                        })
                        .thumb_style(|style| {
                            style
                                .size(px(24.0))
                                .rounded_full()
                                .bg(rgb(0xffffff))
                                .shadow_sm()
                                .checked(|style| style.translate_x(px(22.0)))
                        })
                        .child(
                            text(move || {
                                if saved.get().enabled {
                                    "手柄映射已开启"
                                } else {
                                    "手柄映射已关闭"
                                }
                            })
                            .font_weight(FontWeight::SEMIBOLD),
                        ),
                )
                .child(
                    text(move || {
                        if dirty() {
                            "编辑中 · 桌面控制暂停"
                        } else {
                            "配置已保存"
                        }
                    })
                    .text_color(rgb(pages::MUTED))
                    .text_size(px(12.0)),
                ),
        )
        .child(
            view()
                .flex_row()
                .gap_5()
                .child(
                    view()
                        .flex_1()
                        .gap_3()
                        .on_layout(move |bounds: &Bounds<Points>| {
                            let width = bounds.size.width.0.max(470.0);
                            if diagram_width.get() != width {
                                diagram_width.set(width);
                            }
                        })
                        .child(text("点击手柄选择输入").text_color(rgb(pages::MUTED)))
                        .child(
                            scroll().w_full().child(
                                view()
                                    .relative()
                                    .w(move || px(diagram_width.get()))
                                    .h(px(300.0))
                                    .child(
                                        image(ImageSrc::from(uri))
                                            .absolute()
                                            .left(px(0.0))
                                            .top(px(65.0))
                                            .w(px(470.0))
                                            .h(px(235.0)),
                                    )
                                    .child(
                                        canvas(move || {
                                            let current = selected.get();
                                            let mut painter = Painter::new()
                                                .stroke_style(rgb(pages::MINT))
                                                .line_width(1.5);
                                            if let Some((_, x, y)) =
                                                HOTSPOTS.iter().find(|(id, _, _)| {
                                                    *id == match current.as_str() {
                                                        "l3" => "left_stick",
                                                        "r3" => "right_stick",
                                                        other => other,
                                                    }
                                                })
                                            {
                                                painter = painter
                                                    .begin_path()
                                                    .move_to(*x, *y)
                                                    .line_to(diagram_width.get() - 24.0, *y)
                                                    .line_to(diagram_width.get() - 1.0, 17.0)
                                                    .stroke();
                                            }
                                            painter
                                        })
                                        .absolute()
                                        .w_full()
                                        .h_full(),
                                    )
                                    .children(HOTSPOTS.iter().map(|&(id, x, y)| {
                                        button()
                                            .absolute()
                                            .left(px(x - 15.0))
                                            .top(px(y - 15.0))
                                            .on_press(move || {
                                                selected.set(id.into());
                                                recording.set(false);
                                            })
                                            .child(
                                                view()
                                                    .w(px(30.0))
                                                    .h(px(30.0))
                                                    .rounded_full()
                                                    .border(px(2.0))
                                                    .border_color(move || {
                                                        rgb(if selected.get() == id {
                                                            pages::MINT
                                                        } else {
                                                            0x99b5ab
                                                        })
                                                    })
                                                    .bg(move || {
                                                        if model
                                                            .get()
                                                            .held
                                                            .iter()
                                                            .any(|key| key.eq_ignore_ascii_case(id))
                                                        {
                                                            rgba(0x21ad8955)
                                                        } else {
                                                            rgba(0x00000000)
                                                        }
                                                    })
                                                    .items_center()
                                                    .justify_center()
                                                    .child(
                                                        text(match id {
                                                            "l1" => "L1",
                                                            "r1" => "R1",
                                                            "l2" => "L2",
                                                            "r2" => "R2",
                                                            "left_stick" => "L",
                                                            "right_stick" => "R",
                                                            _ => "",
                                                        })
                                                        .text_size(px(10.0)),
                                                    ),
                                            )
                                    })),
                            ),
                        )
                        .child(
                            view()
                                .flex_row()
                                .gap_2()
                                .child(small_button("L3 · 按下", move || {
                                    selected.set("l3".into())
                                }))
                                .child(small_button("R3 · 按下", move || {
                                    selected.set("r3".into())
                                })),
                        )
                        .child(
                            text(
                                "默认 X 左键 · Y 右键 · A 回车 · B Esc。有语音草稿时，A 确认、B 丢弃。",
                            )
                            .text_size(px(12.0))
                            .text_color(rgb(pages::MUTED)),
                        )
                        .child(pages::rule())
                        .child(
                            view()
                                .flex_row()
                                .gap_2()
                                .child(small_button("测试指针", move || {
                                    testing.set(!testing.get());
                                    recording.set(false);
                                }))
                                .child(
                                    text(move || {
                                        if testing.get() {
                                            "测试中 · 桌面输出暂停"
                                        } else {
                                            "左摇杆在下方测试区移动"
                                        }
                                    })
                                    .text_size(px(12.0)),
                                ),
                        )
                        .child(
                            canvas(move || {
                                let (x, y) = preview.get();
                                Painter::new()
                                    .fill_style(rgb(0xe0f0ed))
                                    .fill_rect(0.0, 0.0, 220.0, 90.0)
                                    .fill_style(rgb(pages::MINT))
                                    .begin_path()
                                    .arc(x, y, px(4.0), 0.0, std::f32::consts::TAU)
                                    .fill()
                            })
                            .w(px(220.0))
                            .h(px(90.0)),
                        ),
                )
                .child(
                    view()
                        .w(px(310.0))
                        .flex_none()
                        .gap_2()
                        .child(
                            text("输入与动作")
                                .text_size(px(18.0))
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(
                            scroll()
                                .element_ref(&binding_list)
                                .h(px(260.0))
                                .child(dynamic(move || {
                                    view().gap_1().children(
                                        {
                                            let mut inputs = mapping::INPUTS.to_vec();
                                            inputs.sort_by_key(|(id, _)| {
                                                if *id == selected.get() { 0 } else { 1 }
                                            });
                                            inputs
                                        }
                                        .into_iter()
                                        .map(
                                            |(id, label)| {
                                                button()
                                                    .on_press(move || {
                                                        selected.set(id.into());
                                                        recording.set(false);
                                                    })
                                                    .child(
                                                        view()
                                                            .flex_row()
                                                            .justify_between()
                                                            .items_center()
                                                            .gap_2()
                                                            .p_2()
                                                            .rounded_md()
                                                            .bg(move || {
                                                                rgb(if selected.get() == id {
                                                                    0xe0f0ed
                                                                } else {
                                                                    pages::SURFACE
                                                                })
                                                            })
                                                            .child(text(label).text_size(px(12.0)))
                                                            .child(
                                                                text(move || {
                                                                    selected_action(
                                                                        &draft.get(),
                                                                        id,
                                                                    )
                                                                    .label()
                                                                })
                                                                .text_size(px(12.0))
                                                                .text_color(rgb(pages::MINT)),
                                                            ),
                                                    )
                                            },
                                        ),
                                    )
                                })),
                        )
                        .child(pages::rule())
                        .child(
                            text(move || {
                                mapping::INPUTS
                                    .iter()
                                    .find(|(id, _)| *id == selected.get())
                                    .map(|(_, label)| *label)
                                    .unwrap_or("输入")
                            })
                            .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(
                            view().flex_row().flex_wrap().gap_2().children(
                                [
                                    ("无动作", Action::None),
                                    (
                                        "快捷键",
                                        Action::Shortcut(Shortcut {
                                            key: "Space".into(),
                                            modifiers: vec![],
                                        }),
                                    ),
                                    (
                                        "保持按键",
                                        Action::Hold(Shortcut {
                                            key: "Space".into(),
                                            modifiers: vec![],
                                        }),
                                    ),
                                    ("切换应用", Action::AppSwitcher),
                                    ("鼠标左键", Action::MouseHold),
                                    ("鼠标右键", Action::RightMouseHold),
                                    ("移动指针", Action::Pointer),
                                    ("滚动", Action::Scroll),
                                    ("加速", Action::Faster),
                                    ("减速", Action::Slower),
                                ]
                                .into_iter()
                                .map(|(label, action)| {
                                    button()
                                        .on_press(move || {
                                            let input = selected.get();
                                            let analog = input.ends_with("_stick");
                                            let valid = match &action {
                                                Action::Pointer | Action::Scroll => analog,
                                                Action::Faster | Action::Slower => {
                                                    input == "l2" || input == "r2"
                                                }
                                                Action::None => true,
                                                _ => !analog,
                                            };
                                            if !valid {
                                                notice.set("请选择与此输入匹配的动作。".into());
                                                return;
                                            }
                                            set_action(draft, selected, action.clone());
                                            recording.set(false);
                                        })
                                        .child(
                                            pages::control().child(text(label).text_size(px(11.0))),
                                        )
                                }),
                            ),
                        )
                        .child(
                            view().flex_row().gap_2().children(
                                [
                                    ("保持 ⌘", "cmd"),
                                    ("保持 ⌃", "ctrl"),
                                    ("保持 ⇧", "shift"),
                                    ("保持 ⌥", "alt"),
                                ]
                                .into_iter()
                                .map(|(label, key)| {
                                    small_button(label, move || {
                                        if selected.get().ends_with("_stick") {
                                            return;
                                        }
                                        set_action(
                                            draft,
                                            selected,
                                            Action::Hold(Shortcut {
                                                key: key.into(),
                                                modifiers: vec![],
                                            }),
                                        );
                                    })
                                }),
                            ),
                        )
                        .child(
                            text(move || selected_action(&draft.get(), &selected.get()).label())
                                .text_color(rgb(pages::MINT)),
                        )
                        .child(
                            view()
                                .flex_row()
                                .gap_2()
                                .child(small_button("录制快捷键", move || {
                                    if selected.get().ends_with("_stick") {
                                        notice.set("摇杆推动请选择移动指针或滚动。".into());
                                        return;
                                    }
                                    recording.set(true);
                                    recorder.focus();
                                    testing.set(false);
                                    notice.set("在 Mac 键盘按下快捷键，或点击取消。".into());
                                }))
                                .child(small_button("取消录制", move || recording.set(false))),
                        )
                        .child(
                            text(move || {
                                if recording.get() {
                                    "等待键盘输入…"
                                } else {
                                    ""
                                }
                            })
                            .text_color(rgb(pages::MINT)),
                        ),
                ),
        )
        .child(pages::rule())
        .child(
            view()
                .flex_row()
                .gap_4()
                .child(
                    view()
                        .flex_1()
                        .gap_1()
                        .child(text("指针速度 · 点/秒").text_size(px(12.0)))
                        .child(
                            input()
                                .value(move || speed.get())
                                .on_change(move |value: &str| speed.set(value.into()))
                                .w_full(),
                        ),
                )
                .child(
                    view()
                        .flex_1()
                        .gap_1()
                        .child(text("中心死区 · 0.05–0.5").text_size(px(12.0)))
                        .child(
                            input()
                                .value(move || deadzone.get())
                                .on_change(move |value: &str| deadzone.set(value.into()))
                                .w_full(),
                        ),
                )
                .child(
                    view()
                        .flex_1()
                        .gap_1()
                        .child(text("滚动速度 · 点/秒").text_size(px(12.0)))
                        .child(
                            input()
                                .value(move || scroll_speed.get())
                                .on_change(move |value: &str| scroll_speed.set(value.into()))
                                .w_full(),
                        ),
                )
                .child(small_button("反转滚动", move || {
                    let mut config = draft.get();
                    config.reverse_scroll = !config.reverse_scroll;
                    draft.set(config);
                })),
        )
        .child(
            text(move || {
                if draft.get().reverse_scroll {
                    "滚动方向 · 已反转"
                } else {
                    "滚动方向 · 默认"
                }
            })
            .text_size(px(12.0))
            .text_color(rgb(pages::MUTED)),
        )
        .child(behavior_panel())
        .child(
            text(move || notice.get())
                .text_size(px(12.0))
                .text_color(rgb(pages::MUTED)),
        )
}

fn behavior_panel() -> impl IntoElement {
    let snapshots = signal(mapping::behavior_status());
    let feedback = signal(String::new());
    set_interval(std::time::Duration::from_millis(500), move || {
        snapshots.set(mapping::behavior_status());
    });
    view()
        .gap_3()
        .child(pages::rule())
        .child(
            text("行为运行时 · 框架验证")
                .text_size(px(20.0))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            text("启用时，规则和脚本优先处理输入；关闭后恢复上方的基础映射。要让按键完全无动作，请将基础映射设为「无动作」。")
                .text_size(px(12.0))
                .text_color(rgb(pages::MUTED)),
        )
        .child(
            view()
                .flex_row()
                .gap_2()
                .child(small_button("安装手写示例", move || {
                    feedback.set(
                        match crate::behaviors::install_examples()
                            .and_then(|_| mapping::reload_behaviors())
                        {
                            Ok(()) => "示例已就绪，已有文件会保留。".into(),
                            Err(error) => error.to_string(),
                        },
                    );
                }))
                .child(small_button("重新加载", move || {
                    feedback.set(match mapping::reload_behaviors() {
                        Ok(()) => "已重新加载，修改的输入回中后生效。".into(),
                        Err(error) => error.to_string(),
                    });
                }))
                .child(small_button("打开行为目录", move || {
                    let _ = std::fs::create_dir_all(crate::behaviors::directory());
                    let _ = std::process::Command::new("/usr/bin/open")
                        .arg(crate::behaviors::directory())
                        .spawn();
                })),
        )
        .child(dynamic(move || {
            view()
                .gap_2()
                .children(snapshots.get().0.into_iter().map(|snapshot| {
                    let id = snapshot.id.clone();
                    let active = snapshot.status == wave_behavior::Status::Active;
                    let status = match &snapshot.status {
                        wave_behavior::Status::Disabled => "已关闭 · 使用基础映射".into(),
                        wave_behavior::Status::Active => "运行中".into(),
                        wave_behavior::Status::Faulted(error) => format!("故障停用 · 使用基础映射 · {error}"),
                    };
                    view()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(0xe0f0ed))
                        .child(
                            view()
                                .flex_1()
                                .gap_1()
                                .child(text(snapshot.name).font_weight(FontWeight::SEMIBOLD))
                                .child(
                                    text(status)
                                        .text_size(px(12.0))
                                        .text_color(rgb(pages::MUTED)),
                                ),
                        )
                        .child(
                            switch()
                                .checked(active)
                                .on_change(move |enabled: &bool| {
                                    feedback.set(match mapping::enable_behavior(&id, *enabled) {
                                        Ok(()) => String::new(),
                                        Err(error) => error.to_string(),
                                    });
                                    snapshots.set(mapping::behavior_status());
                                })
                                .track_style(|s| {
                                    s.w(px(44.0))
                                        .p(px(2.0))
                                        .rounded_full()
                                        .bg(rgb(0x889b94))
                                        .checked(|s| s.bg(rgb(pages::MINT)))
                                })
                                .thumb_style(|s| {
                                    s.size(px(20.0))
                                        .rounded_full()
                                        .bg(rgb(0xffffff))
                                        .checked(|s| s.translate_x(px(20.0)))
                                })
                                .child(text("覆盖基础映射").text_size(px(12.0))),
                        )
                }))
        }))
        .child(
            text(move || snapshots.get().1)
                .text_size(px(12.0))
                .text_color(rgb(pages::MUTED)),
        )
        .child(
            text(move || feedback.get())
                .text_size(px(12.0))
                .text_color(rgb(pages::MUTED)),
        )
}
