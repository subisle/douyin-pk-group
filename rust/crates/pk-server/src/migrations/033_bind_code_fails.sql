-- 033: 对接码防爆破（2026-10-17 审计 4-P1-2）。
--
-- 背景：对接码是 4 位数字（10^4 空间、30 分钟有效、一次性），而兑换口
-- push_bind_by_code 的唯一限流是机器人层「每会话 10 条/分钟」—— 攻击者用
-- N 个微信号并行即 N×10 次/分钟，30 分钟窗口内单号可试 300 次；命中即把
-- 受害者的微信推送通道绑到攻击者（个人音浪数据外泄 + 真用户被抢绑）。
--
-- 本迁移建一张计数表，语义全部在 store/wxpush.rs 里实现：
-- 1. 按码失败计数：同一码失败 ≥5 次 → 立即作废该码（防单码在线爆破）；
-- 2. 全局失败速率：哨兵行 code='__global__'，1 小时窗口失败 ≥600 次 →
--    全局冷却 10 分钟（唯一能挡跨会话分布式匀速试码的闸）。
--
-- 不复用 push_bindings 的行承载「错码」计数：码不对时根本定位不到行。

CREATE TABLE IF NOT EXISTS bind_code_fails (
    code       VARCHAR(16) PRIMARY KEY,   -- 4 位对接码，或哨兵 '__global__'
    fails      INTEGER      NOT NULL DEFAULT 0,
    window_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),  -- 当前计数窗口起点
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT now()
);
