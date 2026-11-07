import time
import unittest

from collector import build_ingest_payload


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


if __name__ == "__main__":
    unittest.main()
