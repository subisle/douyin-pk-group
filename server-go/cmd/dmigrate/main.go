// dmigrate：把 sqlpub 源库（douyinxs）全量搬进本地 MariaDB（S2 数据迁移）。
//
// 用法：
//
//	SRC_DSN='douyinxs:***@tcp(mysql7.sqlpub.com:3312)/douyinxs' \
//	DST_DSN='douyin_writer:***@tcp(127.0.0.1:3306)/douyin' \
//	go run ./cmd/dmigrate
//
// 幂等：person 按 id 主键、account 按 uk_anchor、daily_metric 按 uk_person_date
// 全部 ON DUPLICATE KEY UPDATE，可重复执行对齐。
package main

import (
	"database/sql"
	"fmt"
	"log"
	"os"

	_ "github.com/go-sql-driver/mysql"
)

type nullDate = sql.NullString

func scanPerson(rows *sql.Rows) ([]any, error) {
	var (
		id, masterID, generation              sql.NullInt64
		name, gender, groupName, avatar, stat sql.NullString
		hide                                  sql.NullBool
		joinedAt, deletedAt                   nullDate
	)
	if err := rows.Scan(&id, &name, &gender, &masterID, &generation, &groupName,
		&avatar, &hide, &stat, &joinedAt, &deletedAt); err != nil {
		return nil, err
	}
	return []any{id, name, gender, masterID, generation, groupName, avatar, hide, stat, joinedAt, deletedAt}, nil
}

func main() {
	src, err := sql.Open("mysql", envOr("SRC_DSN",
		// 注意：迁移全程用字符串透传 DATETIME/DATE，绝不能开 parseTime——
		// 开了之后 driver 会把值格式化成 RFC3339（2026-09-23T14:27:52.065Z），MariaDB 拒收
		"douyinxs:WABZfpfGGlPSxlrs@tcp(mysql7.sqlpub.com:3312)/douyinxs?charset=utf8mb4"))
	must(err)
	dst, err := sql.Open("mysql", envOr("DST_DSN",
		"douyin_writer:Dw2026#rk3318@tcp(127.0.0.1:3306)/douyin?charset=utf8mb4&multiStatements=true"))
	must(err)
	must(src.Ping())
	must(dst.Ping())

	must(migratePersons(src, dst))
	must(migrateAccounts(src, dst))
	must(migrateDaily(src, dst))
	report(src, dst)
}

func migratePersons(src, dst *sql.DB) error {
	rows, err := src.Query(`SELECT id, name, gender, master_id, generation, group_name,
		avatar_url, hide_in_daily_report, status, joined_at, deleted_at FROM person`)
	if err != nil {
		return err
	}
	defer rows.Close()

	stmt, err := dst.Prepare(`INSERT INTO person
		(id, name, gender, master_id, generation, group_name, avatar_url, hide_in_daily_report, status, joined_at, deleted_at)
		VALUES (?,?,?,?,?,?,?,?,?,?,?)
		ON DUPLICATE KEY UPDATE name=VALUES(name), gender=VALUES(gender), master_id=VALUES(master_id),
		generation=VALUES(generation), group_name=VALUES(group_name), avatar_url=VALUES(avatar_url),
		hide_in_daily_report=VALUES(hide_in_daily_report), status=VALUES(status),
		joined_at=VALUES(joined_at), deleted_at=VALUES(deleted_at)`)
	if err != nil {
		return err
	}
	defer stmt.Close()

	n := 0
	for rows.Next() {
		vals, err := scanPerson(rows)
		if err != nil {
			return err
		}
		if _, err := stmt.Exec(vals...); err != nil {
			return fmt.Errorf("person %v: %w", vals[0], err)
		}
		n++
	}
	log.Printf("person: %d rows", n)
	return rows.Err()
}

