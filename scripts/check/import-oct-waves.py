#!/usr/bin/env python3
"""一次性导入 10 月音浪数据（9 天主播榜）。

用法：
  python3 -I scripts/check/import-oct-waves.py --token <管理员token> [--dry-run]

行为：
  - 逐日走 /api/admin/import/upload（multipart，xlsx/CSV 转文本走后台同一条路）
  - 再走 /api/admin/import/commit（mode=merge 幂等，同一天同文件重跑不重复写）
  - 每天一个 import_batch 审计记录
  - 未建档的行进 import_pending 暂存，不落 daily_metric
"""
import json
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "音浪数据"
BASE = "http://127.0.0.1:8787"

# 9 天主播榜（创想 10.X 是同公会的切片，不导；10.8 文件名少个点号，照传）
FILES = [
    ("主播榜10.1号.csv", "2026-10-01"),
    ("主播榜10.2.csv", "2026-10-02"),
    ("主播榜10.3.csv", "2026-10-03"),
    ("主播榜10.4.csv", "2026-10-04"),
    ("主播榜10.5.csv", "2026-10-05"),
    ("主播榜10.6.csv", "2026-10-06"),
    ("主播榜10.7.csv", "2026-10-07"),
    ("主播榜10.8csv", "2026-10-08"),
    ("主播榜10.9.csv", "2026-10-09"),
]


def api_post_json(path: str, payload: dict, token: str) -> dict:
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
    )
    try:
        return json.load(urllib.request.urlopen(req))
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise RuntimeError(f"{e.code} {body[:200]}")


def upload(filename: str, token: str) -> dict:
    # multipart：用标准库手工拼
    path = DATA / filename
    raw = path.read_bytes()
    boundary = "----pkimportboundary"
    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + raw + (
        f"\r\n--{boundary}\r\n"
        f'Content-Disposition: form-data; name="filename"\r\n\r\n{filename}\r\n'
        f"--{boundary}--\r\n"
    ).encode()
    req = urllib.request.Request(
        BASE + "/api/admin/import/upload",
        data=body,
        headers={
            "Content-Type": f"multipart/form-data; boundary={boundary}",
            "Authorization": f"Bearer {token}",
        },
    )
    try:
        return json.load(urllib.request.urlopen(req))
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise RuntimeError(f"{e.code} {body[:200]}")


def main() -> int:
    args = [a for a in sys.argv[1:]]
    dry = "--dry-run" in args
    token = None
    for a in args:
        if a.startswith("--token="):
            token = a.split("=", 1)[1]
            break
    if not token:
        print("用法: python3 -I scripts/check/import-oct-waves.py --token <管理员token> [--dry-run]")
        return 1

    total_ok = total_pending = total_failed = 0
    for filename, biz_date in FILES:
        try:
            up = upload(filename, token)
            rows = up.get("rows", [])
            if dry:
                print(f"[dry] {filename} 解析 {len(rows)} 行（跳过提交）")
                continue
            report = api_post_json(
                "/api/admin/import/commit",
                {"rows": rows, "team": "auto", "bizDate": biz_date, "mode": "merge", "filename": filename},
                token,
            )
            ok = report.get("okRows", 0) or 0
            pending = report.get("pending") or []
            failed = report.get("failedRows", 0) or 0
            unchanged = report.get("unchanged", 0) or 0
            total_ok += ok
            total_pending += len(pending)
            total_failed += failed
            print(
                f"{biz_date}  {filename:22s} 入库 {ok:3d} 暂存 {len(pending):2d} 失败 {failed:2d} 未变 {unchanged}"
            )
        except RuntimeError as e:
            print(f"{biz_date}  {filename:22s} ❌ {e}")
            return 1

    print(f"\n合计：入库 {total_ok} 行，暂存 {total_pending} 行，失败 {total_failed} 行")
    return 0


if __name__ == "__main__":
    sys.exit(main())
