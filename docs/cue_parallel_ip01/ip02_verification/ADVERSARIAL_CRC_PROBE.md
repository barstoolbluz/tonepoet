# Rejected candidate: `explode` without FLAC CRC checking

**Linux, FFmpeg 7.1.5, 2026-10-24 user-local date.** During the deliberately CRC-damaged-frame test, the first command candidate was `-xerror -err_detect explode` with integer-second pre-input seek and sample-indexed `atrim`. It was **not safe**:

| Evidence | Observed |
|---|---|
| Original valid FLAC SHA-256 | `11f5b509db787393161d1b5f656e6eab917cc7630d65652449771b67aff7c025` |
| CRC-damaged FLAC SHA-256 | `e8f13e20d17a8c73fabe951a5cf9c39479830b0d372681b5dc978e7b733f4de0` |
| Baseline decoded PCM SHA-256 | `eaf1ec0476248896969152d272f302a095367a44dbf19b9e419e4007bcb0b4e4` |
| CRC-damaged decoded PCM SHA-256 with `explode` only | `4e88be5815d6139c786791804048960cf76d7cf9b55794c3b3e24838a382afe0` |
| `explode`-only FFmpeg exit | `0` and full expected output length, **but PCM changed** |
| `flac -t` on damaged FLAC | Nonzero; reported frame CRC mismatch and lost sync near sample 327168 |
| `-err_detect crccheck+explode` FFmpeg exit | `183`, frame rejected |

This establishes why exit code plus expected byte count are necessary but **not sufficient**: the decoder must also be told to verify CRCs. The production adapter and the final native suite both require `crccheck+explode`. The native script regenerates damage and verifies its failure; it never runs an ordinary conversion through this adapter.
