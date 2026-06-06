"""Unit tests for parse-bench-timing.py.

Run: python3 -m unittest discover -s scripts/tests -v
"""
import unittest
from pathlib import Path
import sys
import importlib.util

# Load the parser module from scripts/parse-bench-timing.py (hyphenated name needs importlib)
HERE = Path(__file__).resolve().parent
PARSER_PATH = HERE.parent / "parse-bench-timing.py"
spec = importlib.util.spec_from_file_location("parse_bench_timing", PARSER_PATH)
ptm = importlib.util.module_from_spec(spec)
sys.modules["parse_bench_timing"] = ptm
spec.loader.exec_module(ptm)

FIXTURES = HERE / "fixtures"


class TestTslogReader(unittest.TestCase):
    def test_complete_tslog_loads(self):
        events = ptm.read_tslog(FIXTURES / "complete-run" / "aot-cycle-aot-20260601_120000.tslog")
        self.assertEqual(len(events), 19)
        self.assertEqual(events[0]["phase"], "run_start")
        self.assertEqual(events[-1]["phase"], "run_end")
        self.assertEqual(events[0]["mode"], "aot")
        self.assertEqual(events[8]["phase"], "warmup_start")
        self.assertEqual(events[8]["block"], "8")  # block kept as string from regex
        self.assertAlmostEqual(events[0]["ts"], 1717228800.0, places=3)

    def test_partial_tslog_loads_without_run_end(self):
        events = ptm.read_tslog(FIXTURES / "partial-tslog" / "aot-cycle-nojit-20260601_120100.tslog")
        phases = [e["phase"] for e in events]
        self.assertIn("warmup_start", phases)
        self.assertNotIn("run_end", phases)


class TestNodeLogReader(unittest.TestCase):
    def test_complete_node_log(self):
        result = ptm.read_node_log(FIXTURES / "complete-run" / "aot-node-aot-20260601_120000.log")
        self.assertGreater(len(result["blocks"]), 250)
        first_real = next(b for b in result["blocks"] if b["block"] == 42)
        self.assertEqual(first_real["n_seq_txs"], 1)
        self.assertGreater(first_real["n_pool_txs"], 100)
        self.assertGreater(len(result["jit_snapshots"]), 10)
        self.assertIsNotNone(result["aot_open"])
        self.assertEqual(result["aot_open"]["loaded"], 4)
        self.assertEqual(result["aot_open"]["dlopen_us"], 142000)

    def test_no_bench_timing_node_log(self):
        result = ptm.read_node_log(FIXTURES / "no-bench-timing" / "aot-node-nojit-20260601_120200.log")
        self.assertEqual(result["blocks"], [])
        self.assertIsNone(result["aot_open"])


class TestPolycliReader(unittest.TestCase):
    def test_polycli_final_tps(self):
        result = ptm.read_polycli_out(FIXTURES / "complete-run" / "aot-result-aot-20260601_120000.out")
        self.assertAlmostEqual(result["final_tps"], 287.30, places=2)
        self.assertEqual(result["num_errors"], 0)


if __name__ == "__main__":
    unittest.main()
