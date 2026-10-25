//! TTS → MP3（2026-10-25）。
//!
//! 支持两种后端：
//! - `TTS_PROVIDER=openai`：调用 OpenAI 兼容的 `/audio/speech` 接口，返回 MP3；
//! - `TTS_PROVIDER=macos`：本地 macOS `say` 生成 AIFF，再交给 ffmpeg 转 MP3。
//!
//! 默认 `auto`：配置了 `TTS_API_URL` 就走 HTTP；否则仅在 macOS 上尝试 `say`。
//! 未配置且不是 macOS 时返回明确错误，由调用方保留文字播报兜底。
//! 本模块只负责生成 MP3 字节，不直接对外发送。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const DEFAULT_MAX_CHARS: usize = 4_000;
const MAX_MP3_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    OpenAi,
    MacOs,
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn provider() -> Option<Provider> {
    match env_nonempty("TTS_PROVIDER")
        .unwrap_or_else(|| "auto".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "off" | "disabled" | "none" => None,
        "openai" | "http" => Some(Provider::OpenAi),
        "macos" | "say" => Some(Provider::MacOs),
        _ if env_nonempty("TTS_API_URL").is_some() => Some(Provider::OpenAi),
        _ if cfg!(target_os = "macos") => Some(Provider::MacOs),
        _ => None,
    }
}

/// TTS 是否具备可尝试的后端。真正生成失败时调用方仍走文字兜底。
pub fn enabled() -> bool {
    provider().is_some()
}

/// 把一段中文播报稿合成为 MP3。
pub async fn synthesize_mp3(text: &str) -> anyhow::Result<Vec<u8>> {
    let text = text.trim();
    anyhow::ensure!(!text.is_empty(), "TTS 文本为空");
    let max_chars = env_nonempty("TTS_MAX_CHARS")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CHARS)
        .clamp(200, 20_000);
    let chars = text.chars().count();
    anyhow::ensure!(
        chars <= max_chars,
        "TTS 文本过长：{chars} 字，上限 {max_chars} 字"
    );

    let started = std::time::Instant::now();
    let provider = provider().ok_or_else(|| {
        anyhow::anyhow!("TTS 未配置：请设置 TTS_API_URL，或在 macOS 上使用 TTS_PROVIDER=macos")
    })?;
    let bytes = match provider {
        Provider::OpenAi => synthesize_openai(text).await?,
        Provider::MacOs => synthesize_macos(text).await?,
    };
    validate_mp3(&bytes)?;
    tracing::info!(
        provider = ?provider,
        chars,
        bytes = bytes.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "TTS MP3 生成完成"
    );
    Ok(bytes)
}

async fn synthesize_openai(text: &str) -> anyhow::Result<Vec<u8>> {
    let url = env_nonempty("TTS_API_URL")
        .context("TTS_API_URL 未配置（需要完整的 audio/speech 接口地址）")?;
    let key = env_nonempty("TTS_API_KEY").unwrap_or_default();
    let model = env_nonempty("TTS_MODEL").unwrap_or_else(|| "gpt-4o-mini-tts".into());
    let voice = env_nonempty("TTS_VOICE").unwrap_or_else(|| "alloy".into());
    let format = env_nonempty("TTS_FORMAT").unwrap_or_else(|| "mp3".into());
    anyhow::ensure!(
        format.eq_ignore_ascii_case("mp3"),
        "当前链路只接收 MP3，TTS_FORMAT 必须为 mp3"
    );
    let speed = env_nonempty("TTS_SPEED")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0)
        .clamp(0.25, 4.0);
    let timeout_secs = env_nonempty("TTS_TIMEOUT_SECS")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90)
        .clamp(10, 300);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("创建 TTS HTTP 客户端失败")?;
    let mut req = client.post(&url).json(&serde_json::json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": format,
        "speed": speed,
    }));
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.context("TTS 请求失败")?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.context("读取 TTS 响应失败")?;
    if !status.is_success() {
        let preview = String::from_utf8_lossy(&bytes)
            .chars()
            .take(300)
            .collect::<String>();
        anyhow::bail!("TTS 接口返回 HTTP {status}：{preview}");
    }
    anyhow::ensure!(
        !bytes.is_empty(),
        "TTS 接口返回空内容（content-type: {content_type}）"
    );
    Ok(bytes.to_vec())
}

