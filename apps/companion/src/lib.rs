mod asr;
mod desktop;
mod dictation;
mod local_bridge;
mod mapping;
mod mapping_page;
pub mod network;
mod pages;
mod statistics;

use base64::Engine;
use nsh_base::button;
use nsh_core::desktop::{WindowAttributes, WindowKind, WindowPosition};
use nsh_core::element::desktop::{
    MenuItem, current_window, on_app_quit, quit, set_tray, tray, window as desktop_window,
};
use nsh_core::element::{ImageSrc, image};
use nsh_core::prelude::*;
use nsh_core::storage::Storage;

use std::time::Duration;

#[native_module("wave-companion/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

#[component]
pub fn App() -> impl IntoElement {
    let preview_position = nsh_core::element::desktop::displays()
        .into_iter()
        .find(|display| display.primary)
        .map(|display| {
            WindowPosition::At(Point::new(
                display.visible_bounds.origin.x
                    + (display.visible_bounds.size.width - px(560.0)) / 2.0,
                display.visible_bounds.origin.y,
            ))
        })
        .unwrap_or(WindowPosition::Center);
    let page = signal(0usize);
    let mapping_dirty = signal(false);
    let mapping_notice = signal(String::new());
    let update_notice = signal(String::new());
    let main_window = current_window();
    let mut window_options = WindowAttributes::default();
    window_options.close_on_press = Some(false);
    main_window.set(window_options);
    let hide_window = main_window.clone();
    main_window.on_close_press(move |_| hide_window.hide());
    let dock_visible = computed({
        let main_window = main_window.clone();
        move || {
            main_window
                .state()
                .is_none_or(|state| state.visible || state.minimized)
        }
    });
    effect(move || desktop::set_dock_visible(dock_visible.get()));
    on_app_quit(|| async {
        mapping::pause(true);
        let (done, result) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = done.send(local_bridge::stop());
        });
        match result.await {
            Ok(Ok(())) => {}
            result => log::warn!("Could not stop the local Bridge on quit: {result:?}"),
        }
    });
    let show_window = main_window.clone();
    let tray_png =
        base64::engine::general_purpose::STANDARD.encode(include_bytes!("../assets/menubar.png"));
    let wave_mark = ImageSrc::from(format!("data:image/png;base64,{tray_png}"));
    let about_mark = wave_mark.clone();
    set_tray(tray().icon(wave_mark.clone()).tooltip("Stadia Wave").menu([
        MenuItem::new("打开 Wave Companion").on_select(move || {
            desktop::set_dock_visible(true);
            show_window.focus();
        }),
        MenuItem::new("退出 Stadia Wave").on_select(quit),
    ]));
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
    let selected = signal(Storage::get_item("bridge.selected").unwrap_or_default());
    let auto_connect = signal(Storage::get_item("bridge.autoconnect").as_deref() == Some("true"));
    let reconnect = worker.commands.clone();
    let path = signal(String::new());
    let shared = worker.state;
    mapping::start(shared.clone(), dictation.clone());
    set_interval(Duration::from_millis(150), move || {
        let (next, draft, notice) = {
            let view = shared.lock().unwrap();
            let (draft, notice) = poll.update_desktop(view.active_bridge.as_deref());
            (view.clone(), draft, notice)
        };
        if auto_connect.get()
            && let Some(bridge) = next
                .bridges
                .iter()
                .find(|bridge| bridge.id == selected.get())
        {
            let _ = reconnect.send(network::Command::Restore {
                bridge: bridge.clone(),
            });
            auto_connect.set(false);
        }
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
    let settings_commands = worker.commands.clone();
    let rumble = worker.commands.clone();
    view()
        .flex_1()
        .bg(rgb(pages::SURFACE))
        .text_color(rgb(pages::INK))
        .child(
            desktop_window()
                .id("dictation-preview")
                .kind(WindowKind::Panel)
                .title("Stadia Wave")
                .default_size(Size::new(px(560.0), px(230.0)))
                .default_position(preview_position)
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
                        let copy = actions.clone();
                        let retry = actions.clone();
                        view()
                            .flex_1()
                            .p_4()
                            .gap_3()
                            .bg(rgb(0xf7f9f6))
                            .text_color(rgb(0x202b28))
                            .child(
                                text(move || match panel.get() {
                                    Some((_, dictation::Phase::Ready, _)) => "A 粘贴 · B 丢弃",
                                    Some((_, dictation::Phase::Error(_), _)) => {
                                        "转写失败 · 可在主窗口重试"
                                    }
                                    Some(_) => "正在转写…",
                                    None => "正在录音…",
                                })
                                .text_size(px(16.0)),
                            )
                            .child(
                                scroll().h(px(92.0)).child(
                                    text(move || match panel.get() {
                                        Some((_, dictation::Phase::Error(error), _)) => error,
                                        _ => draft_text.get(),
                                    })
                                    .text_size(px(18.0))
                                    .text_color(rgb(0x202b28)),
                                ),
                            )
                            .child(text(move || dictation_notice.get()).text_size(px(12.0)))
                            .child(
                                view()
                                    .flex_row()
                                    .gap_3()
                                    .child(
                                        button()
                                            .p_2()
                                            .on_press(move || {
                                                if let Some((id, _, _)) = panel.get() {
                                                    paste.confirm(&id);
                                                }
                                            })
                                            .child(text("粘贴")),
                                    )
                                    .child(
                                        button()
                                            .p_2()
                                            .on_press(move || {
                                                if let Some((id, _, _)) = panel.get() {
                                                    discard.discard(&id);
                                                }
                                            })
                                            .child(text("丢弃")),
                                    )
                                    .child(
                                        button()
                                            .p_2()
                                            .on_press(move || {
                                                if let Some((id, _, _)) = panel.get() {
                                                    copy.copy(&id);
                                                }
                                            })
                                            .child(text("复制")),
                                    )
                                    .child(
                                        button()
                                            .p_2()
                                            .on_press(move || {
                                                if let Some((id, _, _)) = panel.get() {
                                                    retry.retry(&id);
                                                }
                                            })
                                            .child(text("重试转写")),
                                    ),
                            )
                    }
                }),
        )
        .child(
            view()
                .flex_row()
                .flex_1()
                .child(
                    view()
                        .w(px(206.0))
                        .flex_none()
                        .p_3()
                        .pt_6()
                        .gap_2()
                        .bg(rgb(0xe0f0ed))
                        .text_color(rgb(pages::INK))
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .p_2()
                                .pb_5()
                                .child(image(wave_mark).w(px(24.0)).h(px(24.0)).flex_none())
                                .child(
                                    text("Stadia Wave")
                                        .text_size(px(20.0))
                                        .font_weight(FontWeight::SEMIBOLD),
                                ),
                        )
                        .children(
                            ["概览", "按键映射", "设置", "诊断", "关于"]
                                .into_iter()
                                .enumerate()
                                .map(|(index, label)| {
                                    button()
                                        .selected(move || page.get() == index)
                                        .on_press(move || {
                                            if page.get() == 1 && mapping_dirty.get() && index != 1
                                            {
                                                mapping_notice
                                                    .set("请先保存或放弃修改，再切换页面。".into());
                                            } else {
                                                page.set(index);
                                            }
                                        })
                                        .child(
                                            view()
                                                .flex_row()
                                                .items_center()
                                                .gap_3()
                                                .px_3()
                                                .h(px(40.0))
                                                .rounded_md()
                                                .bg(move || {
                                                    rgb(if page.get() == index {
                                                        0xc3e6df
                                                    } else {
                                                        0xe0f0ed
                                                    })
                                                })
                                                .child(dynamic(move || {
                                                    pages::nav_icon(index, page.get() == index)
                                                }))
                                                .child(text(label).text_size(px(15.0))),
                                        )
                                })
                                .collect::<Vec<_>>(),
                        )
                        .child(view().flex_1())
                        .child(
                            view()
                                .flex_row()
                                .gap_2()
                                .items_center()
                                .justify_between()
                                .pt_3()
                                .child(
                                    text(concat!("版本 ", env!("CARGO_PKG_VERSION")))
                                        .text_size(px(12.0)),
                                )
                                .child(
                                    button()
                                        .on_press(move || {
                                            update_notice.set("尚未配置更新发布来源。".into());
                                            if page.get() == 1 && mapping_dirty.get() {
                                                mapping_notice
                                                    .set("请先保存或放弃修改，再切换页面。".into());
                                            } else {
                                                page.set(4);
                                            }
                                        })
                                        .child(
                                            pages::control()
                                                .child(text("检查更新").text_size(px(11.0))),
                                        ),
                                ),
                        ),
                )
                .child(
                    scroll().flex_1().child(
                        view()
                            .p_6()
                            .gap_4()
                            .child(when(
                                move || page.get() == 0,
                                move || pages::overview(model, endpoint),
                            ))
                            .child(when(
                                move || page.get() == 1,
                                move || mapping_page::page(model, mapping_dirty, mapping_notice),
                            ))
                            .child(when(
                                move || page.get() == 2,
                                move || {
                                    view()
                                        .gap_5()
                                        .child(
                                            text("设置")
                                                .text_size(px(33.0))
                                                .font_weight(FontWeight::SEMIBOLD),
                                        )
                                        .child(pages::bridge_settings(
                                            model,
                                            selected,
                                            path,
                                            auto_connect,
                                            settings_commands.clone(),
                                        ))
                                        .child(pages::rule())
                                        .child(pages::asr_settings(
                                            endpoint,
                                            language,
                                            dictation.clone(),
                                        ))
                                },
                            ))
                            .child(when(
                                move || page.get() == 3,
                                move || {
                                    pages::diagnostics(model, endpoint, language, rumble.clone())
                                },
                            ))
                            .child(when(
                                move || page.get() == 4,
                                move || pages::about(about_mark.clone(), update_notice),
                            )),
                    ),
                ),
        )
}
