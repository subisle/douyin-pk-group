#!/usr/bin/env python3
"""抖音直播间 PK 纯代码采集器（WebSocket + Protobuf，2026-11-12）。

设计目标：
- 不依赖 Playwright / Chromium / Chrome，不启动浏览器；
- 纯 Python 生成当前 WSS `signature`；
- 直接解析直播 Webcast WebSocket Protobuf；
- 断线重连、心跳、ACK、protobuf unknown fields 容错；
- 继续复用 collector.py 的 Observation 结构与后端上报格式。

X-Bogus 算法参考 Apache-2.0 项目 ylcangel/a_bogus 的 x_bogus/xbogus.py。
许可证与来源见 scripts/live/THIRD_PARTY_NOTICES.md。

用法：
    python3 scripts/live/collector_ws.py --room 90968758361 --dry-run
    python3 scripts/live/collector_ws.py --event 1 --room 90968758361
"""

from __future__ import annotations

import argparse
import asyncio
from contextlib import suppress
import gzip
import hashlib
import json
import math
import os
import random
import re
import secrets
import signal
import socket
import sys
import time
import urllib.parse
from collections import Counter, deque
from dataclasses import dataclass, field
from typing import Any, Iterable

import requests

try:  # websockets >= 14 的新 API
    from websockets.asyncio.client import connect as ws_connect
    _NEW_WEBSOCKETS = True
except ImportError:  # websockets 13.x
    from websockets.legacy.client import connect as ws_connect
    _NEW_WEBSOCKETS = False

sys.path.insert(0, os.path.dirname(__file__))
from collector import Api, _bonus_map, _empty_bonus, parse_linkmic, to_observation  # noqa: E402

DEFAULT_API = "http://127.0.0.1:8787"
DEFAULT_INTERVAL = 0.25
DEFAULT_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/126.0.0.0 Safari/537.36"
)
WSS_HOSTS = (
    "webcast5-ws-web-lq.douyin.com",
    "webcast100-ws-web-lq.douyin.com",
    "webcast5-ws-web-hl.douyin.com",
)
SIGN_FIELDS = "live_id,aid,version_code,webcast_sdk_version,room_id,sub_room_id,sub_channel_id,did_rule,user_unique_id,device_platform,device_type,ac,identity".split(",")


# ---------------------------------------------------------------------------
# X-Bogus / WSS signature（纯 Python）
# ---------------------------------------------------------------------------


class XBogusSigner:
    """生成抖音 WebSocket 握手使用的 16 位 X-Bogus。

    参考 Apache-2.0 项目 ylcangel/a_bogus。
    """

    _charset = "Dkdpgh4ZKsQB80/Mfvw36XI1R25-WUAlEi7NLboqYTOPuzmFjJnryx9HVGcaStCe="

    def __init__(self) -> None:
        self._index = 0

    def sign(self, md5_hex: str) -> str:
        try:
            data = bytes.fromhex(md5_hex)
        except ValueError as exc:
            raise ValueError("WSS signature md5 不是十六进制") from exc
        if len(data) != 16:
            raise ValueError("WSS signature md5 必须为 16 字节")

        self._index += 1
        payload = bytearray(9)
        payload[0] = self._index & 0x3F
        payload[1] = 0
        payload[2] = 1  # 跟踪源码时为 1；正常环境可保持
        payload[3] = 0  # action_valid: 正常环境为 0
        salt_md5 = hashlib.md5(b"").digest()
        salt_md5_md5 = hashlib.md5(salt_md5).digest()
        payload[4] = salt_md5_md5[14]
        payload[5] = salt_md5_md5[15]
        data_md5 = hashlib.md5(data).digest()
        payload[6] = data_md5[14]
        payload[7] = data_md5[15]
        payload[8] = secrets.randbelow(256)

        checksum = 0
        for value in payload:
            checksum ^= value
        payload.append(checksum)

        magic = 0x40 | (secrets.randbelow(100) & 1) << 4
        key = secrets.randbelow(256)
        encrypted = _rc4(bytes([key]), bytes(payload))
        raw = bytes([magic, key]) + encrypted.encode("iso-8859-1")
        return _dy_base64(raw, self._charset)


def _rc4(key: bytes, data: bytes) -> str:
    state = list(range(256))
    j = 0
    for i in range(256):
        j = (j + state[i] + key[i % len(key)]) % 256
        state[i], state[j] = state[j], state[i]
    i = j = 0
    out = bytearray()
    for value in data:
        i = (i + 1) % 256
        j = (j + state[i]) % 256
        state[i], state[j] = state[j], state[i]
        out.append(value ^ state[(state[i] + state[j]) % 256])
    return bytes(out).decode("iso-8859-1")


