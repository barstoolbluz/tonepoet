# DVD-Audio Stereo Downmix Extraction — Implementation Brief

## Purpose

Implement stereo output for DVD-Audio groups identified as authored
stereo presentations of a multichannel MLP source. When the user
converts such a group (e.g., Brothers in Arms group 3), produce a
2-channel WAV using the foo_input_dvda-compatible downmix matrix
instead of extracting raw 5.1.

---

## 1. Current MLP decode pipeline

The DVD-Audio MLP extraction path is:

```
materializer_dvda.rs → PreparedTrack with TrackSourceRef::DvdaTrack
stages.rs → realize_dvda_track()
dvda_realize.rs → extract_track_audio_payload() → temp .mlp file
dvda_realize.rs → decode_mlp_to_wav():
  ffmpeg -f mlp -i temp.mlp -map 0:a:0 -c:a pcm_s32le -f wav output.wav
dvda_realize.rs → validate_dvda_wav() → checks channels match expectation
```

No downmix, no channel manipulation. The output has whatever channel
layout the MLP stream declares (e.g., 6 channels for 5.1).

The existing `downmix_matrix: Option<u8>` field on `TrackSourceRef::DvdaTrack`
is logged but explicitly NOT applied (dvda_realize.rs line 1291-1296).

---

## 2. The downmix matrix

From the reasoning model's guidance (`docs/stereo-downmix-feedback-from-reasoning-model.md`):

```
L = 0.500*Lf + 0.354*C + 0.177*LFE + 0.250*Ls
R = 0.500*Rf + 0.354*C + 0.177*LFE + 0.250*Rs
```

This is the foo_input_dvda-compatible conservative downmix. It is NOT
ATSC A/52 and differs from ffmpeg's default `-ac 2` (which has zero
LFE contribution by default).

This can be applied via ffmpeg's `-af pan` filter inline during MLP
decode, requiring no post-decode Rust DSP:

```
ffmpeg -f mlp -i temp.mlp -map 0:a:0 \
  -af "pan=stereo|FL=0.500*FL+0.354*FC+0.177*LFE+0.250*BL|FR=0.500*FR+0.354*FC+0.177*LFE+0.250*BR" \
  -c:a pcm_s32le -f wav output.wav
```

ffmpeg's MLP decoder maps DVD-Audio 5.1 channels to standard names:
Lf→FL, Rf→FR, C→FC, LFE→LFE, Ls→BL, Rs→BR.

---

## 3. What to implement

### 3a. Downmix policy type

Add to `src/convert/pipeline/types.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaDownmixPolicy {
    /// Extract all channels as-is (default for multichannel groups)
    None,
    /// Apply the foo_input_dvda-compatible conservative downmix matrix
    FooInputDvdaCompatible,
    /// Let ffmpeg choose the downmix via -ac 2 (different coefficients)
    FfmpegDefault,
}
```

### 3b. Thread the policy through the pipeline

**`TrackSourceRef::DvdaTrack`** (types.rs ~line 393):
Add `dvda_downmix_policy: DvdaDownmixPolicy` field.

**`DvdaTrackRealizeInput`** (dvda_realize.rs ~line 400):
Add `downmix_policy: DvdaDownmixPolicy` field. Populated from
`TrackSourceRef::DvdaTrack.dvda_downmix_policy`.

**`DvdaRealizationAudioPolicy`** (dvda_realize.rs ~line 312):
Add `downmix_policy: DvdaDownmixPolicy` field.

### 3c. Set the policy in the materializer

**`materializer_dvda.rs`** — in the track source ref builder
(`ats_track_source_ref` ~line 806):

Detect the stereo downmix condition using the same heuristic as
`detect_stereo_downmix_source()` in `dvda_utils.rs`:
- The group's title set has no existing AOBs
- The resolved MLP stream is multichannel (channel count > 2)
- A sibling group with AOBs exists with matching track count and
  near-matching duration

When detected, set `dvda_downmix_policy: DvdaDownmixPolicy::FooInputDvdaCompatible`.
Otherwise, set `dvda_downmix_policy: DvdaDownmixPolicy::None`.

