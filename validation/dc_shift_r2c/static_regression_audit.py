#!/usr/bin/env python3
"""Focused source-consistency audit; NOT a Rust compiler or test runner.

Run at repository root: python3 validation/dc_shift_r2c/static_regression_audit.py
"""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
checks = []
def record(label, ok):
    checks.append((label, bool(ok)))
    print(('PASS' if ok else 'FAIL') + ': ' + label)

def read(rel):
    return (ROOT / rel).read_text(encoding='utf-8')

# Conservative brace balancer for structural field inspection, with Rust
# line/block comments and ordinary/byte/raw strings ignored.
def block(text, pos):
    opening = text.index('{', pos)
    depth, i, n = 1, opening + 1, len(text)
    mode, quote = 'code', ''
    while i < n:
        c = text[i]
        if mode == 'code':
            if text.startswith('//', i): mode = 'line'; i += 2; continue
            if text.startswith('/*', i): mode = 'block'; i += 2; continue
            if c == '"': mode = 'string'; i += 1; continue
            if c == '{': depth += 1
            if c == '}':
                depth -= 1
                if depth == 0: return text[opening + 1:i]
        elif mode == 'line':
            if c == '\n': mode = 'code'
        elif mode == 'block':
            if text.startswith('*/', i): mode = 'code'; i += 2; continue
        elif mode == 'string':
            if c == '\\': i += 2; continue
            if c == '"': mode = 'code'
        i += 1
    raise ValueError('unmatched brace at byte ' + str(pos))

receipt = read('src/convert/pipeline/execution_evidence.rs')
for fn in ('decision_kind_name', 'decision_summary'):
    m = re.search(r'fn ' + fn + r'\(', receipt)
    record(fn + ': exhaustive match recognizes DC', bool(m) and
           'DecisionKind::DcCorrection' in block(receipt, m.start()) if m else False)

bridge = read('src/convert/pipeline/plan_bridge.rs')
stages = read('src/convert/pipeline/stages.rs')
core = read('src/convert/pipeline/dc_shift.rs')
plugin = read('tonepoet-pipeline/src/plugins.rs')
record('typed bridge checks observation input, owner, policy and scoped dependency',
       all(x in bridge for x in ('struct DcEffectDependency',
            'observation.subject != *input', 'observation_satisfies_dependency(observation, scoped)',
            'dc_dependencies: nodes.iter()', 'decision.observations.len() != 1')))
record('executor resolves values before applying and never calls monolithic Auto routine',
       stages.index('dc_shift::resolve(') < stages.index('dc_shift::apply_resolved(')
       and 'dc_shift::run_scoped(' not in stages)
record('native physical writer consumes resolved values and checks observed SHA-256',
       all(x in core for x in ('pub fn apply_resolved(', 'observed_hash',
            'DC input changed between measurement and correction',
            'sample - records[channel].subtraction')))
record('typed binding carried into evidence instead of reconstructed after execution',
       all(x in stages for x in ('dc_dependencies[index].clone()', 'typed_dependency: binding'))
       and 'binding.decision != *decision' in receipt)
record('original verified layout required for multichannel DC processing',
       'stereo_fold_requested || dc_shift_requested' in stages and
       'verified_stereo_fold_source_layout(' in stages and
       'multichannel DC carrier reached final encoding without verified source speaker roles' in bridge)
record('Float64 FFmpeg input binds verified speaker mask before input',
       plugin.index('"-channel_layout".into(), layout.to_string()') < plugin.index('"-i".into(),\n        input.into()')
       and 'bound raw Float64 speaker mask does not match input geometry' in plugin)
record('off route has no source-mask binding in the new carrier',
       'channel_layout: if dc_shift_requested {' in stages and
       'if let Some(layout) = context.request.raw_input_channel_layout.as_deref()' in plugin)

# Complete constructor coverage for new required Rust struct fields. Ignore
# patterns with rest (...) and `pub struct` definitions.
missing = []
count = 0
for file in ROOT.rglob('*.rs'):
    s = file.read_text(encoding='utf-8')
    for m in re.finditer(r'\bPlanRequest\s*\{', s):
        prefix = s[max(0, m.start()-30):m.start()]
        if re.search(r'\bstruct\s+$', prefix): continue
        b = block(s, m.end()-1)
        if not re.search(r'\binput_path\s*:', b): continue  # pattern/struct-like search
        count += 1
        if 'raw_input_channel_layout' not in b and not re.search(r'\.\.\s*\w+', b):
            missing.append(f'{file.relative_to(ROOT)}:{s.count(chr(10),0,m.start())+1}')
record(f'all {count} explicit PlanRequest constructors initialized', not missing)
for item in missing[:20]: print('  missing:', item)

for name in ('RegisteredEffectCarrier', 'PcmTruePeakCarrier', 'DsdTruePeakCarrier'):
    missing = []
    n = 0
    for file in ROOT.rglob('*.rs'):
        s = file.read_text(encoding='utf-8')
        for m in re.finditer(r'TrackSourceRef::' + name + r'\s*\{', s):
            b = block(s, m.end()-1)
            n += 1
            if 'channel_layout' not in b and '..' not in b:
                missing.append(f'{file.relative_to(ROOT)}:{s.count(chr(10),0,m.start())+1}')
    record(f'{name}: {n} matches/constructors explicitly bind new field or rest', not missing)
    for item in missing[:20]: print('  missing:', item)

assert all(ok for _, ok in checks), 'source-consistency audit FAILED'
print(f'Source-consistency assertions: {len(checks)} PASS (compilation NOT performed)')
