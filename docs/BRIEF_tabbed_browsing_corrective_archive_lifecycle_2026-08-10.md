# tonepoet — Tabbed browsing CORRECTIVE: archive-tab close/reopen lifecycle + quit-drain hang — 2026-08-10

Round 2 for the tabbed-browsing feature. **The world-class core is correct and stays** — this is a
**bounded corrective** for one cluster: the **archive-tab close → reopen → relist → repackage
lifecycle** (C1/C2/C6), a **quit-drain infinite-loop hazard** (C3), and two small UX defects
(C4/C5). Note up front: **C1 and C2 are one bug** (a reopen index-collision), so one fix likely
clears both.

Outcomes + guardrails; diagnosis is *evidence*, not prescription — **you are the arbiter of HOW**.

## Context (read first)
The tabbed-browsing delivery is applied and, after the applier fixed 13 compile errors it shipped
with, the workspace gate is **4609 passed / 2 failed** — the only failures are two archive-tab
close/reopen tests **that never compiled in the original delivery (so they never ran)**. A
two-source audit confirmed the load-bearing feature is **world-class and correct**: INV-SCAN tab-id
routing (reducers route by `tab_id` before the generation check; colliding-generation test passes),
per-tab independence, cross-tab clipboard, app-global singleton workers, byobu keybindings, and the
picker are all sound. **Do NOT re-architect any of that.** Fix only the items below; keep the full
suite green ×2 and do not weaken/delete the two failing tests (they encode the intended behavior).

