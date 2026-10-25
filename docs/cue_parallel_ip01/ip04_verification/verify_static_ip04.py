#!/usr/bin/env python3
"""IP-04 static source/receipt/native-evidence audit. NOT a Rust test gate."""
import argparse
import hashlib
import json
import stat
from pathlib import Path

from make_manifest import ROOT, BASE, EXCLUDED, INPUT_SHA, scan, make_diff

RUST_CHANGED = {
    'src/convert/pipeline/cue_flac_range.rs',
    'src/convert/pipeline/tool.rs',
    'src/convert/pipeline/track_executor.rs',
    'src/convert/pipeline/stages.rs',
}
RUST_TESTS = (
    'cue_ip04_supervised_exact_pipe_rejects_short_and_surplus_and_proves_success',
    'cue_ip04_supervised_exact_pipe_reaps_both_on_consumer_error_and_cancel',
    'cue_ip04_supervised_exact_pipe_preserves_tool_failures_and_stderr_load',
    'cue_ip04_supervised_exact_pipe_retains_peer_when_primary_has_no_command',
    'cue_ip04_private_real_three_tracks_16_and_24_exact_pcm',
    'cue_ip04_private_real_twelve_and_thirty_tracks_bounded',
    'cue_ip04_private_reorders_three_two_one_completion_by_source_identity',
    'cue_ip04_private_fake_runner_three_one_two_preserves_attribution_and_gaps',
    'cue_ip04_private_preflight_refuses_unqualified_source_without_spawning',
)

def require(condition, message):
    if not condition:
        raise AssertionError(message)

def comparable(info):
    """Modes under normal extraction with umask 022/077: only owner exec is stable."""
    out = {key:value for key,value in info.items() if key!='mode'}
    if info['type']=='file':
        out['owner_executable'] = bool(int(info['mode'],8) & stat.S_IXUSR)
    return out

