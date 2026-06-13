# DVD-Audio Stereo Downmix Guidance

## Core finding

The Brothers in Arms DVD-Audio case has a stereo presentation group, but not a separate stereo MLP essence.

The important facts are:

```text
ATS 1: owns the AOB files and contains the 5.1 MLP stream
ATS 2: has an IFO only and no AOB files
Group 1: points to ATS 1 and represents the 5.1 presentation
Group 3: points to ATS 2 and represents the authored stereo presentation
MLP major sync: num_substreams == 1 for all groups
MLP major sync: channel_arrangement == 12, i.e. 5.1
IFO downmix matrices: all zeroed
Chapter downmix refs: all None
```

So the stereo group should not be detected through `num_substreams > 1`. That test failed for this disc. The stereo group also should not use the IFO downmix matrices, because the relevant matrices are zeroed and foo_input_dvda’s MLP path does not use those matrices for this case.

The safest interpretation is:

```text
The group is an authored stereo presentation that reuses a multichannel MLP stream.
The decoded codec payload is 5.1.
The intended presentation is stereo.
The stereo result must therefore be produced by downmixing.
```

## Main recommendation

For an AOB-less ATS group like this, tonepoet should model both truths:

```text
codec_channel_label = "5.1"
presentation_channel_label = "Stereo"
presentation_transform = DownmixFrom("5.1")
```

Recommended display:

```text
Group 1: MLP 96kHz/24-bit 5.1
Group 3: MLP 96kHz/24-bit Stereo (derived from 5.1)
```

For technical views, this can be rendered as:

```text
MLP 96kHz/24-bit 5.1 → Stereo
```

For compact end-user views, prefer:

```text
MLP 96/24 Stereo
```

with details available on demand:

```text
Source stream: 5.1 MLP
Presentation: stereo downmix
```

This avoids showing only “5.1,” which hides the authored stereo presentation, and avoids showing only “Stereo” in technical contexts, which hides the fact that the MLP major sync describes a 5.1 stream.

## 1. foo_input_dvda hardcoded coefficients

`audio_stream.cpp:set_downmix_coef()` defines this fixed 8×2 matrix:

| Input channel | Left output | Right output |
| ------------- | ----------: | -----------: |
| Lf            |       0.500 |        0.000 |
| Rf            |       0.000 |        0.500 |
| C             |       0.354 |        0.354 |
| LFE           |       0.177 |        0.177 |
| Ls            |       0.250 |        0.000 |
| Rs            |       0.000 |        0.250 |
| ch6           |       0.000 |        0.000 |
| ch7           |       0.000 |        0.000 |

Equivalent formulas:

```text
L = 0.500*Lf + 0.354*C + 0.177*LFE + 0.250*Ls
R = 0.500*Rf + 0.354*C + 0.177*LFE + 0.250*Rs
```

`downmix_channels()` applies this matrix per sample and rewrites the decoded buffer as two interleaved channels.

Important nuance: this is best described as a **foo_input_dvda-compatible conservative downmix**, not as a generic “standard ATSC A/52 downmix.”

Relative to the front-left/front-right contribution, the matrix behaves like this:

```text
Front L/R: 0.500 absolute
Center:    0.354 absolute, about -3 dB relative to front
Surround:  0.250 absolute, about -6 dB relative to front
LFE:       0.177 absolute, about -9 dB relative to front
```

So it resembles a Lo/Ro-style downmix with conservative overall attenuation, center mixed at about -3 dB relative to the front channels, surround mixed at about -6 dB relative to the front channels, and LFE mixed in at a lower level.

That differs from bare FFmpeg `-ac 2`. FFmpeg/libswresample generates a matrix from resampler/rematrix options such as `center_mix_level`, `surround_mix_level`, `lfe_mix_level`, `rematrix_volume`, and `rematrix_maxval`. FFmpeg’s defaults are not equivalent to the fixed foo_input_dvda matrix. In particular, FFmpeg’s default LFE mix level is zero, while the foo_input_dvda fallback includes LFE at 0.177.

## 2. IFO matrix vs. hardcoded matrix

The two paths should be treated separately.

### PCM path

