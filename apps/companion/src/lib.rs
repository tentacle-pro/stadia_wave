mod asr;
mod desktop;
mod dictation;
pub mod network;

use nsh_base::{button, theme};
use nsh_core::desktop::{WindowAttributes, WindowKind};
use nsh_core::element::desktop::window as desktop_window;
use nsh_core::element::input;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;

use std::time::Duration;

#[native_module("wave-companion/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

#[component]
pub fn App() -> impl IntoElement {
    let endpoint = signal(
        std::env::var("WAVE_ASR_URL")
            .ok()
            .or_else(|| Storage::get_item("asr.endpoint"))
            .unwrap_or_else(|| dictation::DEFAULT_ENDPOINT.into()),
    );
    let language = signal(
        std::env::var("WAVE_ASR_LANGUAGE")
            .ok()
            .or_else(|| Storage::get_item("asr.language"))
            .unwrap_or_else(|| "zh".into()),
    );
    let dictation = dictation::Dictation::start(endpoint.get(), language.get());
    let worker = network::start_for_desktop(Some(dictation.clone()));
    let panel = signal(None::<(String, dictation::Phase, Option<String>)>);
    let draft_text = signal(String::new());
    let dictation_notice = signal(String::new());
    let poll = dictation.clone();
    let model = signal(network::ViewState::default());
    let selected = signal(String::new());
    let path = signal(String::new());
    let shared = worker.state;
    set_interval(Duration::from_millis(150), move || {
        let (next, draft, notice) = {
            let view = shared.lock().unwrap();
            let (draft, notice) = poll.update_desktop(view.active_bridge.as_deref());
            (view.clone(), draft, notice)
        };
        let draft_value = draft
            .as_ref()
            .map(|draft| draft.text.as_str())
            .unwrap_or_default();
        if draft_text.get() != draft_value {
            draft_text.set(draft_value.to_owned());
        }
        let summary = draft.map(|draft| {
            (
                draft.id().to_owned(),
                draft.phase.clone(),
                draft.target_name().map(str::to_owned),
            )
        });
        if summary != panel.get() {
            panel.set(summary);
        }
        if notice != dictation_notice.get() {
            dictation_notice.set(notice);
        }
        if next != model.get() {
            model.set(next);
        }
    });
    let connect = worker.commands.clone();
    let rumble = worker.commands.clone();
    let disconnect = worker.commands;
    view()
        .flex_1()
        .bg(move || theme().background)
        .text_color(move || theme().foreground)
        .child(desktop_window()
            .id("dictation-preview")
            .kind(WindowKind::Panel)
            .title("Stadia Wave")
            .default_size(Size::new(px(440.0), px(240.0)))
            .attributes({
                let mut attributes = WindowAttributes::default();
                attributes.activate = Some(false);
                attributes.focusable = Some(false);
                attributes.resizable = Some(false);
                attributes.closable = Some(false);
                attributes
            })
            .open(move || panel.get().is_some() || model.get().recording.is_some())
            .content({
                let actions = dictation.clone();
                move || {
                    let paste = actions.clone();
                    let discard = actions.clone();
                    view().p_4().gap_3()
                        .bg(move || theme().background)
                        .text_color(move || theme().foreground)
                        .child(text(move || match panel.get() {
                            Some((_, dictation::Phase::Ready, _)) => "A 粘贴 · B 丢弃",
                            Some((_, dictation::Phase::Error(_), _)) => "转写失败 · 可在主窗口重试",
                            Some(_) => "正在转写…",
                            None => "正在录音…",
                        }).text_size(px(16.0)))
                        .child(scroll().flex_1().child(text(move || match panel.get() {
                            Some((_, dictation::Phase::Error(error), _)) => error,
                            _ => draft_text.get(),
                        })))
                        .child(text(move || dictation_notice.get()).text_size(px(12.0)))
                        .child(view().flex_row().gap_3()
                            .child(button().p_2().on_press(move || {
                                if let Some((id, _, _)) = panel.get() { paste.confirm(&id); }
                            }).child(text("粘贴")))
                            .child(button().p_2().on_press(move || {
                                if let Some((id, _, _)) = panel.get() { discard.discard(&id); }
                            }).child(text("丢弃")))
                            .child(text("编辑草稿请切回 Companion").text_size(px(12.0))))
                }
            }))
        .child(
            scroll().flex_1().child(
                view().p_6().gap_4()
                    .child(text("Wave Companion").text_size(px(28.0)))
                    .child(text(move || model.get().status).text_size(px(15.0)))
                    .child(text(move || model.get().hardware.message).text_size(px(14.0)))
                    .child(dynamic(move || {
                        let commands = rumble.clone();
                        model.get().hardware.controllers.into_iter().map(move |controller| {
                            let commands = commands.clone();
                            let label = format!("{} · {} · 电量 {}",
                                controller.id,
                                if controller.ready { "已就绪" } else { "等待连接" },
                                controller.battery_percent.map(|v| format!("{v}%")).unwrap_or_else(|| "未知".into()));
                            view().gap_2()
                                .child(text(label))
                                .child(button().p_2()
                                    .on_press(move || { let _ = commands.send(network::Command::Rumble { controller: controller.id.clone() }); })
                                    .child(text("测试震动")))
                        }).collect::<Vec<_>>()
                    }))
                    .child(text(move || model.get().input).text_size(px(13.0)))
                    .child(text(move || if model.get().recording.is_some() {
                        "正在录音，松开 Assistant 结束"
                    } else { "按住 Assistant 说话，等手柄灯闪后开始。" }).text_size(px(14.0)))
                    .child(text(move || dictation_notice.get()).text_size(px(14.0)))
                    .child(dynamic({
                        let dictation = dictation.clone();
                        move || panel.get().map(|(id, phase, target)| render_draft(dictation.clone(), id, phase, target, draft_text))
                    }))
                    .child(text(move || model.get().command_result).text_size(px(13.0)))
                    .child(dynamic(move || {
                        model.get().audio.map(|audio| {
                            let path = audio.path.clone();
                            view().gap_2()
                                .child(text(format!("已收到录音 · {:.2} 秒", audio.seconds)))
                                .child(button().p_2().on_press(move || {
                                    let path = path.clone();
                                    spawn(async move { let _ = open_path(path).await; });
                                }).child(text("打开录音")))
                        })
                    }))
                    .child(text("本机语音识别").text_size(px(18.0)))
                    .child(input().value(move || endpoint.get()).h(px(38.0)).p_2()
                        .placeholder("http://127.0.0.1:58078")
                        .on_change({
                            let dictation = dictation.clone();
                            move |value: &str| {
                                endpoint.set(value.to_owned());
                                dictation.endpoint(value.to_owned());
                                if asr::local_endpoint(value).is_ok() {
                                    let _ = Storage::set_item("asr.endpoint", value.trim());
                                }
                            }
                        }))
                    .child(view().flex_row().gap_3()
                        .child(text("识别语言").text_size(px(13.0)))
                        .child(input().value(move || language.get()).h(px(32.0)).w(px(160.0)).p_2()
                            .placeholder("留空自动识别")
                            .on_change(move |value: &str| {
                                language.set(value.to_owned());
                                dictation.language(value.to_owned());
                                let _ = Storage::set_item("asr.language", value.trim());
                            })))
                    .child(text("Voxt 重启后端口可能变化。").text_size(px(13.0)))
                    .child(text("局域网中的 Bridge").text_size(px(18.0)))
                    .child(dynamic(move || {
                        model.get().bridges.into_iter().map(move |bridge| {
                            let id = bridge.id.clone();
                            button()
                                .p_3()
                                .bg(move || if selected.get() == id { theme().secondary } else { theme().background })
                                .on_press(move || selected.set(bridge.id.clone()))
                                .child(text(format!("{}  ·  {}:{}", bridge.name, bridge.address, bridge.port)))
                        }).collect::<Vec<_>>()
                    }))
                    .child(text(move || if model.get().bridges.is_empty() {
                        "正在查找。请启动 Bridge，并允许局域网访问。"
                    } else { "选择一个 Bridge，然后连接。" }))
                    .child(text("首次配对文件路径"))
                    .child(input()
                        .value(move || path.get())
                        .placeholder("/…/bridge/pairing.json")
                        .on_change(move |value: &str| path.set(value.to_owned()))
                        .h(px(38.0)).p_2())
                    .child(text("使用 Bridge 启动时显示的 pairing.json 路径。配对后凭据保存在 Keychain。").text_size(px(13.0)))
                    .child(view().flex_row().gap_3()
                        .child(button().p_3().bg(move || theme().primary)
                            .on_press(move || {
                                if let Some(bridge) = model.get().bridges.into_iter().find(|bridge| bridge.id == selected.get()) {
                                    let _ = connect.send(network::Command::Connect { bridge, invitation_path: path.get() });
                                }
                            })
                            .child(text("连接").text_color(move || theme().primary_foreground)))
                        .child(button().p_3().on_press(move || { let _ = disconnect.send(network::Command::Disconnect); }).child(text("断开"))))
                    .child(text(move || format!("已确认的心跳  {}", model.get().heartbeats)).text_size(px(13.0)))
            )
        )
}

