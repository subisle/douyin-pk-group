import time
import unittest
import json
from unittest import mock

from collector import build_ingest_payload, parse_linkmic, to_observation


class RefereeFilterTests(unittest.TestCase):
    def test_referee_is_removed_from_competition_scores(self):
        raw = {
            "nowMs": 1_100_000,
            "battle": {
                "battleId": "battle-ref",
                "scores": [
                    {"userId": "3358343524517819", "nickname": "ST-001", "score": 20},
                    {"userId": "1001", "nickname": "选手A", "score": 200},
                    {"userId": "1002", "nickname": "选手B", "score": 100},
                ],
            },
        }
        obs = to_observation("room-1", raw, 1_100_000)
        self.assertEqual(
            [s["userId"] for s in obs["battle"]["scores"]],
            ["1001", "1002"],
        )


class SnapshotFreshnessTests(unittest.TestCase):
    def test_stale_snapshot_is_not_used_for_scores(self):
        now = time.time()
        raw = {
            "nowMs": int(now * 1000),
            "battle": {
                "battleId": "old-battle",
                "scores": [{"userId": "1", "score": 9999}],
            },
        }

        obs, fresh = build_ingest_payload(
            "room-1",
            raw,
            int(now * 1000),
            now - 31,
            now,
            interval=4,
        )

        self.assertFalse(fresh)
        self.assertEqual(obs["health"], "error")
        self.assertNotIn("battle", obs)


    def test_fresh_snapshot_keeps_battle_scores(self):
        now = time.time()
        raw = {
            "nowMs": int(now * 1000),
            "battle": {
                "battleId": "current-battle",
                "scores": [{"userId": "1", "score": 100}],
            },
        }

        obs, fresh = build_ingest_payload(
            "room-1",
            raw,
            int(now * 1000),
            now - 2,
            now,
            interval=4,
        )

        self.assertTrue(fresh)
        self.assertEqual(obs["health"], "ok")
        self.assertEqual(obs["battle"]["battleId"], "current-battle")

    def test_parse_linkmic_extracts_active_bonus_and_real_score_source(self):
        now_ms = 1_100_000
        raw = {
            "extra": {"now": now_ms},
            "data": {
                "has_room": True,
                "room_id": "room-1",
                "user": [],
                "battle_stats": {
                    "battle_id": "battle-1",
                    "battle_settings": {
                        "start_time_ms": 1_000_000,
                        "duration": 300,
                        "battle_status": 1,
                        "lynx_data": json.dumps({
                            "battle_easter_egg_info": {
                                "easter_egg_info_map": {
                                    "uid-1": {
                                        "config": {
                                            "addition_multiple": "150",
                                            "period_list": [
                                                {"stage": 1, "start_time_sec": 1_000, "duration_sec": 2},
                                                {"stage": 4, "start_time_sec": 1_001, "duration_sec": 144},
                                            ],
                                        },
                                        "update_info": {
                                            "reward_info": {
                                                "addition_score_map": {"uid-1": "298"},
                                            },
                                        },
                                    },
                                },
                            },
                        }, ensure_ascii=False),
                    },
                    "battle_scores": [{
                        "user_id_str": "uid-1",
                        "score": 1194,
                        "battle_rank": 1,
                        "score_version": 2,
                        "current_bonus": 7,
                    }],
                },
            },
        }

        parsed = parse_linkmic(raw)
        score = parsed["battle"]["scores"][0]

        self.assertEqual(score["bonusMultiple"], 150)
        self.assertEqual(score["bonusScore"], 298)
        self.assertTrue(score["bonusActive"])
        self.assertEqual(score["bonusEndMs"], 1_145_000)
        self.assertEqual(score["currentBonus"], 7)
        self.assertEqual(score["score"] - score["bonusScore"], 896)

    def test_parse_linkmic_keeps_monitor_order_and_drops_referee(self):
        raw = {
            "extra": {"now": 1_100_000},
            "data": {
                "has_room": True,
                "room_id": "room-1",
                "user": [
                    {"user": {"id_str": "uid-b", "nick_name": "主播B"}},
                    {"user": {"id_str": "3358343524517819", "nick_name": "裁判"}},
                    {"user": {"id_str": "uid-a", "nick_name": "主播A"}},
                    {"user": {"id_str": "uid-b", "nick_name": "重复B"}},
                ],
            },
        }

        parsed = parse_linkmic(raw)
        self.assertEqual(
            [row["userId"] for row in parsed["linkmicUsers"]],
            ["uid-b", "uid-a"],
        )
        obs = to_observation("room-1", parsed, 1_100_000)
        self.assertEqual(
            [row["userId"] for row in obs["linkmicUsers"]],
            ["uid-b", "uid-a"],
        )