In `audio_track.cpp`, IFO downmix matrices are loaded only when the track is PCM:

```cpp
if (audio_track.audio_stream_info.stream_id == PCM_STREAM_ID) {
    int downmix_matrix = dvda_track.get_downmix_matrix();
    if (downmix_matrix >= 0) {
        for (int ch = 0; ch < DOWNMIX_CHANNELS; ch++) {
            audio_track.LR_dmx_coef[ch][0] =
                dvda_titleset.get_downmix_coef(downmix_matrix, ch, 0);
            audio_track.LR_dmx_coef[ch][1] =
                dvda_titleset.get_downmix_coef(downmix_matrix, ch, 1);
        }
        audio_track.audio_stream_info.can_downmix = true;
    }
}
```

That means IFO matrices are part of the PCM downmix path.

### MLP path

For MLP, `mlp_audio_stream.cpp` sets:

```cpp
si.can_downmix = ctx->mh.num_substreams > 1;
```

Then, during stream initialization:

```cpp
do_downmix = downmix;

if (downmix) {
    if (info.can_downmix) {
        av_channel_layout_default(&...downmix_layout, 2);
    } else {
        set_downmix_coef();
    }
}
```

During decode:

```cpp
if (do_downmix && !info.can_downmix) {
    downmix_channels(out_data, &frame_size);
}
```

So if `mlp_audio_stream_t::init()` is called with `downmix == true` and the MLP stream has `num_substreams == 1`, the stream-level MLP decoder uses the no-argument `set_downmix_coef()` fallback and applies the hardcoded matrix.

### Caveat

The provided `track_list_t::init()` logic also uses `audio_stream_info.can_downmix` when deciding whether to add downmix tracks. For single-substream MLP, `can_downmix` is false. That means the stream-level decoder can perform the hardcoded fallback if invoked with `downmix == true`, but the surrounding track-list code may not expose such a downmix track through every listing path.

For tonepoet, this distinction is useful:

```text
IFO matrices:
  use only for PCM when explicitly referenced and valid

MLP with num_substreams > 1:
  embedded/substream downmix path may exist

MLP with num_substreams == 1:
  no embedded stereo substream
  no IFO-matrix MLP path in the inspected code
  stereo presentation requires an explicit policy downmix
```

## 3. Display label strategy

Use a split model rather than a single overloaded channel label.

Recommended internal representation:

```rust
struct PresentationAudioInfo {
    codec: AudioCodec,                 // MLP
    sample_rate_hz: u32,               // 96000
    bits_per_sample: u8,               // 24
    source_channel_label: String,      // "5.1"
    presentation_channel_label: String,// "Stereo"
    transform: Option<AudioTransform>, // DownmixFrom("5.1")
    confidence: PresentationConfidence,
}
```

Recommended display levels:

```text
Simple user-facing:
  MLP 96kHz/24-bit Stereo

Detailed user-facing:
  MLP 96kHz/24-bit Stereo (derived from 5.1)

Technical/debug:
  MLP 96kHz/24-bit 5.1 → Stereo
```

This gives the browser the expected “Stereo” result while preserving the codec truth in technical output.

## 4. Extraction strategy

Default behavior for the inferred stereo presentation should be:

```text
Produce stereo by applying a deterministic downmix matrix.
Do not extract raw 5.1 as the default for the stereo presentation.
Do not use the zeroed IFO matrices for this MLP case.
Do not rely on bare ffmpeg -ac 2 if reproducibility is a goal.
```

Recommended policy:

```text
Default:
  use an explicit tonepoet downmix policy

Recommended initial preset:
  foo_input_dvda-compatible conservative matrix

Advanced options:
  extract underlying 5.1 unchanged
  use FFmpeg default rematrixing
  use a named standard Lo/Ro preset
```

The default preset can be:

```text
L = 0.500*Lf + 0.354*C + 0.177*LFE + 0.250*Ls
R = 0.500*Rf + 0.354*C + 0.177*LFE + 0.250*Rs
```

Name it clearly:

```text
DownmixPreset::FooInputDvdaCompatible
```

Avoid naming it:

```text
DownmixPreset::ATSC_A52
```

