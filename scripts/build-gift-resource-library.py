#!/usr/bin/env python3
"""Build the public gift-resource library from the exported ZIP.

Usage:
  python3 scripts/build-gift-resource-library.py \
    "/path/to/礼物链接+SVG_仅礼物_1949.zip" \
    "apps/douyin-web/public/resources"

The ZIP contains one row per gift with a UC netdisk URL and an SVG thumbnail.
We copy the SVG into a numeric, URL-safe filename and emit a compact JSON file
that the public resource page loads lazily.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import re
from pathlib import Path
from zipfile import ZipFile


CATEGORY_LABELS = {
    "gift": "礼物特效",
    "buff": "加成卡",
    "ride": "座驾展馆",
    "event": "节日活动",
    "ui": "动画交互",
}


def classify(title: str) -> str:
    text = title.lower()
    if any(k in text for k in ("加成", "buff", "倍", "经验", "等级", "亲密")):
        return "buff"
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
            "特效",
        )
    ):
        return "ui"
    if any(
        k in text
        for k in (
            "飞机",
            "跑车",
            "展馆",
            "座驾",
            "马车",
            "龙舟",
            "飞船",
            "火箭",
            "摩托",
            "游艇",
            "城堡",
            "宫殿",
            "华灯",
            "花园",
            "银河",
            "宇宙",
            "龙",
            "凤凰",
            "云霄",
            "云端",
            "乐园",
            "仙宫",
        )
    ):
        return "ride"
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
        )
    ):
        return "event"
    return "gift"


def clean_title(value: str) -> str:
    return re.sub(r"\s+", " ", value.replace("\ufeff", "")).strip()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("zip_path", type=Path)
    parser.add_argument("out_dir", type=Path)
    args = parser.parse_args()

    out_dir = args.out_dir.resolve()
    thumbs_dir = out_dir / "thumbs"
    thumbs_dir.mkdir(parents=True, exist_ok=True)

    with ZipFile(args.zip_path) as archive:
        csv_name = "礼物缩略图清单_仅礼物.csv"
        rows = list(
            csv.DictReader(
                io.StringIO(archive.read(csv_name).decode("utf-8-sig"))
            )
        )
        if not rows:
            raise SystemExit("CSV 没有数据行")

        items: list[dict[str, object]] = []
        for index, row in enumerate(rows, start=1):
            title = clean_title(row.get("礼物昵称", ""))
            netdisk_url = (row.get("网盘链接") or "").strip()
            svg_name = (row.get("无黑边SVG") or "").strip()
            if not title or not netdisk_url or not svg_name:
                raise SystemExit(f"第 {index} 行字段不完整: {row!r}")
            if not netdisk_url.startswith(("https://", "http://")):
                raise SystemExit(f"第 {index} 行网盘链接非法: {netdisk_url}")

            svg_bytes = archive.read(svg_name)
            if b"<svg" not in svg_bytes[:4096].lower():
                raise SystemExit(f"第 {index} 行不是有效 SVG: {svg_name}")

            category = classify(title)
            thumb_name = f"{index:04d}.svg"
            (thumbs_dir / thumb_name).write_bytes(svg_bytes)
            items.append(
                {
                    "id": f"gift-{index:04d}",
                    "code": Path(svg_name).stem.split("-", 1)[0] or f"{index:04d}",
                    "title": title,
                    "category": category,
                    "desc": "礼物视频素材 · 点击打开 UC 网盘",
                    "netdiskUrl": netdisk_url,
                    "provider": "UC网盘",
                    "thumbnail": f"/resources/thumbs/{thumb_name}",
                    "tags": ["无黑边", CATEGORY_LABELS[category]],
                }
            )

    payload = {
        "version": 1,
        "total": len(items),
        "source": args.zip_path.name,
        "items": items,
    }
    manifest = out_dir / "videos.json"
    manifest.write_text(
        json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
        encoding="utf-8",
    )

    counts: dict[str, int] = {}
    for item in items:
        category = str(item["category"])
        counts[category] = counts.get(category, 0) + 1
    print(f"生成 {len(items)} 条资源 -> {manifest}")
    print(f"缩略图 {len(items)} 个 -> {thumbs_dir}")
    print("分类:", ", ".join(f"{k}={v}" for k, v in counts.items()))


if __name__ == "__main__":
    main()