## Ground rules
- Base = the **current applied working tree** in the bundle (original `2d854e9` + the tabbed-browsing
  feature + the applier's compile fixes + the two now-running failing tests). Version **0.4.6 — do
  not bump.** No merge.
- Gate: `cargo test --workspace --no-fail-fast` green **×2** (the applier runs it; you have no cargo
  — say so honestly). The two named tests below must PASS, unweakened, alongside everything else.

---

# C1 — Reopening a closed in-archive tab must restore the archive context (gate RED)

**Failing test (direct `BrowseState` API):**
`src/tui/browse.rs` — `tabbed_browsing_tests::unstaged_archive_close_reopen_preserves_archive_directory_context`
panics at browse.rs:20029 `expect("restored archive")` — after closing an in-archive tab and
reopening it, `browse.archive` is `None`. The test's contract (lines ~20022–20033):
```
browse.enter_archive(<listing>, Some("secret"));   // inner listing has "Disc 2"
browse.enter_archive_dir("Disc 2");
browse.open_dir_in_new_tab(other, /*activate=*/false);
browse.close_active_tab();                          // closes the in-archive tab
assert_eq!(browse.current_dir, other);
browse.reopen_closed_tab();
let restored = browse.archive.as_ref().expect("restored archive");
assert_eq!(restored.listing.archive_path, archive);
assert_eq!(restored.inner_path, "Disc 2");
assert_eq!(restored.password.as_deref(), Some("secret"));
assert!(restored.staging.is_none());
```

> **C1 and C2 share ONE root cause — a reopen index-collision. One fix likely resolves both. Read
> this diagnosis before starting either.** (My earlier "discarded descriptor" / "pending-archive
> guard" hypotheses were wrong; two independent audits + a direct trace converge on the mechanism
> below. The outcomes and the two named tests still bind — verify the mechanism yourself, but do not
> chase the discarded diagnoses.)

**Diagnosis (evidence — SHARED with C2).** The closed tab's per-tab state (including `archive =
Some`) **is** retained: `archive` is not in the `swap_tab_shared_state` allowlist and is not cleared
by `quiesce_for_closed_tab`. The failure is that reopen **never activates the restored state into
`self`**, due to an index collision:
- `switch_to_tab_internal(index)` (browse.rs) early-returns `already_active` **without performing
  the state swap** when `index == tabs.active`:
  `let already_active = index == tabs.active; if index >= slots.len() || already_active { return already_active; }`.
- Close sets `tabs.active = new_active` and `slots[new_active].state = None` (browse.rs:3746–3747)
  — the invariant is "the active slot's `state` is `None` because the live state lives in `self`."
- Reopen re-inserts the closed tab at `index = closed.original_index.min(slots.len())`
  (browse.rs:3778) and calls `switch_to_tab_internal(index)` (browse.rs:3781). When
  `original_index == tabs.active` (e.g. closing tab 0 → `active = 0`, reopening at 0), the call hits
  the `already_active` branch, returns `true`, and **does not swap** — so the reopened archive state
  is stranded in `slots[index]` while `self` still holds the *other* tab's state (`archive == None`).
  This violates the "active slot is `None`" invariant (a stateful slot now sits at the active index).

**Outcome (C1).** Closing an in-archive tab and reopening it **restores the same archive navigation
context into the active view** — `self.archive` is `Some` with `listing.archive_path`, `inner_path`,
`password`, and `staging == None`. The fix is almost certainly in the reopen/switch path (activate
the reopened slot's state into `self` even when the insertion index equals the current active, or
insert/reconcile so the active-slot-is-`None` invariant holds) — NOT threading a new descriptor
through the direct API (the state is already retained; it just isn't activated). Staging is never
retained on reopen, per the existing INV-ARCHIVE design.

---

# C2 — Clean-staged close → reopen → relist must let the user switch away (gate RED)

**Failing test (keybinding path):**
`src/tui/keybindings.rs` — `browse_tab_input_tests::clean_staged_archive_close_reopen_relists_same_inner_directory_without_staging`
panics at keybindings.rs:81389 `assert!(request_switch_browse_tab(&mut app, foreground_index, &tx))`
returning `false`. Flow (lines ~81365–81395): a clean-staged archive tab is closed
(`request_close_browse_tab`), leaving the "other" tab focused; `request_reopen_browse_tab` reopens
the archive tab and enqueues an async relist (`pending_archive_listings.get(&reopened_id)` present);
the test then **switches back to the foreground "other" tab** and expects that switch to succeed —
its own comment: *"A staged-archive restore is background work just like an ordinary archive open.
Switching away and navigating must not cancel it."*

**Diagnosis (evidence — SAME ROOT CAUSE as C1, NOT a guard).** `browse_tab_action_ready`
(keybindings.rs:6550) checks only `browse_archive_repackage` / `pending_browse_archive_metadata` /
`pending_browse_archive_rename` / `pending_browse_archive_delete` / `browse_inline_edit` — **none**
of which is the async relist state (`pending_archive_listings`). So it returns `true`; the relist is
**not** what blocks the switch. The switch fails for the **same reopen index-collision as C1**: after
reopen leaves `self` holding the wrong tab and the foreground tab's slot `state == None`, the later
`request_switch_browse_tab(app, foreground_index, tx)` → `switch_to_tab_internal(foreground_index)`
finds `slots[foreground_index].state` is `None`, `.take()` yields `None`, and it returns `false`
(browse.rs). Do **not** weaken `browse_tab_action_ready` chasing a phantom relist guard.

**Outcome (C2).** After reopening a clean-staged archive tab (async relist pending), the user can
**switch to another tab and that switch succeeds** (`request_switch_browse_tab` returns `true`); the
pending relist keeps running in the background and completes into its owning (reopened) tab without
being cancelled — consistent with INV-SCAN. Fixing the C1 reopen-activation bug should make this test
pass; make it pass without weakening it.

---

# C3 — Quit-drain loop must always make progress (SUSPECTED HANG)

**Diagnosis (evidence).** The quit path drains tab-owned archive staging
(`event_loop.rs:563`):
```
while let Some(tab_id) = app.browse.first_archive_staging_tab_id() {
    ... switch to tab_id ...
    if dirty { defer repackage; return true; }
    exit_browse_archive(app, tx);   // clean staging
}
```
But `exit_browse_archive` (keybindings.rs:25488) **early-returns without clearing staging** when any
of `pending_browse_archive_rename` / `pending_browse_archive_delete` / `pending_browse_archive_metadata`
is `Some`. If any of those is set while a clean-staging tab exists at quit time,
`first_archive_staging_tab_id()` keeps returning the same tab and the loop **spins forever (hang)**.
The pre-loop guard at event_loop.rs:545 only covers the *distinct* `pending_metadata_editor` field,
not these three.

**Outcome (C3).** Quitting with a pending archive rename/delete/metadata operation in flight **never
hangs**: the quit either defers cleanly (like the existing `pending_metadata_editor` guard) until
those reconcile, or the drain loop otherwise makes guaranteed forward progress. No infinite loop
under any combination of pending archive-edit state + clean/dirty staging across tabs.

---

# C4 — Dead tab chords must be true no-ops (cosmetic)

**Diagnosis (evidence).** `browse_tab_action_ready` (keybindings.rs:6550) unconditionally commits an
inline edit and calls `close_options_menu()` + `bookmarks.close_dropdown()` **before** the tab action
is known to succeed, and `request_switch_browse_tab` calls it before `switch_to_tab`. A no-op chord —
e.g. `Alt+5` when only 2 tabs exist, where `request_switch_browse_tab(app, 4, tx)` then fails — still
closes the options menu / bookmarks dropdown as a side effect.

**Outcome (C4).** A tab keybinding that resolves to **no change** (jump to a nonexistent tab index,
reorder past the end, etc.) is a **true no-op** — it does not close the options menu, close the
bookmarks dropdown, or commit an inline edit. Run the pre-action commit/close side effects only when
the action will actually proceed. **Non-regression:** actions that DO proceed (a real switch, close,
duplicate, new, reopen) must keep their existing behavior — menus/dropdown still close and the inline
edit still commits. `browse_tab_action_ready` is shared by all tab ops; don't change the success path.

---

# C5 — Single-tab tab strip (decide deliberately — optional)

The Browse tab strip reserves `Constraint::Length(1)` unconditionally (draw_browse.rs:283) and
renders the `⧉`/`+` controls even with one tab, so the single-tab content pane is one row shorter
than before — a deviation from strict single-tab no-regress. **Decide and state:** either suppress
the strip (and its row) at `tab_count() == 1` to preserve exact single-tab layout, or keep it
always-visible as a deliberate browser-style choice. Either is acceptable; just make it intentional
and consistent with the picker. (Low priority; do not destabilize the strip's overflow/hit-region
logic.)

---

# C6 — Deferred dirty-archive-tab close must not be dropped when focus moves

**Diagnosis (evidence).** When closing a tab whose archive staging is **dirty**, the close is
deferred until the repackage completes. `handle_archive_repackage_result` (event_loop.rs:3204)
unconditionally `.take()`s `pending_browse_tab_close_after_archive_repackage` (event_loop.rs:3285),
then performs the close **only if** `app.browse.active_tab_id() == closing_tab_id` (3288). Because
C2 establishes that switching away during background archive work is a *valid* contract, a user who
switches to another tab before the repackage finishes causes the `if` to be false — the pending
record was already `take()`n, so the close (and its `return_tab_id` / `archive_restore`) is
**silently dropped**: the tab stays open forever.

**Outcome (C6).** A deferred dirty-archive-tab close **always completes**, even if focus moved: close
the originally-requested `closing_tab_id` **by id** (not only when it happens to be active), applying
its `archive_restore` and honoring `return_tab_id`, or otherwise guarantee the close is never lost
(e.g. re-defer rather than `take()`-and-drop). Do not rely on focus staying put.

---

# Guardrails / invariants (hard)
- **Do NOT regress the world-class core.** INV-SCAN tab-id routing, per-tab independence, cross-tab
  clipboard, app-global singleton tag/transfer workers, byobu keybindings (no plain-letter in
  Browse; Ctrl+T/Ctrl+W/Alt+arrows/Alt+[/]/Alt+digit/Alt+u/Alt+d/Alt+,/Alt+.), the picker, and the
  existing passing tab tests all stay intact. Full workspace suite green ×2.
- **Keep INV-ARCHIVE semantics:** staging is never retained across reopen; dirty close still routes
  through repackage; duplicate never aliases staging; quit drains **all** tabs' staging.
- **Do not weaken or delete the two named failing tests** — they define the contract; make them
  pass. Add focused tests for C3 (quit with pending archive-edit + clean staging does not hang), C4
  (dead chord leaves menus open), and C6 (deferred dirty-close completes after a focus switch).
- **Keep these PASSING tests green** — the reopen/close fix must not regress them:
  `tabbed_browsing_tests::duplicate_archive_tab_never_aliases_staging_ownership` and
  `dirty_archive_duplicate_and_open_in_new_tab_keep_staging_source_owned` (staging ownership on
  duplicate), the INV-SCAN colliding-generation test, and the reorder/close/reopen determinism test.
  Note: closing the **last** tab is already guarded (`close_tab_with_archive_restore` returns false at
  `count <= 1`, browse.rs:3721) and covered — keep it green; no new work there.
- If you disagree with any diagnosis, say so and do what's correct — the outcomes bind, not the
  mechanism.

# Deliverables
- Patch or changed files (expected to be a small delta, mostly `src/tui/browse.rs`,
  `src/tui/keybindings.rs`, `src/tui/event_loop.rs`, maybe `src/tui/draw_browse.rs`); a short WHY per
  fix; honest note that you can't run cargo (the applier gates ×2).

# Bundle manifest
- This brief. The complete current applied tree (feature + compile fixes + the two failing tests):
  - `src/tui/browse.rs`, `src/tui/keybindings.rs`, `src/tui/event_loop.rs`, `src/tui/draw_browse.rs`,
    `src/tui/app.rs`, `src/tui/message.rs`, `src/tui/context_menu.rs`, `src/tui/button_map.rs`,
    `src/tui/bookmark_workers.rs`, and the `crates/tui-file-picker/` crate.
  - Full `src/` + `crates/` + `tonepoet-pipeline/` + root `Cargo.toml` + `flake.nix` + `CLAUDE.md`
    so it compiles. NOT `target/`. If anything's missing, say so rather than guessing.
