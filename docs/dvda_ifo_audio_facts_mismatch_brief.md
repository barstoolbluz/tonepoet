# DVD-Audio IFO Audio-Facts Mismatch — Track Type Bit 3 Convention

## Problem

DVD-Audio discs with multichannel + stereo presentations in the same
ATS fail extraction on the stereo group because the materializer
stamps IFO-derived audio expectations (channel count, sample rate,
bit depth) from the multichannel format entry onto the stereo tracks.

The LPCM decoder then rejects the packets:
```
DVD-Audio LPCM packet/header mismatch: IFO channel layout code 20:
group1=[L,R,Ls,Rs], group2=[C,LFE] differs from LPCM packet layout
code 1: group1=[L,R], group2=[-]
```

The MLP inspector similarly rejects:
```
DVD-Audio MLP channel count mismatch: IFO expected 6, MLP major-sync
reports 2
```

## Verified pattern across disc corpus

Track type byte low 4 bits consistently distinguish multichannel from
stereo titles, with bit 3 as the differentiator:

```
Disc                    | Fmts | Titles | t1 low4 | t2 low4 | t3 low4
Naked (Talking Heads)   | 2    | 2      | [0]     | [8]     |
Remain in Light         | 2    | 2      | [0]     | [8]     |
Machine Head            | 2    | 3      | [0]     | [8]     | [0]
Document (REM)          | 1    | 3      | [0]     | [0]     | [8]
L.A. Woman (Doors)      | 2    | 3      | [0]     | [8]     | [0]
Around the Sun (REM)    | 2    | 2      | [0]     | [8]     |
```

The current `track_type_low_bits_candidate` uses `& 0x07` (3 bits),
which maps ALL titles to format index 0. Bit 3 (`0x08`) is lost.

But `& 0x0F` doesn't help either — value 8 doesn't map to any valid
format index (0–7). Bit 3 is NOT a format index selector. It's a
flag that means "this title uses a different audio format than the
ATS's primary format entries describe."

## Affected discs

### AOTT-referenced groups (not orphans)

REM Document: 3 AOTT audio groups, 1 audio format entry (5.1/96kHz).
Group 3 is LPCM stereo at 192kHz — completely different format.
The IFO has NO format entry describing this group's actual format.

### Orphan PGC titles

Talking Heads Naked, Remain in Light, Fear of Music, etc.: stereo
title is an orphan (not in AOTT). Our `OrphanPgcTitle` fix already
clears audio facts for these using `unknown_audio_facts()`. These
discs already work.

### The gap

Non-orphan groups where the IFO format entry doesn't match the actual
stream. REM Document is the known case. The `OrphanPgcTitle` fix
doesn't apply because group 3 IS in the AOTT.

## Root cause

`audio_facts_for_title_chapter()` resolves audio format by matching
`chapter.track_type_low_bits_candidate` (= `track_type & 0x07`)
against `audio_format[i].format_index`. When there's only one present
format entry, it's used for all tracks regardless of track type.
When there are multiple entries, it tries to match — but when bit 3
is set and no format[8] exists, it falls through to unknown.

The problem manifests in two scenarios:

1. **Single format entry** (Document): `present.len() == 1`, so the
   only entry (5.1/96kHz) is applied to ALL titles including stereo.
   No opportunity to fall through to unknown.

2. **Multiple format entries** (Naked, Remain in Light): Two entries
   exist. Track type matching with `& 0x07` maps both multichannel
   AND stereo tracks to format[0] (multichannel), because bit 3 is
   masked away. (This case is already fixed for orphans by clearing
   audio facts via `OrphanPgcTitle`.)

## What needs to change

### Detect track-type bit 3 mismatch

When `track_type & 0x08` is set (bit 3 = 1), the audio format entry
resolved by `track_type & 0x07` may not describe this track's actual
format. The materializer should treat audio facts as unreliable in
this case — same as the orphan approach.

### Proposed fix

In `audio_facts_for_title_chapter()` or in `append_title_tracks()`
where audio facts are consumed: when `chapter.track_type & 0x08 != 0`
AND the resolved format entry's channel assignment doesn't match what
the probe found for this group, clear the IFO-derived expectations.

Alternatively, simpler: when `chapter.track_type & 0x08 != 0`, always
return unknown audio facts. This is safe because:
- The MLP/LPCM stream self-describes during extraction
- The probe already correctly identifies the format for display
- No existing working extraction relies on IFO facts for bit-3 tracks

### Where to apply the fix

