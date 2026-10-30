#!/usr/bin/env python3
"""Offline SQLite regression gate for directory-accounting SQL and v29/v30 DDL.

Runs directly against SQL literals in the Rust source with Python's bundled
SQLite. This is a supplemental gate, NOT a replacement for cargo tests: it
cannot prove Rust compilation or TUI/scanner concurrency behavior.

Usage: python3 validation/directory_accounting_sqlite_gate.py
"""
from __future__ import annotations

import pathlib
import re
import sqlite3
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
REPO = (ROOT / 'src/directory_accounting/repository.rs').read_text(encoding='utf-8')
DB = (ROOT / 'src/db.rs').read_text(encoding='utf-8')
MODULE = ROOT / 'src/directory_accounting'


def v29_schema() -> str:
    body = DB.split('fn migrate_v29(conn: &Connection)', 1)[1].split('fn migrate_v30(', 1)[0]
    match = re.search(r'conn\.execute_batch\(r#"(.*?)"#\)', body, flags=re.S)
    if not match:
        raise AssertionError('cannot locate v29 schema SQL in Rust source')
    return match.group(1)


def v30_ddl() -> tuple[list[str], str]:
    body = DB.split('fn migrate_v30(conn: &Connection)', 1)[1].split('fn migrate_v16(', 1)[0]
    additions = re.findall(r'"(ALTER TABLE directory_accounting_[^"]+)"', body, flags=re.S)
    index = re.search(r'"(CREATE INDEX IF NOT EXISTS directory_accounting_edges_generation_v30[^"]+)"',
                      body, flags=re.S)
    if len(additions) != 5 or index is None:
        raise AssertionError('unexpected v30 DDL shape; update independent gate')
    return additions, index.group(1)


def source_sql_literals() -> list[str]:
    # All SQL strings in the repository. The three affected statements are
    # multiline raw strings; other statements use ordinary Rust strings.
    # Refuse continued literals so Rust's whitespace folding cannot silently
    # reintroduce token fusion into this module.
    for path in MODULE.glob('*.rs'):
        if re.search(r'\\\r?\n', path.read_text(encoding='utf-8')):
            raise AssertionError(f'Rust line-continuation token fusion risk in {path}')
    literals = []
    pattern = r'r#"(.*?)"#|"((?:[^"\\]|\\.)*)"'
    for match in re.finditer(pattern, REPO, flags=re.S):
        literal = match.group(1) if match.group(1) is not None else match.group(2)
        # Ordinary Rust string literals here have no SQL-specific escapes.
        if re.match(r'^\s*(SELECT|INSERT|UPDATE|DELETE|REPLACE|WITH)\b', literal, flags=re.I) \
                and 'directory_accounting_' in literal:
            literals.append(literal)
    if len(literals) < 18:
        raise AssertionError(f'expected many repository SQL statements; found {len(literals)}')
    return literals


def prepare_schema() -> sqlite3.Connection:
    conn = sqlite3.connect(':memory:')
    conn.executescript(v29_schema())
    for sql in v30_ddl()[0]:
        conn.execute(sql)
    conn.execute(v30_ddl()[1])
    return conn


def migrate_v30_replay(conn: sqlite3.Connection) -> None:
    # Independently emulate the Rust column presence/mismatch guards using
    # schema information extracted from the *actual* source DDL.
    additions, index = v30_ddl()
    for sql in additions:
        match = re.fullmatch(r'ALTER TABLE (\w+) ADD COLUMN (\w+)\s+(.*)', sql, flags=re.S)
        if match is None:
            raise AssertionError(f'unsupported migration shape: {sql}')
        table, column, definition = match.groups()
        actual = {row[1]: row for row in conn.execute(f'PRAGMA table_info({table})')}
        if column in actual:
            expected_type = definition.split()[0].upper()
            not_null = 'NOT NULL' in definition.upper()
            default_match = re.search(r'DEFAULT\s+(\S+)', definition, re.I)
            expected_default = default_match.group(1) if default_match else None
            row = actual[column]
            if (row[2].upper(), bool(row[3]), row[4]) != (expected_type, not_null, expected_default):
                raise ValueError(f'incompatible existing column {table}.{column}')
        else:
            conn.execute(sql)
    conn.execute(index)


