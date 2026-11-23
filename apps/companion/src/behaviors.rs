//! File management stays in Companion; the behavior crate owns execution.
use crate::mapping;
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use wave_behavior::{Definition, Prepared};

pub fn directory() -> PathBuf {
    directories::ProjectDirs::from("dev", "stadiawave", "companion")
        .unwrap()
        .data_dir()
        .join("behaviors")
}
pub struct Plan {
    pub prepared: BTreeMap<String, Prepared>,
    pub paths: BTreeMap<String, PathBuf>,
    pub files: BTreeSet<PathBuf>,
    pub errors: Vec<String>,
}
pub fn scan() -> Result<Plan> {
    let directory = directory();
    std::fs::create_dir_all(&directory)?;
    let mut paths: Vec<_> = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    paths.sort();
    let mut plan = Plan {
        prepared: BTreeMap::new(),
        paths: BTreeMap::new(),
        files: BTreeSet::new(),
        errors: vec![],
    };
    let mut duplicates = BTreeSet::new();
    let mut count = 0;
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        plan.files.insert(path.clone());
        count += 1;
        if count > 32 {
            plan.errors.push("每次最多加载 32 个行为文件。".into());
            continue;
        }
        let result = (|| -> Result<()> {
            let metadata = std::fs::metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.len() <= 128 * 1024,
                "Behavior file too large"
            );
            let definition: Definition = serde_json::from_slice(&std::fs::read(&path)?)?;
            let id = definition.id.clone();
            if plan.paths.contains_key(&id) || duplicates.contains(&id) {
                duplicates.insert(id.clone());
                plan.prepared.remove(&id);
                plan.paths.remove(&id);
                anyhow::bail!("Duplicate behavior ID {id}");
            }
            let prepared = Prepared::new(definition, mapping::key_code)?;
            plan.prepared.insert(id.clone(), prepared);
            plan.paths.insert(id, path.clone());
            Ok(())
        })();
        if let Err(error) = result {
            plan.errors.push(format!(
                "{} · {error}",
                path.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    Ok(plan)
}
pub fn write(path: &Path, definition: &Definition) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let data = serde_json::to_vec_pretty(definition)?;
        std::fs::write(&temp, data)?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.context("无法保存行为定义")
}
pub fn install_examples() -> Result<()> {
    std::fs::create_dir_all(directory())?;
    for (name, source) in [
        (
            "mouse-buttons.json",
            include_str!("../behaviors/mouse-buttons.json"),
        ),
        (
            "app-switcher.json",
            include_str!("../behaviors/app-switcher.json"),
        ),
    ] {
        let path = directory().join(name);
        // Existing definitions belong to the user.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(source.as_bytes())?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
