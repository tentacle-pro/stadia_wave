use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Url, header::CONTENT_TYPE, redirect::Policy};
use serde::Deserialize;

#[derive(Deserialize)]
struct Models {
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

#[derive(Deserialize)]
struct Transcript {
    text: String,
}

/// Which request shape a provider speaks. The HTTP details live behind
/// [`Connector`]; the settings page only reads [`Provider`] metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// OpenAI-shaped HTTP service on a loopback address (Voxt and similar).
    LocalOpenAi,
    /// OpenAI-shaped HTTP service elsewhere, authenticated with an API key.
    OpenAiCompatible,
}

pub struct Provider {
    pub id: &'static str,
    pub kind: Kind,
    pub label: &'static str,
    pub summary: &'static str,
    pub address_label: &'static str,
    pub address_placeholder: &'static str,
    /// Whether the form offers a language field.
    pub language: bool,
    /// Whether the address can be found from a running local process.
    pub discovery: bool,
    /// Whether the form offers an API key, kept in the Keychain.
    pub secret: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "local-openai",
        kind: Kind::LocalOpenAi,
        label: "本机 OpenAI 兼容服务",
        summary: "读取运行中的 qwen3_asr_server 端口，调用 /v1/models 与 /v1/audio/transcriptions。",
        address_label: "本机服务地址",
        address_placeholder: "http://127.0.0.1:58078",
        language: true,
        discovery: true,
        secret: false,
    },
    Provider {
        id: "openai-compatible",
        kind: Kind::OpenAiCompatible,
        label: "OpenAI 兼容服务",
        summary: "自建网关或云端兼容 API；填服务根地址和 API key，密钥保存在钥匙串。",
        address_label: "服务地址",
        address_placeholder: "https://api.example.com",
        language: true,
        discovery: false,
        secret: true,
    },
];

pub fn default_provider() -> &'static Provider {
    &PROVIDERS[0]
}

pub fn provider_by_id(id: &str) -> &'static Provider {
    PROVIDERS
        .iter()
        .find(|provider| provider.id == id)
        .unwrap_or_else(|| default_provider())
}

impl Provider {
    /// The endpoint this provider accepts, already parsed.
    pub fn validate(&self, value: &str) -> Result<Url> {
        match self.kind {
            Kind::LocalOpenAi => local_endpoint(value),
            Kind::OpenAiCompatible => remote_endpoint(value),
        }
    }
}

/// Everything a request needs besides the audio itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub provider: String,
    pub endpoint: String,
    pub api_key: String,
}

const ASR_KEYRING_SERVICE: &str = "dev.stadiawave.companion.asr";

pub fn stored_api_key(provider_id: &str) -> Option<String> {
    keyring::Entry::new(ASR_KEYRING_SERVICE, provider_id)
        .ok()?
        .get_password()
        .ok()
        .filter(|key| !key.trim().is_empty())
}

pub fn store_api_key(provider_id: &str, value: &str) -> Result<()> {
    let entry = keyring::Entry::new(ASR_KEYRING_SERVICE, provider_id).context("无法访问钥匙串")?;
    let value = value.trim();
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(anyhow::Error::msg(format!("无法删除已保存的密钥：{error}"))),
        }
    } else {
        entry.set_password(value).context("无法把密钥写入钥匙串")
    }
}

/// One instance per provider. Adding a provider means adding a [`Kind`] and its
/// validation or request shape here; screens keep reading the same metadata.
pub struct Connector {
    provider: &'static Provider,
}

impl Connector {
    pub fn discover(&self) -> Option<String> {
        if self.provider.discovery {
            discover_endpoint()
        } else {
            None
        }
    }

    pub async fn check(&self, target: &Target) -> Result<String> {
        openai_check(self.provider, target).await
    }

    pub async fn transcribe(
        &self,
        target: &Target,
        model: Option<&str>,
        language: &str,
        recording: &Path,
    ) -> Result<String> {
        openai_transcribe(self.provider, target, model, language, recording).await
    }
}

pub fn connector_for(provider_id: &str) -> Connector {
    Connector {
        provider: provider_by_id(provider_id),
    }
}

pub fn validate(provider_id: &str, endpoint: &str) -> Result<()> {
    provider_by_id(provider_id).validate(endpoint).map(|_| ())
}

pub async fn check(target: &Target) -> Result<String> {
    connector_for(&target.provider).check(target).await
}

pub async fn transcribe(
    target: &Target,
    model: Option<&str>,
    language: &str,
    recording: &Path,
) -> Result<String> {
    connector_for(&target.provider)
        .transcribe(target, model, language, recording)
        .await
}

fn client(timeout: Duration) -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(timeout)
        .build()?)
}

