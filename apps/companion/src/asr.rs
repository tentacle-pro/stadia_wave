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

pub async fn transcribe(
    endpoint: &str,
    model: Option<&str>,
    language: &str,
    recording: &Path,
) -> Result<String> {
    let endpoint = local_endpoint(endpoint)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(90))
        .build()?;
    let model = match model {
        Some(id) => id.to_owned(),
        None => {
            let mut models = client
                .get(endpoint.join("v1/models")?)
                .send()
                .await
                .context("无法连接本机 ASR，请检查监听端口")?
                .error_for_status()?
                .json::<Models>()
                .await
                .context("无法读取 ASR 模型列表")?
                .data;
            ensure!(
                models.len() == 1,
                "ASR 需要恰好一个可用模型，或通过 WAVE_ASR_MODEL 指定模型"
            );
            models.remove(0).id
        }
    };
    let wav = decode_wav(recording).await?;
    let boundary = uuid::Uuid::new_v4().simple().to_string();
    let mut body = Vec::with_capacity(wav.len() + 512);
    for (name, value) in [("model", model.as_str()), ("language", language.trim())] {
        if !value.is_empty() {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
            );
        }
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"dictation.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let response = client
        .post(endpoint.join("v1/audio/transcriptions")?)
        .header(
            CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .context("本机 ASR 转写请求失败")?;
    ensure!(
        response.status().is_success(),
        "本机 ASR 返回 HTTP {}",
        response.status()
    );
    let text = response.json::<Transcript>().await?.text;
    let text = text.trim();
    ensure!(!text.is_empty(), "未识别到文字");
    Ok(text.to_owned())
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
    use super::local_endpoint;

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
}
