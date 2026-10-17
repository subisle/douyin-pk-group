//! 邮箱发信：163 SMTP（smtp.163.com:465 隐式 TLS，AUTH LOGIN）。
//!
//! 2026-10-17 用户口径：忘记密码重置改为「随机密码发到绑定邮箱」，
//! 这里提供最小的 SMTP 发信能力（不引入 lettre 等重量级依赖，复用
//! workspace 里已有的 rustls + tokio-rustls + webpki-roots）。
//!
//! 协议只走隐式 TLS（SMTPS，连接即握手），顺序：
//!   220 greeting → EHLO → AUTH LOGIN（base64 账号/授权码）→ MAIL FROM
//!   → RCPT TO → DATA（body 走 base64，中文不乱码）→ QUIT。
//!
//! 安全口径（与项目其余出网路径一致）：
//! - SMTP 账号/授权码从环境变量读（config::smtp_*），不硬编码；
//! - 未配置（SMTP_USER / SMTP_PASS 为空）→ 发信直接报错 fail-closed；
//! - 收件地址在入库前已过 validate_email（无 CR/LF/空格），这里再兜一层防头注入。

use std::sync::Arc;

use base64::Engine as _;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// 单次发信的总超时（连接 + TLS + 完整 SMTP 会话）。
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// rustls 0.23 需要「恰好一个」crypto provider 特性：本 workspace 里 reqwest 走
/// aws-lc-rs、tokio-tungstenite 走 ring，两个特性都被编译进来，rustls 无法自动
/// 二选一，第一次建 ClientConfig 会 panic。这里显式安装 ring provider 兜底
/// （与 tokio-tungstenite 同源，进程内幂等，重复调用被忽略）。
///
/// 2026-10-18：除 `send_mail` 外，`main` 启动时也会先调一次 —— 否则后台任务
/// （微信长轮询/推送等）可能抢在第一次发信之前初始化 TLS，导致 worker panic。
pub fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// RFC 2047 B 编码主题头：中文主题到信里不乱码。
pub fn encode_subject(subject: &str) -> String {
    format!("=?UTF-8?B?{}?=", B64.encode(subject.as_bytes()))
}

/// 组装一封 `text/plain; charset=UTF-8` 的邮件原文（body 走 base64 传输编码）。
/// 不包含 DATA 结束符 `\r\n.\r\n`（由发送方追加）。
pub fn build_message(from: &str, to: &str, subject: &str, body: &str) -> String {
    let encoded = B64.encode(body.as_bytes());
    let mut out = String::new();
    out.push_str(&format!("From: {from}\r\n"));
    out.push_str(&format!("To: {to}\r\n"));
    out.push_str(&format!("Subject: {}\r\n", encode_subject(subject)));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    out.push_str("Content-Transfer-Encoding: base64\r\n");
    out.push_str("\r\n");
    // base64 按 76 字符一行折行（SMTP 行长上限）。
    for chunk in encoded.as_bytes().chunks(76) {
        // 输入恒为合法 UTF-8（B64 输出是 ASCII），unwrap 安全
        out.push_str(std::str::from_utf8(chunk).expect("base64 输出非 ASCII"));
        out.push_str("\r\n");
    }
    out
}

/// 读一行（以 `\r\n` 结尾）。SMTP 响应恒为 CRLF 行终止。
async fn read_line<S: AsyncRead + Unpin>(s: &mut S) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        s.read_exact(&mut byte)
            .await
            .map_err(|e| format!("读 SMTP 响应失败：{e}"))?;
        buf.push(byte[0]);
        let n = buf.len();
        if n >= 2 && buf[n - 2] == b'\r' && buf[n - 1] == b'\n' {
            return String::from_utf8(buf).map_err(|_| "SMTP 响应非 UTF-8".to_string());
        }
        if n > 8192 {
            return Err("SMTP 响应行过长".to_string());
        }
    }
}

/// 读一个完整 SMTP 响应（单行 `250 OK` 或多行 `250-a\r\n250 b`）。
/// 返回 `(最后的状态码, 全部行)`。
async fn read_reply<S: AsyncRead + Unpin>(s: &mut S) -> Result<(String, Vec<String>), String> {
    let mut lines = Vec::new();
    loop {
        let line = read_line(s).await?;
        if line.len() < 4 {
            return Err(format!("SMTP 响应格式异常：{line:?}"));
        }
        let multiline = line.as_bytes()[3] == b'-';
        lines.push(line.clone());
        if !multiline {
            return Ok((line[..3].to_string(), lines));
        }
    }
}

