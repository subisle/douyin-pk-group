package data

import (
	"context"
	"database/sql"
	"embed"
	"fmt"
	"io/fs"
	"path/filepath"
	"sort"
	"strings"
)

// migration SQL 编译进二进制：部署时只有一个可执行文件，
// 不用再保证容器里 internal/data/migrations/ 的相对路径存在。
//
//go:embed migrations/*.sql
var migrationsFS embed.FS

// MigrateAll 依次执行 migrations/*.sql（按文件名排序），
// 已在 schema_migrations 里登记的跳过。SQL 本身全部 IF NOT EXISTS，双保险幂等。
func (s *Store) MigrateAll(ctx context.Context) ([]string, error) {
	if _, err := s.db.ExecContext(ctx, `
CREATE TABLE IF NOT EXISTS schema_migrations (
  version    VARCHAR(64) NOT NULL,
  applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (version)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4`); err != nil {
		return nil, fmt.Errorf("create schema_migrations: %w", err)
	}

	entries, err := fs.Glob(migrationsFS, "migrations/*.sql")
	if err != nil {
		return nil, err
	}
	sort.Strings(entries)

	done := map[string]bool{}
	rows, err := s.db.QueryContext(ctx, "SELECT version FROM schema_migrations")
	if err != nil {
		return nil, err
	}
	for rows.Next() {
		var v string
		if err := rows.Scan(&v); err != nil {
			rows.Close()
			return nil, err
		}
		done[v] = true
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}

	var applied []string
	for _, name := range entries {
		version := strings.TrimSuffix(filepath.Base(name), ".sql")
		if done[version] {
			continue
		}
		sqlBytes, err := migrationsFS.ReadFile(name)
		if err != nil {
			return applied, err
		}
		if _, err := s.db.ExecContext(ctx, string(sqlBytes)); err != nil {
			return applied, fmt.Errorf("apply %s: %w", version, err)
		}
		if _, err := s.db.ExecContext(ctx,
			"INSERT INTO schema_migrations (version) VALUES (?)", version); err != nil {
			return applied, fmt.Errorf("record %s: %w", version, err)
		}
		applied = append(applied, version)
	}
	return applied, nil
}

type Store struct {
	db *sql.DB
}

type StaticStore struct{}

func NewStaticStore() StaticStore {
	return StaticStore{}
}

func (StaticStore) Query(_ context.Context, args []string) (string, error) {
	if len(args) == 0 {
		return "请输入主播名。", nil
	}
	return "查询 " + args[0] + "：数据层尚未接入 MariaDB。", nil
}

func (StaticStore) Rank(_ context.Context, args []string) (string, error) {
	date := "全部日期"
	if len(args) > 0 {
		date = args[0]
	}
	return "排行（" + date + "）：数据层尚未接入 MariaDB。", nil
}

func Open(driverName, dsn string) (*Store, error) {
	db, err := sql.Open(driverName, dsn)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(8)
	db.SetMaxIdleConns(2)
	return &Store{db: db}, nil
}

func (s *Store) Close() error {
	return s.db.Close()
}

func (s *Store) Migrate(ctx context.Context, migrationSQL string) error {
	_, err := s.db.ExecContext(ctx, migrationSQL)
	return err
}

func (s *Store) Health(ctx context.Context) error {
	if s == nil || s.db == nil {
		return fmt.Errorf("data store is not initialized")
	}
	return s.db.PingContext(ctx)
}

func (s *Store) Query(ctx context.Context, args []string) (string, error) {
	if len(args) == 0 {
		return "请输入主播名。", nil
	}
	var status string
	err := s.db.QueryRowContext(ctx, "SELECT status FROM person WHERE name = ? LIMIT 1", args[0]).Scan(&status)
	if err == sql.ErrNoRows {
		return "未找到主播：" + args[0], nil
	}
	if err != nil {
		return "", err
	}
	return args[0] + "：" + status, nil
}

func (s *Store) Rank(ctx context.Context, args []string) (string, error) {
	rows, err := s.db.QueryContext(ctx, `
SELECT p.name, SUM(d.wave) AS total_wave
FROM person p
JOIN daily_metric d ON d.person_id = p.id
GROUP BY p.id, p.name
ORDER BY total_wave DESC, p.name
LIMIT 10`)
	if err != nil {
		return "", err
	}
	defer rows.Close()

	result := "音浪排行："
	for rank := 1; rows.Next(); rank++ {
		var name string
		var wave int64
		if err := rows.Scan(&name, &wave); err != nil {
			return "", err
		}
		result += fmt.Sprintf("\n%d. %s %d", rank, name, wave)
	}
	if result == "音浪排行：" {
		result += "\n暂无数据"
	}
	return result, rows.Err()
}
