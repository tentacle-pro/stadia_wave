use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::watch;

use crate::{asr, desktop, network::ReceivedRecording};

#[derive(Clone, PartialEq)]
pub enum Phase {
    Working,
    Error(String),
    Ready,
}

#[derive(Clone, PartialEq)]
pub struct Draft {
    recording: ReceivedRecording,
    controller: String,
    bridge: String,
    target: Option<desktop::Target>,
    pub phase: Phase,
    pub text: String,
}

impl Draft {
    pub fn id(&self) -> &str {
        &self.recording.id
    }

    pub fn target_name(&self) -> Option<&str> {
        self.target.as_ref().map(|target| target.name.as_str())
    }
}

#[derive(Clone)]
struct Job {
    recording: ReceivedRecording,
    target: asr::Target,
    language: String,
}

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:58078";

#[derive(Clone, Copy)]
enum Action {
    Paste,
    Discard,
}

#[derive(Default)]
struct State {
    recording: Option<(String, Option<desktop::Target>)>,
    draft: Option<Draft>,
    notice: String,
    action: Option<Action>,
    paste_deadline: Option<Instant>,
}

struct Inner {
    state: State,
    target: asr::Target,
    language: String,
}

#[derive(Clone)]
pub struct Dictation {
    inner: Arc<Mutex<Inner>>,
    jobs: watch::Sender<Option<Job>>,
}

