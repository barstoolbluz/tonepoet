# Outstanding Issues

Running list of diagnosed-but-unfixed issues. Newest at the top. Each entry records the
symptom, the root cause (with code anchors), and the intended fix direction — enough to
hand to a reasoning-model brief without re-diagnosing.

---

## 1. Single-image (taggable) FLAC + sidecar CUE: metadata SAVE rewrites the multi-GB image and embeds a CUESHEET instead of writing the sidecar only

> **⚠ DO NOT brief or implement yet.** The root cause is diagnosed and verified, but the *handling* is
> an open design question (see "Open design questions" below) — it touches the LODESTAR
> metadata-source-selection area (regressed 6–7×) **and** raises whether the editor SAVE path should honor
> `aggregate_metadata_target_priority` at all. **Prompt the user to discuss the approach afresh before
> writing any brief.**

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