def _dy_base64(raw: bytes, charset: str) -> str:
    out: list[str] = []
    for pos in range(0, len(raw), 3):
        block = raw[pos : pos + 3]
        c1 = block[0]
        c2 = block[1] if len(block) > 1 else None
        c3 = block[2] if len(block) > 2 else None
        value = (c1 << 16) | ((c2 or 0) << 8) | (c3 or 0)
        out.append(charset[(value >> 18) & 63])
        out.append(charset[(value >> 12) & 63])
        out.append(charset[(value >> 6) & 63] if c2 is not None else "=")
        out.append(charset[value & 63] if c3 is not None else "=")
    return "".join(out)


def signature_md5(url: str) -> str:
    query = urllib.parse.parse_qs(urllib.parse.urlparse(url).query)
    canonical = ",".join(
        f"{name}={query.get(name, [''])[0]}" for name in SIGN_FIELDS
    )
    return hashlib.md5(canonical.encode()).hexdigest()


def build_wss_url(room_id: str, user_unique_id: str, host: str) -> str:
    now_ms = int(time.time() * 1000)
    wrds_v = str(secrets.randbelow(9_000_000_000_000_000_000) + 1_000_000_000_000_000_000)
    params = {
        "app_name": "douyin_web",
        "version_code": "180800",
        "webcast_sdk_version": "1.0.14-beta.0",
        "update_version_code": "1.0.14-beta.0",
        "compress": "gzip",
        "device_platform": "web",
        "cookie_enabled": "true",
        "screen_width": "1536",
        "screen_height": "864",
        "browser_language": "zh-CN",
        "browser_platform": "Win32",
        "browser_name": "Mozilla",
        "browser_version": DEFAULT_UA,
        "browser_online": "true",
        "tz_name": "Asia/Shanghai",
        "cursor": f"d-1_u-1_fh-{wrds_v}_t-{now_ms}_r-1",
        "internal_ext": (
            f"internal_src:dim|wss_push_room_id:{room_id}|wss_push_did:{user_unique_id}"
            f"|first_req_ms:{now_ms}|fetch_time:{now_ms}|seq:1|wss_info:0-{now_ms}-0-0"
            f"|wrds_v:{wrds_v}"
        ),
        "host": "https://live.douyin.com",
        "aid": "6383",
        "live_id": "1",
        "did_rule": "3",
        "endpoint": "live_pc",
        "support_wrds": "1",
        "user_unique_id": user_unique_id,
        "im_path": "/webcast/im/fetch/",
        "identity": "audience",
        "need_persist_msg_count": "15",
        "insert_task_id": "",
        "live_reason": "",
        "room_id": room_id,
        "heartbeatDuration": "0",
    }
    query = urllib.parse.urlencode(params, safe="|:,/-_")
    return f"wss://{host}/webcast/im/push/v2/?{query}"


def signed_wss_url(room_id: str, user_unique_id: str, host: str, signer: XBogusSigner) -> str:
    url = build_wss_url(room_id, user_unique_id, host)
    signature = signer.sign(signature_md5(url))
    return f"{url}&signature={urllib.parse.quote(signature, safe='')}"


# ---------------------------------------------------------------------------
# Minimal protobuf wire codec（不生成 .proto，unknown fields 自动忽略）
# ---------------------------------------------------------------------------


def _read_varint(data: bytes, pos: int) -> tuple[int, int]:
    value = 0
    shift = 0
    while pos < len(data) and shift <= 70:
        byte = data[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, pos
        shift += 7
    raise ValueError("protobuf varint 截断或过长")


def iter_fields(data: bytes) -> Iterable[tuple[int, int, Any]]:
    pos = 0
    while pos < len(data):
        key, pos = _read_varint(data, pos)
        number, wire = key >> 3, key & 7
        if number <= 0:
            raise ValueError("protobuf field number 非法")
        if wire == 0:
            value, pos = _read_varint(data, pos)
        elif wire == 1:
            if pos + 8 > len(data):
                raise ValueError("protobuf fixed64 截断")
            value = data[pos : pos + 8]
            pos += 8
        elif wire == 2:
            size, pos = _read_varint(data, pos)
            if pos + size > len(data):
                raise ValueError("protobuf bytes 截断")
            value = data[pos : pos + size]
            pos += size
        elif wire == 5:
            if pos + 4 > len(data):
                raise ValueError("protobuf fixed32 截断")
            value = data[pos : pos + 4]
            pos += 4
        else:
            raise ValueError(f"不支持的 protobuf wire type: {wire}")
        yield number, wire, value


def _encode_varint(value: int) -> bytes:
    out = bytearray()
    while value > 0x7F:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value & 0x7F)
    return bytes(out)


def encode_field(number: int, wire: int, value: int | bytes) -> bytes:
    key = _encode_varint((number << 3) | wire)
    if wire == 0:
        return key + _encode_varint(int(value))
    if wire == 2:
        raw = value if isinstance(value, bytes) else str(value).encode()
        return key + _encode_varint(len(raw)) + raw
    raise ValueError(f"不支持的编码 wire type: {wire}")


def _text(value: bytes) -> str:
    try:
        return value.decode("utf-8")
    except UnicodeDecodeError:
        return value.decode("utf-8", "replace")


