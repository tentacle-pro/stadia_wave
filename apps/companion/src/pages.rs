use crate::{
    asr, desktop,
    dictation::Dictation,
    network::{Command, ViewState},
};
use base64::Engine;
use nsh_base::button;
use nsh_core::element::{ImageSrc, Painter, canvas, image, input};
use nsh_core::prelude::*;
use nsh_core::storage::Storage;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn controller_image() -> nsh_core::element::Image {
    static CONTROLLER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let uri = CONTROLLER.get_or_init(|| {
        let data = base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("../assets/controller.webp"));
        format!("data:image/webp;base64,{data}")
    });
    image(ImageSrc::from(uri.as_str()))
}

fn controller(width: f32) -> impl IntoElement {
    controller_image().w(px(width)).h(px(width / 2.0))
}

pub(crate) const INK: u32 = 0x172321;
pub(crate) const MUTED: u32 = 0x4c605b;
pub(crate) const MINT: u32 = 0x126b58;
pub(crate) const RULE: u32 = 0xd2ddda;
pub(crate) const SURFACE: u32 = 0xf8faf9;

fn heading(label: &'static str, size: f32) -> impl IntoElement {
    text(label)
        .text_size(px(size))
        .font_weight(FontWeight::SEMIBOLD)
}

pub(crate) fn rule() -> impl IntoElement {
    view().h(px(1.0)).flex_none().bg(rgb(RULE))
}

fn vertical_rule() -> impl IntoElement {
    view().w(px(1.0)).flex_none().bg(rgb(RULE))
}

pub(crate) fn control() -> nsh_core::element::View {
    view()
        .py_1()
        .px_3()
        .rounded_md()
        .border(px(1.0))
        .border_color(rgb(0xb9cbc5))
        .bg(rgb(SURFACE))
}

fn row(label: &'static str, value: String) -> impl IntoElement {
    view()
        .child(
            view()
                .flex_row()
                .gap_2()
                .py_2()
                .child(text(label).w(px(80.0)).flex_none().text_color(rgb(MUTED)))
                .child(text(value).flex_1()),
        )
        .child(rule())
}

fn link(label: &'static str, url: &'static str) -> impl IntoElement {
    button()
        .py_1()
        .on_press(move || {
            spawn(async move {
                let _ = open_url(url).await;
            });
        })
        .child(text(format!("{label} ↗")).text_color(rgb(MINT)))
}

// The modern extension host ignores `?privacy-…` as an anchor; the legacy host
// forwards to the same pane and keeps the anchor, which the system resolves.
const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
// The documented anchor. This macOS build ignores it for Local Network (the pane
// has no reveal key), so the row also says which entry to click.
const LOCAL_NETWORK_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";
const LOGIN_ITEMS_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

fn shorten_path(path: &str) -> String {
    const LIMIT: usize = 38;
    if path.chars().count() <= LIMIT {
        return path.to_string();
    }
    let tail: String = path
        .chars()
        .skip(path.chars().count() - (LIMIT - 1))
        .collect();
    format!("…{tail}")
}

fn permission_row(
    label: &'static str,
    status: impl Fn() -> String + 'static,
    settings: &'static str,
    hint: &'static str,
) -> impl IntoElement {
    view()
        .gap_1()
        .py_1()
        .child(text(label).text_size(px(12.0)).text_color(rgb(MUTED)))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(text(status).flex_1().text_size(px(13.0)))
                .child(
                    button()
                        .on_press(move || {
                            spawn(async move {
                                let _ = open_url(settings).await;
                            });
                        })
                        .child(control().child(text("打开"))),
                ),
        )
        .child(when(
            move || !hint.is_empty(),
            move || text(hint).text_size(px(11.0)).text_color(rgb(MUTED)),
        ))
}

pub fn about(mark: ImageSrc, update_notice: Signal<String>) -> impl IntoElement {
    let licenses = std::env::current_exe().ok().and_then(|executable| {
        let contents = executable.parent()?.parent()?;
        let directory = contents.join("Resources/Licenses");
        directory.is_dir().then_some(directory)
    });
    let project_license = licenses
        .as_ref()
        .map(|directory| directory.join("Stadia-Wave-LICENSE.txt"))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../LICENSE"));
    let notice_feedback = signal(String::new());
    let open_licenses = |label: &'static str, path: PathBuf| {
        button()
            .on_press(move || {
                if std::process::Command::new("/usr/bin/open")
                    .arg(&path)
                    .status()
                    .is_ok_and(|status| status.success())
                {
                    notice_feedback.set(String::new());
                } else {
                    notice_feedback.set("无法打开许可文件。".into());
                }
            })
            .child(control().child(text(label)))
    };

    view()
        .gap_5()
        .text_color(rgb(INK))
        .child(heading("关于", 33.0))
        .child(
            view()
                .flex_row()
                .gap_4()
                .items_center()
                .p_5()
                .rounded_lg()
                .bg(rgb(0xe0f0ed))
                .child(
                    view()
                        .w(px(64.0))
                        .h(px(64.0))
                        .flex_none()
                        .rounded_lg()
                        .bg(rgb(SURFACE))
                        .items_center()
                        .justify_center()
                        .child(image(mark).w(px(38.0)).h(px(38.0))),
                )
                .child(
                    view()
                        .flex_1()
                        .gap_1()
                        .child(heading("Stadia Wave", 26.0))
                        .child(
                            text(concat!("Wave Companion · ", env!("CARGO_PKG_VERSION")))
                                .text_color(rgb(MUTED)),
                        ),
                )
                .child(
                    button()
                        .on_press(move || update_notice.set("尚未配置更新发布来源。".into()))
                        .child(control().child(text("检查更新"))),
                ),
        )
        .child(when(
            move || !update_notice.get().is_empty(),
            move || text(move || update_notice.get()).text_color(rgb(MUTED)),
        ))
        .child(
            view()
                .gap_2()
                .child(heading("开源许可", 20.0))
                .child(rule())
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .py_2()
                        .child(
                            view()
                                .gap_1()
                                .child(heading("Stadia Wave", 16.0))
                                .child(text("Apache License 2.0").text_color(rgb(MUTED))),
                        )
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .child(link(
                                    "源代码",
                                    "https://github.com/tentacle-pro/stadia_wave",
                                ))
                                .child(if cfg!(target_os = "macos") && project_license.is_file() {
                                    open_licenses("查看许可", project_license).into_element()
                                } else {
                                    link("查看许可", "https://www.apache.org/licenses/LICENSE-2.0")
                                        .into_element()
                                }),
                        ),
                )
                .child(rule())
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .py_2()
                        .child(
                            view()
                                .gap_1()
                                .flex_1()
                                .child(heading("Stadiacast", 16.0))
                                .child(
                                    text("MIT License · 三份 Google 协议定义的例外见上游 NOTICE。")
                                        .text_color(rgb(MUTED)),
                                ),
                        )
                        .child(link(
                            "项目与许可",
                            "https://github.com/jkfran/stadiacast#legal",
                        )),
                )
                .child(rule())
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .py_2()
                        .child(
                            view()
                                .gap_1()
                                .child(heading("第三方组件", 16.0))
                                .child(text("依赖清单、许可正文和版权声明").text_color(rgb(MUTED))),
                        )
                        .child(if let Some(directory) = licenses {
                            open_licenses("打开许可文件夹", directory).into_element()
                        } else {
                            link(
                                "查看依赖",
                                "https://github.com/tentacle-pro/stadia_wave/blob/main/Cargo.lock",
                            )
                            .into_element()
                        }),
                )
                .child(rule())
                .child(when(
                    move || !notice_feedback.get().is_empty(),
                    move || text(move || notice_feedback.get()).text_color(rgb(MUTED)),
                )),
        )
}

