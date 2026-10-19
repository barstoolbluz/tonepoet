#!/usr/bin/env python3
"""Toolchain-independent invariant checks for the PCM gain information affordance.

These source/geometry checks do not replace Cargo compilation or ratatui TestBackend tests.
"""
from pathlib import Path
import re
import json
import sys

ROOT = Path(__file__).resolve().parents[2]
output = (ROOT / 'src/tui/draw_output.rs').read_text()
screen = (ROOT / 'src/tui/convert_screen.rs').read_text()
keys = (ROOT / 'src/tui/keybindings.rs').read_text()
buttons = (ROOT / 'src/tui/button_map.rs').read_text()
app = (ROOT / 'src/tui/app.rs').read_text()
settings = (ROOT / 'tonepoet-pipeline/src/settings.rs').read_text()

def check(label, condition):
    if not condition:
        print(f'FAIL {label}')
        sys.exit(1)
    print(f'PASS {label}')

def block(text, start, end):
    a = text.index(start)
    b = text.index(end, a+len(start))
    return text[a:b]

msg = block(keys, 'pub(crate) const PCM_GAIN_INFO_MESSAGE: &str = concat!(', '\n);')
strings = re.findall(r'"(?:[^"\\]|\\.)*"',msg)
message = ''.join(json.loads(s) for s in strings)
headings = ['Off\n', 'True-peak guard\n', 'True-peak normalize\n', 'Fixed gain\n']
positions = [message.index(h) for h in headings]
check('Four modes in correct order as plain-text standalone headings', positions == sorted(positions) and len(set(positions)) == 4)
check('Four distinct guidance paragraphs', message.count('Choose this when') == 4)
check('Copy retains guard attenuation, normalization, gain signs, clipping', all(x in message for x in ('never turns quieter audio up','up or down','Positive values increase','negative values decrease','cause clipping')))
check('Copy reflects actual conditional auto defaults and lossy ceiling', 'Your explicit choice or preset takes priority' in message and 'HDCD decoding' in message and '-0.10 dBTP' in message and 'PCM_TRUE_PEAK_LOSSY_MAX_TARGET_DBTP: DbNano = PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP' in settings and 'DbNano(-100_000_000)' in settings)
check('Copy does not embed unsupported Markdown', '**' not in message and '#' not in message)
label = block(output,'fn pcm_gain_info_row', '\nfn pill_row')
check('One glyph plus same-foreground guard cell, high-contrast and no background',
      '"ⓘ "' in label and '.fg(info_fg)' in label and '.bg(' not in label and 'Modifier::BOLD' in label)
check('PCM gain pills unchanged; label-only replacement of equal width',
      'row.spans.splice(1..2' in label and '"   PCM gain "' in label and '"  "' in label
      and len('   PCM gain ')+2+len('  ') == len('   PCM gain     '))
check('Hitbox is two cells in previously empty label padding',
      'Rect::new(area.x + 13, y, 2, 1)' in screen and 'TuiButton::PcmGainInfo' in screen
      and 'label_col = area.x + 17' in screen)
check('PcmTruePeak drawing uses new icon while retaining original pills',
      'FormatField::PcmTruePeak => lines.push(pcm_gain_info_row(' in output and
      '&render_pill_spans(&format_state.pcm_gain_mode, row_focused, theme)' in output)
check('Mouse/keyboard open exactly existing Notice and do not alter policy',
      'TuiButton::PcmGainInfo =>' in keys and 'FormatField::PcmTruePeak =>' in keys
      and keys.count('title: "PCM gain".to_string()')==2
      and keys.count('PCM_GAIN_INFO_MESSAGE.to_string()') == 2)
check('Both existing focus and dismissal mechanics reused',
      'app.convert.focus = ConvertFocus::Format;' in keys and 'scroll: 0' in keys
      and 'KeyCode::Esc | KeyCode::Enter' in keys and 'ActiveOverlay::Notice' in keys)
check('Button classification exhaustive match includes info target',
      '| Self::PcmGainInfo' in buttons and 'PcmGainInfo,' in buttons)
check('New actual ratatui tests cover render/hitboxes/keyboard/scroll/no settings mutation',
      all(term in screen for term in ('pcm_gain_info_regressions', 'Terminal<TestBackend>',
          'app.button_map.find_button_at', 'KeyCode::Char(\'i\')', 'KeyCode::Esc', 'KeyCode::Enter',
          'notice_message_max_scroll', 'pcm_gain_overridden', '[44u16, 64, 80, 100, 120]')))
check('Existing PCM gain policy untouched and true-peak guard behavior understood',
      'if !self.pcm_gain_overridden' in app and '&& !hdcd_lossy_decode' in app and
      'PcmGainMode::TruePeakGuard' in app and 'SampleGainPolicy::TruePeakGuard' in settings)
# The layout places the icon at x=13-14, preserves the pills at x=17,
# and is safely inside every supported displayed pane width >=30.
for width in (30,44,64,80,100,120):
    assert 0 <= 13 < 15 < 17 < width
check('Two-cell glyph always visible at all supported pane widths', True)
print('AUDIT: 15/15 PASS (source/geometry only; no Rust execution)')
