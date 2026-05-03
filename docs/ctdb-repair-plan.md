# CTDB Reed-Solomon Repair Integration

## Context

The RS codec (`src/ctdb_rs/mod.rs`) is implemented and tested. It can verify and repair a full disc image buffer (`&mut [i16]`) given parity bytes from the CTDB API. We need to wire this into the TUI so users can repair damaged CD rips.

The repair operates on the full disc image — all tracks concatenated with one STRIDE (11,760 i16 values) of leadin at the start and one STRIDE of leadout at the end. For per-track rips, we assemble the image from individual files, repair, then split back and re-encode.

## Safety model

Same as AR offset correction: write repaired files to /tmp, verify via CTDB CRC32 at offset 0, only replace originals if ALL tracks verify. Backup originals to .bak, restore on failure.

## Data flow

```
User runs :ctdb-repair (or clicks pill in CTDB overlay)
  → Collect track paths
  → Download parity bytes from hasparity URL
  → Decode all tracks to i16
  → Assemble disc image: [leadin zeros] + [track1] + [track2] + ... + [leadout zeros]
  → codec.repair(&mut image, &parity, npar, offset)
  → Split repaired image back into per-track segments
  → Encode each track to FLAC in /tmp
  → Copy metadata from originals
  → Verify repaired files via CTDB CRC32
  → If ALL pass: backup originals, replace with repaired, delete backups
  → If ANY fail: abort, clean up /tmp, report error
```

## Changes

### 1. `src/tui/ctdb.rs` — extend result structs + add repair function

**Extend `CtdbVerifyResult`** to store repair-relevant metadata from the API:
```rust
pub struct CtdbVerifyResult {
    pub tracks: Vec<CtdbTrackResult>,
    pub toc: String,
    pub npar: Option<u32>,           // NEW
    pub stride: Option<usize>,       // NEW
    pub parity_url: Option<String>,  // NEW
}
```

**Add `repair_album` function:**
```rust
pub async fn repair_album(
    paths: &[PathBuf],
    parity_url: &str,
    npar: usize,
    offset: i32,
    tx: tokio::sync::mpsc::Sender<AppMessage>,
) -> Result<String, String>
```

Steps:
1. Download parity bytes from `parity_url` via HTTP GET
2. Decode all tracks to `Vec<i16>` (parallel via `spawn_blocking`, sequential concat)
3. Assemble disc image: prepend STRIDE zeros, append STRIDE zeros
4. Run `CtdbCodec::repair(&mut image, &parity, npar, offset)`
5. If repair succeeds: split image back into per-track segments (using original track lengths)
6. Encode each track to FLAC in `/tmp/tonepoet-ctdb-repair-{pid}/`
7. Copy metadata via `copy_metadata` (reuse from accuraterip.rs)
8. Verify repaired files via CTDB CRC32 (compute per-track CRC, compare against API)
9. Replace originals with backup/restore pattern (reuse from accuraterip.rs offset correction)
10. Return summary or error

### 2. `src/tui/command.rs` — `:ctdb-repair` command

Add `Command::CtdbRepair` variant. Parse `"ctdb-repair"`.

**Handler behavior:**
- If CTDB overlay is open with parity available: extract offset from AR results (if available in the AR cache, or run `:ar` first to get them), then show confirmation dialog
- If no overlay: run CTDB verify first, then auto-repair if parity available and mismatches found

**Offset detection flow:**
1. Check AR cache (`db.get_cached_ar`) for this album's tracks
2. If cached AR results exist with a uniform offset → use that offset
3. If no cached AR results → run AR verification first (set `auto_fix_on_complete`-style flag), get offset from results
4. Default to 0 if AR shows offset +0 or no uniform offset

### 3. `src/tui/app.rs` — confirmation action

Add `ConfirmAction::CtdbRepair { paths, parity_url, npar, offset }`.

### 4. `src/tui/message.rs` — completion message

Add `AppMessage::CtdbRepairComplete { result: Result<String, String> }`.

### 5. `src/tui/draw_overlays.rs` — repair pill

In `draw_ctdb_verify`, add a `:ctdb-repair` pill when:
- Any track has `status == Mismatch`
- `has_parity == true` for at least one track

### 6. `src/tui/keybindings.rs` — pill in mouse list + confirmation handler

Add `:ctdb-repair` to the CTDB overlay's mouse pill list (conditional).
Add `ConfirmAction::CtdbRepair` handler in `execute_confirm_action`.

### 7. `src/tui/event_loop.rs` — completion handler

Handle `CtdbRepairComplete`: set status, close overlay, refresh browse.

### 8. Context menu

Add "CUETools DB repair" to the Verify submenu. Dispatches `Command::CtdbRepair`.

### 9. `src/tui/accuraterip.rs` — make helper functions public

`encode_corrected_track`, `copy_metadata`, and the backup/restore pattern need to be reusable. Either make them public or extract to a shared module.

## Disc image assembly details

For per-track rips with N tracks:
```
let mut image: Vec<i16> = Vec::new();
image.extend(vec![0i16; STRIDE]);  // leadin (11760 i16 values)
for track in tracks {
    image.extend(decoded_track);
}
image.extend(vec![0i16; STRIDE]);  // leadout (11760 i16 values)
```

After repair, split back:
```
let mut offset = STRIDE;  // skip leadin
for (i, original_len) in track_lengths.iter().enumerate() {
    let repaired_track = &image[offset..offset + original_len];
    // encode repaired_track to FLAC
    offset += original_len;
}
```

## Files modified

| File | Change |
|------|--------|
| `src/lib.rs` | Register `ctdb_rs` module (already done) |
| `src/tui/ctdb.rs` | Extend result structs, add `repair_album`, download parity |
| `src/tui/command.rs` | `:ctdb-repair` command with smart flow |
| `src/tui/app.rs` | `ConfirmAction::CtdbRepair` |
| `src/tui/message.rs` | `CtdbRepairComplete` message |
| `src/tui/draw_overlays.rs` | `:ctdb-repair` pill in CTDB overlay |
| `src/tui/keybindings.rs` | Pill in mouse list, confirmation handler |
| `src/tui/event_loop.rs` | Repair completion handler |
| `src/tui/context_menu.rs` | "CUETools DB repair" menu item |
| `src/tui/accuraterip.rs` | Make encode/metadata helpers public |
| `src/tui/help.rs` | `:ctdb-repair` help entry |

## Verification

1. Album with CTDB mismatches + parity: `:ctdb-repair` downloads parity, repairs, re-encodes, verifies, replaces originals
2. Album with no parity: "No parity data available" error
3. Album with no mismatches: "No repair needed"
4. Repair failure (too many errors): "Uncorrectable" error, originals untouched
5. CTDB CRC32 verification passes after repair for all tracks
6. Metadata (tags, art) preserved after re-encoding
7. Multi-disc: repair works per disc
8. Originals backed up to .bak, deleted only after successful verification
