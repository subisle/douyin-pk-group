-- 目标端角色/权限引导（幂等）：对齐 docs/17-数据库对齐与64部署方案.md §5.3 第 4 步
-- 与本机实查的权限模型（docs/16 §2.3）：owner=pk_writer、pk_reader 只读、默认 ACL 4 条。
--
-- 由 scripts/deploy/bootstrap-roles-64.sh 做占位符替换后执行：
--   :'reader_pass' / :'writer_pass'  → 角色口令（为空字符串则不改口令）
--   :"app_db"                        → 目标库名
--   :"app_user"                      → 目标库属主（默认 ACL 的 FOR ROLE）

\set ON_ERROR_STOP on

-- 1) 角色（幂等：本地集群可能已存在同名角色）
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pk_reader') THEN CREATE ROLE pk_reader LOGIN; END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pk_writer') THEN CREATE ROLE pk_writer LOGIN; END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pk_backup') THEN CREATE ROLE pk_backup LOGIN; END IF;
END $$;

-- 2) 口令（未提供则保持现状，避免把空口令刷上去）
DO $$
BEGIN
  IF :'reader_pass' <> '' THEN
    EXECUTE format('ALTER ROLE pk_reader WITH PASSWORD %L', :'reader_pass');
  END IF;
  IF :'writer_pass' <> '' THEN
    EXECUTE format('ALTER ROLE pk_writer WITH PASSWORD %L', :'writer_pass');
  END IF;
END $$;

-- 3) 库 / 模式权限
GRANT CONNECT ON DATABASE :"app_db" TO pk_reader, pk_writer, pk_backup;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO pk_reader, pk_writer, pk_backup;

-- 4) 表 / 序列权限
GRANT SELECT ON ALL TABLES IN SCHEMA public TO pk_reader, pk_backup;
GRANT SELECT ON ALL SEQUENCES IN SCHEMA public TO pk_reader;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO pk_writer;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO pk_writer;

-- 5) owner 移交 pk_writer（DDL 由 migrator=pk_writer 执行）
DO $$
DECLARE r record;
BEGIN
  FOR r IN SELECT tablename FROM pg_tables WHERE schemaname = 'public' LOOP
    EXECUTE format('ALTER TABLE public.%I OWNER TO pk_writer', r.tablename);
  END LOOP;
  FOR r IN SELECT sequencename FROM pg_sequences WHERE schemaname = 'public' LOOP
    EXECUTE format('ALTER SEQUENCE public.%I OWNER TO pk_writer', r.sequencename);
  END LOOP;
  FOR r IN SELECT p.oid::regprocedure AS sig
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
           WHERE n.nspname = 'public' LOOP
    EXECUTE format('ALTER FUNCTION public.%s OWNER TO pk_writer', r.sig);
  END LOOP;
END $$;

-- 6) 默认 ACL：将来 migrate 新建的表/序列自动授权（docs/16 §2.4 #3）
ALTER DEFAULT PRIVILEGES FOR ROLE :"app_user" IN SCHEMA public GRANT SELECT ON TABLES TO pk_reader;
ALTER DEFAULT PRIVILEGES FOR ROLE :"app_user" IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO pk_writer;
ALTER DEFAULT PRIVILEGES FOR ROLE :"app_user" IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO pk_reader, pk_writer;

-- 7) 读账号不得持有 DDL 权（docs/16 §2.4 #2）
REVOKE CREATE ON SCHEMA public FROM pk_reader;

-- 8) 结果报告
SELECT '角色' AS 项, string_agg(rolname || '(login=' || rolcanlogin::text || ')', ', ' ORDER BY rolname) AS 值
  FROM pg_roles WHERE rolname IN ('pk_reader', 'pk_writer', 'pk_backup')
UNION ALL
SELECT 'owner=pk_writer 的表数', count(*)::text
  FROM pg_tables WHERE schemaname = 'public' AND tableowner = 'pk_writer'
UNION ALL
SELECT 'pk_reader 可建表(应为 false)', has_schema_privilege('pk_reader', 'public', 'CREATE')::text
UNION ALL
SELECT '默认 ACL 条数', count(*)::text FROM pg_default_acl;