def _int_text(value: bytes | int | None) -> int:
    if value is None:
        return 0
    if isinstance(value, int):
        return value
    text = _text(value).strip()
    return int(text) if text.isdigit() else 0


def _first(fields: Iterable[tuple[int, int, Any]], number: int, wire: int) -> Any:
    for field_number, field_wire, value in fields:
        if field_number == number and field_wire == wire:
            return value
    return None


def decode_push_frame(data: bytes) -> tuple[int, bytes, str]:
    fields = list(iter_fields(data))
    log_id = _first(fields, 2, 0) or 0
    payload_type = _first(fields, 7, 2)
    payload = _first(fields, 8, 2) or b""
    return int(log_id), payload, _text(payload_type) if payload_type else ""


@dataclass
class ResponseEnvelope:
    messages: list[dict[str, Any]] = field(default_factory=list)
    internal_ext: str = ""
    need_ack: bool = False
    now_ms: int = 0


def decode_response(payload: bytes) -> ResponseEnvelope:
    if payload[:2] == b"\x1f\x8b":
        payload = gzip.decompress(payload)
    top_fields = list(iter_fields(payload))
    internal_ext = ""
    need_ack = False
    now_ms = int(_first(top_fields, 4, 0) or 0)
    messages: list[dict[str, Any]] = []
    for number, wire, value in top_fields:
        if number == 5 and wire == 2:
            internal_ext = _text(value)
        elif number == 9 and wire == 0:
            need_ack = bool(value)
        elif number == 1 and wire == 2:
            message_fields = list(iter_fields(value))
            method = _first(message_fields, 1, 2)
            body = _first(message_fields, 2, 2)
            msg_id = _first(message_fields, 3, 0) or 0
            # Webcast Message wrapper: field 7 实测为纳秒级 create_time；
            # 兼容历史微秒 / 毫秒数据，统一归一到毫秒。
            timestamp_raw = int(_first(message_fields, 7, 0) or 0)
            if timestamp_raw > 100_000_000_000_000_000:  # 纳秒
                timestamp_ms = timestamp_raw // 1_000_000
            elif timestamp_raw > 100_000_000_000_000:  # 微秒
                timestamp_ms = timestamp_raw // 1000
            elif timestamp_raw > 0:
                timestamp_ms = timestamp_raw
            else:
                timestamp_ms = now_ms or int(time.time() * 1000)
            if method and body is not None:
                messages.append(
                    {
                        "method": _text(method),
                        "payload": body,
                        "msg_id": int(msg_id),
                        "timestamp_ms": int(timestamp_ms),
                    }
                )
    return ResponseEnvelope(messages, internal_ext, need_ack, now_ms)


def ack_frame(log_id: int, internal_ext: str) -> bytes:
    return (
        encode_field(2, 0, int(log_id))
        + encode_field(7, 2, b"ack")
        + encode_field(8, 2, internal_ext.encode())
    )


def heartbeat_frame() -> bytes:
    return encode_field(7, 2, b"hb")


# ---------------------------------------------------------------------------
# Room / battle state
# ---------------------------------------------------------------------------


