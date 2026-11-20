use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    sync::{mpsc, oneshot},
};
use uuid::Uuid;
use wave_protocol::{
    ControllerState, HardwareState, MAX_AUDIO_BYTES, MAX_RECORDING_SECONDS, ServerMessage,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    pub executable: PathBuf,
    pub controller_ip: String,
    pub advertise_ip: String,
    pub dns_upstream: String,
}

pub enum Event {
    Message(ServerMessage),
    Audio {
        metadata: ServerMessage,
        data: Vec<u8>,
    },
}

enum Control {
    Bind {
        session: String,
        reply: oneshot::Sender<mpsc::Receiver<Event>>,
    },
    Detach {
        session: String,
        reply: oneshot::Sender<()>,
    },
    Rumble {
        session: String,
        controller: String,
        request: String,
    },
    Received {
        session: String,
        recording: String,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct Handle(mpsc::Sender<Control>);

impl Handle {
    pub fn start(options: Option<Options>, data_dir: &Path) -> Result<Self> {
        let (tx, rx) = mpsc::channel(32);
        let run_dir = data_dir.join("captures").join(Uuid::new_v4().to_string());
        fs::create_dir_all(&run_dir)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))?;
        let child = options
            .as_ref()
            .map(|options| launch(options, &run_dir))
            .transpose()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .no_proxy()
            .build()?;
        tokio::spawn(
            Actor {
                child,
                options,
                restart_at: Instant::now(),
                restart_delay: 1,
                run_dir,
                client,
                owner: None,
                log: None,
                partial_line: String::new(),
                recordings: HashMap::new(),
                pending: HashMap::new(),
                enabled: HashSet::new(),
                snapshot: HardwareState {
                    running: false,
                    message: "未配置 Stadiacast".into(),
                    controllers: vec![],
                },
                verified: false,
            }
            .run(rx),
        );
        Ok(Self(tx))
    }

    pub async fn bind(&self, session: &str) -> Result<mpsc::Receiver<Event>> {
        let (reply, receive) = oneshot::channel();
        self.0
            .send(Control::Bind {
                session: session.into(),
                reply,
            })
            .await?;
        Ok(receive.await?)
    }

    pub async fn detach(&self, session: &str) {
        let (reply, receive) = oneshot::channel();
        if self
            .0
            .send(Control::Detach {
                session: session.into(),
                reply,
            })
            .await
            .is_ok()
        {
            let _ = receive.await;
        }
    }

    pub async fn rumble(&self, session: &str, controller: String, request: String) -> Result<()> {
        self.0
            .send(Control::Rumble {
                session: session.into(),
                controller,
                request,
            })
            .await?;
        Ok(())
    }

    pub async fn received(&self, session: &str, recording: String) -> Result<()> {
        self.0
            .send(Control::Received {
                session: session.into(),
                recording,
            })
            .await?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let (reply, receive) = oneshot::channel();
        if self.0.send(Control::Shutdown { reply }).await.is_ok() {
            let _ = receive.await;
        }
    }
}

fn launch(options: &Options, run_dir: &Path) -> Result<Child> {
    let stdout = File::create(run_dir.join("stdout.log"))?;
    let stderr = File::create(run_dir.join("stderr.log"))?;
    Command::new(&options.executable)
        .args([
            "--preset",
            "live",
            "--dns-upstream",
            &options.dns_upstream,
            "--controller-ip",
            &options.controller_ip,
            "--advertise-ip",
            &options.advertise_ip,
            "--mic",
            "off",
            "--mic-record",
            "--start-game",
            "--log-dir",
        ])
        .arg(run_dir)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .context("无法启动 Stadiacast")
}

struct Owner {
    session: String,
    events: mpsc::Sender<Event>,
}

struct Recording {
    id: String,
    started: Instant,
    failed: bool,
    stopped: Option<Instant>,
}

struct Actor {
    child: Option<Child>,
    options: Option<Options>,
    restart_at: Instant,
    restart_delay: u64,
    run_dir: PathBuf,
    client: reqwest::Client,
    owner: Option<Owner>,
    log: Option<BufReader<File>>,
    partial_line: String,
    recordings: HashMap<String, Recording>,
    pending: HashMap<String, (PathBuf, Instant)>,
    enabled: HashSet<String>,
    snapshot: HardwareState,
    verified: bool,
}

