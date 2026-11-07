//! 资源分享目录 API。
//!
//! 前端不再下载 755KB 的 `videos.json` 全量清单，而是按页请求这里：
//! `GET /api/public/resources?group=&category=&q=&sort=&offset=0&limit=30`。
//! 清单只在进程内解析一次；2176 条数据的内存过滤足够快，单次响应只传首屏素材。

use super::*;
use axum::extract::Query;
use axum::http::{header, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::OnceCell;

const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 60;

struct Catalog {
    total: usize,
    groups: Vec<Value>,
    items: Vec<Value>,
}

static CATALOG: OnceCell<Result<Arc<Catalog>, String>> = OnceCell::const_new();

#[derive(Debug, Deserialize)]
pub struct ResourcesQuery {
    pub group: Option<String>,
    pub category: Option<String>,
    pub q: Option<String>,
    pub sort: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

fn parse_catalog(doc: Value) -> Arc<Catalog> {
    let items = doc
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut groups = doc
        .get("groups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // 分组/分类计数在加载时算一次，后续每个分页请求不再重复扫全表。
    let mut category_counts: HashMap<String, usize> = HashMap::new();
    let mut group_counts: HashMap<String, usize> = HashMap::new();
    for item in &items {
        if let Some(cat) = item.get("category").and_then(Value::as_str) {
            *category_counts.entry(cat.to_string()).or_default() += 1;
        }
        if let Some(group) = item.get("group").and_then(Value::as_str) {
            *group_counts.entry(group.to_string()).or_default() += 1;
        }
    }
    for group in &mut groups {
        let group_key = group
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Some(obj) = group.as_object_mut() else {
            continue;
        };
        obj.insert(
            "count".into(),
            json!(group_counts
                .get(group_key.as_str())
                .copied()
                .unwrap_or_default()),
        );
        if let Some(categories) = obj.get_mut("categories").and_then(Value::as_array_mut) {
            for category in categories {
                let key = category
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(cat_obj) = category.as_object_mut() {
                    cat_obj.insert(
                        "count".into(),
                        json!(category_counts
                            .get(key.as_str())
                            .copied()
                            .unwrap_or_default()),
                    );
                }
            }
        }
    }

    Arc::new(Catalog {
        total: items.len(),
        groups,
        items,
    })
}

async fn load_catalog() -> Result<Arc<Catalog>, String> {
    let dist = std::env::var("WEB_DIST").unwrap_or_else(|_| "../../apps/douyin-web/dist".into());
    let path = Path::new(&dist).join("resources/videos.json");
    let raw = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("资源目录读取失败 {}: {e}", path.display()))?;
    let doc: Value = serde_json::from_slice(&raw).map_err(|e| format!("资源目录解析失败: {e}"))?;
    Ok(parse_catalog(doc))
}

async fn catalog() -> Result<Arc<Catalog>, String> {
    CATALOG
        .get_or_init(|| async { load_catalog().await })
        .await
        .clone()
}

fn category_set(groups: &[Value], group_key: &str) -> HashSet<String> {
    groups
        .iter()
        .find(|g| g.get("key").and_then(Value::as_str) == Some(group_key))
        .and_then(|g| g.get("categories"))
        .and_then(Value::as_array)
        .map(|categories| {
            categories
                .iter()
                .filter_map(|c| c.get("key").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn price(item: &Value) -> Option<f64> {
    item.get("price").and_then(Value::as_f64)
}

fn page(catalog: &Catalog, query: ResourcesQuery) -> Value {
    let group = query.group.as_deref().unwrap_or("").trim();
    let category = query.category.as_deref().unwrap_or("").trim();
    let keyword = query.q.as_deref().unwrap_or("").trim().to_lowercase();
    let sort = query.sort.as_deref().unwrap_or("default");
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = query.offset.unwrap_or(0).min(catalog.total);

    let allowed_categories = if group.is_empty() || group == "all" {
        HashSet::new()
    } else {
        category_set(&catalog.groups, group)
    };

    let mut rows: Vec<&Value> = catalog
        .items
        .iter()
        .filter(|item| {
            let item_group = item.get("group").and_then(Value::as_str).unwrap_or("");
            let item_category = item.get("category").and_then(Value::as_str).unwrap_or("");
            if !group.is_empty()
                && group != "all"
                && item_group != group
                && !allowed_categories.contains(item_category)
            {
                return false;
            }
            if !category.is_empty() && category != "all" && item_category != category {
                return false;
            }
            if !keyword.is_empty() {
                let title = item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                if !title.contains(&keyword) {
                    return false;
                }
            }
            true
        })
        .collect();

    if sort == "price-asc" || sort == "price-desc" {
        let direction = if sort == "price-asc" { 1.0 } else { -1.0 };
        rows.sort_by(|a, b| {
            match (price(a), price(b)) {
                (Some(ap), Some(bp)) => (ap - bp) * direction,
                (Some(_), None) => -1.0,
                (None, Some(_)) => 1.0,
                (None, None) => 0.0,
            }
            .partial_cmp(&0.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("id").and_then(Value::as_str).unwrap_or(""))
            })
        });
    }

    let total = rows.len();
    let items: Vec<Value> = rows.into_iter().skip(offset).take(limit).cloned().collect();
    json!({
        "total": total,
        "catalogTotal": catalog.total,
        "offset": offset,
        "limit": limit,
        "items": items,
        "groups": catalog.groups,
    })
}

/// GET /api/public/resources —— 资源分享页分页查询。
pub async fn resources(Query(query): Query<ResourcesQuery>) -> Response {
    match catalog().await {
        Ok(catalog) => (
            [(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300, stale-while-revalidate=86400"),
            )],
            Json(page(&catalog, query)),
        )
            .into_response(),
        Err(error) => ApiError::with_status(axum::http::StatusCode::SERVICE_UNAVAILABLE, error)
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Arc<Catalog> {
        parse_catalog(json!({
            "groups": [
                {"key":"gift","label":"礼物","categories":[{"key":"g","label":"常规"}]},
                {"key":"thanks","label":"感谢","categories":[{"key":"t","label":"女神"}]}
            ],
            "items": [
                {"id":"1","title":"甲礼物","group":"gift","category":"g","price":20},
                {"id":"2","title":"乙感谢","group":"thanks","category":"t","price":null},
                {"id":"3","title":"甲感谢","group":"thanks","category":"t","price":10}
            ]
        }))
    }

    #[test]
    fn paginates_filters_and_counts() {
        let c = sample();
        let body = page(
            &c,
            ResourcesQuery {
                group: Some("thanks".into()),
                category: None,
                q: None,
                sort: Some("price-asc".into()),
                offset: Some(0),
                limit: Some(1),
            },
        );
        assert_eq!(body["total"], 2);
        assert_eq!(body["catalogTotal"], 3);
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["id"], "3");
        assert_eq!(body["groups"][0]["count"], 1);
        assert_eq!(body["groups"][1]["count"], 2);
        assert_eq!(body["groups"][1]["categories"][0]["count"], 2);
    }

    #[test]
    fn searches_by_title_and_caps_limit() {
        let c = sample();
        let body = page(
            &c,
            ResourcesQuery {
                group: None,
                category: None,
                q: Some("感谢".into()),
                sort: None,
                offset: Some(0),
                limit: Some(999),
            },
        );
        assert_eq!(body["total"], 2);
        assert_eq!(body["limit"], MAX_LIMIT);
        assert_eq!(body["items"].as_array().unwrap().len(), 2);
    }
}
