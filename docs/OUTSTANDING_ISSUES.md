# Outstanding Issues

Running list of diagnosed-but-unfixed issues. Newest at the top. Each entry records the
symptom, the root cause (with code anchors), and the intended fix direction — enough to
hand to a reasoning-model brief without re-diagnosing.

---

## 1. Confirmation dialog is fixed-height (9 rows) — long recovery prompts clip their text and buttons

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

## 2. `current_exe()`-deleted → cryptic ENOENT when a file op runs from a pre-rebuild TUI

**Discovered:** 2026-08-09, on a Ctrl+X (cut/move) in a stale tonepoet instance.

**Symptom.** A copy/move (paste or cut) fails immediately with:

```
Status: start isolated file-task helper: No such file or directory (os error 2)
```

**Root cause.** The process-isolated file-task engine runs its worker by **re-executing tonepoet
itself**:

```rust
let executable_result = std::env::current_exe();          // src/tui/keybindings.rs:43518
...
Command::new(executable)                                    // :43531
    .arg("__file-task-worker").arg("--journal").arg(journal.path())
    .spawn()                                                // :43538 → ENOENT
```

`current_exe()` reads `/proc/self/exe`. If the running binary's on-disk file was **replaced or
removed after the process started** (e.g. `cargo build` while the TUI is still open), that link
resolves to `"<path> (deleted)"`, so `Command::new(...).spawn()` returns `os error 2` (ENOENT).
Confirmed live: a stale instance's `/proc/<pid>/exe` pointed at
`…/target/release/tonepoet (deleted)`.

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

## 3. Metadata stage `tool timed out after 30s` on a large multi-track conversion

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

## 4. DSD-to-PCM auto-gain inflates DC-bias readings and flips `negligible`→`significant` — the DC threshold is absolute (level-dependent), not a conversion defect

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

## 5. Cross-process DB init lock can time out during a schema migration under heavy concurrent load (self-recovering, no corruption)

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
