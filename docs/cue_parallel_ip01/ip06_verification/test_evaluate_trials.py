"""Independent fixture tests for H-01 and invalid pairing failure modes."""
import csv
import importlib.util
import tempfile
import unittest
from pathlib import Path

MODULE = Path(__file__).with_name('evaluate_trials.py')
spec = importlib.util.spec_from_file_location('ip06_evaluator', MODULE)
evaluator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluator)


def trial(mode='off', route='S1', pair=1, digest='a' * 64, wall=20.0):
    return {
        'pair_id': str(pair), 'host': 'host-a', 'storage': 'nvme',
        'cache_state': 'warm', 'tracks': '12', 'replaygain': mode,
        'route': route, 'wall_seconds': str(wall),
        'source_sha256': 'b' * 64, 'fixture_sha256': 'c' * 64,
        'configuration_sha256': digest,
    }


class EvaluatorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'trials.csv'

    def evaluate(self, rows):
        with self.path.open('w', newline='', encoding='utf-8') as f:
            writer = csv.DictWriter(f, fieldnames=evaluator.FIELDS)
            writer.writeheader()
            writer.writerows(rows)
        return evaluator.evaluate(self.path)

    @staticmethod
    def valid_rows():
        return [trial(mode, route, pair, digest,
                      20.0 if route == 'S1' else 10.0)
                for mode, digest in (('off', 'a' * 64), ('both', 'd' * 64))
                for pair in range(1, 6)
                for route in ('S1', 'S3')]

    def test_valid_distinct_modes_and_matching_pairs(self):
        result = self.evaluate(self.valid_rows())
        self.assertEqual(len(result), 2)
        self.assertTrue(all('n=5' in line and 'CANDIDATE' in line for line in result))

    def test_reject_cross_mode_reused_digest(self):
        rows = self.valid_rows()
        for row in rows:
            row['configuration_sha256'] = 'a' * 64
        with self.assertRaisesRegex(ValueError, 'reused across ReplayGain modes'):
            self.evaluate(rows)

    def test_reject_pair_digest_disagreement(self):
        rows = self.valid_rows()
        rows[1]['configuration_sha256'] = 'e' * 64
        with self.assertRaisesRegex(ValueError, 'route configuration SHA-256 mismatch'):
            self.evaluate(rows)

    def test_reject_missing_hash(self):
        rows = self.valid_rows()
        rows[0]['fixture_sha256'] = ''
        with self.assertRaisesRegex(ValueError, 'fixture_sha256'):
            self.evaluate(rows)

    def test_reject_duplicate_route(self):
        rows = self.valid_rows()
        rows.append(dict(rows[0]))
        with self.assertRaisesRegex(ValueError, 'duplicate route'):
            self.evaluate(rows)

    def test_reject_nan_and_zero_wall(self):
        for wall in ('nan', 'inf', '0', '-1', 'missing'):
            with self.subTest(wall=wall), self.assertRaises(ValueError):
                rows = self.valid_rows()
                rows[0]['wall_seconds'] = wall
                self.evaluate(rows)

    def test_incomplete_pair_does_not_promote(self):
        rows = [row for row in self.valid_rows()
                if not (row['replaygain'] == 'off' and row['pair_id'] == '5'
                        and row['route'] == 'S3')]
        result = self.evaluate(rows)
        self.assertEqual(len(result), 2)
        self.assertTrue(any('INSUFFICIENT' in line for line in result))
        self.assertTrue(any('CANDIDATE' in line for line in result))

    def test_slow_route_not_candidate(self):
        rows = self.valid_rows()
        for row in rows:
            if row['route'] == 'S3':
                row['wall_seconds'] = '21'
        result = self.evaluate(rows)
        self.assertEqual(len(result), 2)
        self.assertTrue(all('NO AUTOMATIC PROMOTION' in line for line in result))

    def test_empty_csv_not_accepted(self):
        self.path.write_text(",".join(evaluator.FIELDS) + "\n")
        with self.assertRaisesRegex(ValueError, "empty trial evidence"):
            evaluator.evaluate(self.path)

    def test_missing_required_header(self):
        self.path.write_text('pair_id,route,wall_seconds\na,S1,10\n')
        with self.assertRaisesRegex(ValueError, 'expected CSV headers'):
            evaluator.evaluate(self.path)

    def test_reject_unrecognized_mode(self):
        rows = self.valid_rows()
        rows[0]['replaygain'] = 'maybe'
        with self.assertRaisesRegex(ValueError, 'unsupported ReplayGain mode'):
            self.evaluate(rows)


if __name__ == '__main__':
    unittest.main()