def compare_sources(expected, actual):
    require(set(expected) == set(actual), 'source path inventory mismatch')
    for name, left in expected.items():
        right=actual[name]
        require(comparable(left)==comparable(right), f'source file identity mismatch: {name}')
        if left['type']=='file':
            before, after=(int(entry['mode'],8)&0o111 for entry in (left,right))
            require((after&~before)==0, f'unexpected executable-bit grant: {name}')

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path)
    args=parser.parse_args()
    here=ROOT/BASE
    manifest=json.loads((here/'SOURCE_MANIFEST.json').read_text())
    require(manifest['input_archive_sha256']==INPUT_SHA, 'wrong accepted input SHA')
    entries=scan(ROOT)
    compare_sources(manifest['entries'],entries)
    require(manifest['entry_count']==len(entries), 'manifest entry count invalid')
    canonical=json.dumps(manifest['entries'], sort_keys=True, separators=(',',':')).encode()
    require(hashlib.sha256(canonical).hexdigest()==manifest['source_tree_sha256'],
            'manifest tree digest mismatch')
    expected_regular={p for p,i in entries.items() if i['type']=='file'}
    expected_regular.add(str(BASE/'SOURCE_MANIFEST.json'))
    listed={}
    for line in (here/'SHA256SUMS').read_text().splitlines():
        digest,rel=line.split('  ',1)
        require(rel not in listed, f'checksum duplicate {rel}')
        listed[rel]=digest
    require(set(listed)==expected_regular,'SHA256SUMS coverage mismatch')
    for name, digest in listed.items():
        require(hashlib.sha256((ROOT/name).read_bytes()).hexdigest()==digest,
                f'source content mismatch: {name}')
    prod=(ROOT/'src/convert/pipeline').resolve()
    tool=(prod/'tool.rs').read_text()
    stages=(prod/'stages.rs').read_text()
    executor=(prod/'track_executor.rs').read_text()
    range_reader=(prod/'cue_flac_range.rs').read_text()
    require(tool.count('async fn run_pipeline_with_exact_pcm_extent(')==2,
            'exact transport must appear only in trait and real implementation')
    require('extent.observe_bytes(count)' in tool and 'extent.finish()' in tool,
            'exact verifier does not enclose supervised byte stream')
    require('self.run_supervised_with_stdio(' in tool and 'let mut bytes = [0_u8; 64 * 1024];' in tool,
            'no existing supervisor or fixed 64-KiB transport')
    require('async fn run_cue_s3_pipeline_if_admitted(' in executor and
            'runner.run_pipeline_with_exact_pcm_extent(' in executor and
            'try_acquire_cue_s3_pipeline_permits' in executor,
            'IP-03 weighted admission bypassed')
    require('pub(crate) fn for_expected_bytes(' in range_reader,
            'IP-02 verifier constructor missing')
    require(stages.count('run_qualified_flac_ranges_for_group(')==3
            and stages[:stages.index('mod chunk_2_1_3_postprocessing_gate_and_phase_tests {')].count(
                'run_qualified_flac_ranges_for_group(')==1,
            'private group worker missing or production dispatcher activated')
    require('prepare_qualified_flac_ranges_for_group(' in stages and
            'order_qualified_cue_results(&mut outputs)' in stages and
            'Some(prepared.decoder.extent_verifier())' in stages,
            'IP-04 private preflight/aggregation/verified pipe not wired')
    require('if !output.ok {' in stages and 'fs::remove_file(&prepared.staged_path)' in stages,
            'private cleanup on failure absent')
    for name in RUST_TESTS:
        require((tool+stages).count('fn '+name+'(')==1,
                f'IP-04 named Rust test missing or duplicated: {name}')
    require('u16::try_from(source_bits).unwrap(), image_samples' in stages,
            'D-01 FLAC regression source-depth assertion must be type-aligned')
    require('VerifiedPipeFault::Consumer' in tool and 'verified_pipe_other_commands(' in tool
            and 'Duration::from_millis(750)' in tool,
            'D-02 causal consumer exit/peer-command preservation absent')
    require('finish_gates[index].acquire()' in stages and 'assert!(!attributed(0, &swapped_record, first)' in stages,
            'D-03 async attributed fake runner or negative control missing')
    require('cue_s3_max_in_flight().min(2)' in stages,
            'IP-04 concurrency bound missing')
    for filename, expected16, expected24 in (
        ('native_quick_results.json',3,3),
        ('native_full_results.json',12,30),
    ):
        data=json.loads((here/filename).read_text())
        require(data['outcome']=='PASS', f'{filename}: native independent test not PASS')
        require(sorted((case['bit_depth'], len(case['ranges'])) for case in data['cases']) ==
                [(16, expected16),(24, expected24)],f'{filename}: wrong native coverage')
        for case in data['cases']:
            require(case['all_pcm_matched'],f'{filename}: PCM mismatch')
            require(case['max_native_children']<=4, f'{filename}: child cap exceeded')
            for item in case['ranges']:
                require(item['decoded_bytes']==8*(item['end']-item['start']),
                        f'{filename}: exact byte count mismatch')
                require(len(item['pcm_sha256'])==64 and len(item['artifact_sha256'])==64,
                        f'{filename}: missing source or artifact hash')
    gap=json.loads((here/'native_gap_reference_results.json').read_text())
    require(gap['outcome']=='PASS' and gap['sample_count']==40013
            and gap['bit_depth']==16 and gap['gaps']==[31,28,54]
            and len(gap['ranges'])==3,
            'D-03 independent gapped PCM reference receipt missing or invalid')
    require([(r['start'],r['end']) for r in gap['ranges']] ==
            [(31,17111),(17139,35137),(35191,40013)],
            'D-03 reference cut geometry drift')
    for item in gap['ranges']:
        require(item['bytes']==8*(item['end']-item['start'])
                and len(item['sha256'])==64,
                'D-03 native exact PCM extent/digest invalid')
    receipt=json.loads((here/'SOURCE_DIFF.json').read_text())
    require(not receipt['removed'], 'source input files have been removed')
    changed_rust={p for p in receipt['changed'] if p.startswith('src/')}
    removed_rust={p for p in receipt['removed'] if p.startswith('src/')}
    added_rust={p for p in receipt['added'] if p.startswith('src/')}
    require(changed_rust==RUST_CHANGED and not removed_rust and not added_rust,
            'non-IP04 source files changed or expected changes missing')
    if args.baseline:
        before=scan(args.baseline.resolve())
        old={p:comparable(e) for p,e in before.items() if p != str(BASE/'SOURCE_DIFF.json')}
        new={p:comparable(e) for p,e in entries.items() if p != str(BASE/'SOURCE_DIFF.json')}
        expected=make_diff(old,new)
        actual={
            'added':{p:comparable(e) for p,e in receipt['added'].items()},
            'removed':{p:comparable(e) for p,e in receipt['removed'].items()},
            'changed':{p:{s:comparable(v) for s,v in change.items()}
                       for p,change in receipt['changed'].items()},
            'unchanged_count':receipt['unchanged_count'],
        }
        require(actual==expected,'original-to-IP04 source delta receipt mismatch')
    print(f"PASS IP04 static identity: {len(entries)} entries, {len(listed)} files")
    print(f"PASS IP04 source scope: {len(changed_rust)} Rust modules, {len(RUST_TESTS)} named tests present (NOT RUN)")
    print('PASS IP04 native independent evidence: 6 quick, 42 full and 3 gap PCM range checks')
    print('PASS source-diff receipt; host Rust compile/tests remain NOT RUN')

if __name__=='__main__':
    main()