fn connection(
    label: impl Fn() -> &'static str + 'static,
    active: impl Fn() -> bool + Copy + 'static,
) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .gap_2()
        .child(
            view()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    view()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_full()
                        .bg(move || rgb(if active() { MINT } else { 0x82948d })),
                )
                .child(
                    text(label)
                        .text_size(px(12.0))
                        .text_color(move || rgb(if active() { MINT } else { MUTED })),
                ),
        )
        .child(
            view()
                .h(px(1.0))
                .w_full()
                .bg(move || rgb(if active() { 0x5aa795 } else { RULE })),
        )
}

pub fn overview(
    model: Signal<ViewState>,
    endpoint: Signal<String>,
    bridge_connections: impl IntoElement,
) -> impl IntoElement {
    let bridge_notice = signal(String::new());
    let bridge_status = signal(crate::local_bridge::status());
    let authorized = signal(desktop::authorized());
    let stats = signal(crate::statistics::read());
    let identity = desktop::identity();
    let driver = signal(desktop::openjoystick_driver());
    set_interval(std::time::Duration::from_millis(750), move || {
        let status = crate::local_bridge::status();
        if status != bridge_status.get() {
            bridge_status.set(status);
        }
        let next = desktop::authorized();
        if next != authorized.get() {
            authorized.set(next);
        }
        let next = crate::statistics::read();
        if next != stats.get() {
            stats.set(next);
        }
    });
    set_interval(std::time::Duration::from_secs(15), move || {
        if driver.get().is_some() || desktop::openjoystick_installed() {
            let next = desktop::openjoystick_driver();
            if next != driver.get() {
                driver.set(next);
            }
        }
    });
    let controller_ready = move || {
        model
            .get()
            .hardware
            .controllers
            .iter()
            .any(|controller| controller.ready)
    };
    let bridge_connected = move || model.get().connected;
    view()
        .gap_5()
        .text_color(rgb(INK))
        .text_size(px(14.0))
        .child(
            view().gap_1().child(heading("概览", 33.0)).child(
                text(move || {
                    if controller_ready() && bridge_connected() {
                        "所有设备已就绪。".into()
                    } else {
                        model.get().status
                    }
                })
                .text_color(rgb(MUTED)),
            ),
        )
        .child(
            view()
                .pt_5()
                .gap_4()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(controller(120.0))
                        .child(connection(
                            move || {
                                if controller_ready() {
                                    "Wi-Fi 已连接"
                                } else {
                                    "等待手柄"
                                }
                            },
                            controller_ready,
                        ))
                        .child(node_icon(false))
                        .child(connection(
                            move || {
                                if bridge_connected() {
                                    "已认证"
                                } else {
                                    "未连接"
                                }
                            },
                            bridge_connected,
                        ))
                        .child(node_icon(true)),
                )
                .child(
                    view()
                        .flex_row()
                        .gap_5()
                        .child(
                            view()
                                .flex_1()
                                .gap_2()
                                .child(heading("Stadia 手柄", 20.0))
                                .child(dynamic(move || {
                                    let controllers = model.get().hardware.controllers;
                                    if controllers.is_empty() {
                                        return vec![
                                            view()
                                                .p_2()
                                                .child(text("等待手柄连接").text_color(rgb(MUTED))),
                                        ];
                                    }
                                    controllers
                                        .into_iter()
                                        .map(|device| {
                                            view()
                                                .child(row(
                                                    "名称",
                                                    format!("Stadia · {}", device.id),
                                                ))
                                                .child(row(
                                                    "设备 ID",
                                                    device
                                                        .device_id
                                                        .unwrap_or_else(|| "等待上报".into()),
                                                ))
                                                .child(row(
                                                    "电量",
                                                    device
                                                        .battery_percent
                                                        .map(|v| format!("{v}%"))
                                                        .unwrap_or_else(|| "未知".into()),
                                                ))
                                                .child(row(
                                                    "IP",
                                                    device.ip.unwrap_or_else(|| "未知".into()),
                                                ))
                                        })
                                        .collect::<Vec<_>>()
                                }))
                                .child(link(
                                    "如何刷 282115 稳定版 Wi-Fi 固件",
                                    "https://github.com/luigimannoni/stadia-controller-flasher",
                                )),
                        )
                        .child(vertical_rule())
                        .child(
                            view()
                                .flex_1()
                                .gap_2()
                                .child(heading("Wave Bridge", 20.0))
                                .child(dynamic(move || {
                                    let state = model.get();
                                    let bridge = state.bridges.into_iter().find(|bridge| {
                                        Some(&bridge.id) == state.active_bridge.as_ref()
                                    });
                                    view()
                                        .child(row(
                                            "名称",
                                            bridge
                                                .as_ref()
                                                .map(|bridge| bridge.name.clone())
                                                .unwrap_or_else(|| "未连接".into()),
                                        ))
                                        .child(row(
                                            "地址",
                                            bridge
                                                .map(|bridge| {
                                                    if bridge.id == crate::network::LOCAL_ID {
                                                        bridge.address
                                                    } else {
                                                        format!(
                                                            "{}:{}",
                                                            bridge.address, bridge.port
                                                        )
                                                    }
                                                })
                                                .unwrap_or_else(|| "—".into()),
                                        ))
                                        .child(row(
                                            "Stadiacast",
                                            if state.connected {
                                                state.hardware.message
                                            } else {
                                                "等待 Bridge".into()
                                            },
                                        ))
                                        .child(row(
                                            "版本",
                                            state.hardware.version.unwrap_or_else(|| "未知".into()),
                                        ))
                                }))
                                .child(
                                    view()
                                        .pt_2()
                                        .gap_2()
                                        .child(
                                            view()
                                                .flex_row()
                                                .gap_2()
                                                .items_center()
                                                .flex_wrap()
                                                .child(text("本机 Bridge").text_color(rgb(MUTED)))
                                                .child(
                                                    button()
                                                        .on_press(move || {
                                                            bridge_notice.set(
                                                                match crate::local_bridge::start() {
                                                                    Ok(()) => String::new(),
                                                                    Err(error) => error.to_string(),
                                                                },
                                                            );
                                                            bridge_status
                                                                .set(crate::local_bridge::status());
                                                        })
                                                        .child(control().child(text("启动"))),
                                                )
                                                .child(
                                                    button()
                                                        .on_press(move || {
                                                            bridge_notice.set(
                                                                match crate::local_bridge::stop() {
                                                                    Ok(()) => String::new(),
                                                                    Err(error) => error.to_string(),
                                                                },
                                                            );
                                                            bridge_status
                                                                .set(crate::local_bridge::status());
                                                        })
                                                        .child(control().child(text("停止"))),
                                                ),
                                        )
                                        .child(
                                            text(move || bridge_status.get())
                                                .text_size(px(12.0))
                                                .text_color(rgb(MUTED)),
                                        )
                                        .child(when(
                                            move || !bridge_notice.get().is_empty(),
                                            move || {
                                                text(move || bridge_notice.get())
                                                    .text_size(px(12.0))
                                            },
                                        )),
                                )
                                .child(bridge_connections)
                                .child(link(
                                    "部署 Bridge 的多种方式",
                                    "https://github.com/tentacle-pro/stadia_wave#readme",
                                )),
                        )
                        .child(vertical_rule())
                        .child(
                            view()
                                .flex_1()
                                .gap_2()
                                .child(heading("Wave Companion", 20.0))
                                .child(dynamic(move || {
                                    row("语音识别", format!("本机 · {}", endpoint.get()))
                                }))
                                .child(dynamic(move || {
                                    row(
                                        "状态",
                                        if model.get().recording.is_some() {
                                            "正在录音"
                                        } else {
                                            "等待语音输入"
                                        }
                                        .into(),
                                    )
                                }))
                                .child(row("运行身份", identity.name.clone()))
                                .child(row("位置", shorten_path(&identity.path)))
                                .child(row("启动方", identity.launcher.clone()))
                                .child(
                                    view()
                                        .pt_3()
                                        .gap_2()
                                        .child(heading("系统权限", 16.0))
                                        .child(permission_row(
                                            "设备控制和数据访问",
                                            move || {
                                                if authorized.get() {
                                                    "已授权".into()
                                                } else {
                                                    "未授权".into()
                                                }
                                            },
                                            ACCESSIBILITY_SETTINGS,
                                            "",
                                        ))
                                        .child(permission_row(
                                            "本地网络",
                                            move || {
                                                if model.get().hardware.network_blocked {
                                                    "可能未授权（收不到手柄输入）".into()
                                                } else {
                                                    "首次连接手柄时询问".into()
                                                }
                                            },
                                            LOCAL_NETWORK_SETTINGS,
                                            "打开后在「隐私与安全性」列表里选「本地网络」",
                                        ))
                                        .child(when(
                                            move || driver.get().is_some(),
                                            move || {
                                                permission_row(
                                                    "驱动扩展",
                                                    move || {
                                                        if driver
                                                            .get()
                                                            .is_some_and(|state| state.enabled)
                                                        {
                                                            "已启用".into()
                                                        } else {
                                                            "等待启用".into()
                                                        }
                                                    },
                                                    LOGIN_ITEMS_SETTINGS,
                                                    "",
                                                )
                                            },
                                        ))
                                        .child(
                                            text(move || {
                                                match model.get().active_bridge.as_deref() {
                                                    Some(id) if id != crate::network::LOCAL_ID => {
                                                        "权限由 Bridge 所在电脑授予".to_string()
                                                    }
                                                    _ => "授权对象是上方「运行身份」".to_string(),
                                                }
                                            })
                                            .text_size(px(12.0))
                                            .text_color(rgb(MUTED)),
                                        ),
                                ),
                        ),
                ),
        )
        .child(rule())
        .child(
            view()
                .gap_4()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_4()
                        .child(heading("使用统计", 22.0))
                        .child(text("累计 · 本机").text_color(rgb(MUTED))),
                )
                .child(
                    view()
                        .flex_row()
                        .gap_4()
                        .child(metric("已接收录音", move || {
                            stats.get().received.to_string()
                        }))
                        .child(vertical_rule())
                        .child(metric("转写成功", move || {
                            stats.get().success.to_string()
                        }))
                        .child(vertical_rule())
                        .child(metric("转写失败", move || {
                            stats.get().failure.to_string()
                        }))
                        .child(vertical_rule())
                        .child(metric("录音时长", move || {
                            let seconds = stats.get().seconds.round() as u64;
                            format!("{} 分 {} 秒", seconds / 60, seconds % 60)
                        })),
                ),
        )
        .child(rule())
        .child(
            text("按住 Assistant 说话，松开后检查浮窗。A 粘贴，B 丢弃重说。")
                .text_color(rgb(MUTED)),
        )
}

