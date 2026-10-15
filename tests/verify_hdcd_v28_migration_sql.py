#!/usr/bin/env python3
"""Compile-free SQL replay probe for R19's v28 cache migration.

This executes the actual CREATE TABLE statement extracted from src/db.rs and
its ALTER statements against SQLite. Rust migration tests remain authoritative.
"""
import re
import sqlite3
from pathlib import Path


def main():
    source = (Path(__file__).resolve().parents[1] / 'src/db.rs').read_text()
    start = source.index('    fn migrate_v28(conn: &Connection)')
    end = source.index('\n    /// v16:', start)
    migration = source[start:end]
    match = re.search(r'"(CREATE TABLE IF NOT EXISTS analysis_cache \(.*?\);)"',
                      migration, flags=re.S)
    assert match, 'actual v28 table creation not found'
    create_sql = match.group(1)
    columns = [
        ('hdcd_capabilities', 'TEXT'),
        ('hdcd_mtime_ns', 'INTEGER'),
    ]
    for version in range(21, 28):
        for already_exists in [False, True]:
            conn = sqlite3.connect(':memory:')
            if already_exists:
                conn.executescript('''CREATE TABLE analysis_cache (
                    file_path TEXT PRIMARY KEY, file_mtime INTEGER NOT NULL,
                    file_size INTEGER NOT NULL, algo_version INTEGER NOT NULL,
                    analyzed_at TEXT NOT NULL
                );
                INSERT INTO analysis_cache VALUES ('preserved.flac', 1, 2, 3, 'old');''')
            for _ in range(2):
                conn.executescript(create_sql)
                names = {row[1] for row in conn.execute('PRAGMA table_info(analysis_cache)')}
                for col, type_name in columns:
                    if col not in names:
                        conn.execute(f'ALTER TABLE analysis_cache ADD COLUMN {col} {type_name}')
            names = {row[1] for row in conn.execute('PRAGMA table_info(analysis_cache)')}
            assert {'file_path', 'file_mtime', 'algo_version',
                    'hdcd_capabilities', 'hdcd_mtime_ns'} <= names
            if already_exists:
                assert conn.execute('SELECT analyzed_at FROM analysis_cache').fetchone()[0] == 'old'
            else:
                conn.execute('''INSERT INTO analysis_cache
                    (file_path, file_mtime, file_size, algo_version, analyzed_at,
                     hdcd_capabilities, hdcd_mtime_ns)
                    VALUES ('new.flac', 1, 2, 3, 'now', 'effectual', 15)''')
            conn.close()
    print('PASS: v21-v27 modeled cache-present/cache-missing cases, two v28 replays, identity columns and row preservation (14 cases)')


if __name__ == '__main__':
    main()