impl Actor {
    fn owns(&self, session: &str) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.session == session)
    }

    fn emit(&mut self, message: ServerMessage) {
        if let Some(owner) = &self.owner {
            let _ = owner.events.try_send(Event::Message(message));
        }
    }

    fn update(&mut self, snapshot: HardwareState) {
        if self.snapshot != snapshot {
            self.snapshot = snapshot;
            self.publish_snapshot();
        }
    }

    fn publish_snapshot(&mut self) {
        if let Some(owner) = &self.owner {
            self.emit(ServerMessage::Hardware {
                session_id: owner.session.clone(),
                snapshot: self.snapshot.clone(),
            });
        }
    }

    async fn post(&self, controller: &str, action: Value) -> Result<Value> {
        let response = self
            .client
            .post("http://127.0.0.1:8008/_mic")
            .query(&[("controller", controller)])
            .json(&action)
            .send()
            .await?;
        Ok(response.error_for_status()?.json().await?)
    }

    async fn clear_owner(&mut self) {
        let previous = self.owner.take();
        for recording in self.recordings.values() {
            if let Some(owner) = &previous {
                let _ = owner
                    .events
                    .try_send(Event::Message(ServerMessage::RecordingCancelled {
                        session_id: owner.session.clone(),
                        recording_id: recording.id.clone(),
                        reason: "连接结束或被接管".into(),
                    }));
            }
        }
        self.recordings.clear();
        self.enabled.clear();
        if self.verified {
            for controller in &self.snapshot.controllers {
                let _ = self
                    .post(&controller.id, json!({"do":"ptt","on":false}))
                    .await;
                let _ = self.post(&controller.id, json!({"do":"off"})).await;
            }
        }
        self.read_log();
        for (_, (path, _)) in self.pending.drain() {
            let _ = fs::remove_file(path);
        }
    }

    async fn run(mut self, mut controls: mpsc::Receiver<Control>) {
        let mut logs = tokio::time::interval(Duration::from_millis(50));
        let mut status = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                biased;
                command = controls.recv() => {
                    match command {
                        Some(Control::Bind { session, reply }) => {
                            self.clear_owner().await;
                            let (events, rx) = mpsc::channel(128);
                            self.owner = Some(Owner { session, events });
                            self.publish_snapshot();
                            let _ = reply.send(rx);
                        }
                        Some(Control::Detach { session, reply }) => {
                            if self.owns(&session) { self.clear_owner().await; }
                            let _ = reply.send(());
                        }
                        Some(Control::Received { session, recording }) if self.owns(&session) => {
                            if let Some((path, _)) = self.pending.remove(&recording) { let _ = fs::remove_file(path); }
                        }
                        Some(Control::Rumble { session, controller, request }) if self.owns(&session) => {
                            let result = self.rumble(&controller).await;
                            self.emit(ServerMessage::CommandResult {
                                session_id: session, request_id: request, ok: result.is_ok(),
                                message: match result { Ok(()) => "已发送 300 毫秒震动".into(), Err(error) => format!("{error:#}") },
                            });
                        }
                        Some(Control::Shutdown { reply }) => {
                            self.stop().await;
                            let _ = reply.send(());
                            break;
                        }
                        None => { self.stop().await; break; }
                        _ => {}
                    }
                }
                _ = logs.tick() => {
                    self.read_log();
                    self.expire_audio().await;
                }
                _ = status.tick() => self.poll().await,
            }
        }
    }

    async fn stop(&mut self) {
        self.clear_owner().await;
        if self.verified {
            let _ = self
                .client
                .post("http://127.0.0.1:8008/_shutdown")
                .send()
                .await;
        }
        if let Some(child) = &mut self.child
            && tokio::time::timeout(Duration::from_secs(4), child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
        }
    }

    async fn rumble(&self, controller: &str) -> Result<()> {
        if !self.verified
            || !self
                .snapshot
                .controllers
                .iter()
                .any(|item| item.id == controller && item.ready && item.rumble_ready)
        {
            bail!("手柄离线或震动通道尚未就绪");
        }
        self.client
            .post("http://127.0.0.1:8008/_rumble")
            .query(&[("controller", controller)])
            .json(&json!({"left":25000,"right":25000,"ms":300}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn poll(&mut self) {
        if self.child.is_none()
            && Instant::now() >= self.restart_at
            && let Some(options) = &self.options
        {
            let directory = self
                .run_dir
                .parent()
                .unwrap()
                .join(Uuid::new_v4().to_string());
            let result = fs::create_dir(&directory)
                .map_err(anyhow::Error::from)
                .and_then(|_| launch(options, &directory));
            self.restart_at = Instant::now() + Duration::from_secs(self.restart_delay);
            self.restart_delay = (self.restart_delay * 2).min(30);
            match result {
                Ok(child) => {
                    self.run_dir = directory;
                    self.log = None;
                    self.partial_line.clear();
                    self.child = Some(child);
                }
                Err(error) => {
                    self.snapshot.message = format!("Stadiacast 重启失败，将重试：{error:#}");
                    self.publish_snapshot();
                }
            }
        }
        let Some(child) = &mut self.child else { return };
        match child.try_wait() {
            Ok(Some(code)) => {
                let error = fs::read_to_string(self.run_dir.join("stderr.log")).unwrap_or_default();
                self.verified = false;
                self.update(HardwareState {
                    running: false,
                    message: format!(
                        "Stadiacast 已退出 {code} {}",
                        error
                            .chars()
                            .rev()
                            .take(300)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ),
                    controllers: vec![],
                });
                let owner = self.owner.as_ref().map(|owner| Owner {
                    session: owner.session.clone(),
                    events: owner.events.clone(),
                });
                self.clear_owner().await;
                self.owner = owner;
                self.enabled.clear();
                self.recordings.clear();
                self.restart_at = Instant::now() + Duration::from_secs(self.restart_delay);
                self.child = None;
                return;
            }
            Err(error) => {
                eprintln!("Stadiacast process: {error}");
                return;
            }
            Ok(None) => {}
        }
        let result: Result<Value> = async {
            Ok(self
                .client
                .get("http://127.0.0.1:8008/_status")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?)
        }
        .await;
        let status = match result {
            Ok(status) if status["log_dir"].as_str() == self.run_dir.to_str() => status,
            _ => {
                self.verified = false;
                self.update(HardwareState {
                    running: false,
                    message: "正在等待 Stadiacast 接口".into(),
                    controllers: vec![],
                });
                return;
            }
        };
        self.verified = true;
        let mut controllers = vec![];
        if let Some(items) = status["controllers"].as_object() {
            for (id, value) in items {
                let ready = matches!(
                    value["session"]["display"]["state"].as_str(),
                    Some("ready" | "input")
                ) && value["last_seen_s"].as_f64().unwrap_or(999.0) < 8.0;
                // Buttons are tracked from the display link's exact input log.
                let held = self
                    .snapshot
                    .controllers
                    .iter()
                    .find(|item| item.id == *id)
                    .map(|item| item.held.clone())
                    .unwrap_or_default();
                let controller = ControllerState {
                    id: id.clone(),
                    ready,
                    battery_percent: value["battery_percent"].as_u64().map(|v| v.min(100) as u8),
                    rumble_ready: ready
                        && matches!(
                            value["session"]["game"]["state"].as_str(),
                            Some("game-started" | "game-connected")
                        ),
                    held,
                };
                if !ready {
                    self.enabled.remove(id);
                }
                if self.owner.is_some()
                    && ready
                    && !self.enabled.contains(id)
                    && !controller
                        .held
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case("assistant"))
                    && self.post(id, json!({"do":"ptt","on":true})).await.is_ok()
                {
                    self.enabled.insert(id.clone());
                }
                controllers.push(controller);
            }
        }
        controllers.sort_by(|a, b| a.id.cmp(&b.id));
        self.update(HardwareState {
            running: true,
            message: "Stadiacast 运行中".into(),
            controllers,
        });
    }

    fn read_log(&mut self) {
        if self.log.is_none() {
            self.log = File::open(self.run_dir.join("server.jsonl"))
                .ok()
                .map(BufReader::new);
        }
        // Preserve an incomplete last line until the next file append.
        for _ in 0..1000 {
            let Some(reader) = &mut self.log else { return };
            match reader.read_line(&mut self.partial_line) {
                Ok(0) | Err(_) => break,
                Ok(_) if !self.partial_line.ends_with('\n') => break,
                Ok(_) => {
                    let line = std::mem::take(&mut self.partial_line);
                    if let Ok(event) = serde_json::from_str::<Value>(&line) {
                        self.event(event);
                    }
                }
            }
        }
    }

    fn event(&mut self, event: Value) {
        let controller = event["controller"].as_str().unwrap_or("").to_owned();
        match event["event"].as_str().unwrap_or("") {
            "input" if event["type"].as_str() == Some("rpc_input") => {
                let names = |key: &str| {
                    event[key]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };
                let pressed = names("pressed_names");
                let released = names("released_names");
                let mut held = self
                    .snapshot
                    .controllers
                    .iter()
                    .find(|item| item.id == controller)
                    .map(|item| item.held.clone())
                    .unwrap_or_default();
                held.retain(|name| !released.contains(name));
                for name in &pressed {
                    if !held.contains(name) {
                        held.push(name.clone());
                    }
                }
                if let Some(item) = self
                    .snapshot
                    .controllers
                    .iter_mut()
                    .find(|item| item.id == controller)
                {
                    item.held = held.clone();
                }
                if let Some(owner) = &self.owner
                    && (!pressed.is_empty() || !released.is_empty())
                {
                    self.emit(ServerMessage::Input {
                        session_id: owner.session.clone(),
                        controller,
                        pressed,
                        released,
                        held,
                    });
                }
            }
            "mic_switch" if event["on"] == true && !event["ticket"].is_null() => {
                if let Some(owner) = &self.owner {
                    let id = Uuid::new_v4().to_string();
                    self.recordings.insert(
                        controller.clone(),
                        Recording {
                            id: id.clone(),
                            started: Instant::now(),
                            failed: false,
                            stopped: None,
                        },
                    );
                    self.emit(ServerMessage::RecordingStarted {
                        session_id: owner.session.clone(),
                        recording_id: id,
                        controller,
                    });
                }
            }
            "mic_switch" if event["on"] == false => {
                if let Some(recording) = self.recordings.get_mut(&controller) {
                    recording.stopped = Some(Instant::now());
                }
            }
            "mic_write_failed" | "mic_recording_failed" => {
                if let Some(recording) = self.recordings.get_mut(&controller) {
                    recording.failed = true;
                }
            }
            "mic_recorded" => self.finish_recording(&controller, &event),
            _ => {}
        }
    }

    fn recording_path(&self, event: &Value) -> Result<PathBuf> {
        let path =
            Path::new(event["file"].as_str().context("Missing recording path")?).canonicalize()?;
        if path.parent() != Some(self.run_dir.canonicalize()?.as_path())
            || path.extension().and_then(|v| v.to_str()) != Some("ogg")
        {
            bail!("Recording path outside managed run directory");
        }
        Ok(path)
    }

    fn finish_recording(&mut self, controller: &str, event: &Value) {
        let Ok(path) = self.recording_path(event) else {
            return;
        };
        let Some(recording) = self.recordings.remove(controller) else {
            let _ = fs::remove_file(path);
            return;
        };
        let result: Result<()> = (|| {
            let owner = self.owner.as_ref().context("Companion 已断开")?;
            let seconds = event["seconds"].as_f64().context("Missing duration")?;
            if recording.failed
                || recording.stopped.is_none()
                || !(0.0..=MAX_RECORDING_SECONDS).contains(&seconds)
                || seconds == 0.0
            {
                bail!("录音为空、超时或写入失败");
            }
            let size = fs::metadata(&path)?.len();
            if size == 0 || size > MAX_AUDIO_BYTES as u64 {
                bail!("录音大小超出限制");
            }
            let data = fs::read(&path)?;
            if !data.starts_with(b"OggS") {
                bail!("无效 Ogg 录音");
            }
            let metadata = ServerMessage::Audio {
                session_id: owner.session.clone(),
                recording_id: recording.id.clone(),
                controller: controller.into(),
                bytes: data.len(),
                seconds,
                sha256: hex::encode(Sha256::digest(&data)),
            };
            owner
                .events
                .try_send(Event::Audio { metadata, data })
                .context("Companion 接收队列已满")?;
            self.pending
                .insert(recording.id, (path.clone(), Instant::now()));
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(owner) = &self.owner {
                self.emit(ServerMessage::CommandResult {
                    session_id: owner.session.clone(),
                    request_id: "recording".into(),
                    ok: false,
                    message: format!("{error:#}"),
                });
            }
            let _ = fs::remove_file(path);
        }
    }

    async fn expire_audio(&mut self) {
        let expired: Vec<_> = self
            .recordings
            .iter()
            .filter(|(_, recording)| {
                recording.failed
                    || recording.started.elapsed().as_secs_f64() >= MAX_RECORDING_SECONDS
                    || recording
                        .stopped
                        .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
            })
            .map(|(controller, recording)| (controller.clone(), recording.id.clone()))
            .collect();
        for (controller, id) in expired {
            self.recordings.remove(&controller);
            if let Some(owner) = &self.owner {
                self.emit(ServerMessage::RecordingCancelled {
                    session_id: owner.session.clone(),
                    recording_id: id,
                    reason: "录音超时或按键时间过短，没有收到音频".into(),
                });
            }
            let _ = self.post(&controller, json!({"do":"off"})).await;
        }
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, (_, at))| at.elapsed() > Duration::from_secs(30))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some((path, _)) = self.pending.remove(&id) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        let run_dir = std::env::temp_dir().join(format!("wave-audio-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&run_dir).unwrap();
        Actor {
            child: None,
            options: None,
            restart_at: Instant::now(),
            restart_delay: 1,
            run_dir,
            client: reqwest::Client::new(),
            owner: None,
            log: None,
            partial_line: String::new(),
            recordings: HashMap::new(),
            pending: HashMap::new(),
            enabled: HashSet::new(),
            snapshot: HardwareState::default(),
            verified: false,
        }
    }

    #[tokio::test]
    async fn crashed_process_is_restarted_without_replaying_recording() {
        let mut actor = actor();
        let original = actor.run_dir.clone();
        actor.options = Some(Options {
            executable: "/usr/bin/false".into(),
            controller_ip: "127.0.0.1".into(),
            advertise_ip: "127.0.0.1".into(),
            dns_upstream: "127.0.0.1".into(),
        });
        actor.poll().await;
        let first = actor.child.as_ref().unwrap().id();
        tokio::time::sleep(Duration::from_millis(100)).await;
        actor.poll().await;
        assert!(actor.child.is_none());
        assert!(actor.recordings.is_empty());
        actor.restart_at = Instant::now();
        actor.poll().await;
        assert_ne!(actor.child.as_ref().unwrap().id(), first);
        assert_ne!(actor.run_dir, original);
        actor.stop().await;
        let _ = fs::remove_dir_all(actor.run_dir);
        let _ = fs::remove_dir_all(original);
    }

    #[tokio::test]
    async fn cancelled_owner_cannot_deliver_a_late_recording_to_new_owner() {
        let mut actor = actor();
        let (events, mut old) = mpsc::channel(16);
        actor.owner = Some(Owner {
            session: "old".into(),
            events,
        });
        actor.event(json!({"event":"mic_switch","controller":"c0","on":true,"ticket":1}));
        assert!(matches!(
            old.recv().await,
            Some(Event::Message(ServerMessage::RecordingStarted { .. }))
        ));
        actor.clear_owner().await;
        let (events, mut new) = mpsc::channel(16);
        actor.owner = Some(Owner {
            session: "new".into(),
            events,
        });
        let path = actor.run_dir.join("mic_late.ogg");
        fs::write(&path, b"OggS-late").unwrap();
        actor.event(json!({"event":"mic_recorded","controller":"c0","file":path,"seconds":1.0}));
        assert!(new.try_recv().is_err());
        assert!(!path.exists());
        fs::remove_dir_all(actor.run_dir).unwrap();
    }

    #[tokio::test]
    async fn failed_recording_is_discarded_and_valid_audio_waits_for_ack() {
        let mut actor = actor();
        let (events, mut output) = mpsc::channel(16);
        actor.owner = Some(Owner {
            session: "current".into(),
            events,
        });
        for failed in [true, false] {
            actor.event(json!({"event":"mic_switch","controller":"c0","on":true,"ticket":1}));
            output.recv().await.unwrap();
            let path = actor.run_dir.join(format!("mic_{failed}.ogg"));
            fs::write(&path, b"OggS-fixture").unwrap();
            if failed {
                actor.event(json!({"event":"mic_write_failed","controller":"c0"}));
            }
            actor.event(json!({"event":"mic_switch","controller":"c0","on":false,"ticket":2}));
            actor
                .event(json!({"event":"mic_recorded","controller":"c0","file":path,"seconds":1.0}));
            let event = output.recv().await.unwrap();
            if failed {
                assert!(matches!(
                    event,
                    Event::Message(ServerMessage::CommandResult { ok: false, .. })
                ));
                assert!(!path.exists());
            } else {
                let Event::Audio {
                    metadata:
                        ServerMessage::Audio {
                            session_id,
                            recording_id,
                            sha256,
                            ..
                        },
                    data,
                } = event
                else {
                    panic!("Missing audio")
                };
                assert_eq!(session_id, "current");
                assert_eq!(sha256, hex::encode(Sha256::digest(data)));
                assert!(actor.pending.contains_key(&recording_id));
                assert!(path.exists());
            }
        }
        actor.clear_owner().await;
        assert!(actor.pending.is_empty());
        fs::remove_dir_all(actor.run_dir).unwrap();
    }

    #[test]
    fn recording_paths_cannot_escape_the_managed_run() {
        let actor = actor();
        let outside = actor
            .run_dir
            .parent()
            .unwrap()
            .join(format!("wave-outside-{}.ogg", Uuid::new_v4()));
        fs::write(&outside, b"OggS").unwrap();
        assert!(actor.recording_path(&json!({"file":outside})).is_err());
        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(actor.run_dir).unwrap();
    }
}