fn api_url(endpoint: &Url, relative: &str) -> Result<Url> {
    let mut base = endpoint.as_str().trim_end_matches('/').to_owned();
    if !base.ends_with("/v1") {
        base.push_str("/v1");
    }
    base.push('/');
    base.push_str(relative);
    Url::parse(&base).context("无法拼接 ASR 接口地址")
}

fn authorization(provider: &Provider, target: &Target) -> Option<String> {
    (provider.secret && !target.api_key.trim().is_empty())
        .then(|| format!("Bearer {}", target.api_key.trim()))
}

fn failure(status: reqwest::StatusCode, body: &str) -> String {
    match status.as_u16() {
        401 | 403 => "服务拒绝了凭据（401/403），检查 API key".to_string(),
        404 => "服务没有该接口（404），检查地址是否指向 OpenAI 兼容 API".to_string(),
        _ => {
            let body = body.trim();
            if body.is_empty() {
                format!("服务返回 HTTP {status}")
            } else {
                format!(
                    "服务返回 HTTP {status}：{}",
                    body.chars().take(200).collect::<String>()
                )
            }
        }
    }
}

async fn fetch_models(
    client: &Client,
    provider: &Provider,
    target: &Target,
) -> Result<Vec<String>> {
    let endpoint = provider.validate(&target.endpoint)?;
    let mut request = client.get(api_url(&endpoint, "models")?);
    if let Some(value) = authorization(provider, target) {
        request = request.header("Authorization", value);
    }
    let response = request
        .send()
        .await
        .context("无法连接 ASR 服务，请检查地址与网络")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("{}", failure(status, &body));
    }
    let models = response
        .json::<Models>()
        .await
        .context("服务返回的内容不是 OpenAI 兼容的模型列表")?
        .data;
    Ok(models.into_iter().map(|model| model.id).collect())
}

async fn openai_check(provider: &Provider, target: &Target) -> Result<String> {
    let models = fetch_models(&client(Duration::from_secs(5))?, provider, target).await?;
    ensure!(!models.is_empty(), "服务未提供可用模型");
    Ok(format!("服务可用 · {}", models.join(", ")))
}

async fn openai_transcribe(
    provider: &Provider,
    target: &Target,
    model: Option<&str>,
    language: &str,
    recording: &Path,
) -> Result<String> {
    let client = client(Duration::from_secs(90))?;
    let endpoint = provider.validate(&target.endpoint)?;
    let model = match model {
        Some(id) => id.to_owned(),
        None => {
            let mut models = fetch_models(&client, provider, target).await?;
            ensure!(
                models.len() == 1,
                "ASR 需要恰好一个可用模型，或通过 WAVE_ASR_MODEL 指定模型"
            );
            models.remove(0)
        }
    };
    let wav = decode_wav(recording).await?;
    let boundary = uuid::Uuid::new_v4().simple().to_string();
    let mut body = Vec::with_capacity(wav.len() + 512);
    for (name, value) in [("model", model.as_str()), ("language", language.trim())] {
        if !value.is_empty() {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"dictation.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut request = client
        .post(api_url(&endpoint, "audio/transcriptions")?)
        .header(
            CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body);
    if let Some(value) = authorization(provider, target) {
        request = request.header("Authorization", value);
    }
    let response = request.send().await.context("ASR 转写请求失败")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("{}", failure(status, &body));
    }
    let text = response
        .json::<Transcript>()
        .await
        .context("服务返回的内容不是 OpenAI 兼容的转写结果")?
        .text;
    let text = text.trim();
    ensure!(!text.is_empty(), "未识别到文字");
    Ok(text.to_owned())
}

/// Finds the port Voxt's `qwen3_asr_server` is listening on right now. Voxt picks
/// the port dynamically, so read it from the process instead of configuring it.
pub fn discover_endpoint() -> Option<String> {
    let processes = std::process::Command::new("/usr/bin/pgrep")
        .args(["-f", "qwen3_asr_server"])
        .output()
        .ok()?;
    for pid in String::from_utf8_lossy(&processes.stdout)
        .lines()
        .map(str::trim)
    {
        if pid.is_empty() {
            continue;
        }
        let Ok(listing) = std::process::Command::new("/usr/sbin/lsof")
            .args(["-nP", "-a", "-p", pid, "-iTCP", "-sTCP:LISTEN", "-Fn"])
            .output()
        else {
            continue;
        };
        if let Some(endpoint) = endpoint_from_listing(&String::from_utf8_lossy(&listing.stdout)) {
            return Some(endpoint);
        }
    }
    None
}

fn remote_endpoint(value: &str) -> Result<Url> {
    let url =
        Url::parse(value.trim()).context("请输入服务的完整地址，例如 https://api.example.com")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "请输入 http/https 根地址，不含账号、查询串或片段"
    );
    Ok(url)
}

