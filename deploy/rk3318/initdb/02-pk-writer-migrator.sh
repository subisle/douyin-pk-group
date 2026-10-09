# 初始化业务写账号 douyin_writer 与迁移账号 douyin_migrator（B 阶段）。
#
# 与 01-pk-reader.sh 同样：不可执行 .sh 会被 MariaDB entrypoint source，
# 能直接调用 docker_process_sql（unix socket + root，密码转义安全）。
#
# 权限三分离（docs/00 §4，2026-10-04 决策）：
# - douyin_writer    业务表 DML，**永不持有 DDL**
# - douyin_migrator  DDL + DML，仅 pk-server migrate 显式触发时使用
# - pk_reader        只读（见 01-pk-reader.sh）

if [ -n "${DOUYIN_WRITER_USER:-}" ] && [ -n "${DOUYIN_WRITER_PASSWORD:-}" ]; then
	writer_pass="$(docker_sql_escape_string_literal "${DOUYIN_WRITER_PASSWORD}")"
	docker_process_sql <<SQL
CREATE USER IF NOT EXISTS '${DOUYIN_WRITER_USER}'@'%' IDENTIFIED BY '${writer_pass}';
GRANT SELECT, INSERT, UPDATE, DELETE ON douyin.* TO '${DOUYIN_WRITER_USER}'@'%';
FLUSH PRIVILEGES;
SQL
fi

if [ -n "${DOUYIN_MIGRATOR_USER:-}" ] && [ -n "${DOUYIN_MIGRATOR_PASSWORD:-}" ]; then
	migrator_pass="$(docker_sql_escape_string_literal "${DOUYIN_MIGRATOR_PASSWORD}")"
	docker_process_sql <<SQL
CREATE USER IF NOT EXISTS '${DOUYIN_MIGRATOR_USER}'@'%' IDENTIFIED BY '${migrator_pass}';
GRANT ALL PRIVILEGES ON douyin.* TO '${DOUYIN_MIGRATOR_USER}'@'%';
FLUSH PRIVILEGES;
SQL
fi
