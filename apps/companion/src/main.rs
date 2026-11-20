//! Runs the Rust application directly on the machine's native backend.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;
#[cfg(target_os = "windows")]
use nsh_platform_windows as platform;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn main() -> impl std::process::Termination {
    let mut logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(dirs) = directories::ProjectDirs::from("dev", "stadiawave", "companion") {
        let directory = dirs.data_dir().join("logs");
        if std::fs::create_dir_all(&directory).is_ok()
            && let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join(format!("{}.log", uuid::Uuid::new_v4())))
        {
            logger.target(env_logger::Target::Pipe(Box::new(file)));
        }
    }
    logger.init();
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<wave_companion::App>()),
        platform::AppOptions {
            title: std::env::var("WAVE_COMPANION_PROFILE")
                .map(|profile| format!("Wave Companion · {profile}"))
                .unwrap_or_else(|_| "Wave Companion".to_owned()),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            app_id: "dev.stadiawave.companion".to_owned(),
            size: (720, 700),
            ..Default::default()
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!(
    "Run this binary on macOS, Windows or Linux; use the TSX entry with nsh run for iOS and Android."
);