fn endpoint_from_listing(listing: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let address = line.strip_prefix('n')?;
        if !(address.starts_with("127.0.0.1:") || address.starts_with("[::1]:")) {
            return None;
        }
        Some(format!("http://{address}"))
    })
}

pub(crate) fn local_endpoint(value: &str) -> Result<Url> {
    let url = Url::parse(value.trim()).context("请输入完整的本机 ASR 地址")?;
    let is_loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        is_loopback
            && matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "请输入本机 ASR 根地址，例如 http://127.0.0.1:58078"
    );
    Ok(url)
}

async fn decode_wav(recording: &Path) -> Result<Vec<u8>> {
    let ffmpeg = std::env::var_os("WAVE_FFMPEG").unwrap_or_else(|| {
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .unwrap_or("ffmpeg")
            .into()
    });
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(ffmpeg)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .args(["-nostdin", "-v", "error", "-f", "ogg", "-i"])
            .arg(recording)
            .args([
                "-map",
                "0:a:0",
                "-t",
                "60",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-c:a",
                "pcm_s16le",
                "-f",
                "s16le",
                "pipe:1",
            ])
            .output(),
    )
    .await
    .context("FFmpeg 解码超时")?
    .context("无法启动 FFmpeg，请安装 FFmpeg 或设置 WAVE_FFMPEG")?;
    if !output.status.success() {
        bail!("FFmpeg 无法解码手柄录音");
    }
    let pcm = output.stdout;
    ensure!(!pcm.is_empty() && pcm.len() % 2 == 0, "没有可转写的音频");
    let size = u32::try_from(pcm.len())?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes()); // PCM format chunk length
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM compression code
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2_u16.to_le_bytes()); // bytes per sample frame
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.extend_from_slice(&pcm);
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::{
        Kind, api_url, endpoint_from_listing, local_endpoint, provider_by_id, remote_endpoint,
    };
    use reqwest::Url;

    #[test]
    fn endpoint_only_accepts_explicit_loopback_addresses() {
        assert!(local_endpoint("http://127.0.0.1:58078").is_ok());
        assert!(local_endpoint("http://[::1]:58078/").is_ok());
        for invalid in [
            "http://localhost:58078",
            "http://192.168.1.8:58078",
            "http://127.0.0.1:58078/v1",
            "http://127.0.0.1:58078?foo=bar",
            "http://127.0.0.1@example.com",
        ] {
            assert!(local_endpoint(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn reads_a_loopback_listener_from_lsof_output() {
        let listing = "p92593\nf3\nn127.0.0.1:54510\n";
        assert_eq!(
            endpoint_from_listing(listing).as_deref(),
            Some("http://127.0.0.1:54510")
        );
        assert_eq!(
            endpoint_from_listing("n[::1]:54510\n").as_deref(),
            Some("http://[::1]:54510")
        );
        assert_eq!(endpoint_from_listing("n192.168.1.8:54510\n"), None);
        assert_eq!(endpoint_from_listing(""), None);
    }

    #[test]
    fn the_default_provider_drives_the_settings_form() {
        let provider = super::default_provider();
        assert_eq!(provider.kind, super::Kind::LocalOpenAi);
        assert!(provider.discovery);
        assert!(provider.language);
        assert!(!provider.address_label.is_empty());
        assert!(!provider.address_placeholder.is_empty());
    }

    #[test]
    fn the_second_provider_takes_a_remote_address_and_a_key() {
        let provider = provider_by_id("openai-compatible");
        assert_eq!(provider.kind, Kind::OpenAiCompatible);
        assert!(provider.secret);
        assert!(!provider.discovery);
        assert!(provider.validate("https://api.example.com").is_ok());
        assert!(provider.validate("https://api.example.com/v1").is_ok());
        assert!(provider.validate("http://127.0.0.1:58078").is_ok());
        assert!(provider.validate("ftp://api.example.com").is_err());
        assert!(
            provider
                .validate("https://user:pw@api.example.com")
                .is_err()
        );
    }

    #[test]
    fn an_unknown_provider_falls_back_to_the_local_one() {
        assert_eq!(provider_by_id("missing").kind, Kind::LocalOpenAi);
    }

    #[test]
    fn api_urls_keep_exactly_one_v1_segment() {
        let root = Url::parse("http://127.0.0.1:58078").unwrap();
        assert_eq!(
            api_url(&root, "models").unwrap().as_str(),
            "http://127.0.0.1:58078/v1/models"
        );
        let with_v1 = Url::parse("https://api.example.com/v1/").unwrap();
        assert_eq!(
            api_url(&with_v1, "audio/transcriptions").unwrap().as_str(),
            "https://api.example.com/v1/audio/transcriptions"
        );
        assert!(remote_endpoint("https://api.example.com").is_ok());
    }
}
