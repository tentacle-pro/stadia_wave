use anyhow::{Context, Result, bail, ensure};
use objc2::{MainThreadMarker, rc::autoreleasepool};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSPasteboard,
    NSPasteboardTypeString, NSRunningApplication, NSWorkspace,
};
use objc2_core_graphics::{CGEvent, CGEventFlags};
use objc2_foundation::NSString;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pid: i32,
    launched: f64,
    pub name: String,
}

pub fn frontmost_target() -> Option<Target> {
    autoreleasepool(|_| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let pid = app.processIdentifier();
        if pid <= 0 || pid as u32 == std::process::id() {
            return None;
        }
        Some(Target {
            pid,
            launched: app.launchDate()?.timeIntervalSinceReferenceDate(),
            name: app.localizedName()?.to_string(),
        })
    })
}

pub fn set_dock_visible(visible: bool) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        if app.activationPolicy() != policy && !app.setActivationPolicy(policy) {
            log::warn!("AppKit refused to change the Dock visibility");
        }
    });
}

pub fn authorized() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn copy(text: &str) -> Result<()> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    ensure!(
        pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }),
        "无法写入剪贴板"
    );
    Ok(())
}

impl Target {
    pub fn prepare(&self) -> Result<()> {
        ensure!(
            unsafe { AXIsProcessTrusted() },
            "请在「系统设置 → 隐私与安全性 → 辅助功能」启用 Wave Companion，或先使用复制"
        );
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(self.pid)
            .context("录音时的应用已关闭")?;
        ensure!(
            app.launchDate()
                .is_some_and(|date| date.timeIntervalSinceReferenceDate() == self.launched),
            "录音时的应用已重新启动"
        );
        let front = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .context("无法确认前台应用")?
            .processIdentifier();
        if front == self.pid {
            return Ok(());
        }
        if front as u32 != std::process::id() {
            bail!("请切回 {} 后再确认粘贴", self.name);
        }
        ensure!(
            app.activateWithOptions(NSApplicationActivationOptions::empty()),
            "无法切回 {}，请手动切回后再确认粘贴",
            self.name
        );
        Ok(())
    }

    pub fn paste_if_frontmost(&self, text: &str) -> Result<bool> {
        if frontmost_target().as_ref() != Some(self) {
            return Ok(false);
        }
        ensure!(unsafe { AXIsProcessTrusted() }, "辅助功能权限不可用");
        let mut events = Vec::with_capacity(2);
        for down in [true, false] {
            let event =
                CGEvent::new_keyboard_event(None, 9, down).context("无法创建 macOS 粘贴事件")?;
            CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
            events.push(event);
        }
        copy(text)?;
        for event in &events {
            CGEvent::post_to_pid(self.pid, Some(event));
        }
        Ok(true)
    }
}

// Mapping events share one output-key state so held modifiers accompany mouse
// movement and repeated keyboard events consistently.
#[cfg(not(test))]
pub fn mapping_key(code: u16, down: bool, held: &std::collections::BTreeSet<u16>) {
    use objc2_core_graphics::CGEventTapLocation;
    if let Some(event) = CGEvent::new_keyboard_event(None, code, down) {
        let mut flags = CGEventFlags::empty();
        // AppKit identifies arrow/navigation keys with the secondary Fn flag.
        // System symbolic shortcuts include that flag when matching arrows.
        if matches!(code, 115..=117 | 119 | 121 | 123..=126) {
            flags |= CGEventFlags::MaskSecondaryFn;
        }
        for (key, flag) in [
            (55, CGEventFlags::MaskCommand),
            (56, CGEventFlags::MaskShift),
            (58, CGEventFlags::MaskAlternate),
            (59, CGEventFlags::MaskControl),
        ] {
            if (held.contains(&key) && (key != code || down)) || (key == code && down) {
                flags |= flag;
            }
        }
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}
#[cfg(not(test))]
pub fn mapping_mouse_button(button: usize, down: bool) {
    use objc2_core_graphics::{CGEventTapLocation, CGEventType, CGMouseButton};
    if let Some(current) = CGEvent::new(None) {
        if let Some(event) = CGEvent::new_mouse_event(
            None,
            match (button, down) {
                (0, true) => CGEventType::LeftMouseDown,
                (0, false) => CGEventType::LeftMouseUp,
                (_, true) => CGEventType::RightMouseDown,
                (_, false) => CGEventType::RightMouseUp,
            },
            CGEvent::location(Some(&current)),
            if button == 0 {
                CGMouseButton::Left
            } else {
                CGMouseButton::Right
            },
        ) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
}
#[cfg(not(test))]
pub fn mapping_move(dx: f64, dy: f64, buttons: [bool; 2]) {
    use objc2_core_graphics::{CGEventTapLocation, CGEventType, CGMouseButton};
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    if let Some(current) = CGEvent::new(None) {
        let mut point = CGEvent::location(Some(&current));
        point.x += dx;
        point.y += dy;
        if let Some(event) = CGEvent::new_mouse_event(
            None,
            if buttons[0] {
                CGEventType::LeftMouseDragged
            } else if buttons[1] {
                CGEventType::RightMouseDragged
            } else {
                CGEventType::MouseMoved
            },
            point,
            if buttons[1] && !buttons[0] {
                CGMouseButton::Right
            } else {
                CGMouseButton::Left
            },
        ) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
}
#[cfg(not(test))]
pub fn mapping_scroll(dx: i32, dy: i32) {
    use objc2_core_graphics::{CGEventTapLocation, CGScrollEventUnit};
    if dx == 0 && dy == 0 {
        return;
    }
    if let Some(event) =
        CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, dy, dx, 0)
    {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}
