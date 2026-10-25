#!/usr/bin/env python3
"""Offline regression cases for IP-03 D-01 extraction-mode verification.

Run from any working directory with:
    python3 -B docs/cue_parallel_ip01/ip03_verification/test_verify_static_ip03.py
Full tar extraction with umask 022/077 is independently tested in the handoff.
"""
import unittest

from verify_static_ip03 import assert_source_entries, comparable_entries, comparable_entry
from make_manifest import make_diff


HASH = '0' * 64


def file_entry(mode='0o664', sha=HASH):
    return {'type': 'file', 'sha256': sha, 'mode': mode}


class VerificationIdentityTests(unittest.TestCase):
    def test_umask_022_and_077_preserve_file_identity(self):
        for old, extracted in [('0o664', '0o644'), ('0o664', '0o600'),
                               ('0o775', '0o755'), ('0o775', '0o700')]:
            with self.subTest(archived=old, extracted=extracted):
                assert_source_entries({'script': file_entry(old)},
                                      {'script': file_entry(extracted)})

    def test_changing_bytes_or_owner_executable_bit_fails(self):
        expected = {'script.sh': file_entry('0o775')}
        for observed in [file_entry('0o700', '1' * 64),
                         file_entry('0o600')]:
            with self.subTest(observed=observed):
                with self.assertRaises(AssertionError):
                    assert_source_entries(expected, {'script.sh': observed})

    def test_added_executable_grants_fail(self):
        expected = {'plain.txt': file_entry('0o664')}
        for actual_mode in ('0o744', '0o654', '0o645'):
            with self.subTest(actual_mode=actual_mode):
                with self.assertRaises(AssertionError):
                    assert_source_entries(expected, {'plain.txt': file_entry(actual_mode)})

    def test_symlink_target_and_file_type_are_checked(self):
        symlink = {'type': 'symlink', 'target': '../fixtures',
                   'sha256': '2' * 64, 'mode': '0o777'}
        assert_source_entries({'link': symlink}, {'link': dict(symlink)})
        for bad in [dict(symlink, target='../other'),
                    dict(symlink, sha256='3' * 64),
                    file_entry()]:
            with self.subTest(bad=bad):
                with self.assertRaises(AssertionError):
                    assert_source_entries({'link': symlink}, {'link': bad})

    def test_inventory_changes_fail(self):
        with self.assertRaises(AssertionError):
            assert_source_entries({'a': file_entry()}, {'b': file_entry()})

    def test_diff_normalization_preserves_content_and_execute_changes(self):
        previous = {'unchanged': file_entry('0o664'),
                    'changed': file_entry('0o664', 'a' * 64)}
        current = {'unchanged': file_entry('0o600'),
                   'changed': file_entry('0o644', 'b' * 64)}
        diff = make_diff(comparable_entries(previous), comparable_entries(current))
        self.assertEqual(set(diff['changed']), {'changed'})
        self.assertEqual(diff['unchanged_count'], 1)
        self.assertNotEqual(comparable_entry(file_entry('0o775')),
                            comparable_entry(file_entry('0o600')))


if __name__ == '__main__':
    unittest.main(verbosity=2)