/// 发一条命令并核对状态码。`cmd` 为空 = 只读下一段响应。
async fn expect<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    cmd: &str,
    ok: &[&str],
) -> Result<String, String> {
    if !cmd.is_empty() {
        s.write_all(cmd.as_bytes())
            .await
            .map_err(|e| format!("写 SMTP 失败：{e}"))?;
    }
    let (code, lines) = read_reply(s).await?;
    if !ok.contains(&code.as_str()) {
        return Err(format!(
            "SMTP 服务器拒绝（命令 {cmd:?} → {code}）：{}",
            lines.join(" ")
        ));
    }
    Ok(code)
}

/// 发一封纯文本邮件。`to` 必须是已校验的邮箱地址。
pub async fn send_mail(to: &str, subject: &str, body: &str) -> Result<(), String> {
    let host = crate::config::smtp_host();
    let port = crate::config::smtp_port();
    let user = crate::config::smtp_user();
    let pass = crate::config::smtp_pass();
    let from = crate::config::smtp_from();

    if user.is_empty() || pass.is_empty() {
        return Err("SMTP 未配置（缺 SMTP_USER / SMTP_PASS）".to_string());
    }
    // 防头注入：收件地址/发件地址绝不能带换行（入库前已校验，这里兜底）。
    if to.contains('\r') || to.contains('\n') || from.contains('\r') || from.contains('\n') {
        return Err("邮箱地址非法（含换行符）".to_string());
    }

    ensure_crypto_provider();
    let roots: Vec<_> = webpki_roots::TLS_SERVER_ROOTS.to_vec();
    let mut store = RootCertStore::empty();
    store.extend(roots);
    let config = ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(host.clone())
        .map_err(|_| format!("非法 SMTP 主机名：{host}"))?
        .to_owned();

    let work = async move {
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("连接 SMTP 服务器失败：{e}"))?;
        let mut s = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| format!("SMTP TLS 握手失败：{e}"))?;

        expect(&mut s, "", &["220"]).await?;
        expect(&mut s, &format!("EHLO {host}\r\n"), &["250"]).await?;
        expect(&mut s, "AUTH LOGIN\r\n", &["334"]).await?;
        expect(&mut s, &format!("{}\r\n", B64.encode(user.as_bytes())), &["334"]).await?;
        expect(&mut s, &format!("{}\r\n", B64.encode(pass.as_bytes())), &["235"]).await?;
        expect(&mut s, &format!("MAIL FROM:<{from}>\r\n"), &["250"]).await?;
        expect(&mut s, &format!("RCPT TO:<{to}>\r\n"), &["250", "251"]).await?;
        expect(&mut s, "DATA\r\n", &["354"]).await?;

        let msg = build_message(&from, to, subject, body);
        s.write_all(msg.as_bytes())
            .await
            .map_err(|e| format!("写邮件正文失败：{e}"))?;
        s.write_all(b"\r\n.\r\n")
            .await
            .map_err(|e| format!("结束邮件正文失败：{e}"))?;
        expect(&mut s, "", &["250"]).await?;
        let _ = s.write_all(b"QUIT\r\n").await;
        Ok(())
    };

    tokio::time::timeout(SEND_TIMEOUT, work)
        .await
        .map_err(|_| "发送邮件超时".to_string())?
}

/* ---------------------- 临时密码邮件（2026-10-18 契约 §3.5） ----------------------
 *
 * 忘记密码改成「手机号 + QQ」双因子后，发的是**15 分钟内有效的临时密码**，
 * 不是直接替换常密码（常密码由 store 侧覆盖成不可知哈希）。模板口径：
 * - 只含临时密码 / 15 分钟有效期 / 「登录后立即修改」/「不是本人操作请忽略」；
 * - **不含账号名与手机号**（邮件可能被转发或长期留存，身份信息一律不进信体）；
 * - 纯文本，由 [`send_mail`] 统一做 base64 正文 + RFC2047 主题编码。
 */

/// 临时密码邮件主题（契约 §3.5 冻结文案）。
pub const TEMP_PASS_SUBJECT: &str = "抖音PK · 密码重置临时密码（15 分钟内有效）";

/// 临时密码邮件正文（纯文本）。只做模板拼接，不读任何账号信息。
pub fn temp_password_body(temp_pass: &str) -> String {
    format!(
        "您好：\n\n\
您在「抖音PK」展示端申请了密码重置，本次登录临时密码为：\n\n\
    {temp_pass}\n\n\
该密码 15 分钟内有效。请使用它登录展示端，登录后立即在「我的」页修改为自己的密码。\n\n\
如果这不是您本人的操作，请忽略本邮件，您的账号密码不会被修改；如连续收到本邮件，请联系管理员。\n\n\
（本邮件由系统自动发送，请勿回复。）"
    )
}