fn temp_paths() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("pk-tts-{}-{nonce}", std::process::id()))
}

struct MacTempFiles {
    aiff: PathBuf,
    mp3: PathBuf,
}

impl Drop for MacTempFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.aiff);
        let _ = std::fs::remove_file(&self.mp3);
    }
}

async fn synthesize_macos(text: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        cfg!(target_os = "macos"),
        "当前系统不支持 macOS say TTS；请配置 TTS_API_URL"
    );
    let voice = env_nonempty("TTS_MACOS_VOICE").unwrap_or_else(|| "Tingting".into());
    let ffmpeg = env_nonempty("TTS_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let base = temp_paths();
    let files = MacTempFiles {
        aiff: base.with_extension("aiff"),
        mp3: base.with_extension("mp3"),
    };

    let mut child = Command::new("say")
        .arg("-v")
        .arg(&voice)
        .arg("-o")
        .arg(&files.aiff)
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("启动 macOS say 失败")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .await
            .context("向 say 写入文本失败")?;
    }
    let said = child
        .wait_with_output()
        .await
        .context("等待 say 完成失败")?;
    anyhow::ensure!(
        said.status.success(),
        "macOS say 失败：{}",
        command_stderr(&said.stderr)
    );
    ensure_nonempty_file(&files.aiff).await?;

    let converted = Command::new(&ffmpeg)
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&files.aiff)
        .arg("-codec:a")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg("64k")
        .arg(&files.mp3)
        .stdin(Stdio::null())
        .output()
        .await
        .context("启动 ffmpeg 失败")?;
    anyhow::ensure!(
        converted.status.success(),
        "ffmpeg 转 MP3 失败：{}",
        command_stderr(&converted.stderr)
    );
    ensure_nonempty_file(&files.mp3).await?;
    tokio::fs::read(&files.mp3)
        .await
        .context("读取生成的 MP3 失败")
}

async fn ensure_nonempty_file(path: &Path) -> anyhow::Result<()> {
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("TTS 输出文件不存在：{}", path.display()))?;
    anyhow::ensure!(meta.len() > 0, "TTS 输出文件为空：{}", path.display());
    anyhow::ensure!(
        meta.len() <= MAX_MP3_BYTES as u64,
        "TTS 输出文件过大：{} 字节",
        meta.len()
    );
    Ok(())
}

fn command_stderr(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let preview = text.trim().chars().take(500).collect::<String>();
    if preview.is_empty() {
        "没有更多错误信息".into()
    } else {
        preview
    }
}

fn validate_mp3(bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(!bytes.is_empty(), "TTS 返回空 MP3");
    anyhow::ensure!(
        bytes.len() <= MAX_MP3_BYTES,
        "TTS 返回的 MP3 过大：{} 字节",
        bytes.len()
    );
    let id3 = bytes.starts_with(b"ID3");
    let frame_sync = bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xe0) == 0xe0;
    anyhow::ensure!(
        id3 || frame_sync,
        "TTS 返回内容不是有效 MP3（缺少 ID3/帧头）"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp3_magic_is_validated() {
        assert!(validate_mp3(b"ID3\x04\x00\x00").is_ok());
        assert!(validate_mp3(&[0xff, 0xfb, 0x90, 0x64]).is_ok());
        assert!(validate_mp3(b"not an mp3").is_err());
        assert!(validate_mp3(b"").is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "手动冒烟：需要 macOS say 与 ffmpeg"]
    async fn macos_tts_smoke() {
        let mp3 = synthesize_macos("播报测试，今天音浪五万二千。")
            .await
            .expect("macOS TTS 应生成 MP3");
        validate_mp3(&mp3).expect("输出应为 MP3");
        assert!(mp3.len() > 1_000, "MP3 太小：{}", mp3.len());
    }
}
