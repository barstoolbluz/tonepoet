#!/usr/bin/env python3
"""Narrow, toolchain-independent source audit for the R2E terminal-admission correction.

No inference about Rust compilation or TonePoet end-to-end execution is made.
"""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
semantic = (ROOT / 'tonepoet-pipeline/src/semantic_plan.rs').read_text()
bridge = (ROOT / 'src/convert/pipeline/plan_bridge.rs').read_text()
dc = (ROOT / 'src/convert/pipeline/dc_shift.rs').read_text()
info = (ROOT / 'src/tui/keybindings.rs').read_text()

checks = []

def check(label, condition):
    assert condition, f'FAIL: {label}'
    checks.append(label)
    print('PASS:', label)

segment = semantic.split('let needs_multichannel_dc_raw_layout =', 1)[1].split('for candidate in &mut candidates', 1)[0]
check('Custom DSD not excluded from multichannel DC terminal eligibility',
      '!request.source.is_dsd()' not in segment and '!intent.reference_delivery' in segment)
check('Requires gain, >2 actual typed channels and registered shift',
      'intent.gain_policy.is_true_peak()' in segment and '*channels > 2' in segment
      and 'RegisteredUnaryEffect::DcShiftAuto' in segment
      and 'RegisteredUnaryEffect::DcShiftManual' in segment)
check('SSRC exception uses already chosen ResamplePcm candidate',
      'selected_ssrc_resampler' in segment
      and 'TypedPlanNode::Operation' in segment
      and 'PlanOperation::ResamplePcm' in segment
      and 'candidates.get(*selected_candidate)' in segment
      and 'ToolIdentifier::Ssrc' in segment)
check('SSRC exception does not displace Auto/FFmpeg or admit explicit SoX',
      'PreferredTool::Auto | PreferredTool::Ffmpeg' in segment
      and '&& !selected_ssrc_resampler' in segment)
check('FFmpeg layout capability obligation still applied to candidates and requirements',
      semantic.count('MULTICHANNEL_DC_RAW_LAYOUT_CONTRACT.to_owned()') >= 2
      and 'candidate.tool.as_ref() == Some(&ToolIdentifier::Ffmpeg)' in semantic)
check('Certified terminal selected with obligation before proof is charged',
      semantic.index('let needs_multichannel_dc_raw_layout =')
      < semantic.index('let selected_candidate = match selected_candidate_index(',
                         semantic.index('let needs_multichannel_dc_raw_layout =')))
check('Reference DSD still rejects registered effects',
      'if intent.reference_delivery && !ordered_effects.is_empty()' in semantic)
check('Existing final raw speaker-mask check stays unchanged',
      'multichannel DC correction requires FFmpeg terminal encoding to preserve verified speaker roles' in bridge
      and 'mask.count_ones() == source.channels.unwrap_or(0) as u32' in bridge)
check('DSD true-peak carrier still carries verified layout to raw encoder',
      'TrackSourceRef::DsdTruePeakCarrier { channel_layout, .. }' in bridge)
check('Native DC has certified finite Float64 output and overload-preservation obligation',
      'let mut native_contract = plain.clone();' in semantic
      and 'native_contract.representation.emitted_processing_domain = Some(ProcessingDomain::Binary64);' in semantic
      and 'native_contract.representation.emitted_value_domain = Some(ValueDomain::FiniteFloating);' in semantic
      and '"preserve_floating_overload_before_true_peak".to_owned(),' in semantic)
check('Native DC worker still subtracts in f64 and refuses nonfinite',
      'let changed = sample - records[channel].subtraction;' in dc
      and 'if !changed.is_finite()' in dc
      and 'slot.copy_from_slice(&changed.to_le_bytes());' in dc)
check('Other registered effects do not automatically acquire native DC contract',
      'contract: native_contract' in semantic and 'contract: plain.clone()' in semantic)
check('Native Float64 claim restricted to DC effect output states',
      semantic.count('RegisteredUnaryEffect::DcShiftAuto | RegisteredUnaryEffect::DcShiftManual { .. }') >= 3
      and 'ValueDomain::Pending(format!(' in semantic)
check('New DSD track/album and SSRC positive/negative Rust tests present',
      'fn custom_dsd_multichannel_dc_true_peak_charges_layout_preserving_terminal_for_both_scopes()' in semantic
      and 'fn custom_dsd_multichannel_dc_refuses_unqualified_terminal_before_certification()' in semantic
      and 'fn forced_qualified_ssrc_resampler_with_multichannel_dc_charges_ffmpeg_terminal()' in semantic
      and 'fn native_float64_dc_effect_explicitly_preserves_finite_overload_for_protected_ssrc()' in semantic)
check('New DSD final mask-to-argv bridge test present',
      'fn custom_dsd_multichannel_dc_charged_terminal_binds_verified_raw_speaker_mask()' in bridge
      and 'arg[0] == "-channel_layout" && arg[1] == "0x3f"' in bridge)
check('Existing PCM gain Notice registration remains unchanged by feature scope',
      'PCM gain' in info and 'PcmGainInfo' in info)
print(f'SOURCE AUDIT: {len(checks)}/{len(checks)} PASS. Rust compilation NOT executed.')