fn metric(label: &'static str, value: impl Fn() -> String + 'static) -> impl IntoElement {
    view()
        .flex_1()
        .gap_1()
        .child(
            text(value)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(24.0)),
        )
        .child(text(label).text_color(rgb(MUTED)))
}

pub(crate) fn bridge_connections(
    model: Signal<ViewState>,
    selected: Signal<String>,
    auto_connect: Signal<bool>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
) -> impl IntoElement {
    let bridges = computed(move || model.get().bridges);
    let connection = computed(move || {
        let state = model.get();
        (
            state.active_bridge,
            state.connecting_bridge,
            state.pairing_required,
        )
    });
    let form = signal(None::<String>);
    let code = signal(String::new());
    let connect = {
        let commands = commands.clone();
        move |bridge: crate::network::Bridge, invitation_path: String| {
            auto_connect.set(false);
            selected.set(bridge.id.clone());
            let _ = Storage::set_item("bridge.selected", bridge.id.clone());
            let _ = Storage::set_item("bridge.autoconnect", "true");
            let _ = commands.send(Command::Connect {
                bridge,
                invitation_path,
            });
        }
    };
    let disconnect = move || {
        auto_connect.set(false);
        let _ = Storage::set_item("bridge.autoconnect", "false");
        let _ = commands.send(Command::Disconnect);
    };
    let disconnect_missing = disconnect.clone();
    view()
        .gap_2()
        .child(text("可用 Bridge").font_weight(FontWeight::SEMIBOLD))
        .child(text(move || model.get().status).text_size(px(12.0)).text_color(rgb(MUTED)))
        .child(when(
            move || bridges.get().is_empty(),
            || text("暂无扫描结果").text_size(px(12.0)).text_color(rgb(MUTED)),
        ))
        .child(dynamic(move || {
            bridges.get().into_iter().map(|bridge| {
                let id = bridge.id.clone();
                let local = id == crate::network::LOCAL_ID;
                let uses_code = bridge.short_pairing;
                let row_state = {
                    let id = id.clone();
                    move || {
                        let (active, pending, needs_pairing) = connection.get();
                        (active.as_ref() == Some(&id), pending.as_ref() == Some(&id),
                            needs_pairing.as_ref() == Some(&id))
                    }
                };
                let label = row_state.clone();
                let action = row_state.clone();
                let open = {
                    let id = id.clone();
                    move || form.get().as_ref() == Some(&id)
                };
                let open_for_label = open.clone();
                let connect_row = connect.clone();
                let connect_form = connect.clone();
                let disconnect_row = disconnect.clone();
                let target = bridge.clone();
                let submit = bridge.clone();
                view().gap_2().p_2().rounded_md().bg(rgb(SURFACE))
                    .child(view().flex_row().items_center().gap_2().flex_wrap()
                        .child(view().flex_1().gap_1()
                            .child(text(if local { "本机 Bridge".into() } else { bridge.name })
                                .font_weight(FontWeight::SEMIBOLD))
                            .child(text(if local { "Unix Socket".into() } else {
                                format!("{}:{}", bridge.address, bridge.port)
                            }).text_size(px(12.0)).text_color(rgb(MUTED))))
                        .child(button()
                            .on_press(move || {
                                let (active, pending, needs_pairing) = action();
                                let was_open = form.get().as_ref() == Some(&id);
                                form.set(None);
                                code.set(String::new());
                                if active || pending {
                                    disconnect_row();
                                } else if !local && needs_pairing {
                                    if !was_open { form.set(Some(id.clone())); }
                                } else {
                                    connect_row(target.clone(), String::new());
                                }
                            })
                            .child(control().bg(rgb(0xc3e6df)).child(text(move || {
                                match label() {
                                    (true, _, _) => "断开",
                                    (_, true, _) => "取消连接",
                                    (_, _, true) if open_for_label() => "收起",
                                    (_, _, true) => "配对",
                                    _ => "连接",
                                }
                            })))))
                    .child(when(open, move || {
                        let connect_form = connect_form.clone();
                        let submit = submit.clone();
                        view().gap_2()
                            .child(text(if uses_code {
                                "在 Bridge 主机运行 wave-bridge pair，输入终端显示的 8 位码。"
                            } else {
                                "输入 Bridge 提供的 pairing.json 路径。"
                            }).text_size(px(12.0)).text_color(rgb(MUTED)))
                            .child(input().value(move || code.get())
                                .placeholder(if uses_code { "8 位配对码" } else { "配对文件路径" })
                                .on_change(move |value: &str| code.set(value.to_owned()))
                                .h(px(36.0)).p_2().rounded_md().border(px(1.0)).border_color(rgb(RULE)))
                            .child(text("配对成功后，凭据将保存到此 Mac 的钥匙串，供下次连接。")
                                .text_size(px(12.0)).text_color(rgb(MUTED)))
                            .child(button()
                                .disabled(move || {
                                    let value = code.get();
                                    let value = value.trim();
                                    if uses_code {
                                        value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit())
                                    } else { value.is_empty() }
                                })
                                .on_press(move || {
                                    let value = code.get().trim().to_owned();
                                    code.set(String::new());
                                    form.set(None);
                                    connect_form(submit.clone(), value);
                                })
                                .child(control().bg(rgb(0xc3e6df)).child(text("配对并连接"))))
                    }))
            }).collect::<Vec<_>>()
        }))
        .child(when(move || {
            let (active, pending, _) = connection.get();
            let current = pending.or(active);
            current.is_some_and(|id| !bridges.get().iter().any(|bridge| bridge.id == id))
        }, move || {
            let disconnect = disconnect_missing.clone();
            button().on_press(disconnect).child(control().child(text("断开")))
        }))
}

