use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Statistics {
    pub received: u64,
    pub seconds: f64,
    pub success: u64,
    pub failure: u64,
}

fn path() -> std::path::PathBuf {
    directories::ProjectDirs::from("dev", "stadiawave", "companion")
        .unwrap()
        .data_dir()
        .join("statistics.json")
}

fn state() -> &'static Mutex<Statistics> {
    static STATE: OnceLock<Mutex<Statistics>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(
            std::fs::read(path())
                .ok()
                .and_then(|v| serde_json::from_slice(&v).ok())
                .unwrap_or_default(),
        )
    })
}

pub fn read() -> Statistics {
    state().lock().unwrap().clone()
}

pub fn update(change: impl FnOnce(&mut Statistics)) {
    let mut state = state().lock().unwrap();
    change(&mut state);
    let result = (|| -> anyhow::Result<()> {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec(&*state)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        log::warn!("Unable to save statistics: {error}");
    }
}
