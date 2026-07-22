# Handoff — sox_ng fork fixes (self-directed, for a future session)

Written 2026-07-22, HEAD `fa6131e` (tonepoet 0.4.1). Read
`memory/session_handoff.md` first for the working model; this doc is the
detailed spec for **parked track #1: sox_ng fork fixes**.

## Goal

Patch two writer bugs in the user's sox_ng fork, rebuild, prove the
fixes with the exact minimal repros below, then carry the fix back into
tonepoet: bump the flake pin, re-attest the toolchain closure, and lift
the two policy accommodations these bugs forced (streamed >4 GiB
capacity cap; the all-zero W64 refusal / F11.2 exact-header assert).

Both are **one-line-class** fixes in SoX's WAV/W64 size finalization.
Neither blocks tonepoet today — policy routes around both fail-closed —
so this is a "simplify future re-attestations + unlock silence/large
carriers" track, not a fire. Do NOT start it unless the user asks.

## The fork

- Pin: `github:barstoolbluz/sox_ng`, rev
  `324b8cf873fd7836e8848bd87f7a90d8faa6f849` = **sox_ng 14.8.0.1**
  (flake.nix input `sox_ng`; flake.lock line ~154).
- Both defects also exist upstream at codeberg.org/sox_ng/sox_ng and in
  14.6.1, so the fixes are candidates for an upstream PR too (paste-ready
  drafts live in `docs/upstream_issue_drafts.md` — the F6 one is written;
  the F10 silence one still needs adding there as the 2nd sox entry,
  do that as part of this track).
- To work on the source: `nix flake clone` / `git clone
  https://github.com/barstoolbluz/sox_ng` and check out `324b8cf`. The
  WAV/W64 writer is expected to live in **`src/wav.c`** (stock SoX's WAV
  handler; W64 and RF64 are size variants of the same handler). The
  writer entry points to read are, by SoX convention, `startwrite`,
  `wavwritehdr` (header emit / patch-up), and `stopwrite` (finalization /
  seek-back-and-patch). **All of the above — file name, function names,
  and whether RIFF/W64/RF64 share the size-accounting code — are
  UNVERIFIED inferences from stock-SoX layout; I have not read the fork
  source. `grep` the fork tree first and confirm before trusting any of
  it. (Note: the ledger records "rf64 unwritable by sox," so the RF64
  write path may be absent or disabled — don't assume it's shared.)**

## Defect A — F6: WAV writer wraps >4 GiB sizes mod 2^32 on unseekable output

**Symptom:** writing WAV to a pipe/stdout with a payload > 4 GiB emits
RIFF and `data` chunk sizes = true size truncated mod 2^32, instead of
the streaming sentinel `0xFFFFFFFF`. A reader that honors declared sizes
truncates. For 4 GiB + 8 bytes the header declares `data` = 8.

**Root-cause shape:** when output is unseekable, SoX cannot seek back in
`stopwrite` to patch real sizes, so the header written up front is
final. The bug is that the up-front path writes the (32-bit-truncated)
computed size rather than the sentinel. The fix path: in the
header-emit code, when the output is unseekable AND the computed
RIFF/data size exceeds `0xFFFFFFFF` (or is simply unknown up front),
emit `0xFFFFFFFF` for both fields. This matches common piped-WAV
practice; SoX's own reader already reads to EOF, so round-trip is safe.

**Exact repro (Linux, sparse — no real 4 GiB I/O):** see
`docs/upstream_issue_drafts.md` §1 for the `make_big_w64.py` script that
mints a valid W64 (f64 mono 48 kHz, data = 4 GiB + 8, sparse). Then:

```console
$ sox --info big.w64            # reader CORRECT: 536,870,913 samples
$ sox -D big.w64 -t wav - | head -c 8 | od -A d -t x1
0000000 52 49 46 46 3a 00 00 00   # RIFF size 0x3a — WRONG (wrapped)
                                   # FIXED: expect ff ff ff ff
```
data-chunk size field is likewise `(4 GiB + 8) mod 2^32 = 8`; fixed
should be `0xFFFFFFFF`.

