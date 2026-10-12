//! public_teams::binds —— 自 public_teams.rs 按业务域拆分（对外语义不变）。

use super::*;

/* --------------------- 抖音号绑定申请（展示端提交 → 管理端审计） --------------------- */

/// GET /api/public/teams/{team}/my/claim（token 走 X-My-Token 头）：我的绑定状态
/// （最近一笔申请 + 已绑定的抖音号列表，让用户知道现在音浪记在哪个号上）
pub async fn my_claim_get(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    Query(_q): Query<MyQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let (person_id, my_team, _) = my_token_required(&headers)?;
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    let bound = pgrs::query_with_binds(
        &st.pools,
        &sid(&st),
        "SELECT douyin_no, is_primary FROM account \
         WHERE person_id = ? AND status = 'active' ORDER BY is_primary DESC, id",
        |q| q.bind(person_id as i64),
    )
    .await?
    .iter()
    .filter_map(|r| {
        let no = r
            .try_get::<String, _>("douyin_no")
            .ok()
            .filter(|s| !s.is_empty())?;
        let primary = r.try_get::<i16, _>("is_primary").unwrap_or(0) == 1;
        Some(json!({ "douyinNo": no, "primary": primary }))
    })
    .collect::<Vec<_>>();
    Ok(Json(json!({
        "claim": crate::store::anchor_claim_mine(&st, person_id as i64).await,
        "bound": bound,
    })))
}

/// POST /api/public/teams/{team}/my/claim {token, douyinNo, screenshot}
/// screenshot = dataURL（data:image/jpeg;base64,…），服务端解码后入库待审计（anchor_claims）。
pub async fn my_claim_post(
    State(st): State<ArcState>,
    Path(team): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let Some((person_id, my_team, _)) = my_token_take(token.trim()) else {
        return Err(ApiError::auth("登录已过期，请重新登录"));
    };
    if my_team != team && team != "all" {
        return Err(ApiError::auth("该账号不属于本团"));
    }
    if person_id == 0 {
        return Err(anyhow::anyhow!("管理员账号缺少对应的主播档案，不能绑定抖音号").into());
    }
    let douyin_no = body
        .get("douyinNo")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let data_url = body
        .get("screenshot")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let (mime, b64) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .ok_or_else(|| anyhow::anyhow!("截图数据格式不对，请重新选择图片"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| anyhow::anyhow!("截图数据格式非法（解码失败），请重新选择图片"))?;

    // 提交前先查重：抖音号已被占用就直接拒，省得用户白等审计
    let no = crate::store::validate_douyin_no(douyin_no).map_err(|e| anyhow::anyhow!(e))?;
    let dup = pgrs::query_with_binds(
        &st.pools,
        &sid(&st),
        "SELECT person_id FROM account WHERE douyin_no = ? LIMIT 1",
        |q| q.bind(no.clone()),
    )
    .await?;
    if let Some(r) = dup.first() {
        let owner = r
            .try_get::<i64, _>("person_id")
            .map(|v| v as u64)
            .unwrap_or(0);
        if owner == person_id {
            return Err(anyhow::anyhow!("这个抖音号已经绑定在你名下了").into());
        }
        return Err(anyhow::anyhow!("该抖音号已绑定其他主播，如有疑问请联系管理员").into());
    }

    // 报错文案统一走 anyhow::anyhow!(中文) → 400（routes.rs 关键词）
    let name = person_name_of(&st, person_id).await?;
    crate::store::anchor_claim_insert(&st, person_id as i64, &name, &team, &no, &bytes, mime)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    crate::events::emit(
        crate::events::SCOPE_IMPORT,
        format!("绑定申请：{} 提交抖音号 {}（待审计）", name, no),
    );
    Ok(Json(json!({ "ok": true })))
}

/// person 表里找名字（claim 提交与 whoami 共用）
pub(super) async fn person_name_of(st: &AppState, person_id: u64) -> anyhow::Result<String> {
    let sql = "SELECT name FROM person WHERE id = ? AND deleted_at IS NULL";
    let rows =
        pgrs::query_with_binds(&st.pools, &sid(st), sql, |q| q.bind(person_id as i64)).await?;
    Ok(rows
        .first()
        .and_then(|r| r.try_get::<String, _>("name").ok())
        .unwrap_or_default())
}

/* --------------------- 抖音号绑定审计（管理端） --------------------- */

/// GET /api/admin/anchor-claims?status=pending：审计列表（不含截图本体）
pub async fn admin_anchor_claims(
    State(st): State<ArcState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let status = q.get("status").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "items": crate::store::anchor_claim_list(&st, &status, 200).await }),
    ))
}