impl Dictation {
    pub fn start(target: asr::Target, language: String) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            state: State::default(),
            target,
            language,
        }));
        let (jobs, mut queue) = watch::channel::<Option<Job>>(None);
        let shared = Arc::downgrade(&inner);
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    if let Some(inner) = shared.upgrade() {
                        inner.lock().unwrap().state.notice = format!("无法启动转写：{error}");
                    }
                    return;
                }
            };
            let model = std::env::var("WAVE_ASR_MODEL")
                .ok()
                .filter(|id| !id.is_empty());
            runtime.block_on(async move {
                while queue.changed().await.is_ok() {
                    let job = queue.borrow_and_update().clone();
                    let Some(job) = job else { continue };
                    let mut replacement = queue.clone();
                    let result = tokio::select! {
                        biased;
                        _ = replacement.changed() => continue,
                        result = asr::transcribe(
                            &job.target,
                            model.as_deref(),
                            &job.language,
                            &job.recording.path,
                        ) => result,
                    };
                    let Some(inner) = shared.upgrade() else { break };
                    let mut inner = inner.lock().unwrap();
                    let Some(draft) = inner.state.draft.as_mut() else {
                        continue;
                    };
                    if draft.id() != job.recording.id {
                        continue;
                    }
                    match result {
                        Ok(text) => {
                            crate::statistics::update(|stats| stats.success += 1);
                            draft.text = text;
                            draft.phase = Phase::Ready;
                        }
                        Err(error) => {
                            crate::statistics::update(|stats| stats.failure += 1);
                            draft.phase = Phase::Error(format!("{error:#}"));
                        }
                    }
                }
            });
        });
        Self { inner, jobs }
    }

    pub fn provider(&self, provider: String) {
        self.inner.lock().unwrap().target.provider = provider;
    }

    pub fn endpoint(&self, endpoint: String) {
        self.inner.lock().unwrap().target.endpoint = endpoint;
    }

    pub fn api_key(&self, api_key: String) {
        self.inner.lock().unwrap().target.api_key = api_key;
    }

    pub fn language(&self, language: String) {
        self.inner.lock().unwrap().language = language;
    }

    pub fn recording_started(&self, id: String) {
        self.inner.lock().unwrap().state.recording = Some((id, desktop::frontmost_target()));
    }

    pub fn recording_cancelled(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .state
            .recording
            .as_ref()
            .is_some_and(|(pending, _)| pending == id)
        {
            inner.state.recording = None;
        }
    }

    pub fn received(&self, recording: ReceivedRecording, controller: String, bridge: String) {
        let mut inner = self.inner.lock().unwrap();
        let target = inner
            .state
            .recording
            .take()
            .filter(|(id, _)| id == &recording.id);
        let Some((_, target)) = target else { return };
        if let Some(draft) = &inner.state.draft {
            if draft.id() != recording.id {
                inner.state.notice = "已有待处理草稿。新录音已保存，可从下方打开。".into();
            }
            return;
        }
        inner.state.notice.clear();
        inner.state.draft = Some(Draft {
            recording: recording.clone(),
            controller,
            bridge,
            target,
            phase: Phase::Working,
            text: String::new(),
        });
        self.submit(&mut inner, recording);
    }

    fn submit(&self, inner: &mut Inner, recording: ReceivedRecording) {
        if self
            .jobs
            .send(Some(Job {
                recording,
                target: inner.target.clone(),
                language: inner.language.clone(),
            }))
            .is_err()
            && let Some(draft) = inner.state.draft.as_mut()
        {
            draft.phase = Phase::Error("转写任务已停止".into());
        }
    }

    pub fn retry(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(draft) = inner.state.draft.as_mut().filter(|draft| draft.id() == id)
            && matches!(draft.phase, Phase::Error(_))
        {
            draft.phase = Phase::Working;
            let recording = draft.recording.clone();
            inner.state.notice.clear();
            self.submit(&mut inner, recording);
        }
    }

    pub fn copy(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(draft) = inner.state.draft.as_ref().filter(|draft| draft.id() == id) else {
            return;
        };
        if draft.phase == Phase::Ready && !draft.text.trim().is_empty() {
            inner.state.notice = match desktop::copy(&draft.text) {
                Ok(()) => "已复制草稿".into(),
                Err(error) => error.to_string(),
            };
        }
    }

    pub fn confirm(&self, id: &str) {
        let mut state = self.inner.lock().unwrap();
        if state
            .state
            .draft
            .as_ref()
            .is_some_and(|draft| draft.id() == id && draft.phase == Phase::Ready)
            && state.state.paste_deadline.is_none()
        {
            state.state.action = Some(Action::Paste);
        }
    }

    pub fn discard(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .state
            .draft
            .as_ref()
            .is_some_and(|draft| draft.id() == id)
        {
            inner.state.clear_draft("已丢弃草稿");
            let _ = self.jobs.send(None);
        }
    }

    pub fn mapping_blocked(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.state.draft.is_some()
            || inner.state.action.is_some()
            || inner.state.paste_deadline.is_some()
    }

    pub fn controller_input(&self, bridge: &str, controller: &str, pressed: &[String]) {
        let mut inner = self.inner.lock().unwrap();
        let state = &mut inner.state;
        if state.action.is_some() || state.paste_deadline.is_some() {
            return;
        }
        let Some(draft) = state
            .draft
            .as_ref()
            .filter(|draft| draft.bridge == bridge && draft.controller == controller)
        else {
            return;
        };
        if pressed.iter().any(|name| name.eq_ignore_ascii_case("b")) {
            state.action = Some(Action::Discard);
        } else if draft.phase == Phase::Ready
            && pressed.iter().any(|name| name.eq_ignore_ascii_case("a"))
        {
            state.action = Some(Action::Paste);
        }
    }

    pub fn update_desktop(&self, active_bridge: Option<&str>) -> (Option<Draft>, String) {
        let mut inner = self.inner.lock().unwrap();
        let state = &mut inner.state;
        let connected = state
            .draft
            .as_ref()
            .is_some_and(|draft| Some(draft.bridge.as_str()) == active_bridge);
        if let Some(action) = state.action.take() {
            match action {
                Action::Discard => {
                    state.clear_draft("已丢弃草稿");
                    let _ = self.jobs.send(None);
                }
                Action::Paste if connected => {
                    if let Err(error) = state.prepare_paste() {
                        state.notice = error.to_string();
                    }
                }
                Action::Paste => state.notice = "请重新连接 Bridge 后再粘贴。也可先复制。".into(),
            }
        }
        if let Some(deadline) = state.paste_deadline {
            let result = if connected {
                state.try_paste(deadline)
            } else {
                Err(anyhow::anyhow!("Bridge 已断开。可复制草稿。"))
            };
            match result {
                Ok(true) => state.clear_draft("已向目标应用发送粘贴指令"),
                Ok(false) => {}
                Err(error) => {
                    state.paste_deadline = None;
                    state.notice = error.to_string();
                }
            }
        }
        (state.draft.clone(), state.notice.clone())
    }
}