**Option A — In `audio_facts_for_title_chapter()`:**
Check `chapter.track_type & 0x08`. If set, return
`unknown_audio_facts(AudioFormatResolution::MultiplePresentFormats)`.
This is the cleanest — one change point, affects both single-format
and multi-format cases.

**Option B — In `append_title_tracks()` alongside the orphan check:**
Extend the existing orphan check to also trigger on bit 3:
```rust
let audio_facts = if matches!(group.correlation, GroupCorrelation::OrphanPgcTitle)
    || chapter.track_type & 0x08 != 0
{
    unknown_audio_facts(AudioFormatResolution::MultiplePresentFormats)
} else {
    audio_facts
};
```

**Option A is preferred** — it's the right abstraction level and
catches the problem at the source rather than patching around it.

## Code to read

```
src/convert/pipeline/materializer_dvda.rs
  1350  audio_facts_for_title_chapter() call site
  1357  existing OrphanPgcTitle check (for context)
  1747  audio_facts_for_title_chapter() function
  1742  single-present-format branch (the problem for Document)

crates/dvda-phase1/src/tui/dvda/model.rs
  525   track_type_low_bits_candidate() — the 3-bit mask
  330   AudioChapter struct — track_type field

src/convert/pipeline/dvda_lpcm.rs
  291   channel assignment validation (skipped when None)

src/convert/pipeline/dvda_realize.rs
  698   mlp_expectation() — sample_rate/channel_count (skipped when None)
  1842  post-decode channel assignment comparison (skipped when None)
```

## Additional issue: LPCM decoder group-2 fallback

The LPCM decoder (`dvda_lpcm.rs`) also fails on this disc because
the LPCM sub-header has `group1_sample_rate = None` (code 0xF) and
`group1_bits = None` (code 0xF). The actual format is in group 2
fields: `group2_sample_rate = 192kHz`, `group2_bits = 24-bit`.

The probe already handles this via `.or()` fallback (applied in the
previous session), but the LPCM decoder's `resolve_params()` does not.

Error: `DVD-Audio LPCM sample rate is unknown`

At line 328-334 of `dvda_lpcm.rs`:
```rust
let group1_rate = choose_u32(
    self.expectation.group1_sample_rate.or(self.expectation.sample_rate),
    header.group1_sample_rate,  // ← None (code 0xF)
    ...
)?
.ok_or(LpcmDecodeError::MissingSampleRate)?;  // ← fails
```

Both the IFO expectation (cleared by the bit-3 fix) and the packet
header's group 1 field are `None`. The decoder doesn't try group 2.

The same fallback pattern is needed for:
- `group1_sample_rate` → fall back to `group2_sample_rate`
- `group1_bits` → fall back to `group2_bits`

This is safe because when ch_assign indicates 0 group-2 channels,
group 2 fields are "don't care" and can hold the actual format for
alternate-presentation encoding.

### Code location

```
src/convert/pipeline/dvda_lpcm.rs
  328   group1_rate resolution — needs group2 fallback
  334   MissingSampleRate error
  350   group1_bits resolution — needs group2 fallback
  356   MissingBitDepth error
```

## What the reasoning model should produce

1. Fix `audio_facts_for_title_chapter()` to return unknown audio facts
   when `chapter.track_type & 0x08 != 0`, so the IFO format entry is
   not trusted for alternate-presentation tracks.

2. Remove the separate `OrphanPgcTitle` audio-facts override in
   `append_title_tracks()` — the bit-3 check subsumes it (orphan
   titles also have bit 3 set on the discs in our corpus).

3. Verify that removing the orphan override doesn't regress:
   - Orphan titles with bit 3 set → caught by the new check ✓
   - Orphan titles with bit 3 clear → if any exist, the orphan
     check would still be needed. Check the corpus.

4. Fix `resolve_params()` in `dvda_lpcm.rs` to fall back to group 2
   sample rate and bit depth when group 1 fields are `None`. Apply
   the same `.or()` pattern used in the probe.

5. No behavior change for normal multichannel tracks (bit 3 = 0).

## Verification

### Confirm bit 3 is set on all orphan titles

From the corpus data above, orphan titles (title 2 on Naked, Remain
in Light, etc.) all have `track_type & 0x0f = 8`. So bit 3 is set.
The reasoning model should verify this holds before removing the
orphan-specific override.

### Test discs

After the fix:
- REM Document group 3 (LPCM stereo): should extract without
  channel/sample-rate mismatch errors
- Talking Heads Remain in Light group 2 (MLP stereo): should still
  extract (orphan + bit 3)
- REM Around the Sun group 2 (MLP stereo): should still extract
  (orphan + bit 3)
- All multichannel groups: should be unchanged (bit 3 = 0)