pub(crate) fn asr_settings(
    provider: Signal<String>,
    endpoint: Signal<String>,
    api_key: Signal<String>,
    language: Signal<String>,
    dictation: Dictation,
) -> impl IntoElement {
    let picker = dictation.clone();
    let change_endpoint = dictation.clone();
    let detect = dictation.clone();
    let key_dictation = dictation.clone();
    let language_dictation = dictation.clone();
    let notice = signal(String::new());
    let key_notice = signal(String::new());
    view()
        .gap_3()
        .child(
            view()
                .gap_1()
                .child(heading("ASR 服务", 22.0))
                .child(
                    text(move || asr::provider_by_id(&provider.get()).summary.to_string())
                        .text_size(px(12.0))
                        .text_color(rgb(MUTED)),
                ),
        )
        .child(dynamic(move || {
            asr::PROVIDERS
                .iter()
                .map(|item| {
                    let selected = provider.get() == item.id;
                    let dictation = picker.clone();
                    button()
                        .on_press(move || {
                            provider.set(item.id.into());
                            let _ = Storage::set_item("asr.provider", item.id);
                            dictation.provider(item.id.into());
                            if asr::validate(item.id, &endpoint.get()).is_err() {
                                endpoint.set(String::new());
                                dictation.endpoint(String::new());
                                let _ = Storage::set_item("asr.endpoint", "");
                            }
                            let key = if item.secret {
                                asr::stored_api_key(item.id).unwrap_or_default()
                            } else {
                                String::new()
                            };
                            api_key.set(key.clone());
                            dictation.api_key(key);
                            notice.set(String::new());
                            key_notice.set(String::new());
                        })
                        .child(
                            control()
                                .bg(if selected { rgb(0xeaf2ef) } else { rgb(SURFACE) })
                                .child(
                                    text(item.label)
                                        .text_color(if selected { rgb(MINT) } else { rgb(INK) }),
                                ),
                        )
                })
                .collect::<Vec<_>>()
        }))
        .child(
            view()
                .gap_2()
                .child(
                    text(move || asr::provider_by_id(&provider.get()).address_label.to_string())
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            input()
                                .value(move || endpoint.get())
                                .placeholder(move || {
                                    asr::provider_by_id(&provider.get())
                                        .address_placeholder
                                        .to_string()
                                })
                                .on_change(move |value: &str| {
                                    endpoint.set(value.to_owned());
                                    change_endpoint.endpoint(value.to_owned());
                                    if asr::validate(&provider.get(), value).is_ok() {
                                        let _ = Storage::set_item("asr.endpoint", value.trim());
                                    }
                                })
                                .flex_1()
                                .h(px(38.0))
                                .p_2()
                                .rounded_md()
                                .border(px(1.0))
                                .border_color(rgb(RULE)),
                        )
                        .child(when(
                            move || asr::provider_by_id(&provider.get()).discovery,
                            move || {
                                let detect = detect.clone();
                                button()
                                    .on_press(move || {
                                        match asr::connector_for(&provider.get()).discover() {
                                            Some(found) => {
                                                endpoint.set(found.clone());
                                                detect.endpoint(found.clone());
                                                let _ = Storage::set_item("asr.endpoint", &found);
                                                notice.set(format!("已找到 {found}"));
                                            }
                                            None => {
                                                notice.set("未找到运行中的 qwen3_asr_server".into())
                                            }
                                        }
                                    })
                                    .child(control().child(text("检测")))
                            },
                        )),
                )
                .child(when(
                    move || !notice.get().is_empty(),
                    move || {
                        text(move || notice.get())
                            .text_size(px(13.0))
                            .text_color(rgb(MINT))
                    },
                ))
                .child(
                    text(move || {
                        if asr::provider_by_id(&provider.get()).discovery {
                            "端口由本机服务动态分配；点「检测」读取当前监听端口，或用 WAVE_ASR_URL 覆盖。"
                                .to_string()
                        } else {
                            "地址填到 API 根（含 /v1 也可以）；密钥保存在钥匙串。".to_string()
                        }
                    })
                    .text_size(px(13.0))
                    .text_color(rgb(MUTED)),
                ),
        )
        .child(when(
            move || asr::provider_by_id(&provider.get()).secret,
            move || {
                let dictation = key_dictation.clone();
                view()
                    .gap_2()
                    .pt_2()
                    .child(text("API key").font_weight(FontWeight::SEMIBOLD))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                input()
                                    .value(move || api_key.get())
                                    .placeholder("sk-…")
                                    .masked(true)
                                    .on_change(move |value: &str| {
                                        api_key.set(value.to_owned());
                                        dictation.api_key(value.to_owned());
                                    })
                                    .flex_1()
                                    .h(px(38.0))
                                    .p_2()
                                    .rounded_md()
                                    .border(px(1.0))
                                    .border_color(rgb(RULE)),
                            )
                            .child(
                                button()
                                    .on_press(move || {
                                        let id = provider.get();
                                        match asr::store_api_key(&id, &api_key.get()) {
                                            Ok(()) => {
                                                key_notice.set("密钥已保存到钥匙串".into())
                                            }
                                            Err(error) => key_notice.set(format!("{error:#}")),
                                        }
                                    })
                                    .child(control().child(text("保存密钥"))),
                            ),
                    )
                    .child(when(
                        move || !key_notice.get().is_empty(),
                        move || {
                            text(move || key_notice.get())
                                .text_size(px(13.0))
                                .text_color(rgb(MINT))
                        },
                    ))
            },
        ))
        .child(when(
            move || asr::provider_by_id(&provider.get()).language,
            move || {
                let dictation = language_dictation.clone();
                view()
                    .gap_2()
                    .pt_2()
                    .child(text("识别语言").font_weight(FontWeight::SEMIBOLD))
                    .child(
                        input()
                            .value(move || language.get())
                            .placeholder("留空自动识别")
                            .on_change(move |value: &str| {
                                language.set(value.to_owned());
                                dictation.language(value.to_owned());
                                let _ = Storage::set_item("asr.language", value.trim());
                            })
                            .w(px(180.0))
                            .h(px(38.0))
                            .p_2()
                            .rounded_md()
                            .border(px(1.0))
                            .border_color(rgb(RULE)),
                    )
            },
        ))
}

