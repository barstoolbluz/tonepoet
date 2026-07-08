# Archive Recovery Dialog: Y/N Behavior is Wrong

## The Bug (Empirically Observed)

When tonepoet starts and finds leftover archive staging directories from a previous run, it shows a recovery confirmation dialog:

```
┌─ Confirm ─────────────────────────────────────┐
│ Recovered staged archive edits from a previous│
│ run:                                          │
│     /tmp/.../album.zip                        │
│                                               │
│ Staging:                                      │
│ /tmp/.../tonepoet-archive-rename-...          │
│          Y yes     N no                       │
└───────────────────────────────────────────────┘
```

**Actual dialog message text** (from `keybindings.rs:22957`):
> "Y resumes the staged archive view. D discards staged edits. N/Esc keeps them for next startup."

So the dialog describes a three-action model (Y=resume, D=discard, N=keep), but:

**Actual key handlers** (all three work — `keybindings.rs:4028-4046`):
- **Y/Enter** = resumes the archive editing session (via `execute_confirm_action` → `resume_startup_archive_recovery`) — works correctly
- **D** = discards staged edits (via `execute_archive_staging_discard_action`) — works correctly, but the user never discovers it
- **N/Esc** = keeps the staged edits for next startup ("kept recovered archive staging for next startup: ...") — works as the code intends

**The actual bug is purely a UI labeling problem.** All three actions are wired and functional. But the visible footer chips show only `Y yes` / `N no` — a generic two-button confirmation layout. The user sees `N no` and naturally reads it as "no, I don't want these, delete them." They never see the `D` key because it's mentioned only in the message body text (which is long and easy to skim past). So functionally, from the user's perspective, N appears to do nothing useful (keeps silently) and there's no visible way to discard.

## What Needs to Change

Two options:

**Option A (simplify to two actions):** Change the chips and behavior to match a simple Y/N model:
- **Y** = resume the archive editing session (already works)
- **N** = discard the staged edits (delete staging dir, remove recovery row)
- **Esc** = dismiss without deciding (keep for next startup)

This is the simplest fix. The `D` action in the message text gets removed. N becomes the discard action. Esc becomes the "keep for later" escape hatch.

**Option B (wire the three-action model):** Keep the three actions but make them all work:
- **Y** = resume (already works)
- **D** = discard (needs key handler wired to `open_archive_staging_discard_confirmation`)
- **N/Esc** = keep for later (already works, but the `N no` chip should say something like `Esc later` to avoid confusion)

Either way, the visible footer chips must match the actual available actions.

## Key Code Reference

The Confirmation overlay key dispatch is at `keybindings.rs:4026-4049`. All three keys are handled:
- Line 4028: `Y`/`Enter` → `execute_confirm_action`
- Line 4032: `D` (guarded by action type match) → `execute_archive_staging_discard_action`
- Line 4044: `N`/`Esc` → `cancel_confirm_action`

The dialog message is built at `keybindings.rs:22957`. The footer chips are rendered generically by the Confirmation overlay draw code (not archive-specific) — that's where the `Y yes` / `N no` chips come from, ignoring the `D` action entirely.

## Files

- `src/tui/keybindings.rs` — the confirm/deny handlers (lines ~22915-23000, ~23170-23190)
- `src/tui/app.rs` — `pending_archive_recovery`, `archive_recovery_prompt_active`
- `src/tui/event_loop.rs` — `start_browse_archive_repackage`

## Your Task

Fix the dialog so that the visible chips, the message text, and the key handlers all agree. The current state has a three-action message, two-action chips, and only two key handlers — all saying different things. Pick Option A or B from above (A is simpler) and make it consistent end-to-end. Verify by tracing Y, N, D, and Esc through the full code path.