@dataclass
class BattleState:
    battle_id: str = ""
    channel_id: str = ""
    start_time_ms: int = 0
    duration_secs: int = 0
    status: int = 1
    scores: dict[str, dict[str, Any]] = field(default_factory=dict)
    team_scores: list[dict[str, Any]] = field(default_factory=list)
    names: dict[str, str] = field(default_factory=dict)
    bonus: dict[str, dict[str, Any]] = field(default_factory=dict)
    battle_updated_mono: float = 0.0
    frame_updated_mono: float = 0.0
    last_message_ms: int = 0
    last_score_time_ms: int = 0
    last_score_msg_id: int = 0
    last_status_time_ms: int = 0
    force_emit: bool = False
    wss_connected: bool = False
    http_ok: bool = False
    finished_battle_ids: set[str] = field(default_factory=set)
    finished_battle_order: deque[str] = field(default_factory=lambda: deque(maxlen=64))

    def _remember_finished(self, battle_id: str) -> None:
        if not battle_id or battle_id in self.finished_battle_ids:
            return
        if len(self.finished_battle_order) == self.finished_battle_order.maxlen:
            self.finished_battle_ids.discard(self.finished_battle_order.popleft())
        self.finished_battle_order.append(battle_id)
        self.finished_battle_ids.add(battle_id)

    def reset_battle(self, battle_id: str, now_ms: int) -> bool:
        if battle_id in self.finished_battle_ids:
            return False
        if battle_id == self.battle_id:
            return True
        if now_ms < self.last_message_ms:
            return False
        self._remember_finished(self.battle_id)
        self.battle_id = battle_id
        self.channel_id = ""
        self.start_time_ms = 0
        self.duration_secs = 0
        self.status = 1
        self.scores.clear()
        self.team_scores.clear()
        self.bonus.clear()
        self.last_score_msg_id = 0
        self.last_score_time_ms = 0
        self.last_status_time_ms = 0
        self.battle_updated_mono = 0.0
        self.last_message_ms = now_ms
        return True

    def clear_battle(self) -> None:
        self._remember_finished(self.battle_id)
        self.battle_id = ""
        self.channel_id = ""
        self.start_time_ms = 0
        self.duration_secs = 0
        self.status = 1
        self.scores.clear()
        self.team_scores.clear()
        self.bonus.clear()
        self.last_score_msg_id = 0
        self.last_score_time_ms = 0
        self.last_status_time_ms = 0
        self.battle_updated_mono = 0.0
        self.force_emit = False

    def handle(self, msg: dict[str, Any]) -> None:
        method = msg["method"]
        payload = msg["payload"]
        msg_id = int(msg.get("msg_id") or 0)
        now_ms = int(msg.get("timestamp_ms") or 0) or int(time.time() * 1000)
        self.frame_updated_mono = time.monotonic()

        accepted = False
        if method in ("LinkMicMethod", "WebcastLinkMicMethod"):
            accepted = self._handle_linkmic(payload, msg_id, now_ms)
        elif method == "WebcastBattleStatusMessage":
            accepted = self._handle_battle_status(payload, now_ms)
        elif method == "WebcastLinkMicArmiesMethod":
            self._handle_armies(payload)

        if accepted:
            self._scan_bonus(payload, now_ms)

    def _handle_battle_status(self, payload: bytes, now_ms: int) -> bool:
        fields = list(iter_fields(payload))
        channel = _first(fields, 2, 2)
        battle = _first(fields, 3, 2)
        status = _first(fields, 4, 0)
        duration = _first(fields, 6, 0)
        start_raw = _first(fields, 8, 2)
        end_raw = _first(fields, 9, 2)
        battle_id = _text(battle).strip() if battle else ""
        if not battle_id or not self.reset_battle(battle_id, now_ms):
            return False
        if now_ms < max(self.last_status_time_ms, self.last_score_time_ms):
            return False
        if channel:
            self.channel_id = _text(channel).strip()
        if status is not None:
            self.status = max(self.status, int(status))
        start = _int_text(start_raw)
        end = _int_text(end_raw)
        if start > 0:
            self.start_time_ms = start
        if start > 0 and end > start:
            self.duration_secs = max(1, (end - start) // 1000)
        elif duration is not None and int(duration) > 0:
            self.duration_secs = int(duration)
        self.last_status_time_ms = now_ms
        self.last_message_ms = max(self.last_message_ms, now_ms)
        self.force_emit = True
        return True

    def _handle_linkmic(self, payload: bytes, msg_id: int, now_ms: int) -> bool:
        fields = list(iter_fields(payload))
        channel = _first(fields, 8, 0)
        battle_num = _first(fields, 104, 0)
        battle_text = _first(fields, 114, 2)
        battle_id = _text(battle_text).strip() if battle_text else str(battle_num or "")
        if not battle_id or not self.reset_battle(battle_id, now_ms):
            return False
        if self.status >= 2:
            return False
        if now_ms < self.last_score_time_ms:
            return False
        if channel:
            self.channel_id = str(channel)
        if msg_id and msg_id <= self.last_score_msg_id:
            return False

        scores: dict[str, dict[str, Any]] = {}
        for number, wire, entry in iter_fields(payload):
            if number != 17 or wire != 2:
                continue
            item = list(iter_fields(entry))
            uid_text = _first(item, 21, 2)
            uid_num = _first(item, 2, 0)
            uid = _text(uid_text).strip() if uid_text else str(uid_num or "")
            if not uid:
                continue
            score = int(_first(item, 1, 0) or 0)
            rank = int(_first(item, 8, 0) or 0)
            if score <= 0:
                score = _int_text(_first(item, 7, 2))
            scores[uid] = {
                "userId": uid,
                "nickname": self.names.get(uid, ""),
                "score": score,
                "rank": rank,
                "scoreVersion": now_ms,
                "teamId": "",
                **_empty_bonus(),
            }
        if scores:
            self.scores = scores
            self.last_score_msg_id = msg_id
            self.last_score_time_ms = max(self.last_score_time_ms, now_ms)
            self.last_message_ms = max(self.last_message_ms, now_ms)
            self.battle_updated_mono = time.monotonic()
            self.force_emit = False
            return True
        return False

    def _handle_armies(self, payload: bytes) -> None:
        # WebcastLinkMicArmiesMethod: field 3 = 队伍；field 1 = 用户；UID 是
        # 用户对象 field 1，昵称是 field 3。这里只取昵称，不用礼物贡献分。
        for number, wire, team in iter_fields(payload):
            if number != 3 or wire != 2:
                continue
            for team_no, team_wire, user in iter_fields(team):
                if team_no != 1 or team_wire != 2:
                    continue
                item = list(iter_fields(user))
                uid = _first(item, 1, 0)
                nickname = _first(item, 3, 2)
                if uid and nickname:
                    self.names[str(uid)] = _text(nickname)
        for uid, score in self.scores.items():
            if not score.get("nickname") and uid in self.names:
                score["nickname"] = self.names[uid]

    def _scan_bonus(self, payload: bytes, now_ms: int) -> None:
        for text_value in _nested_text_values(payload, max_depth=5):
            if "addition_multiple" not in text_value:
                continue
            try:
                start = text_value.find("{")
                data = json.loads(text_value[start:]) if start >= 0 else None
            except Exception:
                continue
            bonus = _bonus_map({"lynx_data": data}, now_ms)
            if bonus:
                self.bonus.update(bonus)
            return

    def apply_http(self, raw: dict[str, Any]) -> None:
        """合并一次 HTTP 快照。

        该接口当前在携带 ttwid/msToken Cookie 与完整参数时可直接成功，
        因此作为 WSS 的补充：补齐昵称、加成、队伍分，并在 WSS 不可用时兜底。
        """
        self.http_ok = True
        self.frame_updated_mono = time.monotonic()
        now_ms = int(raw.get("nowMs") or 0) or int(time.time() * 1000)
        battle = raw.get("battle")
        if not isinstance(battle, dict) or not battle.get("battleId"):
            if not self.battle_id:
                self.last_message_ms = max(self.last_message_ms, now_ms)
            return
        battle_id = str(battle.get("battleId") or "")
        if not self.reset_battle(battle_id, now_ms):
            return
        if now_ms < max(self.last_status_time_ms, self.last_score_time_ms):
            return
        status = int(battle.get("battleStatus") or self.status or 1)
        if self.status >= 2 and status < 2:
            return
        if battle.get("channelId"):
            self.channel_id = str(battle.get("channelId") or "")
        self.start_time_ms = int(battle.get("startTimeMs") or self.start_time_ms or 0)
        self.duration_secs = int(battle.get("durationSecs") or self.duration_secs or 0)
        self.status = max(self.status, status)
        self.last_status_time_ms = now_ms
        self.last_message_ms = max(self.last_message_ms, now_ms)
        self.force_emit = self.status >= 2
        self.team_scores = [
            dict(row) for row in (battle.get("teamScores") or []) if isinstance(row, dict)
        ]

        # HTTP 的 nowMs 比 WSS 分数更旧时不回写分数，避免两边轮询造成倒退。
        if now_ms >= self.last_score_time_ms:
            merged_scores: dict[str, dict[str, Any]] = {}
            for row in battle.get("scores") or []:
                if not isinstance(row, dict):
                    continue
                uid = str(row.get("userId") or "")
                if not uid:
                    continue
                score = dict(row)
                score["scoreVersion"] = now_ms
                merged_scores[uid] = score
                nickname = str(row.get("nickname") or "")
                if nickname:
                    self.names[uid] = nickname
                self.bonus[uid] = {
                    "bonusMultiple": int(row.get("bonusMultiple") or 100),
                    "bonusScore": int(row.get("bonusScore") or 0),
                    "bonusActive": bool(row.get("bonusActive", False)),
                    "bonusEndMs": int(row.get("bonusEndMs") or 0),
                }
            if merged_scores:
                self.scores = merged_scores
                self.last_score_time_ms = now_ms
                self.battle_updated_mono = time.monotonic()

    def raw(self, room_id: str, connected: bool) -> dict[str, Any]:
        if not connected:
            return {"roomId": room_id, "online": False, "nowMs": 0}
        if not self.battle_id:
            return {
                "roomId": room_id,
                "online": True,
                "nowMs": self.last_message_ms,
                "userCount": 0,
            }
        scores = []
        for uid, score in self.scores.items():
            row = dict(score)
            row["nickname"] = self.names.get(uid, row.get("nickname", ""))
            row.update(self.bonus.get(uid) or _empty_bonus())
            scores.append(row)
        scores.sort(
            key=lambda s: (
                int(s.get("rank") or 9999),
                -int(s.get("score") or 0),
                str(s.get("userId") or ""),
            )
        )
        return {
            "roomId": room_id,
            "online": True,
            "nowMs": self.last_message_ms,
            "userCount": len(scores),
            "battle": {
                "battleId": self.battle_id,
                "channelId": self.channel_id,
                "startTimeMs": self.start_time_ms,
                "durationSecs": self.duration_secs,
                "battleStatus": self.status,
                "scores": scores,
                "teamScores": self.team_scores,
            },
        }


def _nested_text_values(data: bytes, max_depth: int, depth: int = 0) -> Iterable[str]:
    if depth > max_depth:
        return
    try:
        fields = list(iter_fields(data))
    except Exception:
        return
    for _, wire, value in fields:
        if wire != 2 or not isinstance(value, bytes):
            continue
        if b"addition_multiple" in value:
            yield _text(value)
            continue
        # 只递归看起来像 protobuf 的对象，避免把任意 JSON/头像 URL 当消息解析。
        if len(value) >= 2 and value[0] < 0x80:
            yield from _nested_text_values(value, max_depth, depth + 1)


# ---------------------------------------------------------------------------
# HTTP session / websocket runner
# ---------------------------------------------------------------------------


@dataclass
class RoomCredentials:
    web_rid: str
    room_id: str
    ttwid: str
    cookie: str


def _generate_ms_token(length: int = 182) -> str:
    alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    return "".join(secrets.choice(alphabet) for _ in range(length))


def _extract_room_id(text: str) -> str:
    patterns = (
        r'roomId\\":\\"(\d+)\\"',
        r'"roomId"\s*:\s*"(\d+)"',
        r'"room_id"\s*:\s*"(\d+)"',
    )
    for pattern in patterns:
        match = re.search(pattern, text)
        if match:
            return match.group(1)
    return ""


def load_room_credentials(web_rid: str, http: requests.Session) -> RoomCredentials:
    headers = {
        "User-Agent": DEFAULT_UA,
        "Accept-Language": "zh-CN,zh;q=0.9",
        "Referer": f"https://live.douyin.com/{web_rid}",
    }
    response = http.get(
        f"https://live.douyin.com/{web_rid}",
        headers=headers,
        timeout=20,
    )
    response.raise_for_status()
    room_id = _extract_room_id(response.text)
    if not room_id:
        raise RuntimeError("直播间页面里没有找到 roomId")
    ttwid = http.cookies.get("ttwid") or ""
    if not ttwid:
        home = http.get("https://www.douyin.com/", headers=headers, timeout=20)
        home.raise_for_status()
        ttwid = http.cookies.get("ttwid") or ""
    if not ttwid:
        raise RuntimeError("没有获取到 ttwid")
    ms_token = _generate_ms_token()
    nonce = secrets.token_hex(8)
    http.cookies.set("msToken", ms_token)
    http.cookies.set("__ac_nonce", nonce)
    cookie = f"ttwid={ttwid}; msToken={ms_token}; __ac_nonce={nonce}"
    return RoomCredentials(web_rid, room_id, ttwid, cookie)


def fetch_linkmic_http(
    credentials: RoomCredentials,
    http: requests.Session,
    channel_id: str = "",
    anchor_id: str = "",
) -> dict[str, Any]:
    """主动请求 linkmic/list，作为 WSS 的昵称/加成补充和断线兜底。"""
    params = {
        "aid": "6383",
        "app_name": "douyin_web",
        "live_id": "1",
        "device_platform": "web",
        "language": "zh-CN",
        "enter_from": "link_share",
        "cookie_enabled": "true",
        "screen_width": "1280",
        "screen_height": "720",
        "browser_language": "zh-CN",
        "browser_platform": "Win32",
        "browser_name": "Chrome",
        "browser_version": "126.0.0.0",
        "room_id": credentials.room_id,
        "offset": "0",
        "count": "50",
        "link_status": "4",
        "scene": "7",
        "request_source": "receive_multi_linker_sei",
    }
    if channel_id:
        params["channel_id"] = channel_id
    if anchor_id:
        params["anchor_id"] = anchor_id
    response = http.get(
        "https://live.douyin.com/webcast/linkmic/list/",
        params=params,
        timeout=15,
    )
    response.raise_for_status()
    body = response.json()
    if int(body.get("status_code") or 0) != 0 or not body.get("data"):
        raise RuntimeError(f"linkmic/list 返回异常: {str(body)[:160]}")
    return parse_linkmic(body)


def _connect_kwargs(headers: dict[str, str]) -> dict[str, Any]:
    kwargs: dict[str, Any] = {
        "open_timeout": 15,
        "close_timeout": 10,
        "ping_interval": None,
        "max_size": 32 * 1024 * 1024,
    }
    if _NEW_WEBSOCKETS:
        kwargs["additional_headers"] = headers
    else:
        kwargs["extra_headers"] = headers
    return kwargs


async def _heartbeat_loop(ws: Any) -> None:
    while True:
        await asyncio.sleep(10)
        try:
            await ws.ping(heartbeat_frame())
        except TypeError:
            # 某些 websockets 版本把 ping payload 限定为 str，则退回普通 ping。
            await ws.ping()


async def run_room(
    args: argparse.Namespace,
    api: Api,
    web_rid: str,
    user_unique_id: str,
    worker_id: str,
    stop: asyncio.Event,
) -> None:
    http = requests.Session()
    http.headers.update({"User-Agent": DEFAULT_UA})
    state = BattleState()
    seen: deque[int] = deque(maxlen=4096)
    seen_set: set[int] = set()
    signer = XBogusSigner()
    backoff = 1.0
    host_index = 0
    credentials: RoomCredentials | None = None
    credential_holder: list[RoomCredentials | None] = [None]
    last_emit = 0.0
    emit_lock = asyncio.Lock()
    started_at = time.monotonic()
    first_score_at = 0.0
    once_seen = False
    frame_count = 0
    message_counts: Counter[str] = Counter()
    last_worker_heartbeat = 0.0

    def mark_seen(msg_id: int) -> bool:
        if msg_id <= 0:
            return True
        if msg_id in seen_set:
            return False
        if len(seen) == seen.maxlen:
            old = seen.popleft()
            seen_set.discard(old)
        seen.append(msg_id)
        seen_set.add(msg_id)
        return True

    async def emit(force: bool = False) -> bool:
        nonlocal last_emit, once_seen
        async with emit_lock:
            now_wall = time.monotonic()
            if not force and now_wall - last_emit < args.interval:
                return False
            last_emit = now_wall
            cred = credential_holder[0] or credentials
            raw = state.raw(cred.room_id if cred else "", state.wss_connected or state.http_ok)
            if state.battle_id and state.battle_updated_mono:
                last_snapshot = state.battle_updated_mono
            else:
                last_snapshot = state.frame_updated_mono
            obs, fresh = _build_observation(
                web_rid, raw, int(time.time() * 1000), last_snapshot, now_wall, args.interval
            )
            if fresh and state.scores:
                once_seen = True
            if args.dry_run:
                if args.print_json:
                    print(json.dumps(obs, ensure_ascii=False, indent=2))
                print(
                    f"[dry-run] {web_rid} phase={'fresh' if fresh else 'idle'} "
                    f"battle={state.battle_id or '-'} users={len(state.scores)} "
                    f"status={state.status}",
                    file=sys.stderr,
                )
                ok = True
            else:
                result = (
                    await asyncio.to_thread(api.ingest, args.event, obs)
                    if args.event > 0
                    else await asyncio.to_thread(api.ingest_room, web_rid, obs)
                )
                ok = result is not None
                print(
                    f"[poll] {web_rid} battle={state.battle_id or '-'} "
                    f"phase={(result or {}).get('phase', '?')} users={len(state.scores)}",
                    file=sys.stderr,
                )
            if ok and state.status >= 2:
                state.clear_battle()
            return ok

    async def http_loop() -> None:
        nonlocal first_score_at
        while not stop.is_set():
            delay = args.http_seconds if state.wss_connected else 2.0
            try:
                cred = credential_holder[0]
                if cred is None:
                    cred = await asyncio.to_thread(load_room_credentials, web_rid, http)
                    credential_holder[0] = cred
                anchor_id = next(iter(state.scores), "")
                raw = await asyncio.to_thread(
                    fetch_linkmic_http, cred, http, state.channel_id, anchor_id
                )
                state.apply_http(raw)
                if state.scores and first_score_at <= 0:
                    first_score_at = time.monotonic()
                if not state.wss_connected or raw.get("battle"):
                    await emit(False)
            except Exception as exc:
                state.http_ok = False
                print(
                    f"[warn] {web_rid} HTTP 兜底失败: {type(exc).__name__}: {exc}",
                    file=sys.stderr,
                )
                if not state.wss_connected and not args.dry_run:
                    error_obs = {
                        "webRid": web_rid,
                        "roomOnline": False,
                        "nowMs": int(time.time() * 1000),
                        "health": "error",
                        "error": f"HTTP 兜底失败: {exc}"[:200],
                    }
                    if args.event > 0:
                        await asyncio.to_thread(api.ingest, args.event, error_obs)
                    else:
                        await asyncio.to_thread(api.ingest_room, web_rid, error_obs)
            await _sleep_or_stop(stop, delay)

    http_task = asyncio.create_task(http_loop())

    while not stop.is_set():
        if args.once and once_seen:
            http_task.cancel()
            return
        try:
            if credentials is None or backoff >= 3:
                credentials = await asyncio.to_thread(load_room_credentials, web_rid, http)
                credential_holder[0] = credentials
            host = WSS_HOSTS[host_index % len(WSS_HOSTS)]
            url = signed_wss_url(credentials.room_id, user_unique_id, host, signer)
            headers = {"Cookie": credentials.cookie, "User-Agent": DEFAULT_UA}
            async with ws_connect(url, **_connect_kwargs(headers)) as ws:
                state.wss_connected = True
                state.force_emit = True
                backoff = 1.0
                print(f"[ws] {web_rid} connected via {host}", file=sys.stderr)
                hb = asyncio.create_task(_heartbeat_loop(ws))
                try:
                    while not stop.is_set():
                        frame = await asyncio.wait_for(ws.recv(), timeout=45)
                        if isinstance(frame, str):
                            continue
                        log_id, payload, payload_type = decode_push_frame(frame)
                        if payload_type != "msg" or not payload:
                            continue
                        response = decode_response(payload)
                        frame_count += 1
                        if response.need_ack and response.internal_ext:
                            await ws.send(ack_frame(log_id, response.internal_ext))
                        for msg in response.messages:
                            message_counts[msg["method"]] += 1
                            if not mark_seen(int(msg.get("msg_id") or 0)):
                                continue
                            state.handle(msg)
                            if state.scores and first_score_at <= 0:
                                first_score_at = time.monotonic()
                        if state.status >= 2 and state.force_emit:
                            await emit(force=True)
                        elif await emit(False):
                            pass
                        if (
                            args.event > 0
                            and time.monotonic() - last_worker_heartbeat >= 20
                        ):
                            await asyncio.to_thread(
                                api.heartbeat, worker_id, len(args.room), args.event
                            )
                            last_worker_heartbeat = time.monotonic()
                        if args.once:
                            if once_seen:
                                http_task.cancel()
                                return
                            if time.monotonic() - started_at >= args.wait:
                                http_task.cancel()
                                return
                finally:
                    state.wss_connected = False
                    hb.cancel()
                    try:
                        await hb
                    except (asyncio.CancelledError, Exception):
                        pass
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            state.wss_connected = False
            state.frame_updated_mono = 0.0
            print(f"[warn] {web_rid} 连接异常: {type(exc).__name__}: {exc}", file=sys.stderr)
            await _sleep_or_stop(stop, backoff + random.random())
            backoff = min(30.0, backoff * 1.8)
            host_index += 1
        if args.once and time.monotonic() - started_at >= args.wait:
            http_task.cancel()
            return
    http_task.cancel()


def _build_observation(
    web_rid: str,
    raw: dict[str, Any],
    now_ms: int,
    last_snapshot: float,
    wall_now: float,
    interval: float,
) -> tuple[dict[str, Any], bool]:
    # 与旧 collector 的新鲜度纪律一致：battle 超过 30 秒无更新时
    # 不再让旧分数继续推进晋级/推送。
    if not raw:
        return (
            {
                "webRid": web_rid,
                "roomOnline": True,
                "nowMs": now_ms,
                "health": "ok",
                "error": "",
            },
            False,
        )
    if raw.get("battle") and last_snapshot > 0:
        stale = wall_now - last_snapshot > max(30.0, interval * 6)
        if stale:
            return (
                {
                    "webRid": web_rid,
                    "roomOnline": True,
                    "nowMs": now_ms,
                    "health": "error",
                    "error": "WebSocket 长时间没有更新 PK 分数",
                },
                False,
            )
    obs = to_observation(web_rid, raw, now_ms)
    if obs.get("nowMs", 0) <= 0:
        obs["nowMs"] = now_ms
    return obs, bool(raw.get("battle"))


async def _sleep_or_stop(stop: asyncio.Event, seconds: float) -> None:
    try:
        await asyncio.wait_for(stop.wait(), timeout=max(0.0, seconds))
    except asyncio.TimeoutError:
        pass


async def async_main(args: argparse.Namespace) -> int:
    if not args.dry_run and not args.token:
        print("缺少 LIVE_COLLECTOR_TOKEN / --token；本地验证可加 --dry-run", file=sys.stderr)
        return 2
    api = Api(args.api, args.token)
    worker_id = f"{socket.gethostname()}:{os.getpid()}"
    user_unique_id = os.environ.get("LIVE_WS_USER_UNIQUE_ID", "").strip()
    if not user_unique_id:
        # 同进程多房间共用设备 id；固定为 19 位数字，避免被服务端当异常设备。
        user_unique_id = str(secrets.randbelow(9_000_000_000_000_000_000) + 1_000_000_000_000_000_000)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, stop.set)
        except (NotImplementedError, RuntimeError):
            pass

    tasks = [
        asyncio.create_task(run_room(args, api, rid, user_unique_id, worker_id, stop))
        for rid in args.room
    ]
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=False)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="抖音 PK 纯代码直播采集器（无 Playwright/Chromium）")
    ap.add_argument("--event", type=int, default=0, help="后端赛事 id；0 = 独立房间监控")
    ap.add_argument("--room", action="append", default=[], help="直播间 web_rid，可重复")
    ap.add_argument("--api", default=os.environ.get("LIVE_API", DEFAULT_API))
    ap.add_argument("--token", default=os.environ.get("LIVE_COLLECTOR_TOKEN", ""))
    ap.add_argument("--interval", type=float, default=DEFAULT_INTERVAL, help="两次上报的最小合并间隔（秒）")
    ap.add_argument(
        "--http-seconds",
        type=float,
        default=15.0,
        help="WSS 正常时 HTTP 补充昵称/加成的间隔；WSS 断线时自动降为 2 秒",
    )
    ap.add_argument("--once", action="store_true")
    ap.add_argument("--wait", type=float, default=60.0)
    ap.add_argument("--dry-run", action="store_true", help="只打印，不回传后端")
    ap.add_argument("--print-json", action="store_true", help="dry-run 时把完整 Observation 打印到 stdout")
    args = ap.parse_args()
    if not args.room:
        ap.error("至少需要一个 --room")
    if args.interval < 0.1:
        ap.error("--interval 不能小于 0.1 秒")
    if args.http_seconds < 5:
        ap.error("--http-seconds 不能小于 5 秒")
    return asyncio.run(async_main(args))


if __name__ == "__main__":
    raise SystemExit(main())
