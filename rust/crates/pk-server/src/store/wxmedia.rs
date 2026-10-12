//! store::wxmedia —— 微信 iLink 图片发送（2026-10-12 每日报告图片推送）。
//!
//! 协议对齐 openclaw-weixin（/private/tmp/openclaw-weixin-src 参考实现）：
//! 1. `getuploadurl`：报明文大小/MD5/密文大小 + 自生成 16 字节 AES key（hex）
//! 2. AES-128-ECB（PKCS#7）加密明文，POST 到 CDN（upload_full_url 优先，
//!    否则 `{CDN_BASE}/upload?encrypted_query_param=…&filekey=…`）
//! 3. 取响应头 `x-encrypted-param` 作为下载参数
//! 4. `sendmessage`：先发一条 TEXT item（问候语/说明），再发一条 IMAGE item
//!    （`image_item.media.encrypt_query_param` + `aes_key`(base64) + `mid_size`）
//!
//! 每条消息独立一次 sendmessage（openclaw 行为：item_list 恒单条）。

use anyhow::Context;

use crate::bot::weixin::{check_code, client_id, wx_json, CDN_BASE_URL, CHANNEL_VERSION};

/// AES-128-ECB + PKCS#7 加密
fn aes_ecb_encrypt(plaintext: &[u8], key: &[u8; 16]) -> Vec<u8> {
    use aes::cipher::{BlockEncrypt, KeyInit};
    let cipher = aes::Aes128::new(key.into());
    let pad = 16 - (plaintext.len() % 16);
    let mut buf = Vec::with_capacity(plaintext.len() + pad);
    buf.extend_from_slice(plaintext);
    buf.resize(plaintext.len() + pad, pad as u8);
    for chunk in buf.chunks_mut(16) {
        cipher.encrypt_block(chunk.into());
    }
    buf
}

/// PKCS#7 填充后的密文大小
fn padded_size(n: usize) -> usize {
    (n / 16 + 1) * 16
}

fn http() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client")
    })
}

/// 上传结果：发 image_item 所需的全部字段
struct Uploaded {
    /// CDN 返回的下载参数（x-encrypted-param）
    download_param: String,
    /// AES key（hex，原始生成值）
    aeskey_hex: String,
    /// 密文大小（PKCS#7 后）
    file_size_cipher: usize,
}

/// 上传一张图片（PNG 字节）到微信 CDN
async fn upload_image(
    base_url: &str,
    token: &str,
    to_user_id: &str,
    png: &[u8],
) -> anyhow::Result<Uploaded> {
    let aeskey: [u8; 16] = rand_bytes();
    let filekey = hex(&rand_bytes());
    let rawsize = png.len();
    let filesize = padded_size(rawsize);
    let md5hex = {
        use md5::Digest;
        let mut h = md5::Md5::new();
        h.update(png);
        hex(&h.finalize())
    };

    // ① getuploadurl
    let resp = wx_json(
        reqwest::Method::POST,
        base_url,
        "getuploadurl",
        &[],
        Some(token),
        Some(serde_json::json!({
            "filekey": filekey,
            "media_type": 1,              // 1 图片
            "to_user_id": to_user_id,
            "rawsize": rawsize,
            "rawfilemd5": md5hex,
            "filesize": filesize,
            "no_need_thumb": true,
            "aeskey": hex(&aeskey),
            "base_info": { "channel_version": CHANNEL_VERSION, "bot_agent": "OpenClaw" }
        })),
    )
    .await
    .context("getuploadurl 请求失败")?;
    check_code(&resp)?;

    let upload_full_url = resp
        .get("upload_full_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let upload_param = resp
        .get("upload_param")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    anyhow::ensure!(
        !upload_full_url.is_empty() || !upload_param.is_empty(),
        "getuploadurl 未返回上传地址"
    );

    // ② AES 加密后 POST CDN
    let ciphertext = aes_ecb_encrypt(png, &aeskey);
    let cdn_url = if !upload_full_url.is_empty() {
        upload_full_url
    } else {
        format!(
            "{CDN_BASE_URL}/upload?encrypted_query_param={}&filekey={}",
            urlencode(&upload_param),
            urlencode(&filekey)
        )
    };
    let r = http()
        .post(&cdn_url)
        .header("Content-Type", "application/octet-stream")
        .body(ciphertext)
        .send()
        .await
        .context("CDN 上传请求失败")?;
    anyhow::ensure!(
        r.status().is_success(),
        "CDN 上传失败 HTTP {}",
        r.status()
    );
    let download_param = r
        .headers()
        .get("x-encrypted-param")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    anyhow::ensure!(
        !download_param.is_empty(),
        "CDN 响应缺 x-encrypted-param 头"
    );

    Ok(Uploaded {
        download_param,
        aeskey_hex: hex(&aeskey),
        file_size_cipher: filesize,
    })
}

/// 发一条单 item 消息（openclaw：每条 item 独立一次 sendmessage）
async fn send_item(
    base_url: &str,
    token: &str,
    to_user_id: &str,
    context_token: &str,
    item: serde_json::Value,
) -> anyhow::Result<()> {
    let out = wx_json(
        reqwest::Method::POST,
        base_url,
        "sendmessage",
        &[],
        Some(token),
        Some(serde_json::json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": client_id(),
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [item]
            },
            "base_info": { "channel_version": CHANNEL_VERSION, "bot_agent": "OpenClaw" }
        })),
    )
    .await?;
    check_code(&out)
}

/// 发图片 + 前置说明文字。`caption` 可空。
pub async fn user_send_image(
    base_url: &str,
    token: &str,
    to_user_id: &str,
    context_token: &str,
    caption: &str,
    png: &[u8],
) -> anyhow::Result<()> {
    let up = upload_image(base_url, token, to_user_id, png).await?;
    if !caption.is_empty() {
        send_item(
            base_url,
            token,
            to_user_id,
            context_token,
            serde_json::json!({ "type": 1, "text_item": { "text": caption } }),
        )
        .await?;
    }
    send_item(
        base_url,
        token,
        to_user_id,
        context_token,
        serde_json::json!({
            "type": 2,
            "image_item": {
                "media": {
                    "encrypt_query_param": up.download_param,
                    "aes_key": b64(up.aeskey_hex.as_bytes()),
                    "encrypt_type": 1
                },
                "mid_size": up.file_size_cipher
            }
        }),
    )
    .await
}

/* ------------------------------ 小工具 ------------------------------ */

fn rand_bytes() -> [u8; 16] {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("rng");
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for c in s.bytes() {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(c as char)
            }
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkcs7_padding_sizes() {
        assert_eq!(padded_size(0), 16);
        assert_eq!(padded_size(15), 16);
        assert_eq!(padded_size(16), 32);
        assert_eq!(padded_size(100), 112);
    }

    #[test]
    fn aes_ecb_roundtrip_shape() {
        let key = [7u8; 16];
        let plain = b"hello world, this is 32 bytes!!!";
        let cipher = aes_ecb_encrypt(plain, &key);
        assert_eq!(cipher.len(), padded_size(plain.len()));
        // 相同明文相同 key → 相同密文（确定性，对齐协议）
        assert_eq!(cipher, aes_ecb_encrypt(plain, &key));
        // 空数据 → 一整块填充
        assert_eq!(aes_ecb_encrypt(b"", &key).len(), 16);
    }

    #[test]
    fn urlencode_matches_openclaw() {
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
        assert_eq!(urlencode("ABC-_.~"), "ABC-_.~");
    }
}