pub fn diagnostics(
    model: Signal<ViewState>,
    provider: Signal<String>,
    endpoint: Signal<String>,
    api_key: Signal<String>,
    language: Signal<String>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
) -> impl IntoElement {
    let result = signal(String::new());
    let busy = signal(false);
    let pending = Arc::new(Mutex::new(None::<String>));
    let poll = pending.clone();
    set_interval(std::time::Duration::from_millis(100), move || {
        if let Some(message) = poll.lock().unwrap().take() {
            result.set(message);
            busy.set(false);
        }
    });
    let health = pending.clone();
    let chart_width = signal(850.0_f32);
    let extensions = signal(desktop::driver_diagnostics());
    view()
        .gap_4()
        .text_size(px(14.0))
        .text_color(rgb(INK))
        .child(
            view()
                .gap_1()
                .child(heading("诊断", 33.0))
                .child(text("实时查看手柄输入与连接质量。").text_color(rgb(MUTED))),
        )
        .child(
            view()
                .flex_row()
                .gap_5()
                .child(
                    view()
                        .flex_1()
                        .gap_3()
                        .child(controller_monitor(model))
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .flex_wrap()
                                .child(
                                    text(move || {
                                        let held = model.get().held;
                                        if held.is_empty() {
                                            "当前无按键按下".into()
                                        } else {
                                            format!(
                                                "当前按下  {}",
                                                held.join(" · ").to_ascii_uppercase()
                                            )
                                        }
                                    })
                                    .text_color(rgb(MINT)),
                                )
                                .child(view().flex_1())
                                .child(
                                    button()
                                        .disabled(move || {
                                            !model
                                                .get()
                                                .hardware
                                                .controllers
                                                .iter()
                                                .any(|controller| controller.rumble_ready)
                                        })
                                        .on_press(move || {
                                            if let Some(controller) = model
                                                .get()
                                                .hardware
                                                .controllers
                                                .into_iter()
                                                .find(|controller| controller.rumble_ready)
                                            {
                                                let _ = commands.send(Command::Rumble {
                                                    controller: controller.id,
                                                });
                                            }
                                        })
                                        .child(control().child(text("测试震动"))),
                                ),
                        )
                        .child(when(
                            move || !model.get().command_result.is_empty(),
                            move || {
                                text(move || model.get().command_result)
                                    .text_size(px(12.0))
                                    .text_color(rgb(MUTED))
                            },
                        )),
                )
                .child(vertical_rule())
                .child(
                    view()
                        .w(px(315.0))
                        .gap_4()
                        .child(
                            view()
                                .gap_2()
                                .child(heading("录音接收", 22.0))
                                .child(
                                    text(move || {
                                        let state = model.get();
                                        if state.recording.is_some() {
                                            "正在录音，松开后接收"
                                        } else if state.audio.is_some() {
                                            "已完整接收"
                                        } else {
                                            "等待录音"
                                        }
                                    })
                                    .text_color(rgb(MINT)),
                                )
                                .child(
                                    text("按住 Assistant 说话，松开后接收。")
                                        .text_color(rgb(MUTED)),
                                )
                                .child(dynamic(move || {
                                    model.get().audio.map(|audio| {
                                        let folder =
                                            audio.path.parent().map(std::path::Path::to_path_buf);
                                        let recording = audio.path;
                                        view()
                                            .gap_2()
                                            .child(text(format!(
                                                "最近一次  {:.2} 秒 · Ogg Opus",
                                                audio.seconds
                                            )))
                                            .child(
                                                view()
                                                    .flex_row()
                                                    .gap_2()
                                                    .child(
                                                        button()
                                                            .on_press(move || {
                                                                let path = recording.clone();
                                                                spawn(async move {
                                                                    let _ = open_path(path).await;
                                                                });
                                                            })
                                                            .child(
                                                                control().child(text("播放录音")),
                                                            ),
                                                    )
                                                    .child(
                                                        button()
                                                            .on_press(move || {
                                                                if let Some(path) = folder.clone() {
                                                                    spawn(async move {
                                                                        let _ =
                                                                            open_path(path).await;
                                                                    });
                                                                }
                                                            })
                                                            .child(
                                                                control().child(text("打开位置")),
                                                            ),
                                                    ),
                                            )
                                    })
                                })),
                        )
                        .child(rule())
                        .child(
                            view()
                                .gap_2()
                                .child(heading("ASR 测试", 22.0))
                                .child(
                                    text(move || {
                                        format!(
                                            "{} · {}",
                                            asr::provider_by_id(&provider.get()).label,
                                            endpoint.get()
                                        )
                                    })
                                    .text_color(rgb(MUTED)),
                                )
                                .child(
                                    view()
                                        .flex_row()
                                        .gap_2()
                                        .child(
                                            button()
                                                .disabled(move || busy.get())
                                                .on_press(move || {
                                                    busy.set(true);
                                                    result.set("正在检查服务…".into());
                                                    let pending = health.clone();
                                                    let target = asr::Target {
                                                        provider: provider.get(),
                                                        endpoint: endpoint.get(),
                                                        api_key: api_key.get(),
                                                    };
                                                    std::thread::spawn(move || {
                                                        let message =
                                                            tokio::runtime::Runtime::new()
                                                                .map_err(anyhow::Error::from)
                                                                .and_then(|runtime| {
                                                                    runtime.block_on(asr::check(
                                                                        &target,
                                                                    ))
                                                                });
                                                        *pending.lock().unwrap() =
                                                            Some(message.unwrap_or_else(|error| {
                                                                format!("{error:#}")
                                                            }));
                                                    });
                                                })
                                                .child(control().child(text("检查服务"))),
                                        )
                                        .child(
                                            button()
                                                .disabled(move || {
                                                    busy.get() || model.get().audio.is_none()
                                                })
                                                .on_press(move || {
                                                    let Some(audio) = model.get().audio else {
                                                        return;
                                                    };
                                                    busy.set(true);
                                                    result.set("正在测试转写…".into());
                                                    let pending = pending.clone();
                                                    let target = asr::Target {
                                                        provider: provider.get(),
                                                        endpoint: endpoint.get(),
                                                        api_key: api_key.get(),
                                                    };
                                                    let language = language.get();
                                                    std::thread::spawn(move || {
                                                        let started = std::time::Instant::now();
                                                        let model =
                                                            std::env::var("WAVE_ASR_MODEL").ok();
                                                        let message =
                                                            tokio::runtime::Runtime::new()
                                                                .map_err(anyhow::Error::from)
                                                                .and_then(|runtime| {
                                                                    runtime.block_on(
                                                                        asr::transcribe(
                                                                            &target,
                                                                            model.as_deref(),
                                                                            &language,
                                                                            &audio.path,
                                                                        ),
                                                                    )
                                                                });
                                                        *pending.lock().unwrap() =
                                                            Some(match message {
                                                                Ok(value) => format!(
                                                                    "最近转写  {:.2} 秒\n{value}",
                                                                    started.elapsed().as_secs_f64()
                                                                ),
                                                                Err(error) => format!("{error:#}"),
                                                            });
                                                    });
                                                })
                                                .child(control().child(text("测试转写"))),
                                        ),
                                )
                                .child(when(
                                    move || !result.get().is_empty(),
                                    move || {
                                        view()
                                            .p_2()
                                            .rounded_md()
                                            .bg(rgb(0xeaf2ef))
                                            .child(text(move || result.get()))
                                    },
                                )),
                        ),
                ),
        )
        .child(rule())
        .child(
            view()
                .gap_2()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(heading("心跳与连接", 22.0))
                        .child(
                            text("最近 5 分钟 · 每 5 秒采样")
                                .text_size(px(12.0))
                                .text_color(rgb(MUTED)),
                        ),
                )
                .child(text(move || heartbeat_summary(&model.get().latency)).text_color(rgb(MUTED)))
                .child(
                    view()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(
                            view()
                                .w(px(58.0))
                                .h(px(110.0))
                                .justify_between()
                                .items_end()
                                .child(
                                    text(move || {
                                        format!("{:.0} ms", chart_max(&model.get().latency))
                                    })
                                    .text_size(px(11.0))
                                    .text_color(rgb(MUTED)),
                                )
                                .child(
                                    text(move || {
                                        format!("{:.0} ms", chart_max(&model.get().latency) / 2.0)
                                    })
                                    .text_size(px(11.0))
                                    .text_color(rgb(MUTED)),
                                )
                                .child(text("0 ms").text_size(px(11.0)).text_color(rgb(MUTED))),
                        )
                        .child(
                            view()
                                .flex_1()
                                .on_layout(move |area: &Bounds<Points>| {
                                    if chart_width.get() != area.size.width.0 {
                                        chart_width.set(area.size.width.0);
                                    }
                                })
                                .child(
                                    canvas(move || {
                                        heartbeat(&model.get().latency, chart_width.get())
                                    })
                                    .w_full()
                                    .h(px(110.0)),
                                ),
                        ),
                )
                .child(
                    view()
                        .flex_row()
                        .justify_between()
                        .pl(px(66.0))
                        .child(text("5 分钟前").text_size(px(11.0)).text_color(rgb(MUTED)))
                        .child(text("现在").text_size(px(11.0)).text_color(rgb(MUTED))),
                ),
        )
        .child(rule())
        .child(
            view()
                .gap_2()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(heading("系统扩展", 22.0))
                        .child(
                            text("systemextensionsctl list")
                                .text_size(px(12.0))
                                .text_color(rgb(MUTED)),
                        )
                        .child(view().flex_1())
                        .child(
                            button()
                                .on_press(move || {
                                    extensions.set(desktop::driver_diagnostics());
                                })
                                .child(control().child(text("刷新"))),
                        )
                        .child(
                            button()
                                .on_press(move || {
                                    let listing = extensions.get();
                                    spawn(async move {
                                        let _ = desktop::copy(&listing);
                                    });
                                })
                                .child(control().child(text("复制"))),
                        ),
                )
                .child(
                    text(move || extensions.get())
                        .text_size(px(12.0))
                        .text_color(rgb(MUTED)),
                ),
        )
}

