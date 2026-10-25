#!/usr/bin/env python3
"""Deterministic source-scope check; NOT a Rust correctness proof."""
from pathlib import Path
from argparse import ArgumentParser
from hashlib import sha256
import sys

EXPECTED = {
    'src/convert/pipeline/cue_flac_range.rs',
    'src/convert/pipeline/stages.rs',
    'src/convert/pipeline/tool.rs',
    'src/convert/pipeline/track_executor.rs',
    'src/convert/replaygain.rs',
}


def files(root):
    return {p.relative_to(root).as_posix(): sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file() and not p.is_symlink()
            and '__pycache__' not in p.parts and p.suffix != '.pyc'}


def check(base, candidate):
    a, b = files(base), files(candidate)
    changed_production = {p for p in a.keys() | b.keys()
                          if p.startswith('src/') and a.get(p) != b.get(p)}
    assert changed_production == EXPECTED, f'Unexpected production changes: {sorted(changed_production ^ EXPECTED)}'
    preserved = {p for p in a.keys() & b.keys() if not p.startswith('docs/cue_parallel_ip01/')
                 and not p.startswith('src/')}
    assert all(a[p] == b[p] for p in preserved), 'Unrelated source, fixture or workspace file was modified'
    assert a.keys() <= b.keys(), f'Unexpected removed source file(s): {sorted(a.keys() - b.keys())}'
    s = (candidate / 'src/convert/pipeline/stages.rs').read_text()
    tool = (candidate / 'src/convert/pipeline/tool.rs').read_text()
    meter = (candidate / 'src/convert/replaygain.rs').read_text()
    must = [('exact opt-in', 'std::env::var("TONEPOET_CUE_S3_PILOT").as_deref() == Ok("1")' in s),
            ('optional separate meter gate', 'cue_source_replaygain_eligible(source, req)' in s),
            ('existing worker', 'run_prepared_qualified_flac_ranges(' in s),
            ('existing source scan application', 'apply_cue_source_replaygain_if_eligible(' in s),
            ('retained post-output path', 'source_replaygain: None' in s),
            ('supervised file-pipe handoff', 'create_cloexec_pipe()' in tool and 'run_supervised_with_stdio(' in tool),
            ('default unsupported runner', 'fn supports_cue_source_range_observation(&self) -> bool {\n        false' in tool),
            ('native normalization', 'decode_pcm_bytes(&raw[..wanted], PcmMirrorEncoding::S32Le)' in meter),
            ('native reducer', 'reduce_cue_observations(' in s)]
    assert all(value for _,value in must), f'Static invariant(s) absent: {[name for name, ok in must if not ok]}'
    print(f'PASS: unchanged baseline files preserved, exactly {len(EXPECTED)} expected Rust production modules modified')
    print('PASS: source-verified default-OFF, separate observer, legacy fallback, existing reducer, supervisor and s32le invariants')
    print('NOT RUN: Rust compiler, tests, process lifetimes, actual native ReplayGain album and publisher')


if __name__ == '__main__':
    cli = ArgumentParser()
    cli.add_argument('--base', type=Path, required=True, help='unpacked SHA-verified accepted IP-05 source tree')
    cli.add_argument('--candidate', type=Path, default=Path(__file__).resolve().parents[3])
    args = cli.parse_args()
    try:
        check(args.base.resolve(), args.candidate.resolve())
    except Exception as ex:
        print(f'FAIL: {ex}', file=sys.stderr)
        sys.exit(1)
