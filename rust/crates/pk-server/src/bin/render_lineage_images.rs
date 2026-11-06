//! Render the exact wxbot "师门日报" PNGs to local files.
//!
//! This intentionally calls the same data builder and renderer used by the
//! delayed wxbot push path:
//!   lineage_report::build
//!   push::lineage_report_pngs_for
//!
//! Default behavior requested by the operator on 2026-10-30:
//! - every master with at least one eligible direct disciple gets one image set;
//! - 薇姐 and 鹏先生 are excluded from this batch;
//! - the exclusion list can be overridden with
//!   LINEAGE_IMAGE_EXCLUDED_MASTERS="name1,name2".
//!
//! Usage:
//!   cargo run -p pk-server --bin render-lineage-images -- \
//!     [--date 2026-10-21 | --month 2026-10] [--out DIR]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pk_server::state::AppState;
use pk_server::{auth, config, lineage_report, pgrs, push, repos, writer};
use sqlx::Row;

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

fn excluded_master_names() -> HashSet<String> {
    let raw = std::env::var("LINEAGE_IMAGE_EXCLUDED_MASTERS")
        .unwrap_or_else(|_| "薇姐,鹏先生".to_string());
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

async fn latest_data_date(st: &AppState) -> anyhow::Result<String> {
    let rows = pgrs::query(
        &st.pools,
        "LOCAL",
        "SELECT TO_CHAR(MAX(biz_date), 'YYYY-MM-DD') AS d FROM daily_metric",
    )
    .await?;
    let date = rows
        .first()
        .and_then(|r| r.try_get::<Option<String>, _>("d").ok().flatten())
        .unwrap_or_default();
    anyhow::ensure!(!date.is_empty(), "本地 daily_metric 没有可用的业务日期");
    Ok(date)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env_path = std::env::var("ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    config::load_dotenv(&env_path);

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("用法: render-lineage-images [--date YYYY-MM-DD | --month YYYY-MM] [--out DIR] [--qualified-today-minutes N] [--severe-today-minutes N]");
        println!("默认: 本地最新业务日；--month 生成整月音浪 + 时长月报");
        println!("--qualified-today-minutes 360 --severe-today-minutes 240: 按天标注 6 小时合格 / 4 小时以下严重不足");
        println!("环境变量: LINEAGE_IMAGE_EXCLUDED_MASTERS=薇姐,鹏先生");
        return Ok(());
    }

    let sources = config::load_sources()?;
    let pools = pgrs::build_pools(&sources).await?;
    let writer_pool = match config::load_writer() {
        Ok(ep) => match writer::connect(&ep).await {
            Ok(pool) => Some(pool),
            Err(e) => {
                eprintln!("警告：写库连接失败，推送图字段将使用默认显隐：{e}");
                None
            }
        },
        Err(e) => {
            eprintln!("警告：写库未配置，推送图字段将使用默认显隐：{e}");
            None
        }
    };
    let st = Arc::new(AppState {
        auth: Arc::new(auth::Auth::from_env()),
        sources,
        pools,
        writer: writer_pool,
        redis: None,
        active: Mutex::new("LOCAL".to_string()),
        cache: Mutex::new(Default::default()),
        wave_ttl: repos::ttl_from_env(),
        bots: std::sync::OnceLock::new(),
    });

    let month_arg = arg_value(&args, "--month");
    let date_arg = arg_value(&args, "--date");
    let qualified_today_minutes = arg_value(&args, "--qualified-today-minutes")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("--qualified-today-minutes 必须是整数分钟"))?
        .unwrap_or(0)
        .max(0);
    let severe_today_minutes = arg_value(&args, "--severe-today-minutes")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("--severe-today-minutes 必须是整数分钟"))?
        .unwrap_or(0)
        .max(0);
    anyhow::ensure!(
        !(month_arg.is_some() && date_arg.is_some()),
        "--month 与 --date 只能二选一"
    );
    let report_kind;
    let business_label;
    if let Some(month) = month_arg {
        anyhow::ensure!(
            month.len() == 7
                && chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_ok(),
            "月份格式必须是 YYYY-MM：{month}"
        );
        business_label = month.clone();
        report_kind = "师门月报";
    } else {
        let date = date_arg.unwrap_or(latest_data_date(&st).await?);
        anyhow::ensure!(
            chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok(),
            "日期格式必须是 YYYY-MM-DD：{date}"
        );
        business_label = date;
        report_kind = "师门日报";
    }
    let period_slug = if report_kind == "师门月报" {
        format!("month-{}", business_label)
    } else {
        business_label[..7].to_string()
    };
    let out_dir = arg_value(&args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!("../design-previews/wxbot-lineage-{period_slug}"))
        });
    std::fs::create_dir_all(&out_dir)?;

    let excluded = excluded_master_names();
    let rows = pgrs::query(
        &st.pools,
        "LOCAL",
        "SELECT m.id, m.name, COUNT(c.id)::BIGINT AS disciples \
         FROM person m JOIN person c ON c.master_id = m.id \
         WHERE m.deleted_at IS NULL AND m.status <> 'left' \
           AND c.deleted_at IS NULL AND c.status <> 'left' AND c.hide_in_daily_report = 0 \
         GROUP BY m.id, m.name HAVING COUNT(c.id) > 0 \
         ORDER BY disciples DESC, m.name ASC",
    )
    .await?;

    let mut manifest = String::from("师傅ID,师傅,徒弟数,业务期,类型,页数,文件\n");
    let mut rendered_masters = 0usize;
    let mut rendered_pages = 0usize;
    for row in rows {
        let master_id = row.try_get::<i64, _>("id").unwrap_or(0).max(0) as u64;
        let master_name = row.try_get::<String, _>("name").unwrap_or_default();
        let disciples = row.try_get::<i64, _>("disciples").unwrap_or(0);
        if excluded.contains(&master_name) {
            println!("跳过\t{master_name}\t{disciples}名徒弟\t排除名单");
            continue;
        }

        let built = if report_kind == "师门月报" {
            lineage_report::build_month(&st, &business_label, master_id).await
        } else {
            lineage_report::build(&st, &business_label, master_id).await
        };
        let mut report = match built {
            Ok(rep) => rep,
            Err(e) => {
                eprintln!("跳过\t{master_name}\t构建失败：{e}");
                continue;
            }
        };
        if report_kind == "师门日报" && (qualified_today_minutes > 0 || severe_today_minutes > 0)
        {
            report.set_daily_minutes_standard(qualified_today_minutes, severe_today_minutes);
        }
        if report.disciple_count() == 0 {
            println!("跳过\t{master_name}\t无可进入报表的直系徒弟");
            continue;
        }
        let reported_disciples = report.disciple_count() as i64;
        // Same entry point as the delayed wxbot push. It applies configured
        // column visibility and the per-recipient watermark.
        let pages = push::lineage_report_pngs_for(&st, &report, &master_name).await;
        if pages.is_empty() {
            eprintln!("跳过\t{master_name}\tPNG 渲染失败");
            continue;
        }

        let stem = sanitize_filename(&master_name);
        let page_count = pages.len();
        for (idx, png) in pages.into_iter().enumerate() {
            let suffix = if page_count > 1 {
                format!("-第{}页", idx + 1)
            } else {
                String::new()
            };
            let file = out_dir.join(format!("{stem}-{report_kind}-{business_label}{suffix}.png"));
            std::fs::write(&file, png)?;
            manifest.push_str(&format!(
                "{master_id},{master_name},{reported_disciples},{business_label},{report_kind},{page_count},{}\n",
                file.display()
            ));
            println!("生成\t{master_name}\t{}", file.display());
            rendered_pages += 1;
        }
        rendered_masters += 1;
    }

    let manifest_path = out_dir.join(format!("{report_kind}清单-{business_label}.csv"));
    std::fs::write(&manifest_path, format!("\u{feff}{manifest}"))?;
    println!(
        "完成：{} 位师傅，{} 张图；输出目录 {}；清单 {}",
        rendered_masters,
        rendered_pages,
        out_dir.display(),
        manifest_path.display()
    );
    Ok(())
}
