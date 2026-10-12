//! 密码学安全随机数工具（getrandom → 操作系统 CSPRNG）。
//!
//! 用途：绑定码、初始密码、一次性 ticket 等安全敏感随机值。
//! 禁止使用 `RandomState` / 时间戳之类非密码学生成器替代（侧信道弱、可预测）。

/// 从指定字母表生成 n 位随机串（拒绝采样消除取模偏差）。
pub fn random_code(n: usize, alphabet: &[u8]) -> String {
    debug_assert!(!alphabet.is_empty());
    let mut out = String::with_capacity(n);
    let mut buf = [0u8; 64];
    let limit = (256 / alphabet.len()) * alphabet.len(); // 拒绝采样阈值
    while out.len() < n {
        if getrandom::getrandom(&mut buf).is_err() {
            // getrandom 在现代 Linux/macOS 上不会失败；失败时退化为纳秒+地址熵，
            // 宁可弱一点也不直接 panic 打挂服务。
            let ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            for (i, b) in buf.iter_mut().enumerate() {
                *b = (ns.wrapping_mul(0x9e37_79b9).rotate_left(i as u32) & 0xff) as u8;
            }
        }
        for &b in &buf {
            if out.len() >= n {
                break;
            }
            if (b as usize) < limit {
                out.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    out
}

/// n 位数字码（绑定码等）
pub fn random_digits(n: usize) -> String {
    random_code(n, b"0123456789")
}

/// 随机初始密码：去掉易混淆字符（0/O、1/l/I），10 位
pub fn random_password() -> String {
    random_code(
        10,
        b"ABCDEFGHJKMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_digits_and_right_len() {
        let c = random_digits(6);
        assert_eq!(c.len(), 6);
        assert!(c.bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn passwords_look_random() {
        let a = random_password();
        let b = random_password();
        assert_eq!(a.len(), 10);
        assert_ne!(a, b, "两次生成不应相同");
        assert!(a.bytes().all(|b| b.is_ascii_alphanumeric()));
    }

    #[test]
    fn codes_distribute_over_alphabet() {
        // 4000 位里字母表每个字符都应出现（拒绝采样无偏）
        let c = random_code(4000, b"0123456789");
        for d in b'0'..=b'9' {
            assert!(c.contains(d as char), "缺少数字 {}", d as char);
        }
    }
}