impl State {
    fn clear_draft(&mut self, notice: &str) {
        self.draft = None;
        self.action = None;
        self.paste_deadline = None;
        self.notice = notice.into();
    }

    fn prepare_paste(&mut self) -> anyhow::Result<()> {
        use anyhow::{Context, ensure};
        let draft = self.draft.as_ref().context("没有待粘贴的草稿")?;
        ensure!(!draft.text.trim().is_empty(), "草稿为空");
        draft
            .target
            .as_ref()
            .context("录音开始时未找到目标应用，请使用复制")?
            .prepare()?;
        self.paste_deadline = Some(Instant::now() + Duration::from_secs(1));
        Ok(())
    }

    fn try_paste(&self, deadline: Instant) -> anyhow::Result<bool> {
        let draft = self.draft.as_ref().unwrap();
        let target = draft.target.as_ref().unwrap();
        anyhow::ensure!(
            Instant::now() <= deadline,
            "无法切回 {}，可先复制草稿",
            target.name
        );
        target.paste_if_frontmost(&draft.text)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn only_originating_controller_can_discard_and_failed_paste_keeps_draft() {
        let (jobs, _queue) = watch::channel(None);
        let dictation = Dictation {
            inner: Arc::new(Mutex::new(Inner {
                state: State {
                    draft: Some(Draft {
                        recording: ReceivedRecording {
                            id: "recording".into(),
                            path: PathBuf::new(),
                            seconds: 1.0,
                        },
                        controller: "c0".into(),
                        bridge: "bridge".into(),
                        target: None,
                        phase: Phase::Ready,
                        text: "测试".into(),
                    }),
                    ..State::default()
                },
                target: asr::Target {
                    provider: "local-openai".into(),
                    endpoint: DEFAULT_ENDPOINT.into(),
                    api_key: String::new(),
                },
                language: "zh".into(),
            })),
            jobs,
        };

        dictation.controller_input("other bridge", "c0", &["b".into()]);
        dictation.controller_input("bridge", "c1", &["b".into()]);
        assert!(dictation.update_desktop(Some("bridge")).0.is_some());

        dictation.controller_input("bridge", "c0", &["a".into()]);
        let (draft, notice) = dictation.update_desktop(None);
        assert_eq!(draft.unwrap().text, "测试");
        assert!(notice.contains("重新连接"));

        dictation.controller_input("bridge", "c0", &["a".into()]);
        let (draft, notice) = dictation.update_desktop(Some("bridge"));
        assert_eq!(draft.unwrap().text, "测试");
        assert!(notice.contains("未找到目标应用"));

        dictation.controller_input("bridge", "c0", &["b".into()]);
        assert!(dictation.update_desktop(Some("bridge")).0.is_none());
    }

    #[test]
    fn retry_uses_the_original_recording_and_updated_settings() {
        let (jobs, queue) = watch::channel(None);
        let dictation = Dictation {
            inner: Arc::new(Mutex::new(Inner {
                state: State {
                    recording: Some(("recording".into(), None)),
                    ..State::default()
                },
                target: asr::Target {
                    provider: "local-openai".into(),
                    endpoint: DEFAULT_ENDPOINT.into(),
                    api_key: String::new(),
                },
                language: "zh".into(),
            })),
            jobs,
        };
        let recording = ReceivedRecording {
            id: "recording".into(),
            path: PathBuf::from("test.ogg"),
            seconds: 1.0,
        };
        dictation.received(recording.clone(), "c0".into(), "bridge".into());
        assert_eq!(
            queue.borrow().as_ref().unwrap().target.endpoint,
            DEFAULT_ENDPOINT
        );

        dictation
            .inner
            .lock()
            .unwrap()
            .state
            .draft
            .as_mut()
            .unwrap()
            .phase = Phase::Error("旧端口无法连接".into());
        dictation.endpoint("http://127.0.0.1:61234".into());
        dictation.language(String::new());
        dictation.retry("recording");

        let job = queue.borrow();
        let job = job.as_ref().unwrap();
        assert_eq!(job.recording.id, recording.id);
        assert_eq!(job.recording.path, recording.path);
        assert_eq!(job.target.endpoint, "http://127.0.0.1:61234");
        assert!(job.language.is_empty());
    }
}