**Acceptance for the fix:** the streamed header shows `0xFFFFFFFF` for
both RIFF and data sizes on the >4 GiB unseekable case; the ordinary
(<4 GiB, or seekable-file) cases are byte-unchanged. Round-trip
`sox big.w64 -t wav - | sox -t wav - out.w64 && sox --info out.w64`
reports the full sample count.

## Defect B — F10: W64 writer finalizes header-only/empty sizes for all-zero content

**Symptom:** writing an **all-zero** (digital silence) payload to W64,
the RIFF-GUID size field and the `data` chunk size field are finalized
as if the file were header-only/empty, while the full zero-sample
payload IS on disk. FFmpeg/ffprobe correctly refuse the file (they honor
the declared extent that excludes the payload); SoX round-trips its own
broken file because its reader reads to EOF and ignores the size fields.

**Measured witness (both files 70,696 bytes, 8,820 frames):**
```text
sox -D -r 88200 -n -e floating-point -b 64 -c 1 tone.w64  synth 0.1 sine 1000 gain -6
sox -D -r 88200 -n -e floating-point -b 64 -c 1 zeros.w64 synth 0.1 sine 1000 vol 0

tone.w64  RIFF-GUID size: 0x00011428 = 70,696   (correct)
zeros.w64 RIFF-GUID size: 0x00000088 = 136      (HEADER-ONLY — bogus)
zeros.w64 data-chunk size: 0x18 = 24            (declares EMPTY payload;
             correct data size = 0x113b8 = 70,584, i.e. 70,560 payload
             bytes + 24-byte W64 chunk header; payload IS present)

ffprobe tone.w64  -> opens
ffprobe zeros.w64 -> "Invalid data"  (correctly honoring the bogus size)
ffprobe -f w64 …  -> forcing the demuxer does NOT bypass
```

**Root-cause shape:** the size finalization/patch-up path treats
all-zero written content as "nothing written" — as if a bytes-written
counter or a "did we emit any data blocks" flag stayed at its initial
state for silent content, so the seek-back patch computes an empty file.
Read `stopwrite`/`wavwritehdr` in the fork and find where the final
`data` (and RIFF) size is computed from a running counter; verify that
counter increments on all-zero blocks the same as nonzero ones. It is
almost certainly a single conditional or counter-update that skips when
content is zero. **Characterize the exact trigger while you are in
there** — all-zero-whole-file vs first-block-silence vs a threshold —
using leading-silence and trailing-silence controls (tonepoet's gate
already writes these 5 fixtures; see below).

