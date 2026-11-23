use anyhow::{Context, Result, ensure};
use std::{
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
};

fn process() -> &'static Mutex<Option<Child>> {
    static PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    PROCESS.get_or_init(|| Mutex::new(None))
}

pub fn status() -> String {
    let mut child = process().lock().unwrap();
    if let Some(process) = child.as_mut() {
        match process.try_wait() {
            Ok(Some(code)) => {
                *child = None;
                return format!("本机 Bridge 已退出 {code}");
            }
            Ok(None) => return "本机 Bridge 运行中".into(),
            Err(error) => return error.to_string(),
        }
    }
    "本机 Bridge 未由 Companion 启动".into()
}

pub fn start() -> Result<()> {
    let mut child = process().lock().unwrap();
    ensure!(child.is_none(), "本机 Bridge 已启动");
    ensure!(!available(), "已有本机 Bridge 在运行");
    let current = std::env::current_exe()?;
    let bin_dir = current.parent().context("无法定位 Companion 安装目录")?;
    let bundled = bin_dir
        .parent()
        .map(|contents| contents.join("Helpers/wave-bridge"));
    let bundle_sibling = bin_dir
        .ancestors()
        .nth(3)
        .map(|dir| dir.join("wave-bridge"));
    let executable = std::env::var_os("WAVE_BRIDGE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            bundled
                .into_iter()
                .chain([bin_dir.join("wave-bridge")])
                .chain(bundle_sibling)
                .find(|path| path.is_file())
                .unwrap_or_else(|| {
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../target/debug/wave-bridge")
                })
        });
    let directory = directories::ProjectDirs::from("dev", "stadiawave", "bridge")
        .unwrap()
        .data_dir()
        .join("logs");
    std::fs::create_dir_all(&directory)?;
    let log =
        std::fs::File::create(directory.join(format!("companion-{}.log", uuid::Uuid::new_v4())))?;
    *child = Some(
        Command::new(executable)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("无法启动本机 Bridge")?,
    );
    Ok(())
}

pub fn stop() -> Result<()> {
    if let Some(mut child) = process().lock().unwrap().take() {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        // SIGTERM lets Bridge shut its managed Stadiacast down.
        let status = Command::new("/bin/kill")
            .args(["-TERM", &child.id().to_string()])
            .status()?;
        ensure!(status.success(), "无法停止本机 Bridge");
        child.wait().context("等待本机 Bridge 退出失败")?;
    }
    Ok(())
}

pub fn socket_path() -> std::path::PathBuf {
    directories::ProjectDirs::from("dev", "stadiawave", "bridge")
        .unwrap()
        .data_dir()
        .join("bridge.sock")
}

pub fn available() -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let path = socket_path();
    let Some(directory) = path
        .parent()
        .and_then(|path| std::fs::symlink_metadata(path).ok())
    else {
        return false;
    };
    directory.is_dir()
        && directory.uid() == unsafe { libc::geteuid() }
        && directory.permissions().mode() & 0o077 == 0
        && std::os::unix::net::UnixStream::connect(path).is_ok()
}
