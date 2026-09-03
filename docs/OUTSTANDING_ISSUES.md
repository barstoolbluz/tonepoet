# Outstanding Issues

Running list of diagnosed-but-unfixed issues. Newest at the top. Each entry records the
symptom, the root cause (with code anchors), and the intended fix direction — enough to
hand to a reasoning-model brief without re-diagnosing.

**Status sweep 2026-08-25:** every entry was re-verified against `main @ ec362ee` by reading the code
path (not by grepping for absence — that method produced a false "open" on #7). Each issue carries a
dated verification note; drifted anchors are corrected in those notes rather than rewritten in the
bodies below, so the original diagnosis stays intact. **#1 and #9 are resolved; #7 is mostly
resolved; the rest are open.**

---

## 1. Single-image (taggable) FLAC + sidecar CUE: metadata SAVE rewrites the multi-GB image and embeds a CUESHEET instead of writing the sidecar only

> **✅ RESOLVED by `466cec0` ("Honor CUE source preference on single-image metadata SAVE").** Re-corroborated
> against `main @ 9bb8d51` (2026-08-23): the SAVE authority predicate is now `paths.len()`-agnostic
> (`dedicated_cue_sidecar_authority`, `src/tui/app.rs:9506` = `cue_album_synthetic_sheet.is_some() &&
> cue_source == Sidecar(_)`), so a single-image taggable FLAC + sidecar edits **sidecar-only**: the image
> is byte- and mtime-invariant and no `CUESHEET` is embedded. The metadata-source priority list IS now
> consulted (authority resolves `cue_source` at editor-open). Codified by passing tests
> `single_image_sidecar_save_is_image_byte_and_mtime_invariant` (`keybindings.rs:79570`),
> `single_image_save_authority_flips_only_with_configured_priority` (`:79518`),
> `taggable_single_image_sidecar_refuses_unrepresentable_field_without_io` (`:79741`). Design note: image-only
> / non-CUE-representable edits are now **refused + reverted with a warning** (`sidecar_unsupported`), not
> written to the image — the strict sidecar-only model. **The diagnosis below is retained for history but is
> now STALE (pre-466cec0 anchors); do not implement from it.**

**Discovered:** 2026-08-16, editing metadata at folder level on single-image vinyl rips
(Led Zeppelin UK/JP discographies, `~/torrents/Led Zeppelin - Discography+ (1968 - 2025)/`). Field cases
kept: `UK/1969 - Led Zeppelin II (…Killing Floor Edition)/` (one 1.7 GB 24/192 FLAC + `Led Zeppelin II.cue`),
`JP/1975 - Physical Graffiti (…P-6317 8N)/` (already operated on — its `LP1.flac`/`LP2.flac` now carry an
embedded `CUESHEET`).

**Symptom.** Editing/updating metadata on a **single-image FLAC album with a sidecar `.cue`** (one big
`.flac` = whole album/side, N logical tracks defined by the sidecar) is extremely slow, and **embeds a
`CUESHEET` tag into the FLAC**. The user expected the small sidecar `.cue` to be rewritten (instant) and the
image left untouched. On 24/192 or 32/192 images the full-file rewrite "takes forever." Empirically
confirmed: the JP FLACs the user operated on carry a `CUESHEET` **vorbis comment** holding tonepoet's
*regenerated* sheet (`FILE "…LP1.flac" FLAC` — the sidecar says `WAVE` — plus the editor's per-track titles);
the UK FLAC not operated on the same way has **no** `CUESHEET` comment.

**Root cause — the editor SAVE path's sidecar-only fast path excludes single-image *taggable* albums.** Both
symptoms (slow image rewrite + embedded CUESHEET) share one cause and are verified against source:

- The metadata-editor SAVE is `metadata_editor_save` (`src/tui/keybindings.rs:11721`). It does **not** use the
  `resolve_directory_metadata_groups`/`resolve_aggregate_metadata_target` machinery (that is the **Transfer
  Tags** path). Its fast/slow fork is a single boolean, `sidecar_only`, from
  `cue_sidecar_writeback_plan_for_state` (`:12118`, computed at **`:12315`**):
  `sidecar_only = metadata_editor_dedicated_sidecar_authority(state) && (!metadata_sidecar_authority || required_audio_paths.is_empty())`.
  For a single-image surface (`paths.len() == 1`) this reduces to `metadata_editor_dedicated_sidecar_authority(state)`.
- `metadata_editor_dedicated_sidecar_authority` (`:11163`) = `native_multi_file_sidecar_authority || untaggable_sidecar_authority`:
  - `native_multi_file_sidecar_authority` (`:11133`) is gated on **`surface.paths.len() > 1`** (`:11137`) — false for a single image.
  - `untaggable_sidecar_authority` (`:11145`) requires **every** file to be `FileReadState::Unsupported` (`:11154`) — false for a taggable FLAC.
  So for a single-image **taggable** FLAC, `dedicated_sidecar_authority = false` → `sidecar_only = false`.
- With `sidecar_only == false`, the save takes the image-write branch: `apply_metadata_editor_tag_changes_…`
  writes full per-file tags into the FLAC (`~:12035`/`:12038`), and `metadata_editor_forced_delete_items`
  (`:11471`, which early-returns empty **only when** `dedicated_sidecar_authority` is true, `:11474`) lets the
  synthetic `CUESHEET` row be written to the image via `lofty::tag::ItemKey::Unknown("CUESHEET")` (`:11481`).
  Result: a multi-GB FLAC rewrite that also embeds the regenerated CUESHEET, then the sidecar writeback.
- The surface itself is genuinely sidecar-authoritative: `build_unified_cue_album_sheet`'s single-image branch
  (`:21627`) fires `requires_synthetic_surface` because `metadata_cue_surface_proves_image_content` (`:27497`,
  one carrier owns >1 track) is true, so `cue_source = Sidecar(_)` and `cue_album_synthetic_sheet = Some(_)`.
  The classification is right; only the save-time *authority predicate* is over-narrow.

**Why the priority preference doesn't help.** The user's `aggregate_metadata_target_priority`
(`[IndividualFiles, SidecarCue, EmbeddedCue]`) is **not consulted by the SAVE path at all** — verified: the
only nearby priority reference is inside `metadata_editor_open_tag_transfer_picker` (`~:12969`, the Transfer
Tags path). Save decides purely on the structural authority predicate above.

**Why the user thought it was fixed (gap, not regression).** `git blame`: the `paths.len() > 1` gate was
introduced by **`086ec65`** ("Round-13: multi-file cue-album authority (sidecar-only editing)"), deliberately
scoped to *multi-file* cue albums. The single-image synthetic-surface branch was added later by **`51b7130`**
(clipboard/untaggable-carrier work), whose save-time authority (`untaggable_sidecar_authority`) covers only the
**untaggable-carrier** sub-case. The **taggable single-image + multi-track sidecar** sub-case was routed into the
synthetic surface but never given a matching save-time sidecar-authority predicate. The multi-value Phase 1/2
(`3ae33e5`/`f65d304`) and CUE-authority (`d520f67`) work did not touch this predicate.

**Data.** No source-audio loss, but two undesired side effects: (a) slow full rewrite of large images on every
save; (b) a `CUESHEET` tag is silently embedded into images that had none (already baked into the JP Physical
Graffiti FLACs — reversible later with `metaflac --remove-tag=CUESHEET`, a separate cleanup, not done).

**Fix direction (verified target; DO NOT act before the design discussion).** Make a single-image, **taggable**,
sidecar-backed synthetic album qualify as a dedicated sidecar authority so `sidecar_only` becomes true and the
FLAC is not rewritten: the predicate consumed at `:12315` should recognize `cue_album_synthetic_sheet.is_some()`
+ `cue_source == Sidecar(_)` + `metadata_cue_surface_proves_image_content`, **regardless of `paths.len()`** — the
`paths.len() > 1` gate at `:11137` is the specific over-narrow condition. Preserve two carve-outs: (a) genuine
per-file edits that are **not** CUE-representable must still be refused/warned (the `sidecar_unsupported` guard,
`~:11891`) so real per-file tags aren't silently dropped; (b) `required_audio_paths` /
`metadata_editor_audio_tag_changes_required_for_sidecar_writeback` (`:12322`) should still allow an image write
when the user edited a field that only lives in the image, not the CUE. A test (`keybindings.rs:69860`,
`single_image_sidecar_albumartist_edit_clear_and_delete_round_trip`) currently **codifies the wrong behavior**
(expects image-write-then-sidecar; never asserts `plan.sidecar_only`) and must be re-pointed.

**Open design questions (discuss with the user before briefing):**
1. **Scope of the fix.** Just close the single-image-taggable gap (minimal, targeted), or the deeper question:
   should the editor SAVE path honor `aggregate_metadata_target_priority` the way Transfer Tags does, so the
   preference is uniformly binding across both paths? The latter is larger and lands squarely in the LODESTAR
   area.
2. **Should embedding a CUESHEET into a taggable image ever be default behavior?** Today it happens as a side
   effect of the slow path; under the fix it would stop for single-image sidecar albums — confirm that is the
   desired outcome (vs. an opt-in "also embed CUESHEET" mode).
3. **Cleanup of already-embedded CUESHEETs** (the JP FLACs) — separate task; decide whether tonepoet should
   offer to strip them or leave manual.

---

## 2. Confirmation dialog is fixed-height (9 rows) — long recovery prompts clip their text and buttons

> **RESOLVED 2026-08-30.** `draw_confirmation` now sizes to its wrapped content instead of
> a fixed height. A new `wrapped_row_count` helper (`src/tui/draw_overlays.rs:1526`) counts
> the rows a word-wrapped paragraph needs — ratatui 0.26's own `Paragraph::line_count` is
> private, and the pre-existing `wrap_to_visual_rows` is character-level, which would pack
> more per line than `Wrap { trim: true }` does and undersize the popup. Height is now
> `message_rows + chrome` (two borders, one button row, plus the consent checkbox row when
> present), with the former fixed heights kept as **floors** so short prompts are unchanged,
> clamped to the terminal.
>
> Measured on the archive startup-recovery message this issue was filed against: it needs
> **14 rows at width 50**, and the old fixed height gave it **7**. It now renders complete —
> message, `Conflict:` line, final `startup.` and both buttons — in an 18-row popup.
>
> Regressions: `long_confirmation_message_is_not_clipped` (renders the real prompt shape and
> asserts its tail and buttons survive) and `wrapped_row_count_matches_hard_and_soft_breaks`
> (hard newlines, word-boundary wrapping, blank lines, over-long words). Gate run 1 green,
> 6496 passed.
>
> **Residual, deliberately not fixed:** a message taller than the terminal still clips,
> because the paragraph has no scroll offset — the popup clamps to the screen rather than
> overflowing it. Adding scroll state to the confirmation overlay is a larger change than
> the fixed-height defect filed here.
>
> Note the earlier `STILL OPEN 2026-08-29` correction below is now itself superseded; it was
> accurate when written, and recorded that `c4bab5b`'s commit message wrongly claimed to
> resolve this issue.

> **STILL OPEN 2026-08-29 — re-verified after `c4bab5b`.** That commit's message claims it
> "resolves OUTSTANDING_ISSUES #2". **That claim is wrong.** `draw_confirmation`
> (`src/tui/draw_overlays.rs:1519`) now sizes with
> `let popup_h = if cue_consent { 14u16 } else { 9u16 };` (`:1536`) — two fixed heights
> instead of one, not content measurement. The new CUE-consent dialog gets 14 rows; every
> other confirmation, including the four-button startup recovery prompt this issue was
> filed against, still gets 9. The function still contains no line counting, no wrap
> measurement, and no scroll offset, so overflow clips exactly as before.
>
> **Verified STILL OPEN 2026-08-25** (read, not grepped). `draw_confirmation` is now at `src/tui/draw_overlays.rs:1519`; the fixed height is `centered_rect(popup_w, 9, area)` at `:1532`. Width DOES adapt (`50.max(footer_w+4).min(area.width-2)`); height has no content measurement, the message `Paragraph` has `Wrap` but **no scroll offset** so overflow is clipped, and the buttons are a single `Constraint::Length(1)` row with no wrap. Anchor `:1428` in the text below has drifted.

**Discovered:** 2026-08-09, on a startup archive-recovery prompt in a second tonepoet instance.

**Symptom.** The startup "resume" prompt surfaces four buttons (`Y resume` / `N discard…` /
`D discard…` / `Esc later`) but the explanatory text describing what each option does is cut
off — the dialog box is too small to show the message adequately.

**Root cause.** `draw_confirmation` (`src/tui/draw_overlays.rs:1415`, sizing at ~1428):

```rust
let popup_w = 50u16.max(footer_w.saturating_add(4)).min(area.width.saturating_sub(2).max(1));
let popup = centered_rect(popup_w, 9, area);   // height is a hardcoded constant 9
```

- The **width** auto-grows to fit the buttons (`footer_w + 4`, clamped to terminal width), but
  the **height is always 9**. After the border, ~6 rows remain for the message, rendered with
  `Wrap { trim: true }` — anything past ~6 wrapped lines is silently clipped off the bottom.
- The offending prompt is `ARCHIVE_STARTUP_RECOVERY` (`src/tui/app.rs:10951`), the only confirm
  with **4 buttons**. Its message (`archive_startup_recovery_prompt_message`,
  `src/tui/app.rs:11031`/`:11036`) is the app's longest: recovered path + staging path + edits
  summary + conflict + a four-sentence "what each key does" block. That tail is exactly what
  overflows the fixed 9-row box.
- On a narrow terminal the four button pills on a single row (`Constraint::Length(1)` for the
  button line, ~1439) can also clip horizontally even though `popup_w` tries to grow.

**Nuance (not the fix, but context).** The file-task (cut/copy) failure that prompted the
restart did **not** itself open this dialog. File-task startup recovery is *silent* — it queues
the interrupted job for auto-reconciliation and writes a status line (`src/tui/app.rs:12255`–
`12285`), no confirm dialog. The 4-button prompt is the separate **archive staged-edits** startup
recovery; the second instance also had a pending archive session waiting.

**Fix direction.** Make `draw_confirmation` **size to content** instead of a fixed 9:
- Measure the wrapped message height at the chosen width, add button row(s) + borders, clamp to
  terminal height, and grow (or scroll) rather than clip.
- Let the button row **wrap/stack** when 4+ pills don't fit one line, keeping the click-rect
  recording (`confirm_rect` / `cancel_rect`, ~1489–1500) in sync with the wrapped layout.
- General fix — every confirmation dialog benefits, not just archive recovery.

---

## 3. `current_exe()`-deleted → cryptic ENOENT when a file op runs from a pre-rebuild TUI

> **Verified STILL OPEN 2026-08-25** (read, not grepped). **Anchor correction:** `current_exe()` is at `src/tui/keybindings.rs:49710`, not `:44365` — and its `Err` arm (`:49712`) is **NOT** the failing path, because `current_exe()` *succeeds* on a deleted binary and returns `"<path> (deleted)"`. The real failure is downstream at `command.spawn()`, whose handler formats the raw `"start isolated file-task helper: {error}"` at **`:49753`**. No `(deleted)` detection and no un-suffixed-path fallback exist. Both fix directions unimplemented.

**Discovered:** 2026-08-09, on a Ctrl+X (cut/move) in a stale tonepoet instance.

**Symptom.** A copy/move (paste or cut) fails immediately with:

```
Status: start isolated file-task helper: No such file or directory (os error 2)
```

**Root cause.** The process-isolated file-task engine runs its worker by **re-executing tonepoet
itself**:

```rust
let executable_result = std::env::current_exe();          // src/tui/keybindings.rs:44365
let executable = match executable_result { Ok(e) => e, Err(error) => { /* "resolve
    tonepoet executable for file task: {error}" — a DIFFERENT error path, :44369 */ } };
Command::new(executable)                                    // :44378
    .arg("__file-task-worker").arg("--journal").arg(journal.path())
    .spawn()                                                // :44385 → ENOENT
    // Err arm: format!("start isolated file-task helper: {error}")   // :44389
```

`current_exe()` reads `/proc/self/exe`. If the running binary's on-disk file was **replaced or
removed after the process started** (e.g. `cargo build` while the TUI is still open), that link
resolves to `"<path> (deleted)"`. Note the failure is at the **`.spawn()`** (`:44385`), **not** at
`current_exe()` — `current_exe()` *succeeds* and returns the `"<path> (deleted)"` string; it is
`Command::new(that path).spawn()` that returns `os error 2` (ENOENT) because no file exists at that
literal path. That is why the user sees `start isolated file-task helper:` (`:44389`) rather than the
`resolve tonepoet executable for file task:` message (`:44369`).

**Field reports (recurring):**
- 2026-08-09 — original, a stale instance's `/proc/<pid>/exe` at `…/target/release/tonepoet (deleted)`.
- 2026-08-13 — Ctrl+X cut/paste, `~/temp` (ext4) → `~/temp/external` (an NTFS bind mount). Same error
  verbatim. **Confirmed live at report time:** the user's running TUI **pid 842486** had
  `/proc/842486/exe → /home/daedalus/dev/tonepoet/target/release/tonepoet (deleted)` (its release binary
  had been rebuilt by this session's gate/build runs). The **ext4→NTFS bind-mount detail is a red
  herring** — the spawn fails before any filesystem work touches the destination, so the source/dest
  filesystems are irrelevant to this error; it is purely the rebuilt-while-running binary.

This is the same **recompile-while-running** hazard family as the parked config-browsing-reset-on-
recompile bug — an old process referencing on-disk state a rebuild pulled out from under it.

**No data loss.** The spawn fails before any file work, so a *move* here is a safe no-op — source
stays intact, destination is never created. (Verified on the field case.)

**Immediate operational workaround.** Don't run file operations from a TUI instance whose binary
was rebuilt underneath it; relaunch tonepoet after any rebuild.

**Fix direction.**
1. **Actionable error** — detect the `(deleted)` suffix (or ENOENT on this specific spawn) and
   surface *"the running tonepoet binary was replaced on disk (rebuild while running) — restart
   tonepoet to resume file operations,"* instead of the raw `os error 2`.
2. **Optional fallback** — if `current_exe()` resolves to a `(deleted)` path but a real file now
   exists at the un-suffixed path, spawn that (a rebuild leaves a valid new binary there). Has a
   mild worker-protocol version-skew nuance — let the reasoning model weigh whether it's worth it.

---

## 4. Metadata stage `tool timed out after 30s` on a large multi-track conversion

> **Verified STILL OPEN 2026-08-25** (read, not grepped). **Sharper than filed:** the metadata dispatcher already tiers timeouts by binary (`stages.rs:6064-6067`) — **ffmpeg 60s, everything else 30s** — so metaflac/opustags/wvtag all get 30s while the FLAC art embed nearby gets 90s (`:6220`). The failing Donna Summer case is FLAC, i.e. **metaflac at 30s**. So this is not one fragile default but an ad-hoc 30/60/90 scheme in which the heaviest real-world path drew the shortest timeout. Still no scaling by source size and no config.

**Discovered:** 2026-08-12, on a Donna Summer conversion. Likely explains an earlier failure of a
7-LP Allman Brothers box-set conversion (a few weeks prior) — same class (large, many-track, heavy
concurrent load).

**Symptom.** The conversion fails at the metadata stage with:
`Metadata: tool error: tool timed out after 30.002571643s`

**Problematic source (kept for reproduction):**
`~/temp/external/Donna Summer - Bad Girls (1979) [FLAC] {Japan Victor VIP 9565-6 LP  24-192} [bazar]/`
— a 15-track album (~3.0 GB of 24/192 FLACs, one file per track + sidecar `.cue`) on the
`/dev/sdc2` **fuseblk** mount, alongside **enormous artwork** (`GF-Front.jpg` = 175 MB / **14796×7392 ≈ 109 MP**,
`GF-Inside.jpg` = 179 MB / 14774×7389, plus 45–47 MB inserts). Likely same-class earlier case (also present
under `~/temp/`):
`The Allman Brothers Band - The 1971 Fillmore East Recordings (1971) [FLAC] {US Mercury B0020496-01 LP DSD256}`
(a 7-LP set — many tracks, DSD256).

**What was ruled OUT (measured — don't re-chase these):**
- *Slow storage:* no — the fuseblk source reads at **2.8 GB/s**; a 179 MB copy is **~0.1 s**; staging/dest
  are local `ext4` (`/dev/sda4`).
- *The companion-folder copy (`companion_folders` preset):* **cannot** be this error — `copy_companion_artifacts_*_best_effort`
  (`src/convert/pipeline/stages.rs:~29370`) is a synchronous `fs` copy with **no subprocess and no timeout**;
  `tool timed out` is structurally a `ToolRunnerError` (`errors.rs:83`) from a spawned tool. (Copying the
  ~470 MB of art to local ext4 is also sub-second.)
- *The FLAC artwork embed:* not it — it's `ffmpeg` with `-c:a copy -c:v copy` (stream-copy, no decode) and a
  **90 s** timeout (`cue_artwork_embed_command`, stages.rs:~5494); reproduced at **1.5 s** on the 109 MP JPEG,
  ffprobe on it **0.6 s**.

**Working root-cause hypothesis (unconfirmed).** The `30.002571643 s` is a **30 s-bounded metadata tool**
(ffprobe duration `stages.rs:1619`, wvtag, a tag-write, or the generic `_ => Duration::from_secs(30)`
`stages.rs:5408`) — not the copy and not the 90 s FLAC art embed. Every candidate is fast **in isolation**,
so the timeout most plausibly arises from **concurrent-load starvation** during the real many-track run (many
simultaneous resample/encode/tag processes on a large box set starving a 30 s-budget tool), or a tool getting
stuck. Not reproducible from a single isolated call — needs an **instrumented full re-run** (trace logging on
the exact conversion) to name the precise tool + args.

**Fix direction.**
1. **The fixed 30 s default tool timeout is too aggressive** for large/complex sources under load — a
   legitimately-busy tool shouldn't be killed at 30 s. Consider raising the default and/or scaling it by
   source size / making it configurable (the FLAC art embed already uses 90 s; the 30 s default is the
   fragile one).
2. **Confirm the exact failing tool** via an instrumented re-run before changing behavior.
3. **(Separate, independent)** 109-megapixel embedded artwork is pathological — a sanity cap/downscale on
   *embedded* art is worth considering, but the user wants companion *copies* to bring over all content
   verbatim; keep those concerns separate.

---

## 5. DSD-to-PCM auto-gain inflates DC-bias readings and flips `negligible`→`significant` — the DC threshold is absolute (level-dependent), not a conversion defect

> **Verified STILL OPEN 2026-08-25.** **Anchor correction:** the absolute classification is at `src/tui/draw_overlays.rs:4094` and `:4099` (not `:4093`/`:4098`). `analyze.rs:394` is still correct. Unchanged.

**Discovered:** 2026-08-13, converting Charles Mingus *Blues & Roots* SACD → FLAC (SoX rate `-u`, DSD64→176.4k, DSD→32-bit int, DSD gain **auto**, margin 0.15 dB).

**Symptom.** With DSD auto-gain **enabled**, every track's DC-bias reading rises and some flip to `significant!`; with auto-gain **disabled**, all read `negligible`. Consistent across bit depths, sample rates, and SoX quality (ultra/insane). The redbook CD of the same album reads `negligible`. Raising the auto-gain margin (0.15→0.5 dB) drops one problem track just under the line (0.000996) but doesn't eliminate the effect. Example (track 1): no-gain DC `0.000353` (peak −10.1 dBFS) → auto-gain DC `0.001114` **significant** (peak −0.1 dBFS); redbook DC `0.000706` negligible (peak −4.1 dBFS).

**Root cause — NOT a conversion bug; it's linear scaling + an absolute threshold.**
- DC bias is measured as the mean sample value in **absolute full-scale units**: `let dc_bias = dc_sum / sample_count` (`src/tui/analyze.rs:394`). That is a **linear** property of the signal.
- DSD auto-gain applies a **linear level gain** (peak-normalize to −margin). DSD→PCM here comes out low (peaks −8 to −10 dBFS), so auto-gain applies ~**+8 to +10 dB**, pushing peaks to ~−0.1 dBFS. Gain constants: `dsd_to_pcm_gain_db` / `dsd_to_pcm_auto_gain_margin_db` (`src/convert/pipeline/stages.rs:~9295`-`9296`).
- Multiply every sample by gain G and the mean (DC) multiplies by G too — so the **absolute** DC scales by exactly the gain. **Proof from the field data:** per-track DC ratio (auto/no-gain) matches the gain factor to rounding (T1 3.16 vs 3.16; T4 3.20 vs 3.20; T5 2.55 vs 2.57). And **DC-relative-to-RMS is identical** across no-gain DSD, auto-gain DSD, *and* the redbook CD (per track: T1 −42.6 dB, T2 −41.3 dB, T5 −44.5 dB). So the DC offset is **intrinsic to the master** (the CD carries the same DC-to-signal ratio), not created by DSD→PCM or the gain stage — auto-gain merely amplifies it along with everything else.
- The `negligible`/`significant` label uses an **absolute** threshold: `if r.dc_bias.abs() < 0.001` (`src/tui/draw_overlays.rs:4093` and `:4098`). Because absolute DC scales with level, louder-normalized content trips the fixed 0.001 line while identical quieter content (and the ~6–10 dB quieter redbook) stays under it. The analyzer's measurement is correct; the classification is level-dependent.

**Fix direction.**
1. **Make the DC-bias classification level-relative** so identical audio classifies consistently regardless of normalization: classify by DC relative to RMS/peak (e.g. "DC is N dB below RMS", or set the negligible/significant threshold relative to track RMS) rather than the absolute `< 0.001`. Under a relative basis, no-gain DSD, auto-gain DSD, and redbook all classify the same — the correct outcome. Keep **displaying** the absolute DC number too (absolute DC is the real headroom cost).
2. **(Separate, optional, policy — not a bug fix)** an opt-in **DC-block** (sub-sonic high-pass ~1–5 Hz / DC removal) in the DSD→PCM path would strip the master's DC before it's amplified. But the DC is in the source master, so this is a deliberate signal alteration (a toggle), not a correctness fix.

---

## 6. Cross-process DB init lock can time out during a schema migration under heavy concurrent load (self-recovering, no corruption)

> **Verified STILL OPEN 2026-08-25.** Acquisition is still poll-based `fs2::FileExt::try_lock_exclusive` (`src/db.rs:714`, and a second site at `:981`), not a blocking `lock_exclusive`. **Anchor correction:** the timeout/message now live at `src/db.rs:725` under `DB_OPEN_INIT_LOCK_WAIT_LIMIT` (not `:322`-`326`), and the wording is `"timed out after {} ms waiting for {mode} database initialization lock <path>"`.

**Discovered:** 2026-08-13, during the adversarial audit of the multi-value Phase-1 cross-session DB-open hardening (`src/db.rs`, committed on `hardening` @ `3ae33e5`). Not observed in the field — a code-review finding on the new lock path, filed for completeness.

**Symptom (predicted, not yet field-seen).** A metadata-journal DB open fails with
`timed out after 30000 ms waiting for exclusive database initialization lock <db>.open-init.lock`
(`src/db.rs:322`–`326`). The affected process's open errors out; **no data is corrupted** and a retry succeeds once contention clears.

**Root cause — flock has no writer preference + a poll-based exclusive acquire.** The new
cross-process design coordinates journal-DB opens via an adjacent sidecar lock
`<db>.open-init.lock` with fs2 advisory locks (`acquire_open_init_file_lock`,
`src/db.rs:293`–`339`):

- **Steady state** (DB already WAL + `user_version == CURRENT_VERSION`, i.e. v23): opens take a
  **shared** lock and do a microsecond-scale readiness check
  (`file_backed_database_is_initialized`, `src/db.rs:345`–`356`). Many processes hold the shared
  lock concurrently. This is the hot path and is not affected.
- **First-open / non-WAL / migration** (e.g. a v22→v23 schema bump right after a tonepoet
  upgrade): one process must take the **exclusive** lock to migrate alone.

The exclusive lock is acquired by **polling** — `try_lock_exclusive` in a loop with 25 ms sleeps
up to a 30 s cap (`DB_OPEN_INIT_LOCK_WAIT_LIMIT`/`DB_OPEN_INIT_LOCK_RETRY_DELAY`,
`src/db.rs:160`–`161`; loop at `:310`–`338`). Under `flock(2)`, a pending `LOCK_EX` request does
**not** block newly-arriving `LOCK_SH` grants (no writer queueing/preference). So if many other
tonepoet processes keep taking brief shared locks (each just for its readiness check), they can
keep the DB from ever being shared-lock-*free* for the instant the exclusive waiter needs — the
migrating process polls, never finds a gap, and times out at 30 s.

**Why it's low severity (why it did not block the commit).**
- Occurs **only** during first-open or a real schema migration — never on the steady-state v23
  path that runs day-to-day.
- Each shared holder holds the lock for microseconds, so a shared-lock-free instant within 30 s
  is overwhelmingly likely; it takes a genuinely dense storm of process launches landing exactly
  during an upgrade to starve the waiter.
- **Self-recovering** — the losing process just errors and can retry; the DB is untouched.

**Fix direction (either suffices; neither is urgent).**
1. Switch the exclusive acquisition from poll-based `try_lock_exclusive` to a **blocking**
   `lock_exclusive` with a watchdog/deadline. A blocking `LOCK_EX` waiter is queued by the kernel
   ahead of subsequently-arriving shared requests, eliminating the starvation entirely — at the
   cost of needing a separate timeout mechanism (a watchdog thread or a blocking-lock-with-timeout
   wrapper) since fs2's blocking call has no deadline.
2. Or consciously **accept and document** the 30 s-timeout-then-retry behavior for the rare
   post-upgrade migration window, and surface a clearer, retry-suggesting error message at
   `src/db.rs:322`.

---

## 7. File-task startup recovery auto-replays every pending operation with no prompt and no supersession

> **MOSTLY RESOLVED — re-verified 2026-08-25 by reading the path (an earlier grep-based pass wrongly
> called this fully open).**
>
> - **(b) auto-replay without a prompt — FIXED.** Recovered journals no longer enter the execution
>   queue. They land in a separate `file_transfers.recovery_queued` **review** queue (`app.rs:12487`),
>   drained only by explicit command: `:recovery-resume` moves one entry to `file_transfers.queued`
>   (`keybindings.rs:44249-44255`), `:recovery-defer` removes one (`:44186-44190`); commands are
>   `Command::FileRecoveryResume` / `FileRecoveryDefer` (`command.rs:3342`/`3344`, parsed at `:3876`).
>   The startup status line enumerates them: *"N exact journal reconciliation job(s) await explicit
>   review (use :recovery-resume [id] or :recovery-defer [id])"*. The filed symptom — everything
>   replaying unasked — can no longer occur.
> - **(4) journal GC — FIXED.** `retire_terminal_clean` (`file_task_runtime.rs:690`) deletes a journal
>   only when it is `Completed | Reconciled` with no pending mappings, temp artifacts, quarantines or
>   rename intents, and runs on the job-completion path after `child.wait()` (`keybindings.rs:49948`).
>   This bounds the directory growth that had reached 101 journals.
> - **(a) supersession — STILL OPEN.** No supersession logic exists in the file-task surface.
>   Multiple journals for the same source+destination pair still each appear as their own review
>   entry, so the reviewed list is not the minimal correct set. **Severity is much reduced** now that
>   (b) is fixed: superseded entries no longer execute, they only clutter the review list and can be
>   `:recovery-defer`red. Cosmetic/ergonomic residual, not the data hazard originally filed.
>
> **Anchor corrections:** `startup_file_task_recovery_inventory` is at `file_task_runtime.rs:1650`
> (not `:1212`); `file_task_journal_dir` at `:1912` (not `:1309`); the `recoveries.last()` clipboard
> special-case at `app.rs:12464` (not `:12367`).

**Discovered:** 2026-08-13, restarting tonepoet after the several Ctrl+X cut/copy operations that
had failed with the `current_exe()`-deleted helper-spawn error (issue #3). On relaunch, **all** of
those failed operations were automatically re-queued and began replaying without the user being asked.

**Symptom (field report).** After restart, the file-task recovery re-triggered every copy/cut that
had failed in the stale instance. Two distinct problems: (a) some of the recovered operations should
have been recognized as **superseded** (a later operation logically replaced an earlier one) and not
replayed; (b) the replay **kicked off automatically** — the user was never prompted, and never shown
a list of exactly which operations were teed up for replay.

**Root cause — every reconstructable journal is auto-enqueued into the serial reconciliation queue.**
At startup, when `startup_options.recover_pending_file_operations` is set (the default),
`AppState` construction (`src/tui/app.rs:12361`–`12400`) calls
`file_task_runtime::startup_file_task_recovery_inventory()` (`src/tui/file_task_runtime.rs:1212`),
which returns **every** pending (non-terminal / `needs_reconciliation()`) journal — by design
(`startup_recovery_inventory_surfaces_every_pending_journal`, `file_task_runtime.rs:2281`). The loop
at `app.rs:12383`–`12400` then pushes **each** recovery into `file_transfers.queued` with
`recovered: true`, which the serial transfer dispatcher processes **automatically**. There is:
- **No confirmation prompt.** The enqueue happens directly during state construction — no dialog,
  just a status line. (Contrast: the *archive* staged-edits recovery **does** prompt — the 4-button
  `ARCHIVE_STARTUP_RECOVERY` dialog, see issue #2. File-task recovery has no equivalent.)
- **No supersession / dedup.** Every pending journal is enqueued independently; nothing collapses two
  journals for the same source→destination, or drops an operation a later one logically superseded.
  Note the code *does* single out `recoveries.last()` (the newest) as the clipboard/retry surface
  (`app.rs:12367`–`12373`) — but then still enqueues **all** recoveries, newest included, so the
  "newest is current" intuition and the "replay everything" behavior coexist inconsistently.

**Risk.** Not necessarily data loss, but auto-replaying stale/superseded operations can produce
unwanted copies/moves and **destination-exists conflicts** (e.g., the same source cut/copied twice,
or replayed into a destination that a later successful operation already populated). The user has no
chance to veto before the filesystem work begins.

**Fix direction.**
1. **Prompt, don't auto-execute.** Gate file-task startup recovery behind a confirmation surface
   (mirroring the archive-recovery prompt, issue #2) that **lists exactly which operations are teed
   up for replay** — source → destination and kind (copy/cut) per item — and lets the user replay
   all, replay a selected subset, or discard. Do not enqueue into the serial dispatcher until the
   user chooses. (Design note: the fixed-height confirmation dialog in issue #2 would need the
   size-to-content fix to list many operations.)
2. **Supersession / dedup before enqueue.** Collapse or drop journals that are logically superseded —
   e.g., multiple journals for the same source+destination pair, or an operation a later journal
   overrides — so the replay list is the minimal correct set, not "every journal ever left pending."
3. Reconcile the `recoveries.last()`-as-clipboard special-case (`app.rs:12367`) with whatever the
   prompt presents, so the newest op isn't both "the clipboard" and "just another queued replay."
4. **Journal garbage-collection (compounding factor, field-confirmed 2026-08-14).** The journal
   directory `file_task_journal_dir()` (`file_task_runtime.rs:1309`) =
   `~/.config/tonepoet/file-operation-journal/` — one append-only `.jsonl` per file-task job — is
   **never pruned**: terminal (Completed/Reconciled/Cancelled) journals accumulate indefinitely.
   Field state had **101** journals (plus `.abandoned` markers) built up since 2026-08-06, so the
   startup `startup_file_task_recovery_inventory()` scan (`file_task_runtime.rs:1212`) re-chews a
   large stale set every launch and surfaces the recovery prompt for any left non-terminal. Manual
   workaround: with tonepoet closed, delete/rename that directory (it is recreated empty). Real fix:
   GC terminal journals after their reconciliation completes (and cap/retire abandoned ones), so the
   scan set stays bounded and the prompt only reflects genuinely-pending work.

---

## 8. Containerless / untaggable outputs (raw PCM, DFF, W64, raw AAC) handle metadata THREE inconsistent ways — need a unified policy

> **Verified STILL OPEN 2026-08-25.** The grab-bag is intact: `w64` -> `MetadataError::PolicyRejected` (`stages.rs:6044`) and `_ => MetadataError::UnsupportedTagFormat` (`:6062`); no `StageOutcome::SkippedWithReason` unification. **Anchor corrections:** `metadata_tag_command` is at `stages.rs:6028` (not `:5386`); `MetadataPersistenceBackend::UnsupportedDff` at `metadata_persistence.rs:699`; `SkippedWithReason` at `types.rs:2776`. NOTE: the Phase-5 multi-value work added an RF64 metadata **fail-closed** policy, which commits that format toward reject-early rather than skip-and-label — reconcile with fix direction 1 before briefing.

**Raised:** 2026-08-14 (reasoning-model observations during Phase-4 pipeline work — raw PCM, then DFF).

**Symptom.** Several conversion *output* formats have **no (usable) tag container**, yet the pipeline handles their metadata **three different, surprising ways** — and none of them tells the user "this output is metadata-free":

| Output | Today's behavior | Surprise |
|---|---|---|
| **raw PCM** (`pcm`/`raw`/`s8`/`u8`/`s16le`…`f64be`, headerless LPCM) | metadata stage reports **SUCCESS** (silently drops tags) | over-reports success; metadata silently lost |
| **DFF** (DSDIFF `.dff`), with materializer-authoritative metadata | **fails LATE** via `UnsupportedTagFormat` at the metadata stage — *after* the expensive DSD encode | wastes the encode, then hard-errors |
| **raw AAC** | **fails** via `UnsupportedTagFormat` | hard-error |
| **W64** | **fails** via `PolicyRejected` (`W64MetadataMutationUnqualified`) | hard-error (but at least reasoned) |

So the same underlying situation — an output with nowhere to put tags plus metadata that wants writing — produces silent-success in one case and late hard-failure in others.

**Root cause / context.** The metadata tool dispatcher `metadata_tag_command` (`src/convert/pipeline/stages.rs:5386`) maps flac/opus/wv/mp3/m4a/wav/aiff to real writers, `w64` → `MetadataError::PolicyRejected`, and **everything else (incl. DFF and raw AAC) → `MetadataError::UnsupportedTagFormat`** — a *late* write-stage error. Raw PCM is short-circuited upstream to a clean metadata-stage success instead. There is **no qualified DFF tag writer** (the editor backend is literally `MetadataPersistenceBackend::UnsupportedDff`, `metadata_persistence.rs:645`; "DFF tag writing" is a documented non-goal — DFF *can* technically hold DIIN/COMT chunks but tonepoet writes none), and building one is out of scope. tonepoet already has the honesty vocabulary — `StageOutcome::NotRequested` / `SkippedWithReason(String)` (`types.rs:2563`, surfaced by `reporter.rs:483/501`) — it just isn't used consistently for containerless outputs.

**No data loss** (source untouched), but the behavior is inconsistent: some outputs silently drop-with-success, others hard-fail late (DFF after wasting the encode).

**Fix direction — one unified policy for the whole containerless/untaggable family (raw PCM, DFF, W64, raw AAC, and any future one):**
1. **Default = skip-and-label-metadata-free, honestly.** Report the metadata stage as `StageOutcome::NotRequested` / `SkippedWithReason("<FORMAT> output has no tag container")`, **produce the output**, and label it **"metadata-free"** in the UI/log. This replaces raw PCM's silent success AND DFF/AAC's late hard-error with one predictable outcome. Do **not** invent DFF/PCM tagging machinery. (Mostly wiring — the outcome variants and reporter handling exist.)
2. **Strict / must-tag mode = reject EARLY (opt-in).** For users who require tags to be written, gate at *planning* time so a containerless-output-with-metadata conversion is refused **before** the encode (a clear "DFF/raw PCM can't hold tags" message), not late after wasting the DSD/PCM encode. This preserves fail-closed for those who want it, moved up-front. (Replaces DFF's current late error as the default.)
3. **Optional metadata sidecar = keep the tags alongside (opt-in, off by default).** Since these formats can't embed tags, write a **companion sidecar**: a **CUE sidecar** is the natural carrier (standard for headerless/raw audio; tonepoet already *generates* CUE sheets via `crates/tonepoet-features/src/cue_generator.rs` **and** already reads sidecar CUEs as conversion sources, so it round-trips), **or** a lighter `.json`/`.txt` sidecar, **or** lean on the conversion log (already captures per-track metadata). Gate behind a config toggle / CLI flag (e.g. `--metadata-sidecar`).

The key is a **single, predictable rule** across the family instead of today's grab-bag (raw PCM silently succeeds, DFF late-errors, W64 policy-rejects).

---

## 9. Multi-cue folder chooser offers a structurally-unmaterializable CUE as an equal "viable" option; materialize then dead-ends with an opaque error

> **⚠ CORRECTION (2026-08-26): the note below was WRONG, and is superseded.** The structural screen it
> credits required a *strictly greater* index at each `FILE` boundary. Every malformed CUE encountered in
> the field — including this issue's own named reproduction case, `Physical Graffiti All LP's.cue` — has an
> **equal** boundary (the author offsets each file by the previous file's last track start rather than its
> duration). The screen therefore **never fired on any real file**, verified by running the production
> `inspect_split_cue_folder_members` against them. The certifying fixture used a greater boundary, and a
> second test asserted the equal case must *not* be rejected, so the suite was green throughout. The
> lesson: passing tests plus verified anchors are not evidence a defect is fixed when the fixtures encode
> a shape the field case does not have.
>
> **Genuinely resolved 2026-08-26** by the cumulative-CUE detection work, which rejects the equal-or-exceeding
> boundary while still failing open for a legitimate small non-zero reset. Verified against all three real
> files (`BBC Sessions`, `Physical Graffiti`, `The Song Remains The Same`) — all now rejected — and
> field-confirmed by a successful conversion through the synthetic album view.
>
> The superseded note follows, kept for history:
>
> **✅ RESOLVED by `2b52c0a` ("Editor foreground fix + multi-CUE chooser + Browse-responsiveness async refactor"),
> Part 2.** Re-corroborated against `main @ a675fef` (2026-08-25). Both gaps are closed:
>
> *(b) — the chooser no longer offers it as an equal option.* A probe-free **structural pre-screen** now runs at
> selection time: `cross_file_cumulative_index_rejection` (`src/convert/split_cue_album.rs:1013`) compares exact
> `INDEX 01` frame integers **at a resolved image boundary** and rejects a later image whose timeline continues
> the previous one (`SplitCueMemberRejectionReason::CrossFileCumulativeIndex`, `:667`). It is deliberately narrow
> and **fails open on any uncertainty**: it requires ≥2 tracks in the preceding image block, is strictly `>`, and
> bails when track/audio/key counts disagree or an `INDEX 01` is missing. `inspect_split_cue_folder_members`
> (`:976`) partitions candidates into `viable` / `rejected`, and the Browse "Advanced CUE choices…" prompt
> (`context_menu.rs:1241` → `keybindings.rs:10774`) is built from that inspection, so a known-bad cue is surfaced
> as rejected rather than as an equal choice. **Canonical member admission is intentionally unchanged** — copy-tags
> and explicit-CUE consumers still see the same admitted member; only *folder* selection policy rejects.
>
> *(1) — the materialize-time error now explains the cause.* The boundary guard is retained as a backstop and its
> message now names the mechanism (`materializer_cue.rs:2293`): "…the CUE's FILE-local timeline exceeds this image.
> In a malformed multi-FILE CUE, a common cause is that later FILE sections use cumulative timestamps instead of
> resetting for each image; try the per-disc CUEs if available".
>
> *(3) — folder expansion degrades instead of dead-ending.* `queue_expansion.rs:1029-1055` drops the malformed cue
> and **preserves the surviving folder CUE set for automatic album assembly**, warning "Ignored malformed cumulative
> multi-FILE CUE … Use CUE choices… from Browse to inspect or override the folder policy." An explicit user choice
> is still honored (different wording, same drop). The `authoritative_failed_merged_groups` exception
> (`queue_expansion.rs:1158`) keeps a redundant *superset* cue from failing closed a group that the per-side cues
> already prove.
>
> Codified by passing tests: `cumulative_cross_file_timeline_is_rejected_only_by_folder_policy`
> (`split_cue_album.rs:2168`), `cross_file_equal_index_is_not_rejected_by_strict_cumulative_screen` (`:2240`),
> `single_track_previous_image_does_not_trigger_cumulative_screen` (`:2264`),
> `cumulative_combined_superset_does_not_fail_closed_a_proven_per_side_album` (`queue_expansion.rs:3529`),
> `only_cumulative_combined_cue_falls_back_to_audio_with_clear_warning` (`:4680`),
> `genuine_overlap_ambiguity_keeps_cumulative_alternative_visible_but_rejected` (`:4712`),
> `copy_tags_keeps_canonical_admission_for_cumulative_combined_cue` (`context_menu.rs:4795`),
> `metadata_folder_open_drops_cumulative_combined_cue_and_keeps_per_side_surfaces` (`keybindings.rs:75139`).
> Fix direction 4 ("avoid full probe-at-selection boundary validation") was honored — the screen is purely
> structural. **The diagnosis below is retained for history but its anchors are now STALE (pre-`2b52c0a`); do not
> implement from it.**

**Discovered:** 2026-08-16, materializing a folder-selected album. Field case (kept for reproduction):
`~/torrents/Led Zeppelin - Discography+ (1968 - 2025)/JP/1975 - Physical Graffiti (Japan 1st Press Swan Song Records P-6317 8N)/`.

**Symptom.** Selecting the **folder** (not a specific `.cue`) surfaced the multi-CUE chooser. The
folder holds three cues — `Physical Graffiti All LP's.cue` (a 2-`FILE`, 15-track combined cue),
`Physical Graffiti LP1.cue` (single-`FILE`, 6 tracks), `Physical Graffiti LP2.cue` (single-`FILE`,
9 tracks). Choosing the combined `All LP's.cue` failed materialization with a truncated status:
`Materialize: source parse failed: track 9 ends beyond image duration for …/Physical Graffiti (Japan 1st Press S…`

**Root cause — two distinct gaps; the CUE file itself is malformed (authored outside tonepoet).**

*(a) The offered cue is genuinely un-materializable (malformed source file, NOT tonepoet-authored).*
`All LP's.cue` is an **invalid multi-`FILE` cue**: per the CUE spec, `INDEX` times reset to
`00:00:00` at each new `FILE`, but this cue's second `FILE` section (`LP2.flac`) keeps **cumulative**
timestamps continuing LP1's timeline — every LP2 track is offset by exactly **+30:30:48** (LP1's last
`INDEX`, "Kashmir"). Proof vs. the folder's clean `LP2.cue`: "Down by the Seaside" is `10:56:54`
there but `41:27:27` in the combined cue (Δ = 30:30:48); "In the Light" is `00:00:00` vs `30:30:48`.
Mapped onto the real image (`LP2.flac` = **43:46**, 504,210,154 samples @ 192 kHz), track 9 starts at
`41:27` (in bounds) but its segment *ends* at track 10's start `46:43` — **past 43:46** — so the
boundary guard `!is_lossy_tail && end > probe.total_samples` fires
(`src/convert/pipeline/materializer_cue.rs:1808`, message at `:1810`). The rejection is **correct**;
using this cue would misalign every LP2 track. **tonepoet did not author this file** (verified): its
synthetic merger emits per-`FILE`-relative `INDEX` times *verbatim* and resets `FILE` per image
(`generate_queue_synthetic_cue_album`, `src/convert/queue_expansion.rs:1694`), so it cannot produce
the cumulative offset; synthetic merges are written to `/tmp/tonepoet-synthetic-cue-albums/…/album.cue`
(`queue_expansion.rs:1772`, `SYNTHETIC_CUE_ALBUM_DIR` `:1701`) and generated output cues to the
*conversion output dir* as `{title}.cue` (`crates/tonepoet-features/src/cue_generator.rs:101`/`:183`)
— tonepoet **never** writes a cue into the source folder, and neither name/shape matches. The Aug-16
mtime (vs. the Aug-10 FLACs/`LP2.cue`) indicates it was created today by the user or an external tool.

*(b) The chooser presents it as an equal, unmarked, "viable" choice — the deep viability check runs
too late.* The multi-CUE prompt is built from cues that pass **admission**, and admission only
verifies that each cue's `FILE` references **resolve to local audio**
(`SplitCueFolderSelection::NeedsChoice` → `QueueCueSelectionPrompt { candidates }`,
`queue_expansion.rs:947`; `admit_split_cue_member` / `resolve_split_cue_file_reference`,
`src/convert/split_cue_album.rs:1277`/`1439`). `All LP's.cue`'s two `FILE` refs *do* resolve, so it is
offered with no signal it cannot materialize. The boundary check that catches the overflow needs the
probed sample counts and only happens at **materialize** time, *after* the user has picked — so the
user is steered into a dead-end error, with no hint that the folder's own `LP1.cue` + `LP2.cue` are
valid alternatives.

**No data loss** (parse fails before any output is written; source untouched).

**Immediate workaround.** Convert with the per-disc cues (`LP1.cue` → `LP1.flac`, `LP2.cue` →
`LP2.flac`); both have correct per-`FILE` timebases. Ignore/delete the combined `All LP's.cue`.

**Fix direction.**
1. **Clearer materialize error** (cheap, safe, standalone). Replace the truncated
   `track N ends beyond image duration` with a message that names the cue and explains the cause —
   *"this cue's second FILE section uses cumulative timestamps instead of resetting to 00:00 (malformed
   multi-FILE cue) — try the per-disc cues."*
2. **Probe-free structural pre-screen at selection time.** The malformed pattern is detectable
   **without probing**: in a multi-`FILE` cue, a non-first `FILE`'s first track `INDEX 01` that does
   **not** reset near `00:00` (specifically, ≈ the previous `FILE`'s last `INDEX 01` — cumulative
   continuation) is the signature. Use it to **mark or drop** such a cue in the chooser so a
   known-bad option isn't presented as equally viable. Purely structural; no sample-count probing.
3. **(Nice-to-have) Post-failure re-offer.** When the chosen cue fails to materialize and the folder
   has other viable cues, re-surface them ("`All LP's.cue` is malformed; the folder also has valid
   per-disc cues — use those?") instead of dead-ending on the error.
4. **Avoid** full probe-at-selection boundary validation for every candidate — expensive, and it lives
   in the LODESTAR/cue-authority selection area that has regressed repeatedly (high blast radius).

---

## 10. gnuDB lookup runs a synchronous CUE directory-scan + parse on the reducer thread (can block the TUI on slow/network folders)

> **Verified STILL OPEN 2026-08-25** (read, not grepped). Confirmed precisely: inside the synchronous reducer fn `execute_gnudb_query(app: &mut AppState, ...)` (`src/tui/context_menu.rs:4221`), `collect_single_image_cue_infos_for_sources` (`:4256`) and `discover_multi_file_cues_for_sources` (`:4260`) both run **before** the `tokio::task::spawn_blocking` boundary at **`:4291`**. **Anchor correction:** `~4181-4188` has drifted to `:4256`/`:4260`.

**Discovered:** 2026-08-17, during the two-source adversarial audit of the per-track multi-value work.
**Pre-existing** — present verbatim at `b8d96d0`; **not** a regression from that work.

**Symptom.** Triggering gnuDB ("get tags") on a large or slow (e.g. sshfs) folder can freeze the TUI
thread for the duration of a CUE directory scan and `.cue` parse.

**Root cause.** `execute_gnudb_query` (`src/tui/context_menu.rs:~4181-4188`) calls
`collect_single_image_cue_infos_for_sources` and `discover_multi_file_cues_for_sources`
**synchronously on the reducer, before** the `spawn_blocking` boundary. Those touch disk:
`collect_cue_paths_from_source` → `gnudb::find_cues_in_dir` (`read_dir`) + `path.is_dir()` stat
(`src/tui/command.rs:~1018-1027`), and `detect_single_image_cue` / `parse_cue_file` /
`resolve_cue_cue_file_reference` open and parse `.cue` files (`command.rs:~1330/1367/1045`). The
2026-08-17 F1 gnuDB corrective correctly moved the *newly-added* ISO/disc-header probing
(`is_*_iso`, `read_optical_disc_*`) off the reducer into `prepare_gnudb_virtual_disc_toc_blocking`
behind `spawn_blocking`, but did not (and was not scoped to) move this pre-existing CUE scan. The
browse-hang invariant guard is substring-scoped to `context_menu.rs` symbols, so it never covered
these `command.rs` cue helpers.

**No data loss** — responsiveness/UX only.

**Fix direction.** Move the CUE discovery (`collect_single_image_cue_infos_for_sources` +
`discover_multi_file_cues_for_sources`) into a `spawn_blocking` worker — either the existing
`prepare_gnudb_virtual_disc_toc_blocking` or a sibling — so the gnuDB reducer performs no synchronous
disk I/O. Preserve the empty-source synchronous fast path (the `"GNUDB: selection contains no
supported lookup sources"` message must stay immediate) and the operation-ID lifecycle.

---

## 11. Native FLAC metadata write refused on an sshfs-mounted, well-formed FLAC — message blames the file, but cause is likely an in-process leaked write-claim (partial diagnosis)

> **Verified STILL OPEN 2026-08-25** — and still blocked on the same thing: it needs a field reproduction, which cannot be settled from source. **Useful finding:** nothing in the code truncates the diagnosis. `native_flac_write_refused_error` (`src/tui/probe.rs:13744`) embeds `{native_err}` verbatim inside its own sentence, so the lost text is whatever the **native FLAC writer** returned at one of the call sites `:11136`, `:13660`, `:17382`, `:17433` — the field truncation was the status-line surface, not the message. (An earlier session speculated the missing text was the journal rollback-marker message at `db.rs:4481`; that is a **separate** string with a different prefix and there is no evidence linking it here.)

**Discovered:** 2026-08-17, field-test editing metadata on
`~/torrents/Led Zeppelin - Discography+ (1968 - 2025)/UK/1969 - Led Zeppelin (UK 1st Press Version 6 … Superhype Publishing)/Led Zeppelin I.flac` (a 1.9 GB 24/192 FLAC). **Partial diagnosis — needs the
untruncated error + a restart test to confirm** (see "Owed" below).

**Symptom.** Save fails: *"Metadata: 0 saved, 1 failed, unsaved changes remain — native FLAC
metadata-region tag write refused for '<path>': <native_err>"*. The `<native_err>` (the text **after**
the path — the actual diagnosis) was **truncated** in the field report. The FLAC is left unmodified.

**Ruled out (empirically).** The message's suggested remedies do **not** apply here:
- **Not padding:** the file has a **1 MB PADDING** block (ample); ironically a sibling FLAC that was
  *not* reported failing has none.
- **Not an ID3 prefix:** header is `fLaC` at offset 0.
- **Not symlink / hardlinks:** regular file, `links=1`.
- **Not a stale on-disk journal/lock sidecar:** the directory holds only `Art/`, `cover.jpg`, `.cue`,
  `.flac` — no `.tonepoet-write-lock` / `.tonepoet-meta-journal` / `.tonepoet-artwork-rollback`.
- **Not basic sshfs durability failure:** the file is on a `fuse.sshfs` mount, but the exact ops the
  native writer uses — create + `fsync` + `rename` a sidecar, `O_RDWR` open + `fsync`/`sync_data` on
  the FLAC (`overwrite_metadata_region`, `src/tui/probe.rs:2349`) — all **succeed** when tested there.

**Leading hypothesis (unconfirmed): an in-process write-claim collision, not a file/FS problem.** The
observed message is the wrapper `native_flac_write_refused_error` (`probe.rs:13271`) around the
truncated `native_err`. Given a well-formed file and a working filesystem, the most consistent producer
of this shape is `acquire_common_write_claim` (`probe.rs:3250`): *"cannot start native FLAC tag write
for '…': another metadata/artwork mutation for the same FLAC is already in progress in this process."*
This is an **in-memory, session-scoped** lock (`COMMON_WRITE_LOCKS`). If a prior write attempt on this
same FLAC in the same TUI session didn't release it (a spawned write task cancelled/panicked, or an
earlier aborted mutation), every subsequent write to that file is refused for the rest of the session,
and it propagates straight into this "refused" message. **This is the same aborted-write family as
issue #3** (`current_exe()`-deleted helper-spawn failures leaving inconsistent state).

**Owed to confirm.**
1. The **untruncated** status line — the text after the path is the `native_err` and names the exact
   cause definitively.
2. Whether a **fresh TUI restart clears it** — restart clears ⇒ leaked in-process claim (confirmed);
   persists ⇒ environmental/file-specific (then copy the FLAC to local disk and test the write there to
   isolate sshfs).

**Fix direction.**
1. **If the leaked-claim hypothesis holds** (a real bug): a leaked `COMMON_WRITE_LOCKS` entry permanently
   blocks all metadata writes to a file for the session after any aborted mutation. Ensure the claim is
   released on every write-task exit path (cancellation, panic, error) — an RAII guard / `Drop`-based
   release rather than an explicit release that an early return can skip.
2. **Message accuracy (regardless of cause):** the refusal hard-codes FLAC-structural remedies
   ("Repair the FLAC, add sufficient FLAC padding") — actively misleading when the file is pristine with
   ample padding and the cause is an in-process lock or environment. Key the surfaced advice off the
   `native_err` category (in-process claim vs. durability failure vs. genuine structural).


---

## 12. `theme_builder` test reads real theme overrides; a sibling test writes a `surface` override to the same path (test-isolation gap)

**Status:** open, mechanism identified 2026-08-28 while gating `983fa0c`. Same
shared-global-state family as the coordination race fixed in that commit.

**Symptom.** `tui::theme_builder::tests::derived_space_toggle_writes_theme_lock_only`
fails intermittently in full-workspace gate runs:

```
src/tui/theme_builder.rs:2825
assertion failed: !state.user_overrides.overrides.contains_key("surface")
```

Observed once across four gate runs on 2026-08-28; **0/25 in isolation**.

**Mechanism.** The failing test looks purely in-memory, but its constructor is not.
`ThemeBuilderState::from_palette` (`theme_builder.rs:355`) seeds state with:

```rust
let user_overrides = ThemeOverrides::load_default().unwrap_or_default();
```

`load_default` reads `theme_overrides_path()` = `<config>/tonepoet/theme_overrides.toml`
(`theme.rs:793`, `918`) — real on-disk state, not a fixture.

A sibling test in `src/tui/theme.rs:2356-2359` writes exactly the offending key to that
same path:

```rust
overrides.overrides.insert("surface".to_string(), Color::Rgb(1, 2, 3));
overrides.save_default().expect("save overrides atomically");
```

When that write is visible while the theme_builder test constructs its state, the
constructor loads `surface` into `user_overrides` and the assertion fails. The toggle
itself is innocent: `toggle_selected_derived_lock` (`theme_builder.rs:576`) writes only
`palette.derived_locks` and never touches `user_overrides`.

**The isolation seam already exists and is documented.** `config_base_dir`
(`theme.rs:894-904`) consults `crate::tui::test_support::test_config_home_override()`
under `#[cfg(test)]`, with a comment stating tests must use that seam so persistence is
not redirected into the live configuration tree. Neither test appears to hold it, so
both operate on the user's real config directory.

**Note:** this means the suite reads and writes `~/.config/tonepoet/theme_overrides.toml`
during a normal gate run. On this machine that file currently contains an empty
`[overrides]` table, which is why the failure is rare rather than constant.

**Owed to confirm.**
1. Verify neither test holds `test_config_home_override`, and check whether other
   `theme.rs` / `theme_builder.rs` tests touch the live config tree.
2. Confirm the ordering — that the failure requires the writing test to land before the
   reading test constructs state.

**Fix direction.** Scope both tests (and any sibling touching theme persistence) to the
established `test_config_home_override` seam, matching the repository fixture rule.
Do not relax the assertion — it encodes the behavior the test is named for
("writes theme lock only"), and the toggle path is in fact correct. Consider whether
`from_palette` loading real user state is appropriate for a constructor at all, since
that is what makes the test environment-dependent.

**Related.** Same root class as the `dsf_tags` coordination-scoping gap in `983fa0c`:
tests reaching global state *indirectly* through a constructor or authority path, where
nothing in the local file signals that a scoping rule applies.

---

## 13. Browse info pane reports some 32-bit float sources as plain `32-bit` — cause unknown, several theories eliminated

**Status:** open, **undiagnosed**. Observed 2026-08-29. This entry records what was
measured and what was ruled out; it does not contain a working theory.

**Symptom.** In the Browse info pane, four WavPack files that are all 32-bit float show
two different codec labels, stably and reproducibly per file:

```
~/torrents/Led Zeppelin/1969 Led Zeppelin II
    (Side A).wv    "WavPack 32-bit float"
    (Side B).wv    "WavPack 32-bit"        ← wrong
~/torrents/Led Zeppelin/1970 Led Zeppelin III
    (Side A).wv    "WavPack 32-bit"        ← wrong
    (Side B).wv    "WavPack 32-bit float"
```

The affected side differs per album, so it is not "Side B" or any per-position rule. The
user reports the labels are stable across sessions — the same file shows the same label
every time.

**This is not an integer misclassification.** `SourceInfo::codec_display`
(`src/tui/probe.rs:257-270`) renders three arms:

```rust
Some(true)  => "{codec} {depth}-bit float"
Some(false) => "{codec} {depth}-bit int"
None        => "{codec} {depth}-bit"        // ← what the screenshots show
```

So `sample_format_is_float` is `None` for the affected files: float-ness is **unknown**,
not decided as integer. Anything classified integer would print `32-bit int` explicitly.

**Ground truth — all four files are identical and all are float.**

```
wvunpack -s   "source: 32-bit floats at 384000 Hz"          (all four)
ffprobe       sample_fmt=fltp  bits_per_raw_sample=32
              bits_per_sample=0  extradata_size=2           (all four)
```

Side A and Side B are indistinguishable in every ffprobe codec field, so nothing in the
stream metadata can justify different classification. foobar2000 also reports all four as
32-bit float.

### Theories tested and eliminated

1. **`probe_audio()` misdetects the format.** Ruled out. Calling
   `crate::tui::probe::probe_audio` directly on all four files returns
   `depth=Some(32) is_float=Some(true)` and `codec_display() == "WavPack 32-bit float"`
   for every one.

2. **`MediaFacts` loses the flag in transit.** Ruled out by reading:
   `impl From<&SourceInfo> for MediaFacts` (`src/tui/app.rs:6643`) copies
   `sample_format_is_float` through unchanged, as does the reverse conversion.

3. **The metadata-editor Details overlay is the wrong display.** Ruled out.
   `apply_metadata_sample_format_label` (`src/tui/draw_overlays.rs:5926`) blanks the row
   when files disagree rather than showing divergent values, and the screenshots are the
   Browse info pane, not that overlay.

4. **The persistent probe cache drops the flag.** *Partially true but eliminated as the
   cause of this split.* The `probe_cache` table (`src/db.rs:1183`) genuinely has **no
   column** for float-ness, and `CachedProbeRow::to_cached_info` (`src/db.rs:6023-6026`)
   hard-codes `sample_format_is_float: None`. A cache hit also returns before any ffmpeg
   probe runs (`src/tui/browse.rs:9631-9647`), and the only follow-up task,
   `spawn_cached_audio_probe_metadata_completion` (`browse.rs:10355`), calls
   `read_metadata` for tags and never re-probes media facts.

   **But this cannot explain the observed split:** all four files have *valid* cache rows
   — verified against the live database, with `file_mtime` and `file_size` matching disk
   exactly for every one. If the cache were the cause, all four would render plain
   `32-bit`. Two do not.

   The missing column is still a real defect worth fixing on its own; it is simply not
   the mechanism here.

**What this means.** Two different producers appear to be supplying `SourceInfo` for
rows in this pane, and one of them yields `sample_format_is_float: None`. We have not
found the second producer. Reading has now produced three wrong theories, so the next
step should be instrumentation — log which code path supplies the `SourceInfo` for each
row and what the flag holds at that moment — rather than further static analysis.

**Open question beyond display.** Whether anything other than the label consumes
`sample_format_is_float` from a possibly-`None` `SourceInfo`. Conversion planning cares
about float-ness; if a `None` reaches it, the consequence is larger than a cosmetic
label. Not investigated.

**Incidental, unverified.** These files probe at 384 kHz while their CUE titles say
`32-176.4`. Noted only so it is not lost; no bearing on this issue established.

---

## 14. `memory_budget` staging-cleanup tests intermittently judge a dead lock live — RESOLVED

> **RESOLVED 2026-08-30 @ `fb69544`.** Two distinct causes shared the one visible symptom.
>
> **Test contamination.** `cleanup_stale_staging_dir` does not delete on the `.run.lock`
> probe alone — it enters shared mutation admission, which scans legacy file-operation
> journals rooted at the process-global `TONEPOET_FILE_OPERATION_JOURNAL_DIR`. File-task
> tests serialize that variable behind an existing lock with per-test roots; the
> scratch-cleanup tests did not join that protocol, so under the full binary they could see
> another test's deliberately nonterminal journal, admission correctly failed closed, and
> the stale tree survived. **This explains the isolation boundary recorded below** — the
> module alone never runs the fixtures that swap that variable, which is why 60 consecutive
> module runs were clean while the full gate failed 3 times in 4.
>
> **A production race.** `O_CLOEXEC` closes an inherited descriptor at *exec*, not at
> *fork*, so a child forked while an `ExecutionStaging` lease is live can transiently retain
> the locked open file description after the parent drops its handle. Retirement had no
> tolerance for that window and reported `persistent lease is live-owned`. Conversion
> execution holds these leases while launching subprocesses, so this was reachable outside
> tests — answering the reachability question this entry left open. Retirement now has a
> bounded 250 ms contention grace, gated on Unix + `ExecutionStaging` + actual contention,
> which never reinterprets contention as death.
>
> `probe_existing_run_lock` was deliberately left unchanged; the captured panics refute the
> same-process re-acquisition theory recorded below.
>
> Gate green ×2, 6504 passed. The scoping gap noted in #16 is closed for these tests.

**Status:** RESOLVED. History below is retained because two theories recorded here were
wrong, and the record of how they died is useful. First observed
2026-08-30 while gating `0f92b5f`; both affected tests' panics captured later the same
day (see the 2026-08-30 updates below). Originally filed as "cause unknown".

**Symptom.** One failure in a full `cargo test --workspace --no-fail-fast` run:

```
convert::pipeline::memory_budget::tests::held_run_lock_skips_only_active_tree
```

Rate: **1 occurrence in 3 full gate runs** on 2026-08-30. The failing assertion was
**not captured** — only the test name from the gate's failure list is known, which
leaves the mechanism unconstrained.

**What the test asserts** (`src/convert/pipeline/memory_budget.rs:1522`). Scratch-staging
cleanup must reap abandoned job trees while leaving alone any tree whose run-lock is
still held. It creates two staging trees in a private `tempfile::tempdir()` — one whose
`.run.lock` it holds via `try_lock_exclusive`, one unlocked — calls
`cleanup_stale_staging_trees` (`:847`), then asserts the locked tree and its lock survive
while the unlocked tree and lock are gone.

### Measurements

| condition | result |
|---|---|
| the single test, isolated | 1/1 pass |
| the whole `memory_budget::` module, 60 consecutive runs | **0 failures** |
| full workspace gate | 1 failure in 3 runs |

`memory_budget.rs` was **not modified** by `0f92b5f` or any recent commit in this
session. The module holds a `scoped_test_coordination_root`, so it is not the
unscoped-coordination pattern behind issue #12.

### 2026-08-30 update (2) — both tests' panics captured; the failure mode is now named

Two further gate runs, while gating `22f32b1`, failed on the **original** test with its
panic preserved:

```
convert::pipeline::memory_budget::tests::held_run_lock_skips_only_active_tree
memory_budget.rs:1548
assertion failed: !stale_dir.exists()
```

**The stale tree was not reaped.** That is the opposite of the theory recorded further
below, which predicted the *active* tree being wrongly deleted
(`assert!(active_dir.exists())` failing). Cleanup is skipping a tree it should remove.

Set beside the sibling test's panic, one failure mode explains both:

| test | observed |
|---|---|
| `execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired` | a released lease still classifies **live-owned**, so retirement is refused |
| `held_run_lock_skips_only_active_tree` | a stale tree is **not reaped**, so cleanup skipped it |

Both are **something judged live when it is not**. The original theory had the polarity
backwards in both cases.

A mechanism consistent with both, still unconfirmed: `flock` locks attach to the open
file description, and two separate `open()` calls on the same file conflict **even within
one process**. `probe_existing_run_lock` (`memory_budget.rs:1075`) reads `WouldBlock` as
`RunLockProbe::Held`, so any other handle open anywhere in the test binary against the
same lock path — or a handle not yet dropped after a release — produces a false "live"
and the tree is skipped. That would explain why these tests pass alone and fail only
under the parallel gate.

**Rate: 3 occurrences in 4 gate runs on 2026-08-30**, alternating between the two tests.
This is frequent enough, and now well-enough characterised, to brief as a defect rather
than track as a flaky test. Note the precedent: the `dsf_tags` intermittent with this same
signature earlier in the session was a genuine production defect, shipped as `983fa0c`.

### 2026-08-30 update (1) — a second test in this module failed, and the panic was captured

While gating the #2 fix, gate run 2 failed on a **different** test in the same module:

```
convert::pipeline::memory_budget::tests::
  execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired

memory_budget.rs:1494
retire_descriptor_after_lifecycle_release(&descriptor, &family)
  → "persistent lease is live-owned:
     /tmp/nix-shell.WYucNL/.tmphyiIZT/claims/execution-staging/….lease"
```

A lease is retired after its lifecycle released, and retirement is refused because the
lease still classifies as **live-owned**. `memory_budget.rs` was unmodified by that
commit and the test passes in isolation.

**This invalidates the elimination recorded below.** That reasoning rested on this very
test passing while exercising the same-process held-lock case. It no longer does.

Note the direction is inverted relative to the original theory: that theory was a *held*
lock reporting unlocked (a live tree wrongly reaped); this is a *released* lease reporting
live (retirement wrongly refused). Both are same-process lock-state visibility problems,
which is consistent with `flock` semantics — two separate `open()` calls create distinct
open file descriptions that conflict with each other **even within one process**, so a
release that drops one handle while another remains open would leave a probe seeing
`WouldBlock` and concluding "live-owned". That is a mechanism both failing tests fit; it
has **not** been confirmed.

### Theory tested and eliminated (superseded by the update above)

**Same-process `flock` re-acquisition.** `probe_existing_run_lock`
(`memory_budget.rs:1075`) decides liveness by opening the lock path and calling
`try_lock_exclusive()`:

```rust
match file.try_lock_exclusive() {
    Ok(())  => Ok(RunLockProbe::Unlocked(file)),   // caller may reap the tree
    Err(err) if is_lock_contention(&err) => Ok(RunLockProbe::Held),
    Err(err) => Err(err),
}
```

On Linux, `flock` locks attach to the open file description and a process can re-acquire
a lock it already holds. Since the test holds the "active" lock in the same process that
runs the cleaner, the probe could in principle report `Unlocked` and the active tree
could be reaped — failing `assert!(active_dir.exists())`.

**This is not supported by the evidence.** If that were the mechanism the test would fail
constantly rather than once in three gates; it passed 60/60. The same-process held-lock
case is also deliberately exercised by
`execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired`
(`:1438`, calling cleanup at `:1480` and `:1486` with "cleanup while live" and "cleanup
while recovery reserved"), and that test passes.

### What this means

The failure requires full-suite conditions — something outside the module. That is the
same signature as the `dsf_tags` coordination-descriptor intermittent investigated
earlier in this session, which needed roughly 120 full-binary runs to characterise and
**turned out to be a genuine production defect**, shipped as `983fa0c`. This one should
therefore not be assumed cosmetic on the strength of "it passes in isolation".

### Next step

Capture the actual failing assertion. That means repeated full-binary runs (~104s each)
until one reproduces, with the panic text preserved — the method that worked for
`dsf_tags`. Static reading has produced one theory and it was wrong.

### Production reachability, unassessed

`cleanup_stale_staging_trees` has exactly one production call site
(`memory_budget.rs:131`), during scratch-directory setup. Whether a real job and the
cleaner can run in the same process — which is what would make the eliminated theory
matter outside tests — was not determined.

---

## 15. A second concurrent tonepoet session blocked the first session's cut/paste — reproduced over days, no longer reproducing, cause never established

**Status:** open, **cause unknown, currently not reproducing.** Logged 2026-08-30 because
a defect that stops occurring without an identified fix has nothing preventing its
return.

**Symptom (field report, reproduced repeatedly over several days, most recently
2026-08-29).** With one tonepoet session already open, launching a second, separate
session caused the **first** session's cut/paste operations to fail. The workarounds the
user found were to switch to the newer session, or to quit the first session and remove
the journal entries/leases by hand.

The user's framing: *"we need an ability for multiple, concurrent users to run sessions
at the same time without invalidating one another's sessions for cut/paste."*

**The refusal.** Not captured verbatim at the time. It is almost certainly
`MutationClaimGuard::acquire` (`src/concurrency.rs:1675`) failing at
`src/concurrency.rs:1731`:

```rust
"filesystem mutation conflicts with {owner}: '{}' overlaps '{}'"
//                                  ^ "live owner" | "recovery reservation"
```

A cut/paste takes a `LeaseFamily::JournalOperation` lease with path claims over its
source and destination (`src/tui/file_task_runtime.rs:586`). Any overlapping claim from
another session — live **or** left behind as a recovery reservation — refuses the new
mutation. That is correct when two sessions genuinely contend for the same files; it is
not correct when a second session merely exists.

**Which of the two owners appeared matters and is unknown.** "live owner" would mean the
second session claims paths it should not while running. "recovery reservation" would
mean a startup path leaves reservations over another session's paths. These are different
defects.

### Verified 2026-08-30: not currently reproducing

The user attempted to replicate and could not. At the time of writing, **two tonepoet
processes are running concurrently** and cut/paste works.

### Two candidate explanations, neither confirmed

**(1) `983fa0c` fixed it.** The coordination-descriptor reclamation race landed
2026-08-28 19:21 and is in exactly this area: a scanner could lock an ownerless inode and
then `lstat` a deliberately retired pathname, aborting legitimate mutation admission. A
long-running TUI started before that commit would still have been running the old binary
on 2026-08-29, which is consistent with the symptom persisting past the fix date.

**(2) A stale reservation was the real blocker.** Earlier on 2026-08-30 this session
inspected a leftover `journal-operation` lease owned by a dead PID, with claims over
`~/temp/tunez/Led Zeppelin - Presence …` and `~/library/zeppelin/Led Zeppelin - Presence …`.
It was created by a CTRL+X that failed to spawn its helper, and the *next* cut was refused
with `conflicts with recovery reservation`. That lease has since been cleared: the
`journal-operation` directory is now empty and there are **0 pending journals**.

If (2) is the explanation, the symptom was never "a second session invalidates the first"
— it was "a dead session's reservation blocks these paths", with the second session
merely being how it was noticed. **That defect would still be latent** and would return
the next time a session dies mid-operation.

Explanation (2) fits what was directly observed on 2026-08-30. Explanation (1) fits the
timeline. They are not mutually exclusive.

### What is already implemented (so it is not re-derived)

The startup-recovery liveness gate **does** exist, added in `a6d7e33` (2026-08-20).
`pending_journals` (`src/tui/file_task_runtime.rs:1548`) classifies each journal's
descriptor and omits live-owned ones from recovery
(`src/tui/file_task_runtime.rs:1598`):

```rust
Ok((_family, ClaimAvailability::Live)) => {
    log::debug!("file-operation journal {} remains live-owned; omit from recovery");
    None
}
```

That closes the cross-session *journal theft* described in the 2026-08-17 research — a
second session adopting a live peer's journal and bumping its generation. It does **not**
obviously explain the reported symptom, which is why this entry exists.

**Coverage gap:** `ClaimAvailability::Live` appears exactly once in
`file_task_runtime.rs` — in production code, never in a test. The three startup-recovery
tests cover restore, cleanup-only, and `startup_recovery_inventory_surfaces_every_pending_journal`,
which codifies the pre-gate behaviour. Nothing would catch a regression that reintroduces
the theft.

### Next step

The decisive evidence is the verbatim refusal text — specifically whether it names a
**live owner** or a **recovery reservation**, and which two paths it reports. Without
that, the mechanism is unconstrained. If the symptom returns, capture the status line
before doing anything else.

A regression test for the live-owner skip is worth adding regardless of this issue's
resolution, since that invariant is currently unguarded.

**Related:** issue #7 (file-task startup recovery: supersession still open), issue #14,
and the 2026-08-17 multi-session research recorded in operator memory.

---

## 16. WORK ORDER — regression test for the live-owner journal skip (the invariant is unguarded)

**Status:** open work item, not a defect. Small, self-contained; fold into a coming batch.

**What is missing.** `pending_journals` (`src/tui/file_task_runtime.rs:1548`) omits a
journal from startup recovery when its coordination descriptor classifies as
`ClaimAvailability::Live` (`:1598`):

```rust
Ok((_family, ClaimAvailability::Live)) => {
    log::debug!("file-operation journal {} remains live-owned; omit from recovery");
    None
}
```

This is the gate that stops a second session from adopting a live peer's in-flight
cut/paste journal, bumping its generation, and failing the first session's next
checkpoint — the cross-session theft described in the 2026-08-17 multi-session research
and closed by `a6d7e33` (2026-08-20).

**`ClaimAvailability::Live` appears exactly once in that file — in production code, never
in a test.** The three startup-recovery tests are:

- `startup_recovery_restores_exact_copy_plan` (`:2994`)
- `startup_recovery_surfaces_cleanup_only_obligations` (`:3028`)
- `startup_recovery_inventory_surfaces_every_pending_journal` (`:3075`)

The third codifies the **pre-gate** contract ("return every pending journal"), so the
existing suite would not catch a regression that reintroduces the theft — and might even
be read as licensing it.

**What we want.** A regression test asserting the invariant directly: a journal whose
descriptor is live-owned must not appear in the startup recovery inventory, while a
journal whose owner is gone must still appear. It should fail if the `Live` arm is
removed or inverted.

**Seams that already exist** (offered so this is not re-derived; the shape of the test is
the implementer's call):

- `scoped_test_coordination_root` (`src/concurrency.rs:2327`) — per-test coordination
  root, and the pattern issue #12 exists because other modules skipped it. Use it.
- `descriptor_availability` (`src/concurrency.rs:1855`) and
  `descriptor_recovery_availability_with_local_handoff` (`:1875`) — the two classifiers
  `pending_journals` calls.
- `permits_same_process_recovery_handoff` (`src/tui/file_task_runtime.rs:498`) selects
  between them, which matters because a same-process test holding a live lease is not
  automatically the same case as a live *peer* process.

That last point is the one to get right: the interesting invariant is about a **live peer**,
and a naive same-process test may take the handoff path instead and prove something
weaker than intended.

**Also worth considering while in here.** Whether
`startup_recovery_inventory_surfaces_every_pending_journal` should be renamed or its
doc-comment amended to say "every pending journal *that is not live-owned*", so the
suite stops asserting a contract the gate deliberately narrowed.

**Related:** issue #15 (the unexplained cut/paste blocking, where this gate is discussed),
issue #7 (same subsystem, supersession still open), issue #12 (unscoped coordination
roots in tests).

---

## 17. "Completed (1 warning)" never says what the warning was — and the warning it fired here was wrong

**Status:** open. Reported 2026-08-31 after converting
`Lionel Richie - Can't Slow Down (1983)`, 8 DSF sources → FLAC.

Two problems, related but separable.

### (a) The warning text never reaches the user

The queue shows `Completed (1 warning)` per item and nothing more. To learn what the
warning was, the user must know a per-album `conversion.log` exists, locate it inside the
output folder, and search it. Nothing in the UI names the file or the text.

**This is structural**, visible in the status type (`src/convert/queue.rs:104-112`):

```rust
Completed {
    output_path: PathBuf,
    log_path: Option<PathBuf>,
    warning_count: u32,          // ← only a count survives
},
CompletedWithActionErrors {
    output_path: PathBuf,
    log_path: Option<PathBuf>,
    errors: Vec<String>,         // ← the sibling variant keeps its text
},
```

`source_warning_count` (`src/convert/processor.rs:2788`) walks `track.warnings` purely to
take `.len()`:

```rust
source.tracks.iter().fold(0_u32, |total, track| {
    total.saturating_add(u32::try_from(track.warnings.len()).unwrap_or(u32::MAX))
})
```

The strings exist at that moment and are discarded. `CompletedWithActionErrors` already
demonstrates the pattern for carrying text into a terminal queue status; `Completed` does
not use it.

**Correction to the original report:** the per-album `conversion.log` *does* record the
text — once per track, 8 occurrences in this job's log, first at line 53. The defect is
discoverability, not absence.

### (b) The warning appears to be a false positive

The text was:

```
Warning: Track ordering unavailable; album publication is shared and the conversion
log records tracks in completion order; filenames numbered by …
```

But the output is correctly ordered. The conversion log records the sources tonepoet
actually consumed — `A1 Can't Slow Down.dsf` … `B4 Hello.dsf` — and the published FLACs
correspond to them in order:

```
01  A1  Can't Slow Down            05  B1  Love Will Find a Way
02  A2  All Night Long             06  B2  The Only One
03  A3  Penny Lover                07  B3  Running with the Night
04  A4  Stuck on You               08  B4  Hello
```

Filenames are sequential `01`–`08` and the user reports `TRACKNUMBER` matches. So
"ordering unavailable" fired on an album whose ordering was in fact fully recovered and
correct — and it fired identically for all eight tracks.

Whether the warning's condition is too broad, or ordering genuinely was unavailable at the
point it was raised and recovered later by another path, was not determined.

Note the source files have since been renamed by the user to drop the `A1`/`B4` side
prefixes, so the current on-disk names no longer match the log. Any future investigation
should take source identity from the conversion log rather than from the directory.

### Why this pairing matters

A warning that cannot be read and is also wrong trains the user to ignore the warning
count entirely. That devalues the mechanism for the cases where it is right.

### Notes

- The album in question is the same one whose queue was interrupted in the
  concurrent-session scenario recorded as #15; no connection established, but the jobs are
  from the same session.
- The user checked `~/.cache/tonepoet.log` as well and did not find it there. Worth noting
  that file **does not exist** on this machine, so its silence is not evidence about
  warning routing — it is a separate question whether a general application log is
  expected to be written there at all.

---

## 18. Two concurrent conversions into one album directory refuse each other — including the one that started first

**Status:** open, **reproduced deterministically from the CLI**. 2026-08-31.

### Reproduction

Two `tonepoet convert` runs, **different tracks of the same album**, same `--output`
directory, the second started ~8s after the first:

```
A: convert "…/B1 Round And Round.dsf" --format flac --output <dir>
B: convert "…/B2 Truly.dsf"           --format flac --output <dir>   # 8s later
```

Both fail:

```
Conversion complete: 0/1 succeeded, 1 failed
  failed: … — PlanOutputs: output concurrency admission failed:
  filesystem mutation conflicts with live owner:
  '<dir>/Lionel Richie,(Motown - VIL-6011,Japan)'
  overlaps '<dir>/Lionel Richie,(Motown - VIL-6011,Japan)'
```

**Control:** the same single conversion with no competitor succeeds — `1/1 succeeded`.
Same binary, same source album, same output directory. Concurrency is the only variable.

### Two observations, stated separately

**(a) The claim is taken on the album directory, not the output files.**
`admit_planned_output_claim` (`src/convert/pipeline/stages.rs:30018`) resolves a
`ClaimMode::Write` / `ClaimScope::Subtree` claim on `plan.album_dir`
(`:30026-30030`). Two conversions of different tracks therefore collide even though
their destination files do not overlap. Whether album-directory granularity is the
intended boundary is a design question we are not answering here — a shared album
namespace may well need coordinating; the observation is only that track-disjoint work
is currently refused.

**(b) The incumbent is refused too.** A had held the claim for ~8 seconds and still
failed with the same error. Note the message reports the path as overlapping *itself* —
the same string on both sides — which is what a process colliding with a claim it cannot
distinguish from its own would look like. Whether (b) is a consequence of (a) or an
independent defect, we did not determine.

### Why this matters beyond the CLI

This is the mechanism behind the `Interrupted — Retry` queue state reported from the TUI,
and the chain is now fully traced:

1. two sessions target the same album output directory;
2. output-claim admission refuses both;
3. no run starts, so **no `conversion_queue_executions` row is ever created**;
4. the crash fence in `run_convert` (`src/main.rs:1826`) — which writes every `Queued`
   item as `Interrupted` *before* `process_queue_with_progress`, so a crash cannot leave a
   false "queued" record — is never lifted by the post-run `sync_queue`
   (`src/main.rs:1923`);
5. the queue displays `Interrupted — Retry`.

Verified against the live database: the 8 stuck items carry `execution_id` null, the
executions table held 0 rows, and the rows remained under their **own** session's scope
(`e601f9e8`) throughout. A separate mid-flight snapshot of a healthy single conversion
shows the fence lifting correctly — `Processing`, `exec=y`, 1 execution row — so the fence
machinery itself works.

### Theories eliminated

Recorded so they are not re-derived. All three were proposed during investigation and are
contradicted by the evidence above:

- **queue-scope theft** — the rows were never reassigned; the
  `UPDATE … WHERE id=? AND owner_scope=?` guard (`src/db.rs:3732`) held;
- **a live session's scope misclassified as dead** — `/proc/locks` confirmed the older
  session held a valid `flock` on its own queue-scope lease throughout;
- **`recover_dead_queue_scopes` interrupting a live peer** — that path skips
  `ClaimAvailability::Live` and the older session classified as live.

### Relationship to #15

#15 records a second session blocking the first session's **cut/paste**, refused with a
`filesystem mutation conflicts with …` message from the same `MutationClaimGuard`
admission family. This entry is **conversion** admission. They may share a root or may
not — we have not established that either way, and are cross-referencing rather than
merging them.

### Snapshot tooling

`/home/daedalus/.cache/tonepoet-snap.sh` captures, in one shot: live tonepoet processes
with ages, every queue-scope lease with owner liveness and whether the `flock` is actually
held, other populated lease families, and all `conversion_queue_v24` rows with owner
scope, position, execution flag, status and source filename. That combination is what made
this diagnosable; earlier attempts failed because a session exited between checks.

---

## 19. WORK ORDER — a "preserve original peaks" DSD auto-gain option, and true-peak-informed headroom

**Status:** open work item. Raised 2026-08-31.

### What the user wants

An option that **preserves the source's original peaks** — apply gain only when it would
raise level, never attenuate. Today both auto-gain scopes will turn an album down if its
loudest peak already exceeds the configured margin.

### Current behaviour, both scopes

**Album-scoped** (`tonepoet-pipeline/src/dsd_album_gain.rs:70`) computes one gain for the
submitted batch:

```rust
gain_db = target_dbfs - (loudest_peak + ALBUM_SOX_STATS_REPORTING_UNCERTAINTY)
```

Plain subtraction with **no clamp to non-negative**, so a hot album is attenuated
uniformly. This is deliberate — the doc comment at `:67` says *"if the source already
exceeds the selected target, album mode attenuates the complete set instead of clipping or
silently clamping"*, and `album_gain_attenuates_when_loudest_peak_exceeds_target` tests it.
Inter-track relationships are preserved because one gain applies to the whole batch.

**Track-scoped** emits SoX **`norm <target>`** (`tonepoet-pipeline/src/dsd_reference.rs:3621`)
under `ResolvedGainPolicy::NormalizePeak`. `norm` moves each track's peak *to* the target —
up or down — so it also attenuates hot material, and additionally **flattens level
differences between tracks**, which album scope exists to avoid.

Worth stating plainly since it came up: `gain`/`norm` are linear amplitude scaling. A track
peaking at −13 dBFS with a −79 dBFS noise floor, boosted 6 dB, becomes −7 dBFS peak and
−73 dBFS floor; dynamic range is unchanged at 60 dB. Neither effect performs dynamics
processing. The only floor that does not move is the output format's quantisation floor,
fixed by target bit depth — relevant at 16-bit, academic at 24/32.

### Why a margin exists at all: intersample peaks

Album mode measures SoX `stats` → `Pk lev dB`
(`tonepoet-pipeline/src/dsd_album_gain.rs:50-55`), which is a **sample peak**. A signal
whose stored samples peak at −0.30 dBFS can reconstruct **above 0 dBFS** in a DAC's
analogue output or a lossy transcode, because the true waveform overshoots between
samples. A blanket 1 dB margin is conventional headroom against that.

Detecting it requires **true-peak measurement**, not sample-peak: an oversampling meter
reconstructs an approximation of the inter-sample waveform and reports the highest
reconstructed peak in dBTP. The same album might report:

```
sample peak   −0.30 dBFS
true peak     +0.15 dBTP
```

— stored samples below full scale, reconstructed waveform above it. `Pk lev dB` cannot see
this. An ITU-R BS.1770-compatible oversampling measurement is what can (FFmpeg's
`ebur128` true-peak mode being one implementation).

**This changes the design space.** With true-peak available, headroom need not be inferred
from a blanket rule — tonepoet could know whether a particular album actually exceeds, say,
−1 dBTP or 0 dBTP, and attenuate only when it does.

### Relevant: true-peak measurement already exists in this codebase

Not wired into auto-gain, but present and **oversampling**:

- `build_true_peak_measurement` (`tonepoet-pipeline/src/dsd_reference.rs:3476`) takes both
  `sample_rate_hz` and `oversampled_rate_hz` (`:3482`, used at `:3505`, `:3562`)
- `parse_reference_true_peak_measurement` (`:2514`) and
  `parse_reference_sox_stats_true_peak_measurement` (`:2452`)
- `validate_post_final_true_peak` (`:2793`), consumed by
  `src/convert/pipeline/track_executor.rs:7283`
- planned as measurement steps at `dsd_reference.rs:3276` and `:3310`

It appears confined to the DSD *reference* path. Whether it can be reused for auto-gain
generally, and at what cost — it is an extra analysis pass over the decoded audio — was not
determined.

### The trade to be explicit about

"Preserve original peaks" and "guarantee headroom against intersample overs" are in genuine
tension on the attenuation side only. A boost case is unaffected: tonight's Led Zeppelin
*Presence* run measured a loudest peak of −2.72 dBFS against a −1.00 target and applied
**+1.71 dB**, which "preserve" would also apply. The conflict is a hot needledrop:

```
loudest sample peak  −0.30 dBFS,  target −1.00 dBFS
  current album mode:  −0.71 dB  → peak lands at −1.00
  "preserve peaks":     0 dB     → peak stays at −0.30, headroom guarantee given up
```

That is a legitimate choice — preserving the master's intent — but it is a trade, not a
free option. With true-peak measurement it becomes an *informed* trade rather than a blind
one.

### Open questions, not answered here

- Whether "preserve" is a third gain policy alongside the existing normalize/headroom
  behaviours, or a modifier on the existing ones.
- Whether track scope should stop using `norm` when preserve is selected, given `norm`
  performs the attenuation inside SoX rather than in a computed value.
- Whether true-peak analysis becomes the default basis for the margin, an opt-in, or is
  used only to warn.
- What the interaction is with the existing 0.01 dB `ALBUM_SOX_STATS_REPORTING_UNCERTAINTY`
  reserve, which currently applies even when no attenuation is needed.

## 20. Low-rate gate flake: `cancel_abandons_a_wedged_helper_without_waiting_for_it`

**Status:** open. Observed 2026-08-31 during the `.iso.wv` mount+edit gate.

Not a blocker for that delivery — see "Why this is not that delivery's regression" below —
but it fails a full-workspace gate often enough to cost a re-run, so it should be settled.

### The failure

```
---- tui::keybindings::file_task_supervisor_tests::cancel_abandons_a_wedged_helper_without_waiting_for_it stdout ----
thread '...' panicked at src/tui/keybindings.rs:53901:
assertion `left == right` failed
  left: 0
 right: 1
```

That line is `assert_eq!(pending.len(), 1)` on the result of
`file_task_runtime::pending_journals()`. The observed count was zero — the journal the test
expects to be awaiting reconciliation was not visible to that call.

### Observed rate

| Condition | Runs | Result |
|---|---|---|
| Test alone (`--exact`) | 20 | 20 clean |
| Full lib binary (5,324 tests, internally parallel) | 4 | 4 clean, `5324 passed; 0 failed` each |
| Full workspace gate (`--workspace --no-fail-fast`) | 3 | 2 clean, 1 failure |

So it needs *something* the lib binary alone does not provide. The distinguishing feature of
the workspace gate is that cargo runs multiple test **binaries** concurrently; internal
multi-threading within one binary was not sufficient to reproduce it in 4 attempts.

### Theories eliminated

- **Env-guard gap.** `pending_journals()` resolves its directory through
  `file_task_journal_base_dir()`, which reads the process-global
  `TONEPOET_FILE_OPERATION_JOURNAL_DIR` and otherwise falls back to the real user config
  directory. That is the shape that caused earlier cross-test contamination, so it was the
  first suspect. All 17 mutation sites of that variable were audited: every one is inside an
  RAII `install()` helper, and all 14 `JournalDirGuard::install` callers in
  `file_task_runtime.rs` hold `test_environment_lock()`, as does the failing test itself.
  No unguarded mutator was found.
- **A regression in the test's own logic.** 20/20 in isolation.
- **Ordinary CPU load.** 4 consecutive full-lib runs under the same machine conditions were
  clean.

### One candidate shape, offered as context and not as a diagnosis

The assert sits *inside* a five-second deadline retry loop:

```rust
let journal_deadline = Instant::now() + Duration::from_secs(5);
loop {
    let pending = super::super::file_task_runtime::pending_journals();
    assert_eq!(pending.len(), 1);
```

The loop exists to tolerate the record not yet having reached
`AwaitingReconciliation`, but the length assert fires on the very first poll and so
tolerates no transient state at all. If the journal is not yet visible, or momentarily does
not satisfy `needs_reconciliation()` (which is the filter `pending_journals()` applies), the
count reads 0 and the test fails immediately rather than retrying within its own deadline.

Whether that is the actual mechanism, and whether the right fix is in the test or in the
runtime, is for the implementer to determine. Note also that `pending_journals()` is not a
pure read: it calls `cleanup_setup_orphan_journal_descriptors()`, which retires setup-orphan
descriptors as a side effect. Whether that side effect can reach a journal belonging to a
concurrently-running test has not been established either way.

### Why this is not the `.iso.wv` delivery's regression

That delivery's `keybindings.rs` change is six new `.iso.wv` CUE-rename tests; its diff
contains zero lines matching `file_task`/`FileTask`/`supervisor`/`journal`, and
`file_task_runtime.rs` and `concurrency.rs` are unmodified. The production path this test
exercises was untouched.

One honest caveat: that delivery adds roughly 76 tests to the workspace (6,526 vs a 6,450
baseline), increasing total parallel load. That could raise this flake's exposure rate
without being its cause.

## 21. WORK ORDER — archive listing can be refused with no way for the user to override it

**Status:** open work item. Raised 2026-08-31 while field-testing `.iso.wv` support.

Predates the `.iso.wv` work; the messages below come from `2de6bdb` ("Phase 4: Archive
metadata editing + archive listing robustness"). Surfaced because an `.iso.wv` image on a
network mount cannot be opened at all.

### The dead end

`start_browse_archive_listing_inner` refuses in two cases and tells the user to press `l`:

```rust
// src/tui/keybindings.rs:8140
app.set_status("archive listing is disabled; press l to list this archive");
// src/tui/keybindings.rs:8148
app.set_status("archive is on a network mount; press l to list contents");
```

There is no bare `l` binding in Browse. The `KeyCode::Char('l')` bindings that exist belong
to the Config screen (`Right | l`) and the metadata editor (`Alt+l`). Even if one were
added, Browse ends its dispatch with a catch-all that routes every bare letter to
type-ahead:

```rust
(KeyCode::Char(c), mods) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
    app.browse.commit_range_selection();
    app.browse.type_ahead_push(c);
```

That catch-all is correct and must stay: bare letters in Browse are reserved for type-ahead.
So the instruction names a key that is both unbound and unreachable.

### There is no other way out either

Every caller that reaches the listing with `force = true` is internal — reopen-closed tab
restore, the password-entry retry, and session restore. None is user-invocable. With
`archive_listing = "auto"` (whose display label is "Auto (skip remote)") and an archive on a
network mount, the only escape is hand-editing `config.toml` to `"always"`. The Config
screen that would otherwise expose this setting does not exist yet, so the setting is not
reachable from the UI at all.

### Direction the user has chosen

Use the existing vi-style command mode: **`:l`**. Both `"l"` and `"list"` are unclaimed in
`command.rs`, and single-letter commands are already the convention there (`a`, `c`, `d`,
`e`, `g`, `h`, `o`, `q`, `u`, `w`).

Two alternatives were considered and rejected:

- **Bare `l`** — violates the standing rule that plain letters in Browse are type-ahead only.
- **`Alt+L`** — already bound to select-all in the metadata editor, which exists precisely
  because tmux users have `Ctrl+A` taken by the tmux prefix.

### Outcomes wanted

- A user-invocable way to force a listing that the refusal messages actually name.
- Both refusal messages updated so their instruction matches the real affordance; today the
  disabled-listing message at 8140 is the same dead end as the network-mount one at 8148.
- The implementer decides scope and shape — whether the command takes an argument, whether
  it also belongs in the Alt+M Browse context menu, and whether forcing should be
  remembered for that archive or that session.

### Related

The user believes they had previously opted into `"always"` and found the setting back at
`"auto"`. `[performance.browsing]` is exactly the section covered by the open
browsing-config-reset-on-recompile defect. Not established for this specific file, but the
section and the symptom match; worth checking whether that defect is the reason a user's
opt-in did not survive.

## 22. The transactional archive copy's fast paths are unavailable on the most common configurations

**Status:** open. Raised 2026-08-31 against the archive native-rename delivery.

The native rename path deliberately avoids mutating the user's only archive copy. It creates
a transactional sibling copy, renames inside that, and installs it through the existing
backup/install/restore swap. That choice is sound and preserves exact rollback.

The copy tries, in order: Linux `FICLONE` reflink, `copy_file_range`, then a buffered copy.
The concern is that neither accelerated path engages on several very common setups, so the
common case silently degrades to a full sequential copy of the whole archive.

### macOS: the filesystem supports cloning, the code cannot use it

APFS has supported copy-on-write cloning since introduction, exposed as `clonefile(2)`, as
`copyfile(3)` with `COPYFILE_CLONE`, and as `cp -c`. It is semantically what this path wants.

Neither primitive the implementation uses exists on macOS:

- `FICLONE` is a Linux ioctl (btrfs, XFS, bcachefs).
- `copy_file_range` is a Linux (and FreeBSD) syscall, not a macOS one.

Both call sites in `src/convert/pipeline/materializer_archive.rs` are
`#[cfg(target_os = "linux")]`. `flake.nix` builds via `flake-utils.lib.eachDefaultSystem`,
which includes `x86_64-darwin` and `aarch64-darwin`, so a macOS build compiles cleanly and
falls through to the buffered copy without any diagnostic.

This is the inverted case: macOS is the platform where cloning is *most* reliably available,
because APFS is the default and always supports it, and it is the platform that currently
cannot use it. On Linux the fast path depends on the user's filesystem choice.

### Linux: the default filesystem on most desktop distributions has no reflink

Measured on this machine (Debian 13, kernel 6.12):

```
/home/daedalus/dev          ext4         cp --reflink=always -> Operation not supported
/home/daedalus/livetorrents fuse.sshfs   (network mount; no reflink, no expected offload)
```

ext4 has no shipped reflink support. It remains the installer default on Debian, Ubuntu,
Linux Mint, and is the common choice on Arch. Distributions that *do* get the fast path:
Fedora (btrfs default since F33), openSUSE (btrfs), and the RHEL family (XFS with
`reflink=1` default since RHEL 8).

So for a Debian/Ubuntu user on ext4, and for any archive on an sshfs/NFS/SMB mount, every
native rename performs a full read-plus-write copy of the archive before the cheap header
operation runs.

### Why this still beats the old behaviour, and by how much

This is a regression in *expectations*, not in behaviour. Rough passes over the payload for a
2.7 GB archive:

| Path | Passes over the data |
|---|---|
| Old: extract, modify, repackage | ~4 (read archive, write members, read members, write archive) |
| New, no reflink: copy then header rename | ~2 |
| New, with reflink/clone: header rename only | ~0 |

The delivery is still a clear improvement everywhere. The point of this entry is that the
headline "renames no longer move the payload" is only true on reflink-capable storage, which
excludes this user's own disk, this user's NAS, and every macOS install.

### Outcomes wanted

- macOS should use `clonefile`/`COPYFILE_CLONE` so APFS gets the fast path it can support.
- Consider whether the user should be able to tell, from the UI, that a rename is about to
  copy several gigabytes rather than complete instantly — the progress reporting added in
  the same delivery may already be sufficient, which is worth checking before adding
  anything.
- Whether any additional offload is worth attempting on network filesystems (for example
  server-side copy where the protocol supports it) is an open question, not a requirement.

Mechanism, scope, and whether the macOS branch is worth the platform-specific code are the
implementer's call.

## 23. Native archive rename commits immediately, breaking the deferred-batch model the other edits use

**Status:** open. Raised 2026-08-31 against the archive native-rename delivery.

Tonepoet's established model for archive edits is *extract once, accumulate edits in staging,
repackage once* when the user navigates away or quits. The new native rename path does not
participate in it: each rename is its own complete transaction against the archive.

### Confirmed behaviour

| Operation | Commit timing | Status text |
|---|---|---|
| Delete | staged, deferred | `deleted N staged archive entries in X; archive changes pending` |
| Rename, extraction fallback | staged, deferred | `renamed archive entry ...; archive changes pending` |
| Rename, native path | **immediate install** | `renamed archive entry in X: old -> new` (no pending/saving wording) |

The deferred paths repackage only when
`quit_after_* || deferred_browse_archive_screen_switch.is_some() || deferred_browse_archive_exit`
(`src/tui/event_loop.rs`, delete success branch and the rename `Ok(None)` fallback branch).
The native branch is the rename `Ok(Some(report))` arm, which reports a completed rename with
no staging session and no pending state.

### Why this matters: the fast path can lose to the path it replaced

Passes over the payload for an archive of size S, performing N renames in one session:

| Path | Cost |
|---|---|
| Old / fallback: extract, N staged renames, repackage | ~4 passes **total**, independent of N |
| Native, reflink available | ~0 |
| Native, no reflink (ext4, sshfs, macOS today) | ~2 passes **per rename** = 2N |

So on storage without reflink — which is this user's ext4 disk, this user's sshfs NAS, and
every macOS install per issue #22 — the native path is better for one rename, break-even at
two, and **worse from three onward**. A user renaming several entries in a 2.7 GB album would
move more data than the old extract-once path did.

### The batching primitive already exists in this delivery

`7z rn` accepts multiple source/destination pairs, and the implementation already exploits
exactly that to rename a synthesized directory's descendants in a single invocation. A
session-level batch of user renames would reuse the same primitive rather than needing a new
one. `xorriso` likewise accepts multiple commands before `-commit`.

### Open question, not a finding

The native gate checks password, format capability, and implicit-directory shape. It does not
appear to consider whether a dirty staging session already exists for the archive. What
happens when a user stages a delete (repackage pending) and then performs a native rename
that installs a new archive immediately underneath that pending staging has not been traced
and may be fine — the pending-owner and fingerprint re-checks may already cover it. It should
be established either way rather than assumed.

### Outcomes wanted

- One consistent commit model for structural archive edits, so a user does not have some
  operations commit instantly and others sit pending in the same archive session.
- Multiple structural edits in a session should cost one repackage/copy, not N.
- Whichever model wins, the status wording should tell the user truthfully whether their
  change is already on disk or still pending.

Mechanism and scope are the implementer's call, including whether the native path should be
deferred into the staging model or the staging model should learn to flush a batch of native
operations at commit time.

### Related

- #22 — the transactional copy's fast paths are unavailable on common configurations, which
  is what makes the per-operation cost of this issue material.
- Measured for context: on a 306 MB album archive built with default (solid) 7z settings,
  every member shares one solid block, so deleting even a 4-byte cue costs a full repack
  (~10s, against ~11s to build the archive). Note that delete *already* batches, so that
  repack is paid once per session rather than once per deletion — no change is needed there.
  The point is that no cheap per-operation delete primitive exists for solid archives, so
  the deferred model is what makes delete affordable, and it is exactly that model the
  native rename path opted out of.

## 24. A staged rename leaves the pre-rename name visible beside the new one

**Status:** open. Reported from field use 2026-09-01, after `653cb1e`.

Renaming a folder inside an archive to a case-only variant — `Artwork` to `artwork` — leaves
**both** entries visible in the in-archive view. The packaged archive is correct: the
duplicate is never persisted, and saving produces the intended single directory. The defect
is confined to what Browse displays while edits are staged, but it reads as data corruption
to the user, who cannot tell that one of the two is a ghost.

This is the same observation recorded earlier as unexplained. It now has a reproduction: a
case-only rename of an existing archive directory.

### Mechanism

`BrowseState`'s archive row construction (`src/tui/browse.rs`, around the
`arc.listing.entries_at(&arc.inner_path)` call) does two passes:

1. It renders **every** entry returned by `entries_at`, which comes from
   `arc.listing.entries` — the listing captured from the archive *before* any edits. The loop
   looks up staged metadata to refine kind/size/mtime, but there is no guard that skips an
   entry whose staged path no longer exists.
2. It then scans the staging directory for that inner path and appends any child not already
   present, gated by `listing_paths.contains(&inner)` — an exact string comparison.

After renaming `Artwork` to `artwork`, the staging tree holds only `artwork`. Pass 1 still
emits `Artwork` from the stale listing; pass 2 sees `artwork`, finds no exact match in
`listing_paths`, and appends it. Both render.

Nothing in either pass consults `ArchiveStagingSession::edits`, even though that log already
records `ArchiveEdit::Rename { from, to }` (`src/tui/browse.rs:2987`) and is persisted for
recovery. The information needed to suppress the superseded name is present and unused.

### Scope is broader than the case-only symptom

The comparison is exact, so the mechanism is not specific to case. Any staged rename should
leave its old name visible. Case-only renames are simply the variant where the two entries
look like duplicates of one thing rather than two unrelated names, which is why this is what
got noticed.

Renames that take the format-native path install a rewritten archive immediately and Browse
re-lists it, so those refresh correctly; the stale view is a property of the staged/deferred
path. Which path a given rename takes depends on format, tool availability, and whether the
directory is explicit or synthesized, so the user-visible behaviour is inconsistent between
otherwise identical-looking operations.

### Cautionary history

An earlier round attempted ASCII-case-insensitive path reconciliation across Browse, probes,
tag extraction, and rename/delete validation. It was cut back before landing because review
found it could suppress a genuinely distinct user-created `artwork` sitting beside `Artwork`,
or resolve a case-only staged rename back to the old spelling even though the staged bytes
carried the user's spelling. Case-insensitive matching is therefore known not to be the
answer here; the edit log is authoritative about what superseded what, and case-folding
discards exactly the distinction that matters.

### Outcomes wanted

- What Browse shows while edits are staged should match what saving would produce.
- A user should not have to know whether their rename took the native or the staged path to
  predict what the view will show.

Mechanism and scope are the implementer's call.

## 25. Long archive transfers should use the existing progress surface, not just the status bar

**Status:** open work item. Raised 2026-09-01 after `653cb1e`.

The locality work landed in `653cb1e` reports archive copy and extraction progress through
the status bar. That is an improvement on silence, but it is a single line of text for an
operation that can move several gigabytes and run for tens of seconds or longer.

The project already owns a surface built for exactly this: a long file transfer the user
should be able to ignore while continuing to work.

- `AppState::minimized_file_task_progress` (`src/tui/app.rs:12702`) holds a
  `FileTaskProgressSession` (`app.rs:6658`) parked outside the modal overlay and rendered in
  the shared footer rather than as a pop-up box.
- Its doc comment records the property that matters: the session retains its
  `controls: mpsc::Sender<FileTaskControl>`, so "cancellation/conflict guarantees are
  identical whether the surface is visible or in the shared footer."
- `FileTransferQueueState::keep_minimized_across_jobs` (`app.rs:6780`) already expresses
  "keep this minimized", and `blocked_for_attention` already expresses "this needs the user
  now".
- Coverage exists, for example `minimized_footer_state_tracks_live_progress_and_fifo_depth`
  and `visible_archive_install_preserves_scheduler_owned_minimized_transfer` in
  `event_loop.rs`.

The user's position is that not using this for archive localization and extraction is a
mistake, and that such an operation could reasonably start **minimized by default** — footer
progress bar rather than a modal box — so the user keeps working, still sees progress, and
retains cancellation.

### Open questions

- Whether minimized-by-default applies to every archive transfer, only above a size
  threshold, or is a user preference.
- Whether the archive copy should become a first-class job in the existing transfer queue or
  merely borrow its progress surface. The queue carries FIFO ordering, preemption, and
  journal-based crash recovery, which may or may not be wanted for an edit-session transfer
  that is already inside its own transaction.
- How this interacts with the copy-back leg, which currently runs inside the
  backup/install/restore transaction and is deliberately non-cancellable past the final
  conflict check.

## 26. Reverting an archive edit still leaves the session marked as needing a repackage

**Status:** open. Reported from field use 2026-09-01, after `653cb1e`.

Extract an archive, change something, then change it back. Tonepoet still wants to repackage
the archive, even though the staged tree once again matches what the archive already
contains.

This is not cosmetic. Repackaging a multi-gigabyte archive is the expensive operation this
whole line of work exists to avoid, and the user is prompted to pay it for a net change of
nothing — on remote storage, including a full sequential copy back.

### Mechanism

`ArchiveStagingSession` (`src/tui/browse.rs:3009`) carries `dirty: bool` alongside its
`edits` log. It has three mutation methods — `append_edit`, `append_metadata_write`, and
`append_content_modified` — and all five of their write paths latch the flag:

```rust
pub fn append_edit(&mut self, edit: ArchiveEdit) {
    self.edits.push(edit);
    self.dirty = true;
}
```

`self.dirty = true` appears at `src/tui/browse.rs:3063`, `:3081`, `:3090`, `:3103` and
`:3108`. The string `dirty = false` does not appear anywhere in `src/tui/browse.rs`. The flag
is monotonic for the lifetime of the session.

Renaming `Artwork` to `artwork` and back therefore appends two `ArchiveEdit::Rename` records
and leaves `dirty` latched over a staging tree identical to the archive.
`append_metadata_write` does coalesce repeated writes to the same field, but it still latches
on the coalesced result, so writing a tag back to its original value dirties the session too.

### Related to #24

#24 (a staged rename leaving the pre-rename name visible) and this entry share an observable
property: in neither case does anything establish the current relationship between the staged
tree and the archive's original contents. #24's view reads the staging tree only to add
entries and refine kind/size/mtime, never to retire a listing entry staging no longer backs;
this entry's flag records that a mutation occurred and cannot be cleared by any later state,
including a state identical to the original.

Whether that shared property means they share a solution, and what any such approach would
cost on a large archive, is open.

Note also that `edits` doubles as the crash-recovery log, so it has a second consumer whose
needs may differ from the view's and the dirty flag's.

### Outcomes wanted

- A user who reverts their changes should not be asked to repackage.
- Whatever answers "does this need saving" should agree with what a save would actually
  produce.

Mechanism and scope are the implementer's call. Described in full as section B of
`BRIEF_archive_staged_view_and_progress_2026-09-01.md`.

## 27. The conversion manifest is written to the output root, not the album folder, and serves nobody

**Status:** open. Established 2026-09-01, confirmed by the user against real DSD conversions.

Tonepoet writes a hidden `.tonepoet-manifest.json` recording how a conversion was performed.
The user does not want this behaviour, has asked for its removal in prior sessions, and has
rejected co-located dotfile designs when they were proposed. The findings below are recorded
because the mechanism took several passes to pin down and is not what the code comments
suggest.

### Where the file actually lands

Not in the album folder. Converting into `~/temp/` writes `~/temp/.tonepoet-manifest.json`,
*beside* the created album directory rather than inside it — confirmed by the user across
several DSD conversions.

The cause is that `AlbumPlan::album_dir` is the per-album subfolder only when
`req.naming.per_album_subdir` is set **and** the folder template contains no disc tokens. A
template using disc tokens cannot be resolved statically at plan time, so `album_dir` remains
the **output root** and the real album folders are created beneath it later. The manifest is
written at `album_dir`, so it lands in the root.

Note that `write_manifest_for_publish` has two call shapes in `stages.rs`: one writes into a
temp directory that is then atomically renamed, and one writes directly to `plan.album_dir`.
A comment on the first ("written under the temporary album directory so it moves with the
atomic album rename") describes behaviour that does not apply to the second, which is what
the DSD reference path reaches. Reading that comment alone gives the wrong answer.

### Consequences

- The file never travels with the album, so it is absent from the user's library.
- Every conversion into the same output root **overwrites** the previous manifest, so it
  cannot hold a per-album record even in principle.
- Its only reader is `rerun.rs`, whose `Skip` / `Verify` / `Proceed` decision exists to avoid
  re-converting an album already produced. Because the file is per-root and overwritten, that
  reader will usually find a manifest belonging to some other album.

There is no correctness hazard: `rerun` compares a `settings_fingerprint` and returns `Redo`
on mismatch, and the native-v2 path additionally requires an exact source/toolchain preflight.
A stale manifest causes regeneration, not an incorrect skip. The cost is a hidden file in the
user's staging root and a feature that cannot do its job.

### Why it is still written despite being disabled

Both production sites set `write_manifest: false` (`unified_request.rs`, `main.rs`); the only
`true` in the tree is a test fixture. The publish site is:

```rust
let conversion_manifest = if req.publish.write_manifest || reference_manifest_required {
```

`reference_manifest_required` is true when any track carries DSD reference evidence, so the
reference pathway forces a manifest regardless of the setting. That OR-clause is the entire
reason the behaviour survives.

### Outcomes wanted

- Conversions should not leave hidden files in the user's output or staging directories.
- Whatever record the reference pathway needs for its own auditability, if any, should live
  somewhere belonging to the application rather than beside the user's audio.

### Notes for whoever picks this up

- The `DsdReferencePolicyVersion` and `MeasurementParser` enum variants should **not** be
  deleted as part of this. They are the deserialization vocabulary for any manifest that does
  exist, and removing them buys nothing once nothing writes new ones. There are sixteen policy
  versions and four parser contracts; execution already rejects everything except the current
  pair, so the historical variants cost only enum size.
- The fate of `rerun`'s skip/verify behaviour is a product decision, not a mechanical one.
  Avoiding re-conversion of an already-produced album is build-farm behaviour; this is a
  desktop tool whose user converts into a staging area and moves results manually.
- This is expected to be folded into the planned pipeline redesign rather than done as
  isolated work, unless the hidden files become annoying enough to justify deleting the
  OR-clause on its own, which is a small and independently safe change.

## 28. Displayed true peak should come from `tonepoet-true-peak`, in its reporting mode

The `tonepoet-true-peak` crate now measures true peak in-process, with no dependencies and no
subprocess. Its first consumer is album DSD auto-gain. The `:analyze` display still gets its
true-peak figure by shelling out to `loudgain` and parsing tab-separated text
(`src/tui/analyze.rs`, `measure_loudness`), which predates the crate. We plan to move that
display onto the crate.

### The mode selection this settles

The crate has two modes, and they are not fast/slow tiers of one measurement -- they answer
different questions:

- `Headroom64x` answers "how much gain is safe?" It carries a declared, qualified accuracy
  bound of 0.030 dB, of which the interpolation grid contributes 0.0026 dB.
- `Reporting4x` reproduces libebur128's *reporting* profile, which is what loudgain, foobar,
  and EBU R128 compliance readouts show. That fidelity deliberately includes behaviours no
  gain decision should use: oversampling drops to 2x at 96-192 kHz and to plain sample peak
  at 192 kHz and above (`crates/tonepoet-true-peak/src/lib.rs`,
  `oversample_factor_for_sample_rate`), and it follows libebur128's zero-initialized
  finite-stream contract, which rings on an abrupt onset -- a hard-onset constant block
  measures +1.03 dB above its own value, correctly, in that mode.

The grid under-read bound is `20*log10(cos(pi/2L))`: 0.6877 dB at 4x against 0.0026 dB at 64x.
So the modes are not interchangeable, and a 4x measurement driving album gain could under-read
true peak by nearly 0.7 dB and clip.

**The right mode is a static property of each call site, not of the material and not a user
setting.** The gain decision asks how much headroom exists, so it uses `Headroom64x`. A
displayed compliance figure should match what other tools report, so it would use
`Reporting4x`. Neither should be surfaced as a "fast/ultra" switch in the Convert UI: that
would let a user silently make auto-gain unsafe in exchange for a speed win on a measurement
that rides along with a far more expensive decode.

### Outcomes wanted

- The `:analyze` true-peak figure comes from `tonepoet-true-peak` in `Reporting4x`, so the
  displayed number stays comparable to what other R128 tools report.
- One fewer external-tool text-parsing dependency on a display path.
- The mode stays chosen at the call site. No config key, no pill, no `:set` option.

### Notes for whoever picks this up

- **This does not retire the `loudgain` shell-out.** `measure_loudness` returns LUFS *and*
  true peak from one invocation, and the crate measures peak only -- it has no loudness
  meter. Only the true-peak column can move. Whether that is worth a second pass over the
  audio, or whether the display should keep taking both from loudgain until something also
  supplies LUFS in-process, is the first thing to decide.
- The crate takes interleaved `f64` frames, so this needs a decode path to feed it. The album
  gain site already has one because it measures a retained PCM carrier; the `:analyze` site
  currently hands `loudgain` a file path and lets it do its own decoding.
- ReplayGain *writing* (`command.rs`) is a separate use of `loudgain` and is not in scope.
- Expected to be folded into the planned pipeline redesign rather than done as isolated work.
