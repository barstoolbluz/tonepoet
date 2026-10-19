#!/usr/bin/env python3
"""Compile-free, deliberately limited audit of the PCM gain/DC R3 correction.

Not a Rust compiler, test runner, terminal renderer, or source-lock requalifier.
Run from any cwd: python3 validation/pcm_gain_dc_20261019/static_review.py
"""
from __future__ import annotations

from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
draw = (ROOT / 'src/tui/draw_output.rs').read_text()
registration = (ROOT / 'src/tui/convert_screen.rs').read_text()
plan = (ROOT / 'tonepoet-pipeline/src/semantic_plan.rs').read_text()

checks = 0


def check(ok: bool, why: str) -> None:
    global checks
    if not ok:
        raise AssertionError(why)
    checks += 1
    print(f'PASS {checks:02d}: {why}')


# Extract the actual branch thresholds and labels, rather than duplicating the
# implementation's table of widths in the verifier.
source = draw.split('pub(super) fn pcm_gain_pill_labels(width: usize)', 1)[1].split('\nfn pcm_gain_pill_spans(', 1)[0]
tiers = re.findall(r'(?:if|else if) width >= (\d+)\s*\{\s*\[([^\]]+)\]', source)
fallback = re.search(r'else\s*\{\s*\[([^\]]+)\]', source)
assert fallback

def parse_labels(array_src: str) -> list[str]:
    return re.findall(r'"([^"]+)"', array_src)

parsed_tiers = [(int(k), parse_labels(labels)) for k, labels in tiers]
base = parse_labels(fallback.group(1))
check(len(parsed_tiers) == 3 and len(base) == 4 and all(len(labels) == 4 for _,labels in parsed_tiers), 'four options in each responsive width tier')
check([t[0] for t in parsed_tiers] == sorted([t[0] for t in parsed_tiers], reverse=True), 'responsive thresholds descending')
check(parsed_tiers[0][1] == ['off', 'true-peak guard', 'true-peak normalize', 'fixed gain'], 'full labels retained on wide panes')

def labels_at(width: int) -> list[str]:
    for threshold, labels in parsed_tiers:
        if width >= threshold:
            return labels
    return base

# Draw code uses pill_row 16-cell label + left border, two-cell gaps,
# two suffix spaces, plus U+24D8 and one same-style guard cell.
check('border_color, w, "PCM gain   ", "ⓘ"' in draw and 'row.spans.splice(1..2' not in draw, 'information rendered as suffix; label is not rewritten')
check('spans[index * 2].content = format!(" {label} ").into();' in draw, 'same common pill painter styling retained')
check('pcm_gain_pill_labels(area.width as usize)' in registration and 'pcm_gain_pill_labels(width: usize)' in draw, 'renderer and hitboxes share one responsive source')
check('Rect::new(x, y, 2, 1)' in registration and 'TuiButton::PcmGainInfo' in registration, 'two-cell glyph/guard hitbox registered after pills')

for width in range(42, 200):
    labels = labels_at(width)
    x = 17
    pill_rects: list[tuple[int, int]] = []
    for label in labels:
        length = len(label) + 2
        pill_rects.append((x, x + length))
        x += length + 2
    glyph_start = x
    assert pill_rects[0][0] == 17
    assert all(right + 2 <= left2 for (_,right), (left2,_) in zip(pill_rects, pill_rects[1:]))
    assert glyph_start == pill_rects[-1][1] + 2
    assert glyph_start + 2 <= width - 1, (width, labels, glyph_start)
check(True, 'all four pills and end-of-row glyph fit and do not overlap at every pane width 42..199')
check(base[0] == 'off' and base[1:] == ['G','N','F'], 'minimum-width labels map unambiguously to the four stable pill positions')
check('assert_eq!(glyph_x, last_pill.x + last_pill.width + 2' in registration, 'click regression checks true end-of-pills location')
check('glyph_x + 1' in registration and 'TuiButton::PcmGainInfo' in registration, 'click regression exercises two-cell icon area')

check('MULTICHANNEL_DC_RAW_LAYOUT_CONTRACT' in plan and
      'matches!(&input_state.channels, Fact::Known(channels) if *channels > 2)' in plan and
      'RegisteredUnaryEffect::DcShiftManual' in plan, 'multichannel requirement uses the post-effect terminal input geometry')
check('assert!(candidates.iter().all(|candidate|' in plan and
      'must not acquire a multichannel DC obligation' in plan, 'legacy regression checks all candidates, not only selected backend')
check('request.settings.preferred_tool = PreferredTool::Sox;' in plan.split('fn multichannel_dc_constraint_does_not_perturb_stereo_fold_or_legacy_paths()',1)[1].split('\n    #[test]',1)[0], 'post-fold regression checks independent SoX admission')
check('Rect::new(0, 0, width, short_height)' in registration and
      'assert!(max_scroll > 0, "short terminals must scroll")' in registration and
      'assert!(final_page.contains("This setting does not fix hum")' in registration, 'DC notice checks full-content access on a short terminal')
check('if tall_max_scroll == 0 {' in registration and 'a notice that fits must show its ending without scrolling' in registration, 'DC notice does not force scrolling when the text fits and allows future longer copy')

print(f'Completed {checks} compile-free source/geometry assertions; Rust qualification still required.')
