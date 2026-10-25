#!/usr/bin/env python3
"""IP-05 source identity / basic wiring audit. Does NOT run Rust tests."""
import hashlib
import json
from pathlib import Path
import stat
from build_manifest_ip05 import BASE, EVIDENCE, EXCLUDED, INPUT_SHA, PRODUCTION, scan


def require(test, message):
    if not test:
        raise AssertionError(message)


def same_entry(expected, observed):
    if {k:v for k,v in expected.items() if k != 'mode'} != {k:v for k,v in observed.items() if k != 'mode'}:
        return False
    return bool(int(expected['mode'],8) & stat.S_IXUSR)==bool(int(observed['mode'],8)&stat.S_IXUSR)


def main():
    root=Path(__file__).resolve().parents[3]
    manifest=json.loads((root/EVIDENCE/'SOURCE_MANIFEST.json').read_text())
    require(manifest['packet']=='IP-05' and manifest['input_archive_sha256']==INPUT_SHA,
            'archive input provenance wrong')
    require(manifest['excluded_self_hashes']==sorted(EXCLUDED),'manifest self-exclusion wrong')
    entries=scan(root)
    require(not any('__pycache__' in Path(name).parts or name.endswith('.pyc')
                    for name in entries),
            'compiled Python caches must not be delivered in a source archive')
    expected=manifest['entries']
    require(set(entries)==set(expected),'file inventory changed')
    for name in entries:
        require(same_entry(expected[name],entries[name]), f'file changed: {name}')
    digest=hashlib.sha256(json.dumps(expected,sort_keys=True,separators=(',',':')).encode()).hexdigest()
    require(digest==manifest['source_tree_sha256'],'wrong source tree digest')
    require(len(expected)==manifest['entry_count'],'wrong source entry count')
    require((root/BASE/'SOURCE_MANIFEST.json').read_bytes()==(root/EVIDENCE/'SOURCE_MANIFEST.json').read_bytes(),
            'cumulative root manifest differs from IP05 manifest')
    global_list=(root/BASE/'SHA256SUMS').read_text()
    require(global_list==(root/EVIDENCE/'SHA256SUMS').read_text(),
            'cumulative SHA256SUMS differs from IP05')
    sums={}
    for line in global_list.splitlines():
        digest, path=line.split('  ',1)
        require(len(digest)==64 and path not in sums,'invalid checksum entry')
        sums[path]=digest
    regular={p for p,item in entries.items() if item['type']=='file'}
    regular.update({str(BASE/'SOURCE_MANIFEST.json'),str(EVIDENCE/'SOURCE_MANIFEST.json')})
    require(regular==set(sums),'SHA256SUMS does not cover all files (excluding self-hashes)')
    for path,digest in sums.items():
        require(hashlib.sha256((root/path).read_bytes()).hexdigest()==digest,f'bad file SHA256: {path}')
    require(not (root/'.git').exists() and not (root/'target').exists(),'build state/.git leaked into source')
    diff=json.loads((root/EVIDENCE/'SOURCE_DIFF.json').read_text())
    changed_src={n for n in diff['changed'] if n.startswith('src/')}
    require(changed_src==set(PRODUCTION),f'unexpected production edits: {sorted(changed_src)}')
    require(not any(n.startswith('src/') for n in diff['added']),'added production code module outside scope')
    stages=(root/PRODUCTION[0]).read_text()
    executor=(root/PRODUCTION[1]).read_text()
    require(stages.count('fn cue_s3_pilot_requested(')==1,'missing pilot switch')
    require('std::env::var("TONEPOET_CUE_S3_PILOT").as_deref() == Ok("1")' in stages,
            'pilot may be enabled without exact opt-in')
    require('|| req.settings.pcm_true_peak.is_active()' in stages,
            'active PCM gain gate absent')
    require('|| req.settings.dither_type != tonepoet_pipeline::DitherType::None' in stages,
            'explicit/configured dither admission gate absent')
    require('if !cfg!(unix)' in stages and 'cue_stream_groups(tracks).len() != 1' in stages,
            'non-Unix/physical-FILE refusal missing')
    require('source.tracks.len() != source.album_metadata.total_tracks as usize' in stages,
            'missing full authored CUE count gate for user-selected subsets')
    require(stages.index('source.tracks.len() != source.album_metadata.total_tracks as usize')<
            stages.index('prepare_qualified_flac_ranges_for_group(tracks, req, convert_root)'),
            'incomplete CUE admission must decline before native source/planner preflight')
    require('prepare_qualified_flac_ranges_for_group(tracks, req, convert_root)' in stages,
            'all-member typed planner gate missing')
    require('run_prepared_qualified_flac_ranges(' in stages
            and 'order_qualified_cue_results(&mut outputs)' in stages,
            'S3 aggregation missing')
    require('source_replaygain: None' in stages and
            '"s3_uses_native_post_output_replaygain"' in stages,
            'post-output ReplayGain cannot be verified from source')
    require(stages.index('if let Some(batch) = try_encode_cue_s3_pilot(')<
            stages.index('let source_rg = cue_source_replaygain_eligible(&source, &req)'),
            'pilot selection not before S1 source-pass observer')
    require('run_cue_s3_pipeline_after_pilot_admission(' in executor and
            'run_cue_s3_pipeline_if_admitted(' in executor and
            '.acquire_many_owned(ffmpeg_cost)' in executor,
            'weighted native shared permits not retained')
    require('runner.run_pipeline_with_exact_pcm_extent(producer, consumer, extent, cancel).await' in executor,
            'existing exact native transport not reused')
    require('fs::remove_file(&prepared.staged_path)' in stages,
            'S3 failed stage cleanup missing')
    for name in (
        'cue_ip05_pilot_opt_out_and_nonidentity_rejections_are_whole_album',
        'cue_ip05_pilot_incomplete_materialized_album_falls_back_before_spawn',
        'cue_ip05_pilot_selected_real_three_tracks_16_24_off_and_both',
        'cue_ip05_pilot_native_twelve_track_rg_mode_matrix',
        'cue_ip05_pilot_waits_for_shared_permit_and_reaps_on_cancel',
    ):
        require((stages+executor).count('fn '+name+'(')==1,f'IP05 test not defined exactly once: {name}')
    for name in ('src/convert/processor.rs','src/convert/pipeline/transactional_state.rs'):
        require(name not in changed_src,'processor or publisher must remain unchanged')
    fixture=json.loads((root/EVIDENCE/'native_full_results.json').read_text())
    require(fixture['outcome']=='PASS','independent full native probe not PASSED')
    require([(c['bit_depth'],c['tracks']) for c in fixture['cases']]==[(16,12),(24,30)],
            'native full probe coverage wrong')
    require(all(c['all_pcm_matched'] and c['max_native_children']<=4 for c in fixture['cases']),
            'native PCM mismatch or child cap violated')
    print(f'PASS: IP05 source manifest {len(entries)} entries / {len(sums)} SHA256 checks')
    print('PASS: 2 scoped Rust modules; prelaunch authored CUE count gate, default-OFF switch, true PCM gain exclusion')
    print('PASS: no Python bytecode caches in delivered source inventory')
    print('PASS: native verified pipe/weighted shared permits, downstream RG/publisher wiring, 5 named tests present')
    print('PASS: independent native full reference fixture recorded (not a Rust runner result)')
    print('NOT RUN: rustc, cargo fmt/test, actual TonePoet album/post-output/visible publication')


if __name__=='__main__':
    main()
