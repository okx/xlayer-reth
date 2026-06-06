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
        self.assertEqual(events[0]["phase"], "run_start")
        self.assertEqual(events[-1]["phase"], "run_end")
        self.assertEqual(events[0]["mode"], "aot")

    def test_partial_tslog_loads_without_run_end(self):
        events = ptm.read_tslog(FIXTURES / "partial-tslog" / "aot-cycle-nojit-20260601_120100.tslog")
        phases = [e["phase"] for e in events]
        self.assertIn("warmup_start", phases)
        self.assertNotIn("run_end", phases)


if __name__ == "__main__":
    unittest.main()
