# DSD Reference policy v17 qualification-basis report

Policy v17 remains an unpromoted qualification candidate. This source-controlled report records the policy delta and the evidence that the real-tool gate must produce; it is not execution evidence and does not promote the candidate.

## Append-only corrections from v16

- Bind SoX-ng 14.8.0.1 to revision `9ed22fb3d813d6c02f67c254e57d162cee014a30` and NAR `sha256-WxMirop+SH3RzUM57QBDjetp9Q4qhFjzcfo2OJHStcs=`. Historical v16 artifacts remain byte-for-byte frozen.
- Keep Int24 on the existing SoX-ng TPDF terminal.
- Correct Reference Int32 from undithered SoX-ng signed-32 output to the already commissioned FFmpeg 7.1.3/libswresample Float64-to-S32 plain-triangular terminal, authority `ffmpeg_7-full-n7.1.3+nixpkgs-dd9b079222d43e1943b6ebd802f04fd959dc8e61/libswresample-dbl-triangular-s32/v1`.
- Normalize protected Float64 Wave64 to headerless true-scale `f64le` with the qualified SoX-ng Float64-W64 reader route before gain. The one selected gain is then applied by the certified in-process binary64 scalar pump; FFmpeg receives the gained Float64 stream and owns the one Int32 TPDF+dither/quantization effect.

## Int32 physical error authority

The commissioned FFmpeg terminal proof bounds the combined triangular-dither plus S32 conversion effect by

```text
E_ffmpeg = next_up(2^-31 + 2^-52 + 2^-31)
         = 9.313227966600836e-10 FS
         = 2.0000004768371586 S32 LSB
Q1.63 ceiling = 8,589,936,641
```

Reference gain is a distinct physical operation before FFmpeg. The certified binary64 scalar multiplication contributes at most `2^-51` FS. Policy v17 therefore charges both once:

```text
E_reference_int32 = next_up(E_ffmpeg + 2^-51)
Q1.63 ceiling     = 8,589,940,737
safe preterminal = -1.010000010 dBTP
```

The safe ceiling uses the existing `-1.000000000 dBTP` public ceiling and one `0.010000000 dB` post-final analyzer-reporting reserve. This is a deterministic worst-case authority, not an observed maximum.

The FFmpeg proof and commissioning record is `tonepoet-pipeline/qualification/ffmpeg_int32_triangular_terminal_bound_2026-09-14.md`. The SoX-ng source-lock proof is `tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v17_source_lock_proof.md` (`sha256:4746ec2e7764d15e31d14b95ea39e8c7c45698b45b8e83129df77bfea28217ac`).

## Qualification contract

`complete_p0_reference_qualification_report` must execute the shared production lowerer/executor, the full lossless package matrix, the 80-cell exact-Wave64 characterization, complete-reader checks, GAIN08/GAIN09, workspace regression, timeout/cancellation/resource checks, and paired performance/resource characterization. Int32 matrix cells must prove that the command transcript uses the exact FFmpeg triangular authority, that no SoX `dither` effect owns Int32, that the carrier bridge is true-scale `f64le`, and that measured terminal realization error remains below the v17 deterministic bound.

The generated common-model report, certification, and evidence JSON are the release evidence. They are written only by the gated qualification path; they are not hand-authored by this policy derivation.

## Status

`not_run`. Production remains fail-closed until the operator executes the exact pinned gate and installs a matching passed report/certification/evidence set.