class PureWebsocketCollectorTests(unittest.TestCase):
    def test_wss_signature_is_pure_python_and_stable_length(self):
        import collector_ws

        signer = collector_ws.XBogusSigner()
        url = collector_ws.build_wss_url("7700000000000000000", "7319483754668557238", "webcast5-ws-web-lq.douyin.com")
        md5_hex = collector_ws.signature_md5(url)
        sig = signer.sign(md5_hex)
        self.assertEqual(len(sig), 16)
        self.assertNotEqual(sig, signer.sign(md5_hex), "签名字节应带随机 key")

    def test_decode_response_normalizes_nanosecond_timestamp_and_dedups(self):
        import collector_ws as c

        # message wrapper: method field 1, payload field 2, msgId field 3,
        # create_time field 7（纳秒），外层 Response: message field 1、need_ack field 9。
        wrapper = (
            c.encode_field(1, 2, b"LinkMicMethod")
            + c.encode_field(2, 2, b"payload")
            + c.encode_field(3, 0, 12345)
            + c.encode_field(7, 0, 1_700_000_000_000_000_000)
        )
        response = c.encode_field(1, 2, wrapper) + c.encode_field(4, 0, 1_700_000_000_123) + c.encode_field(9, 0, 1)
        decoded = c.decode_response(response)
        self.assertTrue(decoded.need_ack)
        self.assertEqual(decoded.now_ms, 1_700_000_000_123)
        self.assertEqual(decoded.messages[0]["timestamp_ms"], 1_700_000_000_000)
        self.assertEqual(decoded.messages[0]["method"], "LinkMicMethod")
        self.assertEqual(decoded.messages[0]["msg_id"], 12345)

    def test_linkmic_ws_scores_and_http_bonus_have_observation_shape(self):
        import collector_ws as c

        state = c.BattleState()
        battle_status = (
            c.encode_field(2, 2, b"channel-1")
            + c.encode_field(3, 2, b"battle-1")
            + c.encode_field(4, 0, 1)
            + c.encode_field(8, 2, b"1700000000000")
            + c.encode_field(9, 2, b"1700000300000")
        )
        state.handle({"method": "WebcastBattleStatusMessage", "payload": battle_status, "msg_id": 1, "timestamp_ms": 1700000000000})

        score_entry = (
            c.encode_field(1, 0, 1234)
            + c.encode_field(2, 0, 999)
            + c.encode_field(8, 0, 1)
            + c.encode_field(21, 2, b"999")
        )
        linkmic = (
            c.encode_field(8, 0, 111)
            + c.encode_field(17, 2, score_entry)
            + c.encode_field(104, 0, 777)
            + c.encode_field(114, 2, b"battle-1")
        )
        state.handle({"method": "LinkMicMethod", "payload": linkmic, "msg_id": 2, "timestamp_ms": 1700000000500})
        self.assertEqual(len(state.scores), 1)
        self.assertEqual(state.scores["999"]["score"], 1234)
        self.assertEqual(state.scores["999"]["rank"], 1)

        state.apply_http(
            {
                "roomId": "room-1",
                "online": True,
                "nowMs": 1700000001000,
                "battle": {
                    "battleId": "battle-1",
                    "channelId": "channel-1",
                    "startTimeMs": 1700000000000,
                    "durationSecs": 300,
                    "battleStatus": 1,
                    "scores": [
                        {
                            "userId": "999",
                            "nickname": "测试主播",
                            "score": 1300,
                            "rank": 1,
                            "scoreVersion": 285,
                            "teamId": "",
                            "bonusMultiple": 150,
                            "bonusScore": 66,
                            "bonusActive": True,
                            "bonusEndMs": 1700000200000,
                            "currentBonus": 7,
                        }
                    ],
                    "teamScores": [],
                },
            }
        )
        raw = state.raw("room-1", True)
        score = raw["battle"]["scores"][0]
        self.assertEqual(score["nickname"], "测试主播")
        self.assertEqual(score["score"], 1300)
        self.assertEqual(score["bonusMultiple"], 150)
        self.assertEqual(score["bonusScore"], 66)
        self.assertTrue(score["bonusActive"])
        obs = __import__("collector").to_observation("room-1", raw, 1700000001000)
        self.assertEqual(obs["battle"]["battleId"], "battle-1")
        self.assertEqual(obs["battle"]["scores"][0]["bonusScore"], 66)

    def test_finished_battle_is_not_replayed(self):
        import collector_ws as c

        state = c.BattleState()
        state.battle_id = "battle-ended"
        state.status = 3
        state.force_emit = True
        state.clear_battle()
        self.assertEqual(state.battle_id, "")
        self.assertIn("battle-ended", state.finished_battle_ids)
        state.apply_http(
            {
                "roomId": "room-1",
                "online": True,
                "nowMs": 1700000000000,
                "battle": {"battleId": "battle-ended", "scores": []},
            }
        )
        self.assertEqual(state.battle_id, "", "已结束对局的残留 HTTP 快照不应复活")

    def test_old_websocket_score_cannot_overwrite_new_http_score(self):
        import collector_ws as c

        state = c.BattleState()
        state.apply_http({"nowMs": 2000, "battle": {
            "battleId": "battle-1", "scores": [{"userId": "999", "score": 200}],
        }})
        payload = c.encode_field(114, 2, b"battle-1") + c.encode_field(
            17, 2, c.encode_field(21, 2, b"999") + c.encode_field(1, 0, 100)
        )
        state.handle({"method": "LinkMicMethod", "payload": payload,
                      "msg_id": 1, "timestamp_ms": 1000})
        self.assertEqual(state.scores["999"]["score"], 200)
        self.assertEqual(state.last_message_ms, 2000)

    def test_prepare_roster_is_kept_before_battle_starts(self):
        import collector_ws as c

        state = c.BattleState()
        changed = state.apply_http({
            "roomId": "room-1",
            "online": True,
            "nowMs": 2000,
            "linkmicUsers": [
                {"userId": "uid-b", "nickname": "主播B"},
                {"userId": "uid-a", "nickname": "主播A"},
            ],
        })
        raw = state.raw("room-1", True)

        self.assertTrue(changed)
        self.assertNotIn("battle", raw)
        self.assertEqual(
            [row["userId"] for row in raw["linkmicUsers"]],
            ["uid-b", "uid-a"],
        )

    def test_websocket_linkmic_users_are_kept_without_battle_id(self):
        import collector_ws as c

        state = c.BattleState()
        first = c.encode_field(21, 2, b"uid-a") + c.encode_field(1, 0, 0)
        second = c.encode_field(21, 2, b"uid-b") + c.encode_field(1, 0, 0)
        payload = c.encode_field(17, 2, first) + c.encode_field(17, 2, second)
        state.handle({
            "method": "LinkMicMethod",
            "payload": payload,
            "msg_id": 1,
            "timestamp_ms": 1000,
        })

        self.assertEqual(
            [row["userId"] for row in state.linkmic_users],
            ["uid-a", "uid-b"],
        )
        self.assertEqual(state.battle_id, "")

    def test_ended_status_cannot_be_reopened_before_delivery(self):
        import collector_ws as c

        state = c.BattleState()
        state.apply_http({"nowMs": 2000, "battle": {
            "battleId": "battle-1", "battleStatus": 3,
            "scores": [{"userId": "999", "score": 200}],
        }})
        state.apply_http({"nowMs": 3000, "battle": {
            "battleId": "battle-1", "battleStatus": 1,
            "scores": [{"userId": "999", "score": 100}],
        }})
        self.assertEqual(state.status, 3)
        self.assertEqual(state.scores["999"]["score"], 200)

    def test_previous_battle_cannot_replace_current_battle(self):
        import collector_ws as c

        state = c.BattleState()
        for battle_id, timestamp in (("old", 1000), ("new", 3000), ("old", 4000)):
            state.apply_http({"nowMs": timestamp, "battle": {
                "battleId": battle_id, "scores": [{"userId": "999", "score": timestamp}],
            }})
        self.assertEqual(state.battle_id, "new")
        self.assertEqual(state.scores["999"]["score"], 3000)

    def test_status_packet_does_not_keep_old_scores_fresh(self):
        import collector_ws as c

        state = c.BattleState()
        state.apply_http({"nowMs": 1000, "battle": {
            "battleId": "battle-1", "scores": [{"userId": "999", "score": 200}],
        }})
        state.battle_updated_mono = 10.0
        state.handle({"method": "WebcastBattleStatusMessage", "payload": (
            c.encode_field(3, 2, b"battle-1") + c.encode_field(4, 0, 1)
        ), "timestamp_ms": 40000})
        obs, fresh = c._build_observation("room-1", state.raw("room-1", True),
                                        40000, state.battle_updated_mono, 50.0, 4.0)
        self.assertFalse(fresh)
        self.assertNotIn("battle", obs)


class RuntimeLimitTests(unittest.TestCase):
    def test_invalid_live_max_minutes_falls_back_to_six_hours(self):
        import collector_ws as c

        with mock.patch.dict("os.environ", {"LIVE_MAX_MINUTES": "invalid"}):
            self.assertEqual(c._env_float("LIVE_MAX_MINUTES", c.DEFAULT_MAX_MINUTES), 360.0)
        with mock.patch.dict("os.environ", {"LIVE_MAX_MINUTES": "90"}):
            self.assertEqual(c._env_float("LIVE_MAX_MINUTES", c.DEFAULT_MAX_MINUTES), 90.0)


class RuntimeLimitAsyncTests(unittest.IsolatedAsyncioTestCase):
    async def test_max_runtime_sets_stop_event(self):
        import asyncio
        import collector_ws as c

        stop = asyncio.Event()
        await c._stop_after_minutes(stop, 0.001)
        self.assertTrue(stop.is_set())


if __name__ == "__main__":
    unittest.main()
