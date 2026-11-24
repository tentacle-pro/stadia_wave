use anyhow::{Context, Result, bail, ensure};
use objc2::{MainThreadMarker, rc::autoreleasepool};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSPasteboard,
    NSPasteboardTypeString, NSRunningApplication, NSWorkspace,
};
use objc2_core_graphics::{CGEvent, CGEventFlags};
use objc2_foundation::NSString;
use std::path::{Path, PathBuf};

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
    if let Some(current) = CGEvent::new(None)
        && let Some(event) = CGEvent::new_mouse_event(
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
        )
    {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
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

/// What macOS sees this process as. Privacy permissions (local network, accessibility)
/// are granted to the app that launched the process tree, not to a helper it spawns,
/// so the interface names that app instead of leaving the user guessing.
#[derive(Clone, Debug)]
pub struct Identity {
    /// Name of the running bundle, or "Wave Companion" outside a bundle.
    pub name: String,
    /// The bundle path, or the executable path outside a bundle.
    pub path: String,
    /// The app macOS attributes the permission to.
    pub launcher: String,
}

pub fn identity() -> Identity {
    static IDENTITY: std::sync::OnceLock<Identity> = std::sync::OnceLock::new();
    IDENTITY.get_or_init(compute_identity).clone()
}

fn compute_identity() -> Identity {
    let executable = std::env::current_exe().ok();
    let bundle = executable.as_deref().and_then(app_bundle);
    let path = bundle
        .clone()
        .or(executable)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let name = bundle
        .as_deref()
        .map_or_else(|| "Wave Companion".to_string(), bundle_name);
    Identity {
        name,
        path,
        launcher: launcher_name(std::process::id() as i32),
    }
}

fn app_bundle(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .map(Path::to_path_buf)
}

fn bundle_name(bundle: &Path) -> String {
    bundle
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| bundle.display().to_string())
}

/// Walks up the parent chain to the first ancestor that is an app bundle. A shell in
/// between says the app came from a terminal, whose own app then holds the permission.
fn launcher_name(pid: i32) -> String {
    let mut parent = parent_of(pid);
    let mut through_shell = false;
    while let Some(current) = parent {
        if current <= 1 {
            break;
        }
        if let Some(executable) = executable_of(current) {
            if let Some(bundle) = app_bundle(&executable) {
                let name = bundle_name(&bundle);
                return if through_shell {
                    format!("{name}（经终端）")
                } else {
                    name
                };
            }
            through_shell |= is_shell(&executable);
        }
        parent = parent_of(current);
    }
    "系统（访达或登录项）".to_string()
}

fn is_shell(executable: &Path) -> bool {
    matches!(
        executable.file_name().and_then(|name| name.to_str()),
        Some("zsh" | "bash" | "sh" | "dash" | "fish" | "nu" | "tcsh" | "ksh")
    )
}

fn parent_of(pid: i32) -> Option<i32> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    Some(unsafe { info.assume_init() }.pbi_ppid as i32)
}

fn executable_of(pid: i32) -> Option<PathBuf> {
    let mut buffer = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let size = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if size <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buffer[..size as usize]);
    Some(PathBuf::from(path.into_owned()))
}

/// OpenJoystickDriver's virtual-gamepad extension. `None` means neither the app
/// nor its system extension is on this Mac, so the row stays out of the interface.
#[derive(Clone, Debug, PartialEq)]
pub struct DriverExtension {
    pub enabled: bool,
}

fn extension_listing() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/systemextensionsctl")
        .arg("list")
        .output()
        .ok()?;
    if output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The raw `systemextensionsctl list` text for the diagnostics page, or a note
/// explaining why there is none.
pub fn driver_diagnostics() -> String {
    match extension_listing() {
        Some(listing) if !listing.trim().is_empty() => listing.trim().to_string(),
        _ => "未检测到系统扩展（systemextensionsctl 没有输出）".to_string(),
    }
}

pub fn openjoystick_driver() -> Option<DriverExtension> {
    let listing = extension_listing().unwrap_or_default();
    match driver_state(&listing) {
        Some(state) => Some(state),
        None if openjoystick_installed() => Some(DriverExtension { enabled: false }),
        None => None,
    }
}

/// Whether OpenJoystickDriver.app sits in /Applications, without running a tool.
pub fn openjoystick_installed() -> bool {
    Path::new("/Applications/OpenJoystickDriver.app").exists()
}

/// Reads one `systemextensionsctl list` line: the extension is installed when it
/// appears, and enabled only after the user flips its switch in Login Items.
fn driver_state(listing: &str) -> Option<DriverExtension> {
    let line = listing
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("openjoystick"))?;
    Some(DriverExtension {
        enabled: line.contains("[activated enabled]"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_bundle_around_an_executable() {
        let executable =
            Path::new("/Applications/Wave Companion.app/Contents/MacOS/wave-companion");
        let bundle = app_bundle(executable).expect("bundle");
        assert_eq!(bundle, Path::new("/Applications/Wave Companion.app"));
        assert_eq!(bundle_name(&bundle), "Wave Companion");
    }

    #[test]
    fn plain_binaries_have_no_bundle() {
        assert!(app_bundle(Path::new("/Users/me/target/debug/wave-companion")).is_none());
    }

    #[test]
    fn recognises_shells_between_an_app_and_this_process() {
        assert!(is_shell(Path::new("/bin/zsh")));
        assert!(is_shell(Path::new("/opt/homebrew/bin/fish")));
        assert!(!is_shell(Path::new(
            "/Applications/Zed.app/Contents/MacOS/zed"
        )));
    }

    #[test]
    fn reads_the_openjoystick_extension_state() {
        let waiting = "enabled\tactive\tteamID\tbundleID\tname\t[state]\n\
                       *\t*\t9PQP6CDMQT\tcom.openjoystickdriver.XboxUSBDevice (0.6.0-alpha.1/1.19.64)\t\
                       XboxUSBDevice\t[activated waiting for user]\n";
        assert_eq!(
            driver_state(waiting),
            Some(DriverExtension { enabled: false })
        );
        let enabled = waiting.replace("[activated waiting for user]", "[activated enabled]");
        assert_eq!(
            driver_state(&enabled),
            Some(DriverExtension { enabled: true })
        );
        assert_eq!(driver_state("no system extensions"), None);
    }
}