func migrateAccounts(src, dst *sql.DB) error {
	rows, err := src.Query(`SELECT id, person_id, anchor_id, douyin_no, anchor_name,
		is_primary, status, created_at FROM account`)
	if err != nil {
		return err
	}
	defer rows.Close()

	stmt, err := dst.Prepare(`INSERT INTO account
		(id, person_id, anchor_id, douyin_no, anchor_name, is_primary, status, created_at)
		VALUES (?,?,?,?,?,?,?,?)
		ON DUPLICATE KEY UPDATE person_id=VALUES(person_id), douyin_no=VALUES(douyin_no),
		anchor_name=VALUES(anchor_name), is_primary=VALUES(is_primary), status=VALUES(status)`)
	if err != nil {
		return err
	}
	defer stmt.Close()

	n := 0
	for rows.Next() {
		var id, personID sql.NullInt64
		var anchorID, douyinNo, anchorName, stat sql.NullString
		var primary sql.NullBool
		var createdAt nullDate
		if err := rows.Scan(&id, &personID, &anchorID, &douyinNo, &anchorName, &primary, &stat, &createdAt); err != nil {
			return err
		}
		if _, err := stmt.Exec(id, personID, anchorID, douyinNo, anchorName, primary, stat, createdAt); err != nil {
			return fmt.Errorf("account %v: %w", id.Int64, err)
		}
		n++
	}
	log.Printf("account: %d rows", n)
	return rows.Err()
}

func migrateDaily(src, dst *sql.DB) error {
	rows, err := src.Query(`SELECT person_id, anchor_id, biz_date, wave, cumulative_wave,
		prev_snapshot_date, wave_span, wave_reliable, minutes, cumulative_minutes,
		minutes_span, minutes_reliable, is_live, tier, source_batch_id, recomputed_at
		FROM daily_metric`)
	if err != nil {
		return err
	}
	defer rows.Close()

	stmt, err := dst.Prepare(`INSERT INTO daily_metric
		(person_id, anchor_id, biz_date, wave, cumulative_wave, prev_snapshot_date, wave_span,
		 wave_reliable, minutes, cumulative_minutes, minutes_span, minutes_reliable, is_live,
		 tier, source_batch_id, recomputed_at)
		VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
		ON DUPLICATE KEY UPDATE wave=VALUES(wave), cumulative_wave=VALUES(cumulative_wave),
		prev_snapshot_date=VALUES(prev_snapshot_date), wave_span=VALUES(wave_span),
		wave_reliable=VALUES(wave_reliable), minutes=VALUES(minutes),
		cumulative_minutes=VALUES(cumulative_minutes), minutes_span=VALUES(minutes_span),
		minutes_reliable=VALUES(minutes_reliable), is_live=VALUES(is_live), tier=VALUES(tier),
		source_batch_id=VALUES(source_batch_id), recomputed_at=VALUES(recomputed_at)`)
	if err != nil {
		return err
	}
	defer stmt.Close()

	n := 0
	for rows.Next() {
		var personID sql.NullInt64
		var anchorID, tier sql.NullString
		var bizDate, prevDate, recomputedAt nullDate
		var wave, cumulative, minutes, cumulativeMinutes sql.NullInt64
		var waveSpan, minutesSpan sql.NullInt64
		var waveReliable, minutesReliable, isLive sql.NullBool
		var batchID sql.NullInt64
		if err := rows.Scan(&personID, &anchorID, &bizDate, &wave, &cumulative, &prevDate,
			&waveSpan, &waveReliable, &minutes, &cumulativeMinutes, &minutesSpan,
			&minutesReliable, &isLive, &tier, &batchID, &recomputedAt); err != nil {
			return err
		}
		if _, err := stmt.Exec(personID, anchorID, bizDate, wave, cumulative, prevDate,
			waveSpan, waveReliable, minutes, cumulativeMinutes, minutesSpan,
			minutesReliable, isLive, tier, batchID, recomputedAt); err != nil {
			return fmt.Errorf("daily %v %v: %w", personID.Int64, bizDate.String, err)
		}
		n++
		if n%5000 == 0 {
			log.Printf("daily: %d rows…", n)
		}
	}
	log.Printf("daily_metric: %d rows", n)
	return rows.Err()
}

func report(src, dst *sql.DB) {
	for _, t := range []string{"person", "account", "daily_metric"} {
		var s, d int
		_ = src.QueryRow("SELECT COUNT(*) FROM " + t).Scan(&s)
		_ = dst.QueryRow("SELECT COUNT(*) FROM " + t).Scan(&d)
		mark := "OK"
		if s != d {
			mark = "MISMATCH"
		}
		log.Printf("count %s: src=%d dst=%d [%s]", t, s, d, mark)
	}
}

func envOr(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func must(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
