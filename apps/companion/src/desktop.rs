use anyhow::{Context, Result, bail, ensure};
use objc2::rc::autoreleasepool;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSPasteboard, NSPasteboardTypeString, NSRunningApplication,
    NSWorkspace,
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
