//! PG 占位符重写器：把 MySQL 风格的 `?` 占位符重写成 PostgreSQL 的 `$1..$N`。
//!
//! 背景：pk-server 里有 200+ 处 `sqlx::query(&crate::pq::rewrite("… WHERE x = ?").bind(v)` 调用点。
//! 切到 PG 后逐处手改 `$1/$2` 既枯燥又容易数错，所以集中在这里做一次机械重写：
//! - 只重写**字符串字面量之外**的 `?`（值里的 `'?'` 不动）
//! - 按出现顺序编号 `$1, $2, …`
//! - SQL 常量里已经写过 `$N` 的（PG 原生写法）不重复编号——重写器从 0 开始数，
//!   所以调用方只要保证 SQL 里**要么全是 `?` 要么全是 `$N`**，不混用。
//!
//! 用法：`sqlx::query(&pq("SELECT … WHERE id = ?"))`。
//!
//! 2026-10-12 起的统一约定（数据库访问收敛）：
//! - **读路径**：走 `pgrs::query` / `pgrs::query_with_binds`，入口内建本重写，调用方无需手动包；
//! - **写路径**：走 writer 池，SQL 过 `writer::write_guard` 后再 `sqlx::query(&crate::pq::rewrite(sql))`；
//! - 新增调用点不再手动裸传 `?` 字面量给 `sqlx::query`（历史上 60+ 处漏包就是这来的）。
//!
//! 注：旧注释提过的编译期 `pq!` 宏从未实现（const fn 也返回不了 String），已移除该说法。

/// 重写 SQL 里的 `?` 占位符为 `$N`（跳过单引号字符串字面量）。
/// 不支持反斜杠转义的字符串（PG 标准 `'` 才是转义），当前代码库里没有这种写法。
#[inline]
pub fn rewrite(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n: usize = 0;
    let mut in_str = false;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\'' {
                // '' 是字符串里转义的单引号
                if chars.peek() == Some(&'\'') {
                    out.push('\'');
                    chars.next();
                } else {
                    in_str = false;
                }
            }
        } else {
            match c {
                '\'' => {
                    in_str = true;
                    out.push(c);
                }
                '?' => {
                    n += 1;
                    out.push('$');
                    out.push_str(&n.to_string());
                }
                _ => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_placeholders_in_order() {
        assert_eq!(
            rewrite("SELECT * FROM t WHERE a = ? AND b = ?"),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
        assert_eq!(
            rewrite("INSERT INTO t (a, b) VALUES (?, ?)"),
            "INSERT INTO t (a, b) VALUES ($1, $2)"
        );
        assert_eq!(rewrite("no placeholders"), "no placeholders");
    }

    #[test]
    fn skips_string_literals() {
        assert_eq!(
            rewrite("SELECT * FROM t WHERE note = 'a?b' AND id = ?"),
            "SELECT * FROM t WHERE note = 'a?b' AND id = $1"
        );
        assert_eq!(
            rewrite("WHERE x = 'it''s ?' AND y = ?"),
            "WHERE x = 'it''s ?' AND y = $1"
        );
    }

    #[test]
    fn handles_utf8() {
        assert_eq!(rewrite("SELECT '中?文' , ?"), "SELECT '中?文' , $1");
    }
}