fn render_draft(
    dictation: dictation::Dictation,
    id: String,
    phase: dictation::Phase,
    target: Option<String>,
    draft_text: Signal<String>,
) -> View {
    let discard = dictation.clone();
    let discard_id = id.clone();
    let base = view().gap_2().p_3().bg(move || theme().secondary);
    let body = match phase {
        dictation::Phase::Working => base.child(text("正在本机转写…")),
        dictation::Phase::Error(error) => base.child(text(format!("转写失败 · {error}"))).child(
            button()
                .p_2()
                .on_press(move || dictation.retry(&id))
                .child(text("重试转写")),
        ),
        dictation::Phase::Ready => {
            let edit = dictation.clone();
            let edit_id = id.clone();
            let paste = dictation.clone();
            let paste_id = id.clone();
            base.child(text("转写草稿").text_size(px(18.0)))
                .child(text(
                    target
                        .map(|name| format!("粘贴到 {name} · A 确认，B 丢弃"))
                        .unwrap_or_else(|| "没有记录到目标应用，可复制草稿 · B 丢弃".into()),
                ))
                .child(
                    input()
                        .multiline(true)
                        .value(move || draft_text.get())
                        .h(px(118.0))
                        .p_2()
                        .bg(move || theme().background)
                        .on_change(move |value: &str| {
                            edit.edit(&edit_id, value);
                            draft_text.set(value.to_owned());
                        }),
                )
                .child(
                    view()
                        .flex_row()
                        .gap_3()
                        .child(
                            button()
                                .p_2()
                                .on_press(move || paste.confirm(&paste_id))
                                .child(text("粘贴")),
                        )
                        .child(
                            button()
                                .p_2()
                                .on_press(move || dictation.copy(&id))
                                .child(text("复制")),
                        ),
                )
        }
    };
    body.child(
        button()
            .p_2()
            .on_press(move || discard.discard(&discard_id))
            .child(text("丢弃")),
    )
}
