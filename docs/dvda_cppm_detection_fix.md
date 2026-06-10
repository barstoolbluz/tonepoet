# DVD-Audio CPPM Detection Fix

## Problem

The DVD-Audio materializer rejects all discs that have `DVDAUDIO.MKB` present,
even when the AOB audio data is already decrypted. In our 7-disc corpus, 3 discs
have MKB files but all have valid (decrypted) MPEG-PS data in their AOBs.

This is because the CPPM detection in the Phase 1 parser equates MKB presence
with CPPM protection:

```rust
// crates/dvda-phase1/src/tui/dvda/parser.rs:58-62
let mkb_present = volume.exists_audio_ts_file("DVDAUDIO.MKB");
let copy_protection = CopyProtectionInfo {
    mkb_present,
    cppm_detected: mkb_present,  // <-- THIS IS THE BUG
    source: if mkb_present { CopyProtectionSource::MkbPresence } else { CopyProtectionSource::NotDetected },
};
```

And the materializer blocks on either flag:

```rust
// src/convert/pipeline/materializer_dvda.rs:155
if disc.copy_protection.mkb_present || disc.copy_protection.cppm_detected {
    // ... return BlockedSource error
}
```

## Evidence

All 7 ISOs have valid MPEG-PS pack headers (`00 00 01 BA`) at sector 0 of their
first AOB file:

| Disc | MKB | AOB sector 0 | Status |
|------|-----|-------------|--------|
| HDAD2009 | No | `000001ba` | Works |
| AP I Robot | No | `000001ba` | Works |
| AP Friendly Card | No | `000001ba` | Works |
| AP Eye in the Sky | No | `000001ba` | Works |
| MGLETSGETITON | **Yes** | `000001ba` | **Blocked** (but readable) |
| Hawks & Doves | **Yes** | `000001ba` | **Blocked** (but readable) |
| Talking Heads 77 | **Yes** | `000001ba` | **Blocked** (but readable) |

The MKB file is leftover metadata from the original disc. The person who ripped
these ISOs used a decryption-capable tool that decrypted the AOBs but left the
MKB file in place.

## What needs to change

### Option A: Probe AOB data (recommended)

Instead of treating MKB presence as proof of encryption, probe the first sector
of the first AOB file and check for a valid MPEG-PS pack header (`00 00 01 BA`).
If the header is valid, the data is readable regardless of MKB presence.

Detection logic:
1. `mkb_present`: unchanged (record whether `DVDAUDIO.MKB` exists)
2. `cppm_detected`: only true when MKB is present AND first AOB sector does NOT
   have a valid MPEG-PS pack header (i.e., the data appears encrypted/garbled)
3. If MKB is present but AOBs probe as valid: `cppm_detected = false`,
   add a diagnostic noting "MKB present but AOB data appears decrypted"

### Option B: --dvda-assume-decrypted flag

Add a CLI flag that overrides CPPM blocking. Users with decrypted ISOs that still
have MKB files can pass this flag to proceed.

### Recommended: Both

Implement Option A for automatic detection, and Option B as an explicit override
for edge cases where the probe might be wrong.

## Where to change

**Phase 1 parser** (`crates/dvda-phase1/src/tui/dvda/parser.rs:58-62`):
- After checking `mkb_present`, probe the first sector of the first AOB
- Set `cppm_detected` based on AOB readability, not MKB presence alone
- This requires the volume to be able to read AOB files (it already can via
  `DvdaVolume::open_audio_ts_file`)

**Materializer** (`src/convert/pipeline/materializer_dvda.rs:155`):
- Change the guard from `mkb_present || cppm_detected` to just `cppm_detected`
- MKB presence alone should not block extraction

**SourceOptions** (`src/convert/pipeline/types.rs`):
- Add `dvda_assume_decrypted: bool` (or reuse an existing flag) for Option B

## Corpus impact

This fix would make all 7 test discs extractable, unlocking:
- Multichannel (MGLETSGETITON 5.0, Talking Heads 5.1)
- 176.4 kHz / 44.1k family (Hawks & Doves)
- Multi-format ATS (MGLETSGETITON: 96/24 multichannel + 192/24 stereo)
- Mixed sample rates (Talking Heads: 96kHz + 48kHz)