/// 发送临时密码邮件（复用 [`send_mail`]）。
///
/// 返回 `Err` 时调用方必须**不落库、不改密码、不吊销 token**（否则用户会拿到一封
/// 「已发信」却登不进的新密码 —— 契约 §3.5 的严格顺序）。
pub async fn send_temp_password_mail(to: &str, temp_pass: &str) -> Result<(), String> {
    send_mail(to, TEMP_PASS_SUBJECT, &temp_password_body(temp_pass)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_is_rfc2047_b64_encoded() {
        let s = encode_subject("密码重置通知");
        assert!(s.starts_with("=?UTF-8?B?"));
        assert!(s.ends_with("?="));
        // 解码回来核对
        let inner = &s["=?UTF-8?B?".len()..s.len() - "?=".len()];
        let decoded = B64.decode(inner).expect("主题应能解码");
        assert_eq!(String::from_utf8(decoded).unwrap(), "密码重置通知");
    }

    #[test]
    fn message_contains_headers_and_base64_body() {
        let m = build_message("a@163.com", "b@163.com", "hi", "你好");
        assert!(m.contains("From: a@163.com\r\n"));
        assert!(m.contains("To: b@163.com\r\n"));
        assert!(m.contains("Content-Transfer-Encoding: base64"));
        // 正文按 base64 传输，原始中文不应裸出现
        assert!(!m.contains("你好"));
    }

    #[test]
    fn message_lines_wrapped_at_76() {
        let long = "x".repeat(200);
        let m = build_message("a@163.com", "b@163.com", "s", &long);
        // 正文是 base64，所有非空行都 ≤ 76
        let encoded = B64.encode(long.as_bytes());
        assert!(encoded.len() > 76, "样例应触发折行");
        let b64_lines: Vec<&str> = m
            .split("\r\n")
            .filter(|l| !l.is_empty() && !l.contains(':'))
            .collect();
        assert!(b64_lines.iter().all(|l| l.len() <= 76));
    }

    /// 临时密码邮件模板（契约 §3.5）：必须含临时密码与「15 分钟 / 立即修改 / 不是本人
    /// 操作请忽略」四条信息；信体只由固定文案 + 密码拼成，不含账号名 / 手机号 / 邮箱。
    #[test]
    fn temp_password_mail_template_matches_contract() {
        let body = temp_password_body("Ab3kP9xQ2m");
        assert!(body.contains("Ab3kP9xQ2m"), "必须带临时密码：{body}");
        assert!(body.contains("15 分钟"), "必须写有效期：{body}");
        assert!(body.contains("立即"), "必须提示登录后立即修改：{body}");
        assert!(body.contains("不是您本人的操作"), "必须有非本人操作提示：{body}");
        assert_eq!(TEMP_PASS_SUBJECT, "抖音PK · 密码重置临时密码（15 分钟内有效）");
        // 信体不含任何身份信息（模板只接受临时密码一个插值）
        assert!(!body.contains('@'), "信体不得出现邮箱：{body}");
        assert!(!body.contains("192."), "信体不得出现 IP/手机号片段：{body}");
        // 主题经 RFC2047 编码后仍能解码回原文（复用 send_mail 的编码路径）
        let header = encode_subject(TEMP_PASS_SUBJECT);
        let inner = &header["=?UTF-8?B?".len()..header.len() - "?=".len()];
        let decoded = B64.decode(inner).expect("主题应能解码");
        assert_eq!(String::from_utf8(decoded).unwrap(), TEMP_PASS_SUBJECT);
    }

    /// 端到端实发冒烟（手动跑，不随 `cargo test` 自动执行）：
    /// `cargo test -p pk-server --lib mailer::tests::smtp_live_smoke -- --ignored --nocapture`
    /// 用 rust/.env 里的凭据发一封到发件箱自己（cszimy@163.com），验证手写 SMTP 全链路。
    #[test]
    #[ignore]
    fn smtp_live_smoke() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let e = crate::mailer::send_mail(
                "cszimy@163.com",
                "抖音PK · SMTP 冒烟测试",
                "这是一封系统自检邮件：手写 SMTP 客户端端到端验证。收到即表示链路正常，请忽略。",
            )
            .await;
            eprintln!("send_mail result: {e:?}");
            assert!(e.is_ok(), "实发失败：{e:?}");
        });
    }
}