const MONITOR_WIDTH: f32 = 500.0;
const PHOTO_WIDTH: f32 = 500.0;
const PHOTO_LEFT: f32 = 0.0;
const PHOTO_TOP: f32 = 42.0;

fn controller_monitor(model: Signal<ViewState>) -> impl IntoElement {
    let scale = signal(1.0_f32);
    let scaled = move |value| px(value * scale.get());
    view()
        .w_full()
        .items_center()
        .on_layout(move |area: &Bounds<Points>| {
            let factor = (area.size.width.0 / MONITOR_WIDTH).clamp(0.3, 1.0);
            if factor != scale.get() {
                scale.set(factor);
            }
        })
        .child(
            view()
                .relative()
                .w(move || scaled(MONITOR_WIDTH))
                .h(move || scaled(326.0))
                .child(
                    view()
                        .absolute()
                        .left(move || scaled(PHOTO_LEFT))
                        .top(move || scaled(PHOTO_TOP))
                        .w(move || scaled(PHOTO_WIDTH))
                        .h(move || scaled(PHOTO_WIDTH / 2.0))
                        .child(controller_image().w_full().h_full()),
                )
                .child(
                    canvas(move || input_overlay(&model.get(), scale.get()))
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .w_full()
                        .h_full(),
                )
                .child(
                    view()
                        .absolute()
                        .top(px(0.0))
                        .left(move || scaled(18.0))
                        .child(
                            text(move || {
                                let left = model.get().triggers[0];
                                format!("L2  {:.0}% · {left}/255", f32::from(left) / 2.55)
                            })
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD),
                        ),
                )
                .child(
                    view()
                        .absolute()
                        .top(px(0.0))
                        .right(move || scaled(18.0))
                        .child(
                            text(move || {
                                let right = model.get().triggers[1];
                                format!("R2  {:.0}% · {right}/255", f32::from(right) / 2.55)
                            })
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD),
                        ),
                )
                .child(stick_label(model, scale, 0, 115.0, "左摇杆"))
                .child(stick_label(model, scale, 2, 310.0, "右摇杆")),
        )
}

