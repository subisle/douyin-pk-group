#!/usr/bin/env python3
"""Build the public gift + thanks-effect resource library from exported ZIPs.

Usage:
  python3 scripts/build-gift-resource-library.py \
    "/path/to/礼物链接+SVG_仅礼物_1949.zip" \
    "apps/douyin-web/public/resources" \
    --thanks-zip "/path/to/感谢特效_链接+缩略图.zip" \
    --thanks-txt "/path/to/感谢特效_链接清单_按分类.txt"

The gift ZIP contains 1949 rows with a UC netdisk URL and an SVG thumbnail.
The thanks-effect ZIP contains WebP thumbnails plus a CSV link/filename index;
the sibling TXT is authoritative for the 8 categories, names and netdisk URLs.
Both sources are emitted into one compact version-2 JSON manifest consumed by
the public resource page. All generated thumbnail filenames are URL-safe.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import re
from collections import Counter
from pathlib import Path
from urllib.parse import urlsplit
from zipfile import ZipFile


GIFT_CSV_NAME = "礼物缩略图清单_仅礼物.csv"
THANKS_CSV_NAME = "礼物缩略图清单.csv"
THANKS_INTERNAL_TXT_NAME = "感谢特效_链接清单_按分类.txt"

DEFAULT_THANKS_ZIP = (
    Path.home()
    / "Library/Containers/com.tencent.qq/Data/Downloads/感谢特效_链接+缩略图.zip"
)
DEFAULT_THANKS_TXT = (
    Path.home()
    / "Library/Containers/com.tencent.qq/Data/Downloads/感谢特效_链接清单_按分类.txt"
)

GIFT_GROUP_META = {
    "key": "gift",
    "label": "礼物动效",
    "icon": "redeem",
    "desc": "抖音礼物特效、互动动画与活动素材",
}
GIFT_CATEGORY_DEFS = [
    {"key": "gift", "label": "常规礼物", "icon": "redeem", "desc": "普通礼物、全屏礼物与礼物升级特效"},
    {"key": "ride", "label": "座驾飞行", "icon": "flight", "desc": "飞机、跑车、游艇、热气球等座驾"},
    {"key": "scene", "label": "场景展馆", "icon": "apartment", "desc": "展馆、前景、城堡、仙宫与场景特效"},
    {"key": "addon", "label": "加成卡", "icon": "bolt", "desc": "倍数加成、buff 与增益卡"},
    {"key": "festival", "label": "节日活动", "icon": "celebration", "desc": "节日、活动、限定与嘉年华素材"},
    {"key": "interactive", "label": "互动动画", "icon": "touch_app", "desc": "入场、选中、送出、升级等互动动画"},
    {"key": "custom", "label": "文字霸屏", "icon": "branding_watermark", "desc": "文字礼物、定制、上位与霸屏特效"},
    {"key": "account", "label": "等级灯牌", "icon": "badge", "desc": "等级特效、粉丝团灯牌与身份展示"},
]
CATEGORY_LABELS = {item["key"]: item["label"] for item in GIFT_CATEGORY_DEFS}

# A dedicated key keeps this import separate from future video-effect packs.
# The page replaces its static group defaults with the groups in this JSON.
THANKS_GROUP_KEY = "thanks_fx"
THANKS_GROUP_LABEL = "感谢特效"
THANKS_GROUP_ICON = "play_circle"
THANKS_GROUP_DESC = "直播间感谢、欢迎、入场与人物视频特效"
THANKS_CATEGORY_DETAILS = {
    "女神特效": ("auto_awesome", "女神、大姐、公主等高级入场与感谢特效"),
    "男神大哥": ("military_tech", "男神、大哥、守护与霸气进场特效"),
    "双人情侣": ("groups", "情侣互动、双人氛围与 CP 特效"),
    "古风水墨": ("history_edu", "古风、水墨、刀剑与江湖意境特效"),
    "感谢欢迎": ("celebration", "通用感谢、欢迎与仪式感视频"),
    "进场入场": ("play_circle", "横屏、入场与直播间进场特效"),
    "可爱搞笑": ("emoji_events", "可爱、搞笑与轻松氛围特效"),
    "坐骑动物": ("flight", "龙、神兽、坐骑与动物主题特效"),
}

ALLOWED_NETDISK_HOSTS = {"drive.uc.cn"}
ALLOWED_THUMB_SUFFIXES = {".webp", ".svg", ".png", ".jpg", ".jpeg"}


def classify(title: str) -> str:
    text = title.lower()
    if any(k in text for k in ("加成", "buff", "倍")):
        return "addon"
    if any(k in text for k in ("飞机", "跑车", "摩托", "游艇", "热气球", "马车", "龙舟", "飞船", "火箭", "座驾")):
        return "ride"
    if any(
        k in text
        for k in (
            "展馆",
            "前景",
            "背景",
            "场景",
            "仙宫",
            "乐园",
            "城堡",
            "宫殿",
            "花园",
            "华灯",
            "星河",
            "银河",
            "宇宙",
            "海岛",
            "森林",
            "城市",
            "云霄",
            "地标",
            "秘境",
        )
    ):
        return "scene"
    if any(k in text for k in ("文字", "定制", "霸屏", "上位", "宣言", "横幅")):
        return "custom"
    if any(k in text for k in ("等级特", "级特效", "粉丝团", "灯牌", "头像", "昵称", "关注", "勋章", "称号")):
        return "account"
    if any(
        k in text
        for k in (
            "新年",
            "春节",
            "中秋",
            "圣诞",
            "万圣",
            "元旦",
            "情人节",
            "七夕",
            "生日",
            "周年",
            "限定",
            "活动",
            "双十一",
            "618",
            "跨年",
            "元宵",
            "端午",
            "国庆",
            "嘉年华",
            "萌兔",
            "520",
        )
    ):
        return "festival"
    if any(
        k in text
        for k in (
            "入场",
            "动画",
            "选中",
            "送出",
            "提示",
            "按钮",
            "呼吸",
            "界面",
            "弹窗",
            "飘屏",
            "loading",
            "ui",
            "交互",
        )
    ):
        return "interactive"
    return "gift"


def clean_title(value: str, where: str) -> str:
    title = re.sub(r"\s+", " ", value.replace("\ufeff", "")).strip()
    if not title:
        raise SystemExit(f"{where} 为空")
    if any(ord(ch) < 32 or ord(ch) == 127 for ch in title):
        raise SystemExit(f"{where} 含控制字符: {title!r}")
    return title


def parse_price(title: str) -> int | None:
    """Extract the gift price from names such as `2999钻`, `3.5w`, `699-...`."""
    explicit = re.search(r"(?<!\d)(\d+(?:\.\d+)?)\s*(钻|[wW])", title)
    if explicit:
        value = float(explicit.group(1))
        unit = explicit.group(2)
        return round(value * (1 if unit == "钻" else 10_000))

    prefix_wan = re.match(r"^(\d+(?:\.\d+)?)\s*万", title)
    if prefix_wan:
        return round(float(prefix_wan.group(1)) * 10_000)

    leading = re.match(r"^(\d+)", title)
    if not leading:
        return None
    value = int(leading.group(1))
    rest = title[leading.end() :]
    if value < 10 or 2000 <= value <= 2099:
        return None
    if rest.startswith(("阶", "级", "倍", "强", "年")):
        return None
    if value in (25, 26) and rest.startswith("-"):
        return None
    return value


def price_label(value: int | None) -> str:
    if value is None:
        return "价格待定"
    if value >= 10_000:
        return f"{value / 10_000:g}万钻"
    return f"{value}钻"


def validate_netdisk_url(value: str | None, where: str) -> str:
    """Only allow canonical HTTPS UC share links, with no userinfo/query/fragment."""
    url = (value or "").strip()
    if any(ord(ch) < 32 or ch in "\"'<>`" for ch in url):
        raise SystemExit(f"{where} 网盘链接含危险字符: {url!r}")
    try:
        parts = urlsplit(url)
        port = parts.port
    except ValueError as exc:
        raise SystemExit(f"{where} 网盘链接非法: {url!r} ({exc})") from exc
    if parts.scheme != "https" or parts.netloc.lower() not in ALLOWED_NETDISK_HOSTS:
        raise SystemExit(f"{where} 网盘链接非法: {url}")
    if parts.username or parts.password or port is not None or parts.query or parts.fragment:
        raise SystemExit(f"{where} 网盘链接含额外参数: {url}")
    if not re.fullmatch(r"/s/[A-Za-z0-9]+", parts.path):
        raise SystemExit(f"{where} 网盘链接路径非法: {url}")
    return url


def safe_zip_member(value: str, where: str) -> str:
    """Reject traversal/absolute names; only exact ZIP member names are read."""
    name = value.strip()
    parts = name.split("/")
    if (
        not name
        or name.startswith("/")
        or re.match(r"^[A-Za-z]:", name)
        or "\\" in name
        or "%" in name
        or any(part in ("", ".", "..") for part in parts)
        or any(ord(ch) < 32 for ch in name)
    ):
        raise SystemExit(f"{where} 缩略图路径不安全: {name!r}")
    return name


def validate_thumbnail_bytes(data: bytes, suffix: str, where: str) -> None:
    if suffix == ".webp":
        valid = len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WEBP"
    elif suffix == ".svg":
        valid = b"<svg" in data[:4096].lower()
    elif suffix == ".png":
        valid = data.startswith(b"\x89PNG\r\n\x1a\n")
    elif suffix in (".jpg", ".jpeg"):
        valid = data.startswith(b"\xff\xd8\xff")
    else:
        raise SystemExit(f"{where} 不支持的缩略图格式: {suffix}")
    if not valid:
        raise SystemExit(f"{where} 不是有效 {suffix} 缩略图")


def normalize_text(text: str) -> str:
    return "\n".join(
        line.rstrip()
        for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
        if line.strip()
    )


def read_zip_text(archive: ZipFile, name: str, where: str) -> str:
    try:
        raw = archive.read(name)
    except KeyError as exc:
        raise SystemExit(f"{where} 缺少 {name}") from exc
    try:
        return raw.decode("utf-8-sig")
    except UnicodeDecodeError as exc:
        raise SystemExit(f"{where} 中 {name} 不是 UTF-8 文本") from exc


def parse_thanks_txt(text: str, source_name: str) -> tuple[list[dict[str, object]], list[dict[str, object]]]:
    declared_total = None
    declared_categories = None
    header = re.search(r"共\s*(\d+)\s*个视频\s*/\s*(\d+)\s*个分类", text)
    if header:
        declared_total = int(header.group(1))
        declared_categories = int(header.group(2))

    categories: list[dict[str, object]] = []
    rows: list[dict[str, object]] = []
    current: dict[str, object] | None = None
    seen_keys: set[str] = set()
    seen_urls: set[str] = set()

    for line_no, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not stripped:
            continue
        category_match = re.fullmatch(r"【(.+?)】\s*(\d+)\s*个", stripped)
        if category_match:
            raw_label = clean_title(category_match.group(1), f"{source_name}:{line_no} 分类")
            expected_count = int(category_match.group(2))
            ordinal = len(categories) + 1
            label = re.sub(r"^\d+\s*[_\-—.]?\s*", "", raw_label).strip() or raw_label
            prefix = re.match(r"^(\d+)", raw_label)
            key = f"fx-{int(prefix.group(1)):02d}" if prefix else f"fx-{ordinal:02d}"
            if key in seen_keys:
                key = f"{key}-{ordinal}"
            seen_keys.add(key)
            current = {
                "key": key,
                "label": label,
                "raw_label": raw_label,
                "expected_count": expected_count,
                "count": 0,
            }
            categories.append(current)
            continue
        if stripped.startswith(("感谢特效 ·", "生成时间:", "共 ")):
            continue
        row_match = re.fullmatch(r"\s*(.+?)\s+(https?://\S+)\s*", line)
        if row_match:
            if current is None:
                raise SystemExit(f"{source_name}:{line_no} 链接出现在分类标题之前")
            title = clean_title(row_match.group(1), f"{source_name}:{line_no} 标题")
            url = validate_netdisk_url(row_match.group(2), f"{source_name}:{line_no}")
            if url in seen_urls:
                raise SystemExit(f"{source_name}:{line_no} 网盘链接重复: {url}")
            seen_urls.add(url)
            rows.append({"title": title, "url": url, "category": current, "line_no": line_no})
            current["count"] = int(current["count"]) + 1
            continue
        raise SystemExit(f"{source_name}:{line_no} 无法解析: {line!r}")

    if not categories or not rows:
        raise SystemExit(f"{source_name} 没有解析到分类或链接")
    if declared_total is not None and declared_total != len(rows):
        raise SystemExit(f"{source_name} 声明 {declared_total} 个视频，实际解析 {len(rows)} 个")
    if declared_categories is not None and declared_categories != len(categories):
        raise SystemExit(f"{source_name} 声明 {declared_categories} 个分类，实际解析 {len(categories)} 个")
    for category in categories:
        if category["count"] != category["expected_count"]:
            raise SystemExit(
                f"{source_name} 分类 {category['raw_label']} 声明 "
                f"{category['expected_count']} 个，实际解析 {category['count']} 个"
            )
    return categories, rows


def thanks_category_def(category: dict[str, object]) -> dict[str, str]:
    label = str(category["label"])
    icon, desc = THANKS_CATEGORY_DETAILS.get(label, ("category", f"感谢特效 · {label}"))
    count = int(category["count"])
    return {
        "key": str(category["key"]),
        "label": label,
        "icon": icon,
        "desc": f"{desc} · 共 {count} 个 · 点击打开 UC 网盘",
    }


def make_group(
    key: str, label: str, icon: str, desc: str, categories: list[dict[str, str]]
) -> dict[str, object]:
    return {
        "key": key,
        "label": label,
        "icon": icon,
        "desc": desc,
        "categories": [dict(category) for category in categories],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("zip_path", type=Path, help="1949 个礼物素材 ZIP")
    parser.add_argument("out_dir", type=Path, help="资源输出目录")
    parser.add_argument("--thanks-zip", type=Path, default=DEFAULT_THANKS_ZIP, help="感谢特效缩略图 ZIP")
    parser.add_argument("--thanks-txt", type=Path, default=DEFAULT_THANKS_TXT, help="感谢特效分类链接 TXT")
    args = parser.parse_args()

    gift_zip = args.zip_path
    thanks_zip = args.thanks_zip
    thanks_txt = args.thanks_txt
    out_dir = args.out_dir.resolve()
    for label, path in (
        ("礼物 ZIP", gift_zip),
        ("感谢特效 ZIP", thanks_zip),
        ("感谢特效 TXT", thanks_txt),
    ):
        if not path.is_file():
            raise SystemExit(f"缺少{label}: {path}")

    thanks_text = thanks_txt.read_text(encoding="utf-8-sig")
    thanks_categories, thanks_rows = parse_thanks_txt(thanks_text, thanks_txt.name)

    thumbs_dir = out_dir / "thumbs"
    thumbs_dir.mkdir(parents=True, exist_ok=True)
    items: list[dict[str, object]] = []
    written_thumbs: set[str] = set()

    # 1) Existing 1949 gift effects: preserve order, IDs, categories and SVG thumbs.
    with ZipFile(gift_zip) as archive:
        try:
            csv_raw = archive.read(GIFT_CSV_NAME)
        except KeyError as exc:
            raise SystemExit(f"礼物 ZIP 缺少 {GIFT_CSV_NAME}") from exc
        rows = list(csv.DictReader(io.StringIO(csv_raw.decode("utf-8-sig"))))
        if not rows:
            raise SystemExit("礼物 CSV 没有数据行")
        for index, row in enumerate(rows, start=1):
            where = f"礼物 CSV 第 {index} 行"
            title = clean_title(row.get("礼物昵称", ""), where)
            netdisk_url = validate_netdisk_url(row.get("网盘链接", ""), where)
            svg_name = safe_zip_member(row.get("无黑边SVG", ""), where)
            if Path(svg_name).suffix.lower() != ".svg":
                raise SystemExit(f"{where} 不是 SVG: {svg_name}")
            try:
                svg_bytes = archive.read(svg_name)
            except KeyError as exc:
                raise SystemExit(f"{where} ZIP 缺少 {svg_name}") from exc
            validate_thumbnail_bytes(svg_bytes, ".svg", where)

            category = classify(title)
            price = parse_price(title)
            thumb_name = f"{index:04d}.svg"
            (thumbs_dir / thumb_name).write_bytes(svg_bytes)
            written_thumbs.add(thumb_name)
            items.append(
                {
                    "id": f"gift-{index:04d}",
                    "title": title,
                    "group": "gift",
                    "category": category,
                    "desc": "礼物视频素材 · 点击打开 UC 网盘",
                    "netdiskUrl": netdisk_url,
                    "provider": "UC网盘",
                    "thumbnail": f"/resources/thumbs/{thumb_name}",
                    "price": price,
                    "priceLabel": price_label(price),
                    "tags": ["无黑边", CATEGORY_LABELS[category]],
                    "code": Path(svg_name).stem.split("-", 1)[0] or f"{index:04d}",
                }
            )

    # 2) Thanks effects: TXT supplies categories/order/links; ZIP supplies WebP thumbs.
    with ZipFile(thanks_zip) as archive:
        names = set(archive.namelist())
        if THANKS_INTERNAL_TXT_NAME in names:
            internal_text = read_zip_text(archive, THANKS_INTERNAL_TXT_NAME, "感谢特效 ZIP")
            if normalize_text(internal_text) != normalize_text(thanks_text):
                raise SystemExit("ZIP 内分类清单与外部 TXT 不一致")
        try:
            csv_raw = archive.read(THANKS_CSV_NAME)
        except KeyError as exc:
            raise SystemExit(f"感谢特效 ZIP 缺少 {THANKS_CSV_NAME}") from exc
        csv_rows = list(csv.DictReader(io.StringIO(csv_raw.decode("utf-8-sig"))))
        if not csv_rows:
            raise SystemExit("感谢特效 CSV 没有数据行")

        by_url: dict[str, dict[str, str]] = {}
        for index, row in enumerate(csv_rows, start=1):
            where = f"感谢特效 CSV 第 {index} 行"
            url = validate_netdisk_url(row.get("网盘链接", ""), where)
            if url in by_url:
                raise SystemExit(f"{where} 网盘链接重复: {url}")
            by_url[url] = row

        expected_urls = {str(row["url"]) for row in thanks_rows}
        if set(by_url) != expected_urls:
            missing = sorted(expected_urls - set(by_url))[:3]
            extra = sorted(set(by_url) - expected_urls)[:3]
            raise SystemExit(f"感谢特效 CSV 与 TXT 链接不一致: 缺少={missing} 多余={extra}")

        for index, entry in enumerate(thanks_rows, start=1):
            where = f"感谢特效 TXT 第 {entry['line_no']} 行"
            row = by_url[str(entry["url"])]
            category = entry["category"]
            if not isinstance(category, dict):
                raise SystemExit(f"{where} 分类数据异常")
            if (row.get("分类") or "").strip() != category["raw_label"]:
                raise SystemExit(f"{where} 分类与 CSV 不一致: {row.get('分类')!r}")
            csv_title = clean_title(row.get("礼物昵称", ""), f"感谢特效 CSV 第 {index} 行")
            if csv_title != entry["title"]:
                raise SystemExit(f"{where} 名称与 CSV 不一致: {csv_title!r} != {entry['title']!r}")

            thumb_member = safe_zip_member(row.get("缩略图文件", ""), where)
            suffix = Path(thumb_member).suffix.lower()
            if suffix not in ALLOWED_THUMB_SUFFIXES:
                raise SystemExit(f"{where} 不支持的缩略图格式: {thumb_member}")
            if thumb_member not in names:
                raise SystemExit(f"{where} ZIP 缺少 {thumb_member}")
            thumb_bytes = archive.read(thumb_member)
            validate_thumbnail_bytes(thumb_bytes, suffix, where)

            code_match = re.match(r"^(\d{4})-", Path(thumb_member).name)
            code = code_match.group(1) if code_match else f"{index:04d}"
            thumb_name = f"fx-{index:04d}{suffix}"
            (thumbs_dir / thumb_name).write_bytes(thumb_bytes)
            written_thumbs.add(thumb_name)
            items.append(
                {
                    "id": f"fx-{index:04d}",
                    "title": str(entry["title"]),
                    "group": THANKS_GROUP_KEY,
                    "category": str(category["key"]),
                    "desc": f"感谢特效 · {category['label']} · 点击打开 UC 网盘",
                    "netdiskUrl": str(entry["url"]),
                    "provider": "UC网盘",
                    "thumbnail": f"/resources/thumbs/{thumb_name}",
                    "price": None,
                    "priceLabel": "价格待定",
                    "tags": [THANKS_GROUP_LABEL, str(category["label"]), "UC网盘"],
                    "code": code,
                }
            )

    # Idempotent reruns: remove only stale generated thanks-effect thumbnails.
    for path in thumbs_dir.glob("fx-*"):
        if path.is_file() and path.name not in written_thumbs:
            path.unlink()

    thanks_category_defs = [thanks_category_def(category) for category in thanks_categories]
    groups = [
        make_group(
            GIFT_GROUP_META["key"],
            GIFT_GROUP_META["label"],
            GIFT_GROUP_META["icon"],
            GIFT_GROUP_META["desc"],
            [dict(category) for category in GIFT_CATEGORY_DEFS],
        ),
        make_group(
            THANKS_GROUP_KEY,
            THANKS_GROUP_LABEL,
            THANKS_GROUP_ICON,
            f"{THANKS_GROUP_DESC} · 共 {len(thanks_rows)} 个 · 点击打开 UC 网盘",
            thanks_category_defs,
        ),
    ]
    categories = [dict(category) for category in GIFT_CATEGORY_DEFS] + [
        dict(category) for category in thanks_category_defs
    ]
    payload = {
        "version": 2,
        "total": len(items),
        "source": f"{gift_zip.name} + {thanks_zip.name} + {thanks_txt.name}",
        "sources": {
            "giftZip": gift_zip.name,
            "thanksZip": thanks_zip.name,
            "thanksTxt": thanks_txt.name,
        },
        "groups": groups,
        "categories": categories,
        "items": items,
    }
    manifest = out_dir / "videos.json"
    manifest.write_text(
        json.dumps(payload, ensure_ascii=False, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )

    group_counts = Counter(str(item["group"]) for item in items)
    category_counts = Counter(str(item["category"]) for item in items)
    print(f"生成 {len(items)} 条资源 -> {manifest}")
    print(f"缩略图 {len(written_thumbs)} 个 -> {thumbs_dir}")
    for group in groups:
        print(
            f"分组 {group['key']} ({group['label']}): "
            f"{group_counts[str(group['key'])]} 个，{len(group['categories'])} 个分类"
        )
        for category in group["categories"]:
            print(
                f"  {category['key']} ({category['label']}): "
                f"{category_counts[str(category['key'])]}"
            )


if __name__ == "__main__":
    main()