/// GET /api/admin/anchor-claims/{id}/screenshot：截图本体
/// （<img> 无法带 Authorization 头，走 ?access_token=，与 auth.rs 的放宽规则一致）
pub async fn admin_anchor_claim_screenshot(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    match crate::store::anchor_claim_image(&st, id).await {
        Some((bytes, mime)) => ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "没有这张截图").into_response(),
    }
}

/// POST /api/admin/anchor-claims/{id}/decide {approve, reason?}
/// 通过 → 写 MySQL account（抖音号精确命中是导入匹配第一优先级，此后音浪自动落到这个人）；
/// 驳回 → 记原因，用户可在展示端重新提交。
pub async fn admin_anchor_claim_decide(
    State(st): State<ArcState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let approve = body
        .get("approve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !approve && reason.is_empty() {
        return Err(anyhow::anyhow!("驳回要填写原因，请输入驳回原因").into());
    }

    // 1. 先占住裁决权（pending → approved/rejected 原子翻转，防双击重复绑定）
    let Some((person_id, no, person_name)) =
        crate::store::anchor_claim_decide(&st, id, approve, &reason, "admin").await
    else {
        return Err(anyhow::anyhow!("这笔申请不存在或已经处理过了").into());
    };

    if !approve {
        crate::events::emit(
            crate::events::SCOPE_IMPORT,
            format!("绑定驳回：{} 的抖音号 {no}（{reason}）", person_name),
        );
        return Ok(Json(json!({ "ok": true, "status": "rejected" })));
    }

    // 2. 通过：写 account。抖音号占用检查 + 主号判定（此人还没有账号 → 主号）
    let writer = st
        .writer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("写库不可用：douyin_writer 未配置"))?;
    let dup = sqlx::query(&crate::pq::rewrite("SELECT person_id FROM account WHERE douyin_no = ? LIMIT 1"))
        .bind(&no)
        .fetch_optional(writer)
        .await?;
    if let Some(r) = dup {
        let owner = r
            .try_get::<i64, _>("person_id")
            .map(|v| v as u64)
            .unwrap_or(0);
        // 回滚裁决，让管理员能改判或处理冲突
        crate::store::anchor_claim_revert(&st, id).await;
        return Err(match owner == person_id as u64 {
            true => anyhow::anyhow!("该抖音号已存在绑定记录（就在该主播名下）"),
            false => anyhow::anyhow!("该抖音号已存在绑定记录（绑在其他主播名下）"),
        }
        .into());
    }
    let cnt = sqlx::query(&crate::pq::rewrite("SELECT COUNT(*) AS n FROM account WHERE person_id = ?"))
        .bind(person_id)
        .fetch_one(writer)
        .await?
        .try_get::<i64, _>("n")
        .unwrap_or(0);
    let ins = sqlx::query(
        "INSERT INTO account (person_id, anchor_id, douyin_no, anchor_name, is_primary) \
         VALUES (?,?,?,?,?)",
    )
    .bind(person_id)
    .bind(&no)
    .bind(&no)
    .bind(&person_name)
    .bind(if cnt == 0 { 1i16 } else { 0i16 })
    .execute(writer)
    .await;
    if let Err(e) = ins {
        // account 写失败必须回滚裁决，否则申请卡在「已通过」却没有账号
        crate::store::anchor_claim_revert(&st, id).await;
        let msg = e.to_string();
        return Err(if msg.contains("duplicate key") || msg.contains("Duplicate entry") {
            anyhow::anyhow!("抖音号或采集 ID 已存在（唯一键冲突），请检查是否重复绑定").into()
        } else {
            anyhow::anyhow!("写入账号失败：{msg}").into()
        });
    }

    // 审核通过 = 这个号正式绑到该主播名下：暂存区里这个号的导入行立刻补账，
    // 不用等下一次导入（用户口径：建档/绑号后才显示，绑定的这一刻就算「添加上主播」）
    let absorbed = match crate::admin::absorb_pending(&st, person_id as u64).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(person_id, error = %e, "绑定通过后暂存补账失败（不影响绑定）");
            0
        }
    };

    crate::events::emit(
        crate::events::SCOPE_ROSTER,
        format!(
            "绑定通过：{} ← 抖音号 {no}，音浪将从下次导入起落到 TA 名下{}",
            person_name,
            if absorbed > 0 {
                format!("（已补账 {absorbed} 行暂存数据）")
            } else {
                String::new()
            }
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "status": "approved",
        "personId": person_id,
        "douyinNo": no,
        "primary": cnt == 0,
        "absorbed": absorbed,
    })))
}

pub(super) fn id_of_pub(r: &sqlx::postgres::PgRow) -> u64 {
    r.try_get::<i64, _>("person_id")
        .map(|v| v.max(0) as u64)
        .unwrap_or(0)
}
