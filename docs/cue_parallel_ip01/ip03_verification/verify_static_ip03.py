#!/usr/bin/env python3
"""Verify the IP-03 cumulative source manifest and bounded production diff.

Unlike Rust compilation, this is deliberately just a content/static gate.
Run with --baseline to independently prove all original files were preserved.
"""
import argparse
import hashlib
import json
import stat
from pathlib import Path
import subprocess

from make_manifest import ROOT, BASE, EXCLUDED, make_diff, scan

NAMED_TESTS = [
    'cue_ip03_ffmpeg_pair_costs_two_and_refuses_before_spawn',
    'cue_ip03_one_ffmpeg_host_rejects_pair_but_admits_sox',
    'cue_ip03_partial_acquisition_never_holds_or_leaks_a_family',
    'cue_ip03_two_albums_share_cap_and_do_not_starve_ordinary_tools',
    'cue_ip03_failure_cancel_and_panic_release_all_permits',
    'cue_ip03_200_track_album_has_constant_inflight_and_no_leaks',
    'cue_ip03_invalid_family_and_closed_semaphore_fail_closed',
    'cue_ip03_supervised_early_encoder_exit_releases_pair',
    'cue_ip03_supervised_slow_consumer_remains_a_bounded_pipe',
    'cue_ip03_supervised_cancel_holds_pair_until_reaped',
]


def expect(condition, message):
    if not condition:
        raise AssertionError(message)


def comparable_entry(info):
    """Identity stable across normal tar extraction with umask 022 or 077.

    Only owner-execute is guaranteed to survive both masks. The archived
    group/other execute bits can be masked on extraction, and symlink mode is
    not meaningful on Linux. File bytes, entry type and symlink target remain
    full-strength integrity checks, independent of the extracting umask.
    """
    result = {key: value for key, value in info.items() if key != 'mode'}
    if info['type'] == 'file':
        result['owner_executable'] = bool(int(info['mode'], 8) & stat.S_IXUSR)
    return result


def comparable_entries(entries):
    return {path: comparable_entry(info) for path, info in entries.items()}


def assert_source_entries(archived, extracted):
    expect(set(archived) == set(extracted), 'source manifest paths differ from current source')
    for path, expected in archived.items():
        actual = extracted[path]
        expect(comparable_entry(expected) == comparable_entry(actual),
               f'source manifest identity differs: {path}')
        if expected['type'] == 'file':
            # Extraction can remove group/other execute bits under umask 077,
            # but cannot add new ones. Reject any unexpected executable grant.
            archived_exec = int(expected['mode'], 8) & 0o111
            extracted_exec = int(actual['mode'], 8) & 0o111
            expect((extracted_exec & ~archived_exec) == 0,
                   f'unexpected executable permissions: {path}')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path)
    args = parser.parse_args()
    evidence = ROOT / BASE
    manifest = json.loads((evidence / 'SOURCE_MANIFEST.json').read_text())
    entries = scan(ROOT)
    assert_source_entries(manifest['entries'], entries)
    expect(manifest['entry_count'] == len(manifest['entries']) == len(entries),
           'wrong manifest entry count')
    # Bind the *archived* manifest, including its original recorded modes,
    # rather than recalculating a different digest from umask-masked files.
    digest = hashlib.sha256(json.dumps(manifest['entries'], sort_keys=True,
                                       separators=(',', ':')).encode()).hexdigest()
    expect(manifest['source_tree_sha256'] == digest, 'source tree digest drift')
    expected = {k for k, v in entries.items() if v['type'] == 'file'}
    expected.add(str(BASE / 'SOURCE_MANIFEST.json'))
    listed = {}
    for line in (evidence / 'SHA256SUMS').read_text().splitlines():
        sha, path = line.split('  ', 1)
        expect(path not in listed, f'duplicate checksum {path}')
        listed[path] = sha
    expect(set(listed) == expected, 'incomplete/extra checksums')
    for path, sha in listed.items():
        expect(hashlib.sha256((ROOT / path).read_bytes()).hexdigest() == sha,
               f'checksum mismatch: {path}')
    probe = json.loads((evidence / 'native_pair_probe_results.json').read_text())
    expect(probe['outcome'] == 'PASS', 'native pair probe not PASS')
    expect(sum(len(x['range_results']) for x in probe['results']) == 16, 'not 16 checked ranges')
    for record in probe['results']:
        expect(record['resource_maxima']['total_native_children'] <= 2 * record['parallel_pairs'],
               'independent probe exceeded requested native process bound')
        for item in record['range_results']:
            expect(item['decoded_pcm_bytes'] == (item['end'] - item['start']) * 2 * 4,
                   'native range extent wrong')
    module = (ROOT / 'src/convert/pipeline/track_executor.rs').read_text()
    prod, marker, tests = module.partition('\n#[cfg(test)]\nmod tests {')
    expect(bool(marker), 'cannot locate Rust test boundary')
    expect(prod.count('run_cue_s3_pipeline_if_admitted(') == 1,
           'private S3 runner is invoked outside its own declaration')
    expect('runner.run_pipeline(producer, consumer, cancel).await' in prod,
           'S3 did not call supervised pair runner directly')
    expect('try_acquire_many_owned(count)' in prod, 'weighted nonblocking pair reservation absent')
    expect('cue_s3_in_flight: Arc<Semaphore>' in prod, 'shared S3 cap absent')
    for name in NAMED_TESTS:
        expect(tests.count(f'fn {name}(') == 1, f'missing or duplicated test: {name}')
    if args.baseline is not None:
        old = scan(args.baseline.resolve())
        old.pop(str(BASE / 'SOURCE_DIFF.json'), None)
        new = dict(entries)
        new.pop(str(BASE / 'SOURCE_DIFF.json'), None)
        # The original receipt records raw archived mode bits. Normalize only
        # for comparison: independently extracted source/baseline trees can
        # carry different umask masks without changing their source identities.
        diff = make_diff(comparable_entries(old), comparable_entries(new))
        receipt = json.loads((evidence / 'SOURCE_DIFF.json').read_text())
        normalized_receipt = {
            'added': comparable_entries(receipt['added']),
            'removed': comparable_entries(receipt['removed']),
            'changed': {
                path: {'old': comparable_entry(change['old']),
                       'new': comparable_entry(change['new'])}
                for path, change in receipt['changed'].items()
            },
            'unchanged_count': receipt['unchanged_count'],
        }
        expect(diff == normalized_receipt,
               'source diff receipt does not match baseline and delivered source')
        changed_prod = {p for p in diff['changed'] if p.startswith('src/') or p.startswith('crates/') or p.startswith('tonepoet-pipeline/')}
        added_prod = {p for p in diff['added'] if p.startswith('src/') or p.startswith('crates/') or p.startswith('tonepoet-pipeline/')}
        expect(changed_prod == {'src/convert/pipeline/track_executor.rs'},
               f'unexpected production edits: {changed_prod}')
        expect(not added_prod, f'unexpected production file additions: {added_prod}')
        expect(not diff['removed'], f'original input files removed: {list(diff["removed"])}')
    print('PASS: IP-03 file hashes/manifest, 10 named tests, private-only S3 adapter, 16 native ranges')
    print('PASS: exact production delta only track_executor.rs' if args.baseline else 'NOTE: baseline diff requires --baseline')
    print('NOT A RUST COMPILATION OR HOST SUPERVISOR VERIFICATION')


if __name__ == '__main__':
    main()