class DirectoryAccountingSqliteTests(unittest.TestCase):
    def test_all_repository_sql_statements_prepare(self):
        with prepare_schema() as conn:
            statements = source_sql_literals()
            for statement in statements:
                numbered = [int(n) for n in re.findall(r'\?(\d+)', statement)]
                parameters = (None,) * max(numbered, default=0)
                with self.subTest(sql=statement[:90]):
                    conn.execute('EXPLAIN ' + statement, parameters)
            print(f'Prepared {len(statements)} repository SQL literals')

    def test_completed_scan_upsert_lookup_update_and_rollback(self):
        with prepare_schema() as conn:
            statements = source_sql_literals()
            state = next(s for s in statements if s.startswith('INSERT INTO directory_accounting_state_v29('))
            totals = next(s for s in statements if s.startswith('INSERT INTO directory_accounting_totals_v29('))
            lookup = next(s for s in statements if s.startswith('SELECT t.files,t.descendant_folders,')
                          and 'LEFT JOIN directory_accounting_state_v29' in s)
            key = b'\x01/tmp/completed-album'
            conn.execute(state, (key, 1, 1, 0, 200))
            initial = (key, 1, 1, '2', '1', '2', '4096', '4096', '0', '0', 1,
                       100, 101, 1, 0, None, None, 200)
            conn.execute(totals, initial)
            row = conn.execute(lookup, (key, 1, 1)).fetchone()
            self.assertIsNotNone(row)
            self.assertEqual(row[0:7], ('2', '1', '2', '4096', '4096', '0', '0'))
            self.assertEqual(row[11:14], (0, 0, 0))
            replacement = list(initial)
            replacement[6] = '8192'
            replacement[13] = 2
            conn.execute(totals, replacement)
            self.assertEqual(conn.execute(lookup, (key, 1, 1)).fetchone()[3], '8192')
            conn.commit()  # The first completed total is durable before the failing batch.
            # A partially failed second batch must not corrupt the previous
            # committed total. Exercise SQLite's CHECK and transaction rollback.
            with self.assertRaises(sqlite3.IntegrityError):
                with conn:
                    conn.execute(state, (key, 1, 1, 0, 400))
                    bad = list(replacement)
                    bad[11], bad[12] = 500, 100  # finish < start
                    conn.execute(totals, bad)
            self.assertEqual(conn.execute(lookup, (key, 1, 1)).fetchone()[3], '8192')
            self.assertEqual(conn.execute(
                'SELECT last_access_utc_us FROM directory_accounting_state_v29 WHERE path_key=?',
                (key,),
            ).fetchone()[0], 200)

    def test_v29_to_v30_fresh_and_repeated_migration(self):
        conn = sqlite3.connect(':memory:')
        conn.executescript(v29_schema())
        conn.execute('UPDATE directory_accounting_generation_v29 SET next_generation=42')
        with conn:
            migrate_v30_replay(conn)
        conn.execute('UPDATE directory_accounting_generation_v29 SET mutation_sequence=7')
        with conn:
            migrate_v30_replay(conn)
        self.assertEqual(conn.execute(
            'SELECT next_generation,mutation_sequence FROM directory_accounting_generation_v29'
        ).fetchone(), (42, 7))
        self.assertEqual(conn.execute('SELECT COUNT(*) FROM sqlite_schema WHERE name=?',
                                     ('directory_accounting_edges_generation_v30',)).fetchone()[0], 1)

    def test_partially_applied_migration_repaired_without_losing_data(self):
        conn = sqlite3.connect(':memory:')
        conn.executescript(v29_schema())
        additions, _ = v30_ddl()
        conn.execute(additions[0])
        conn.execute(additions[3])
        conn.execute('UPDATE directory_accounting_generation_v29 SET mutation_sequence=19')
        with conn:
            migrate_v30_replay(conn)
            migrate_v30_replay(conn)
        self.assertEqual(conn.execute(
            'SELECT mutation_sequence FROM directory_accounting_generation_v29'
        ).fetchone()[0], 19)
        local = {row[1] for row in conn.execute('PRAGMA table_info(directory_accounting_local_v29)')}
        self.assertTrue({'direct_audio_logical_bytes', 'direct_other_entries', 'entry_digest', 'root_identity'} <= local)

    def test_incompatible_partial_migration_rolls_back(self):
        conn = sqlite3.connect(':memory:')
        conn.executescript(v29_schema())
        conn.execute('ALTER TABLE directory_accounting_local_v29 ADD COLUMN direct_other_entries BLOB')
        conn.commit()  # The incompatible partial column predates the retried transaction.
        conn.execute('BEGIN IMMEDIATE')  # DDL needs an explicit SQLite transaction.
        with self.assertRaisesRegex(ValueError, 'incompatible existing column'):
            with conn:
                migrate_v30_replay(conn)
        gen = {row[1] for row in conn.execute('PRAGMA table_info(directory_accounting_generation_v29)')}
        local = {row[1] for row in conn.execute('PRAGMA table_info(directory_accounting_local_v29)')}
        self.assertNotIn('mutation_sequence', gen)
        self.assertNotIn('direct_audio_logical_bytes', local)
        self.assertIn('direct_other_entries', local)


if __name__ == '__main__':
    unittest.main(verbosity=2)
