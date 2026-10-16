-- 028: 绑定码改 4 位数字（10^4 空间）→「一个活码只挂一个账号」必须由库保证。
--
-- 背景（2026-10-16 用户口径）：展示端「我的」页不再走官方扫码登录绑定，改成
-- 「扫码加机器人 → 点获取绑定码 → 把 4 位数字发给机器人」即可绑定（不再要手机号）。
-- 4 位只有 10000 种取值，而 push_bind_by_code 是 `WHERE bind_code = ?` 单行取账号：
-- 两行同码时会把微信绑到**别人**的账号上（从此收到别人的推送数据）。
-- 应用侧 store::push_rotate_code 会先查后写、撞号重摇；并发竞态靠这里的部分唯一
-- 索引兜底（空码不参与，未生成过码的行不受影响）。
--
-- 建索引前先清历史重复码：同一码里只留 updated_at 最新（并列按 name 定序）的一行，
-- 其余清空（本人重新点一次「获取绑定码」即可）。绑定码本来就是一次性、30 分钟有效，
-- 清掉没有副作用。
UPDATE push_bindings p
   SET bind_code = '', code_expires = ''
 WHERE p.bind_code <> ''
   AND EXISTS (
     SELECT 1 FROM push_bindings q
      WHERE q.bind_code = p.bind_code
        AND q.name <> p.name
        AND (q.updated_at > p.updated_at
             OR (q.updated_at = p.updated_at AND q.name > p.name))
   );
CREATE UNIQUE INDEX IF NOT EXISTS uk_push_bind_code
    ON push_bindings (bind_code) WHERE bind_code <> '';