fn stick_label(
    model: Signal<ViewState>,
    scale: Signal<f32>,
    index: usize,
    left: f32,
    label: &'static str,
) -> impl IntoElement {
    view()
        .absolute()
        .left(move || px(left * scale.get()))
        .top(move || px(282.0 * scale.get()))
        .gap_1()
        .child(heading(label, 13.0))
        .child(
            text(move || {
                let axes = model.get().axes;
                format!(
                    "X {:+.2}   Y {:+.2}",
                    axes[index] as f32 / 32768.0,
                    axes[index + 1] as f32 / 32768.0
                )
            })
            .text_size(px(12.0))
            .text_color(rgb(MUTED)),
        )
}

fn chart_max(values: &[Option<f64>]) -> f64 {
    let peak = values.iter().flatten().copied().fold(10.0, f64::max);
    (peak / 10.0).ceil() * 10.0
}

fn heartbeat_summary(values: &[Option<f64>]) -> String {
    if values.is_empty() {
        return "等待心跳样本".into();
    }
    let mut received: Vec<f64> = values.iter().flatten().copied().collect();
    received.sort_by(f64::total_cmp);
    let median = if received.is_empty() {
        "—".into()
    } else {
        let middle = (received[received.len() / 2] + received[(received.len() - 1) / 2]) / 2.0;
        format!("{middle:.1} ms")
    };
    let current = values
        .last()
        .copied()
        .flatten()
        .map(|sample| format!("{sample:.1} ms"))
        .unwrap_or_else(|| "超时".into());
    format!(
        "当前 {current}    中位数 {median}    超时 {} 次",
        values.len() - received.len()
    )
}

fn heartbeat(values: &[Option<f64>], width: f32) -> Painter {
    let maximum = chart_max(values) as f32;
    let end = (width - 4.0).max(4.0);
    let mut drawing = Painter::new().stroke_style(rgb(RULE)).line_width(1.0);
    for y in [7.0, 55.0, 103.0] {
        drawing = drawing
            .begin_path()
            .move_to(4.0, y)
            .line_to(end, y)
            .stroke();
    }
    let mut previous = None;
    for (index, value) in values.iter().enumerate() {
        let slot = 60 - values.len() + index;
        let x = 4.0 + slot as f32 * (end - 4.0) / 59.0;
        if let Some(value) = value {
            let y = 103.0 - *value as f32 / maximum * 96.0;
            if let Some((px, py)) = previous {
                drawing = drawing
                    .stroke_style(rgb(MINT))
                    .line_width(1.5)
                    .begin_path()
                    .move_to(px, py)
                    .line_to(x, y)
                    .stroke();
            }
            drawing = drawing
                .fill_style(rgb(MINT))
                .begin_path()
                .arc(x, y, px(2.0), 0.0, std::f32::consts::TAU)
                .fill();
            previous = Some((x, y));
        } else {
            previous = None;
            drawing = drawing
                .stroke_style(rgb(0xb77722))
                .line_width(1.0)
                .begin_path()
                .move_to(x, 7.0)
                .line_to(x, 103.0)
                .stroke();
        }
    }
    drawing
}

