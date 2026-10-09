# 初始化只读账号 pk_reader（S3：pk-server 连本地主库的只读账号）。
#
# 为什么用 .sh 且故意不加可执行位：
# MariaDB 官方镜像的 /docker-entrypoint-initdb.d 对可执行 .sh 会 fork 子进程跑，
# 那样拿不到 entrypoint 里的 docker_process_sql / SOCKET；不可执行的 .sh 会被 source，
# 能直接调用官方连接助手（自动走 unix socket + root 密码，和「先设 root 密码、
# 再跑 initdb」的执行顺序完全匹配）。
#
# 密码从 compose 透传的环境变量读，与 .env 的 PK_READER_PASSWORD 保持一致；
# SQL 字面量走官方 docker_sql_escape_string_literal 转义，密码含特殊字符也安全。

if [ -n "${PK_READER_USER:-}" ] && [ -n "${PK_READER_PASSWORD:-}" ]; then
	reader_pass="$(docker_sql_escape_string_literal "${PK_READER_PASSWORD}")"
	docker_process_sql <<SQL
CREATE USER IF NOT EXISTS '${PK_READER_USER}'@'%' IDENTIFIED BY '${reader_pass}';
GRANT SELECT ON douyin.* TO '${PK_READER_USER}'@'%';
FLUSH PRIVILEGES;
SQL
fi