**Acceptance for the fix:** `zeros.w64` declares RIFF size 70,696 and
data size 70,584 (matching `tone.w64`'s accounting) with all 70,560
payload bytes present; ffprobe opens it; SoX still reports 8,820 frames.
The 4 nonzero controls stay byte-unchanged.

## Building & proving the fork fix

```bash
# in a clone of the fork at your patched rev
autoreconf -i && ./configure && make      # or the fork's documented build
# run BOTH repros above against ./src/sox and confirm the header fields
```
Do NOT trust a tonepoet build until the standalone binary passes both
repros. The header-field checks are the ground truth; sox's own reader
will happily read either broken or fixed files, so assert on the raw
GUID/chunk size bytes (`od`/`xxd`), not on `sox --info`.

## Carrying the fix back into tonepoet (the real deliverable)

Order matters; each step is append-only and gated.

1. **Bump the flake pin.** Point `sox_ng` at the patched rev; `nix flake
   update sox_ng` (or edit flake.lock rev + narHash). Re-enter
   `nix develop`. Confirm the pinned binary in PATH is the patched one
   (its version string is unchanged — 14.8.0.1 — so verify by running
   the two repros through the flake's `sox`, not by version).

2. **Re-attest the toolchain closure.** The policy bakes tool identities
   via `TONEPOET_REFERENCE_*` env (flake wrapper/shell + build.rs
   re-exports) and the qualification checkers. A changed sox binary
   changes the closure identity → this needs a **new append-only policy
   ID** `sox_ng_14_8_0_1_v17` (or next free vN — check
   `tonepoet-pipeline/qualification/` for the current max) recording the
   patched-tool attestation. Do NOT mutate any frozen vN artifact; append.
   The user wants slow crate versions but policy IDs are a separate
   append-only axis (bump freely with reason).

3. **Run the standing gate** (from `memory/session_handoff.md`) inside
   `nix develop`: `cargo check --workspace --all-targets`; full untruncated
   `cargo test --workspace --no-fail-fast` (assert 0 failed — baseline
   ~4,650); the 12 qualification checkers; the tool-gated
   `dsd_reference_qualification` target; live DSD64 smoke; 0 cold warnings.

4. **Lift the two accommodations — but only with fresh empirical proof
   under the patched binary, not by deleting the guards:**
   - **F6 / streamed >4 GiB capacity cap** (policy v12/v13
     `ReferenceStreamedWavCapacityEvidence*`): re-qualify sample-exact
     transport past the 4 GiB boundary through the frozen `4 GiB + 8`
     witness with the patched writer emitting sentinels, then raise/remove
     the cap as a new policy ID with the measured evidence. See findings
     §F6 / F6-resolution / F7 (the streamed-header byte layout — 58 bytes
     for f64, EXTENSIBLE for Int24 — is frozen and must be re-measured if
     the patch touches header size).
   - **F10 / F11.2 all-zero W64** (qualification is a known **4/5**; the
     exact-W64-header assert fires on silence carriers because the pinned
     writer emitted bogus sizes): with the writer fixed, the all-zero
     witness now declares correct sizes and ffmpeg accepts it. Update the
     permanent 5-fixture all-zero W64 gate (findings §F10-resolution v15
     / v16 `validate_exact_w64_pcm`) to expect the CORRECTED header on the
     all-zero witness, flip F11.2 from accepted-as-documented to passing,
     and target qualification **5/5**. Keep `validate_exact_w64_pcm`'s
     independent structural check — it's the right guard regardless; only
     the expected-values for the silence witness change.

5. **Audit + commit.** Per standing rule every apply/change round gets a
   report-only audit until a zero-finding pass; audits gate merges to
   main. Commit freely; push only on the user's explicit word. If any
   crate version bump is warranted it's a patch bump as the LAST commit of
   the merge set (tonepoet 0.4.2 / pipeline patch) — the user wants slow
   numbers, never propose minors.

6. **File the upstream drafts** (optional, user-parked): add the F10
   silence-header defect to `docs/upstream_issue_drafts.md` as the 2nd
   sox entry (§1 is F6), then file both on codeberg + the fork if the user
   wants.

## Guardrails

- These are the ONLY two sox writer defects in scope. The other three
  toolchain defects in the ledger are **ffmpeg** (f64-W64 2^31 decode
  scale; W64 muxer pad-into-data phantom sample; f32-via-stream misread) —
  out of scope for the sox fork.
- Runtime Reference execution stays **fail-closed / unpromoted** through
  this whole track. Fixing the writer and lifting the caps does NOT
  promote the Reference pathway to default — that's parked track #2
  (promotion round, design §8.2), a separate decision. Ordinary DSD→PCM
  keeps running the exact-legacy chain, protected by the permanent
  live-smoke gate test.
- Append-only everything on the policy axis. Frozen vN artifacts and the
  v15/v16 lineage are immutable; the F11.3 byte-for-byte v15 restoration
  must stay intact.

## Source pointers (verified against tree at fa6131e)

- Defect repros + F6 upstream draft: `docs/upstream_issue_drafts.md`
- Full findings ledger F6/F10/F11: `docs/findings_dsd_reference_p0_admission_round.md`
  (F6 §373, F10 §812, F11 §956 — exact-header/silence-assert disposition)
- Current policy state: `docs/handoff_dsd_reference_p0_current.md`
- Policy IDs / checkers / qualification report:
  `tonepoet-pipeline/qualification/`
- Independent W64 parser (the in-tree exact validator):
  `tonepoet-pipeline/src/w64.rs`
- Flake pin: `flake.nix` input `sox_ng` (line ~11) + `flake.lock` rev
  `324b8cf…` (line ~154); `referenceSox` wired at `flake.nix:73`.