fn input_overlay(state: &ViewState, view_scale: f32) -> Painter {
    let mut drawing = Painter::new()
        .scale(view_scale, view_scale)
        .stroke_style(rgb(0x72978c))
        .line_width(1.0)
        .begin_path()
        .move_to(55.0, 22.0)
        .line_to(96.0, 22.0)
        .line_to(126.0, 49.0)
        .move_to(445.0, 22.0)
        .line_to(404.0, 22.0)
        .line_to(374.0, 49.0)
        .move_to(137.0, 274.0)
        .line_to(183.0, 186.0)
        .move_to(349.0, 274.0)
        .line_to(316.0, 186.0)
        .stroke();
    for (value, x) in [(state.triggers[0], 30.0), (state.triggers[1], 470.0)] {
        let height = f32::from(value) / 255.0 * 42.0;
        drawing = drawing
            .stroke_style(rgb(0xc1d9cf))
            .line_width(7.0)
            .begin_path()
            .move_to(x, 26.0)
            .line_to(x, 68.0)
            .stroke()
            .stroke_style(rgb(MINT))
            .begin_path()
            .move_to(x, 68.0)
            .line_to(x, 68.0 - height)
            .stroke();
    }
    let scale = PHOTO_WIDTH / 1774.0;
    let left = PHOTO_LEFT;
    let top = PHOTO_TOP;
    for (name, x, y, radius) in [
        ("a", 1278.0, 330.0, 45.0),
        ("b", 1368.0, 239.0, 45.0),
        ("x", 1187.0, 239.0, 45.0),
        ("y", 1275.0, 147.0, 45.0),
        ("dpad_up", 497.0, 172.0, 36.0),
        ("dpad_down", 497.0, 313.0, 36.0),
        ("dpad_left", 421.0, 242.0, 36.0),
        ("dpad_right", 574.0, 242.0, 36.0),
        ("assistant", 787.0, 240.0, 28.0),
        ("options", 725.0, 142.0, 25.0),
        ("menu", 1048.0, 142.0, 25.0),
        ("capture", 986.0, 240.0, 28.0),
        ("stadia", 887.0, 427.0, 46.0),
        ("l1", 482.0, 28.0, 25.0),
        ("r1", 1290.0, 28.0, 25.0),
        ("l3", 668.0, 426.0, 76.0),
        ("r3", 1100.0, 426.0, 76.0),
    ] {
        if state
            .held
            .iter()
            .any(|held| held.eq_ignore_ascii_case(name))
        {
            drawing = drawing
                .stroke_style(rgb(0x21ad89))
                .line_width(3.0)
                .begin_path()
                .arc(
                    left + x * scale,
                    top + y * scale,
                    px(radius * scale),
                    0.0,
                    std::f32::consts::TAU,
                )
                .stroke();
        }
    }
    for (index, center) in [(0, 668.0), (2, 1100.0)] {
        let cx = left + center * scale;
        let cy = top + 426.0 * scale;
        let x = cx + state.axes[index] as f32 / 32768.0 * 21.0;
        // The controller reports positive Y up, while the canvas grows downward.
        let y = cy - state.axes[index + 1] as f32 / 32768.0 * 21.0;
        drawing = drawing
            .stroke_style(rgb(0xffffff))
            .line_width(2.2)
            .begin_path()
            .move_to(cx, cy)
            .line_to(x, y)
            .stroke()
            .fill_style(rgb(0xffffff))
            .begin_path()
            .arc(x, y, px(3.0), 0.0, std::f32::consts::TAU)
            .fill();
    }
    drawing
}

fn node_icon(laptop: bool) -> impl IntoElement {
    let mut drawing = Painter::new().stroke_style(rgb(0x253b33)).line_width(2.0);
    if laptop {
        drawing = drawing
            .begin_path()
            .rect(bounds(point(px(24.0), px(12.0)), size(px(64.0), px(40.0))))
            .stroke()
            .begin_path()
            .move_to(15.0, 58.0)
            .line_to(97.0, 58.0)
            .stroke();
    } else {
        for y in [12.0, 38.0] {
            drawing = drawing
                .begin_path()
                .rect(bounds(point(px(24.0), px(y)), size(px(64.0), px(20.0))))
                .stroke();
        }
    }
    canvas(drawing).w(px(112.0)).h(px(74.0))
}

pub(crate) fn nav_icon(index: usize, selected: bool) -> impl IntoElement {
    let color = rgb(if selected { MINT } else { INK });
    let drawing = Painter::new()
        .stroke_style(color)
        .line_width(1.5)
        .begin_path();
    let drawing = match index {
        0 => drawing
            .move_to(2.0, 9.0)
            .line_to(10.0, 2.0)
            .line_to(18.0, 9.0)
            .move_to(4.0, 8.0)
            .line_to(4.0, 17.0)
            .line_to(8.0, 17.0)
            .line_to(8.0, 12.0)
            .line_to(12.0, 12.0)
            .line_to(12.0, 17.0)
            .line_to(16.0, 17.0)
            .line_to(16.0, 8.0),
        1 => drawing
            .move_to(6.0, 6.0)
            .line_to(14.0, 6.0)
            .quadratic_curve_to(point(px(17.0), px(6.0)), point(px(18.5), px(13.0)))
            .quadratic_curve_to(point(px(19.0), px(17.0)), point(px(14.0), px(13.0)))
            .line_to(6.0, 13.0)
            .quadratic_curve_to(point(px(1.0), px(17.0)), point(px(1.5), px(13.0)))
            .quadratic_curve_to(point(px(3.0), px(6.0)), point(px(6.0), px(6.0)))
            .move_to(4.5, 9.5)
            .line_to(8.5, 9.5)
            .move_to(6.5, 7.5)
            .line_to(6.5, 11.5)
            .move_to(13.5, 9.5)
            .line_to(15.0, 9.5),
        2 => drawing
            .arc(10.0, 10.0, px(3.0), 0.0, std::f32::consts::TAU)
            .move_to(8.0, 2.0)
            .line_to(12.0, 2.0)
            .line_to(13.0, 5.0)
            .line_to(16.0, 5.0)
            .line_to(18.0, 9.0)
            .line_to(16.0, 11.0)
            .line_to(16.0, 15.0)
            .line_to(12.0, 17.0)
            .line_to(9.0, 15.0)
            .line_to(6.0, 17.0)
            .line_to(3.0, 14.0)
            .line_to(4.0, 11.0)
            .line_to(2.0, 9.0)
            .line_to(4.0, 5.0)
            .line_to(7.0, 5.0)
            .line_to(8.0, 2.0),
        4 => drawing
            .arc(10.0, 10.0, px(8.0), 0.0, std::f32::consts::TAU)
            .move_to(10.0, 9.0)
            .line_to(10.0, 14.0)
            .move_to(10.0, 6.0)
            .line_to(10.0, 7.0),
        _ => drawing
            .move_to(1.0, 10.0)
            .line_to(5.0, 10.0)
            .line_to(8.0, 4.0)
            .line_to(11.0, 16.0)
            .line_to(14.0, 10.0)
            .line_to(19.0, 10.0),
    };
    canvas(drawing.stroke()).w(px(20.0)).h(px(20.0)).flex_none()
}
