import time
import unittest
import json

from collector import build_ingest_payload, parse_linkmic


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


if __name__ == "__main__":
    unittest.main()