because it is not a pure A/52 metadata-driven matrix. It uses A/52-like center/surround ratios, but with an overall gain reduction and nonzero LFE inclusion.

Recommended extraction behavior for group 3:

```text
Group 3 selected normally:
  decode 5.1 MLP
  apply explicit stereo downmix
  output 2.0

Group 3 selected with raw/advanced mode:
  decode 5.1 MLP
  output 5.1 unchanged
```

## 5. Detection heuristic

Use the AOB-less ATS pattern as a signal, not as the only rule.

Recommended core heuristic:

```text
Candidate stereo presentation if:
  AOTT group points to an ATS with no AOB files
  AND the group is not the primary AOB-owning group
  AND the resolved/cross-ATS MLP stream has more than 2 channels
  AND there is sibling evidence linking it to a multichannel source presentation
```

Strengthen the detection with sibling checks:

```text
Sibling group check:
  same or near-same duration
  same track count
  same or compatible title structure
  one sibling owns the AOB files
  one sibling is the AOB-less alternate presentation
  primary sibling is multichannel
```

Use metadata if available:

```text
Metadata check:
  SAMG / presentation metadata indicates a 2-channel presentation
  AOTT or ATS category fields distinguish the presentation type
  placeholder group structure matches the known authoring pattern
```

Do not use this as the only test:

```text
num_substreams > 1
```

That identifies embedded MLP downmix capability, not this authored presentation pattern.

Do not use this as the only test either:

```text
AOB-less ATS
```

An AOB-less ATS could have other meanings. Treat it as a strong signal only when the resolved stream is multichannel and the surrounding title/group structure supports an alternate stereo presentation.

Recommended confidence model:

```rust
enum StereoPresentationConfidence {
    Strong {
        reasons: Vec<StereoPresentationReason>,
    },
    Probable {
        reasons: Vec<StereoPresentationReason>,
    },
    Weak {
        reasons: Vec<StereoPresentationReason>,
    },
}

enum StereoPresentationReason {
    AoblessAts,
    CrossAtsToMultichannelMlp,
    SiblingTitleTrackCountMatch,
    DurationNearMatch,
    SeparatePresentationGroup,
    SamgIndicatesTwoChannel,
}
```

For Brothers in Arms, the confidence should be strong:

```rust
StereoPresentationConfidence::Strong {
    reasons: vec![
        StereoPresentationReason::AoblessAts,
        StereoPresentationReason::CrossAtsToMultichannelMlp,
        StereoPresentationReason::SiblingTitleTrackCountMatch,
        StereoPresentationReason::DurationNearMatch,
        StereoPresentationReason::SeparatePresentationGroup,
    ],
}
```

## Recommended Brothers in Arms behavior

Display:

```text
Group 1: MLP 96kHz/24-bit 5.1
Group 3: MLP 96kHz/24-bit Stereo (derived from 5.1)
```

Technical/debug display:

```text
Group 3: MLP 96kHz/24-bit 5.1 → Stereo
```

Default conversion:

```text
Decode the 5.1 MLP stream.
Apply the explicit foo_input_dvda-compatible conservative downmix matrix.
Write 2.0 output.
```

Advanced conversion:

```text
Allow “extract underlying 5.1” as a raw mode.
Optionally allow “FFmpeg default downmix” as a separate non-default preset.
Optionally allow a named standard Lo/Ro preset as another separate preset.
```

Never do this for the Brothers in Arms MLP stereo presentation:

```text
Use the zeroed IFO downmix matrix.
Treat num_substreams > 1 as required for stereo-presentation detection.
Silently label the technical stream as only Stereo with no indication that the source MLP is 5.1.
Silently label the presentation as only 5.1 in the user-facing group list.
```

## Summary

The revised recommendation is:

```text
Detect the group as an authored stereo presentation.
Display it as Stereo in normal views and 5.1 → Stereo in technical views.
Default extraction should output 2.0.
Use an explicit deterministic matrix.
Use the foo_input_dvda-compatible conservative matrix as the initial default preset.
Do not call that preset “ATSC A/52.”
Do not use the zeroed IFO matrices for MLP.
Keep raw 5.1 extraction available as an advanced option.
```