Alternatively: the materializer can check if the `DvdaGroupSelection`
indicates a stereo presentation. If the CLI/TUI has already identified
the group as stereo (via `disc-info`'s detection), that flag can be
threaded through `SourceOptions` → materializer → `TrackSourceRef`.

### 3d. Apply the downmix in the MLP decode

**`dvda_realize.rs`** — in `decode_mlp_to_wav()` (~line 1630):

When `downmix_policy == FooInputDvdaCompatible`, add the `-af pan`
filter to the ffmpeg command:

```rust
if matches!(input.downmix_policy, DvdaDownmixPolicy::FooInputDvdaCompatible) {
    args.push("-af".to_string());
    args.push(
        "pan=stereo|\
         FL=0.500*FL+0.354*FC+0.177*LFE+0.250*BL|\
         FR=0.500*FR+0.354*FC+0.177*LFE+0.250*BR"
            .to_string(),
    );
}
```

When `downmix_policy == FfmpegDefault`, add `-ac 2` instead (lets
ffmpeg choose its own downmix matrix).

### 3e. Update WAV validation

**`dvda_realize.rs`** — in `validate_dvda_wav()` (~line 2120):

The validator checks `probe.channels == expectation.channel_count`.
When downmix is active, the expected channel count is 2, not 6. The
`DvdaWavExpectation` must be built with the post-downmix channel count:

```rust
let expected_channels = if matches!(downmix_policy, DvdaDownmixPolicy::None) {
    source_channel_count  // 6 for 5.1
} else {
    2  // stereo output
};
```

### 3f. LPCM downmix path (optional, lower priority)

For completeness, the LPCM mux path (`mux_s32le_to_wav` ~line 1683)
should also support downmix. This would apply when a DVD-Audio disc
has a cross-ATS LPCM presentation (no known test disc has this). The
same `-af pan` filter approach works for raw PCM input.

---

## 4. CLI/TUI surface

### Automatic behavior

When the user converts a group that `disc-info` displays as
"Stereo (derived from 5.1)", the pipeline should automatically
apply `FooInputDvdaCompatible` downmix. The user doesn't need to
specify any flag — the detection heuristic in the materializer
identifies the group.

### Override flags

For advanced users:

```bash
# Force raw 5.1 extraction even for stereo-presentation groups
tonepoet convert disc.iso --dvda-group 3 --dvda-downmix none

# Use ffmpeg's default downmix instead of foo_input_dvda coefficients
tonepoet convert disc.iso --dvda-group 3 --dvda-downmix ffmpeg

# Explicitly request foo_input_dvda downmix (the default for stereo groups)
tonepoet convert disc.iso --dvda-group 3 --dvda-downmix foo-compat
```

This requires adding `--dvda-downmix` to `main.rs` clap args and
threading it through `SourceOptions` and `PipelineRequest`.

---

## 5. Files to read

```
src/convert/pipeline/dvda_realize.rs   — MLP decode, decode_mlp_to_wav, validate_dvda_wav
src/convert/pipeline/types.rs          — TrackSourceRef::DvdaTrack, SourceOptions
src/convert/pipeline/materializer_dvda.rs — ats_track_source_ref, PreparedTrack builder
src/convert/pipeline/stages.rs         — realize_track call site
src/disc/dvda_utils.rs                 — detect_stereo_downmix_source heuristic
docs/stereo-downmix-feedback-from-reasoning-model.md — full reasoning model guidance
```

---

## 6. Files to modify

| File | Change |
|------|--------|
| `src/convert/pipeline/types.rs` | Add `DvdaDownmixPolicy` enum, field on `TrackSourceRef::DvdaTrack` |
| `src/convert/pipeline/dvda_realize.rs` | Add policy to `DvdaTrackRealizeInput` and `DvdaRealizationAudioPolicy`, add `-af pan` to ffmpeg args, update WAV validation |
| `src/convert/pipeline/materializer_dvda.rs` | Detect stereo presentation, set downmix policy on `TrackSourceRef` |
| `src/convert/pipeline/stages.rs` | Thread downmix policy through realize call |
| `src/main.rs` (optional) | Add `--dvda-downmix` CLI flag |

---

## 7. Test validation

- Brothers in Arms group 3: should produce a 2-channel WAV (not 6-channel)
- Brothers in Arms group 1: should produce a 6-channel WAV (unchanged)
- All other discs: should produce unchanged output (no stereo detection triggers)
- `cargo test -p dvda-phase1` and `cargo test --bin tonepoet` pass

---

## 8. What the reasoning model should produce

1. `DvdaDownmixPolicy` enum definition
2. Modified `TrackSourceRef::DvdaTrack` with the new field
3. Modified `dvda_realize.rs` with `-af pan` filter application and updated validation
4. Modified `materializer_dvda.rs` with stereo presentation detection
5. Guidance on where exactly in `stages.rs` to thread the policy
6. Updated test fixtures or test expectations if any existing tests assert channel counts
