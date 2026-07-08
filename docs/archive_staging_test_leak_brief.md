# Archive Staging Leak: Tests Leave Orphaned Recovery Rows and Staging Dirs

## The Problem

Every time the user launches tonepoet, the archive recovery dialog appears offering to recover staged archive edits from a previous run. The user has never edited any archives. The staging directories are all test fixtures:

```
/tmp/nix-shell.5PhYXd/.tmpMA6Wm0/album.zip
/tmp/nix-shell.5PhYXd/tonepoet-archive-rename-4e3a4875-9087-400c-9622-79234b5e7c5a
```

The `.tmpXXXXXX` paths are from `tempfile::tempdir()` in tests. There are 11 orphaned staging dirs under various `/tmp/nix-shell.*` paths.

## Root Cause

Two resources leak from tests:

### 1. Database rows in `pending_archive_sessions`

Tests that call `app.db.upsert_pending_archive_session()` (keybindings.rs lines 8973, 8993, 9168) write rows to the production SQLite database. If a test panics before calling `delete_pending_archive_session`, the row persists.

On the next real startup, `recover_pending_archive_sessions_at_startup()` (db.rs:804) loads all rows, checks if the staging directory still exists on disk, and shows the recovery dialog for each one that has a surviving staging dir.

### 2. Staging directories in `/tmp`

Staging dirs are created at `std::env::temp_dir().join(format!("tonepoet-archive-rename-{}", uuid))` (app.rs:1403). In the nix dev shell, `TMPDIR` points to `/tmp/nix-shell.XXXXX/`. Tests create these dirs but if they panic before cleanup, the dirs persist. Even `tempfile::tempdir()` cleanup doesn't remove them because the staging dir is created separately from the test's tempdir.

### Why it keeps reappearing

The recovery scan at db.rs:828-829 deletes rows where the staging dir no longer exists. But `/tmp/nix-shell.*` dirs survive for the duration of the nix shell session, so the staging dirs ARE still present on disk. The dialog reappears on every launch within the same nix shell session.

## What Needs to Change

### Fix 1: Test isolation — tests must not write to the production DB

Tests that exercise archive staging should use an isolated in-memory or temp-file database, not the production DB at `~/.cache/tonepoet/tonepoet.db`. The `AppState::new_for_test()` constructor (app.rs:8336) already creates an `AppState` with `without_archive_recovery_for_tests()`, but the DB connection itself may still point to the production database.

Check: does `AppState::new()` / `AppState::new_for_test()` use a test-isolated DB? If not, make it so. Every test that creates an `AppState` should get its own in-memory or temp-file SQLite database.

### Fix 2: Staging dir cleanup on test teardown

Tests that create staging dirs via `ArchiveStagingSession` or `BrowseArchiveStagingContext` should ensure cleanup even on panic. Options:
- Wrap the staging dir in a RAII guard that removes it on drop
- Use `tempfile::tempdir()` for the staging dir itself (not just the parent)
- Add a `Drop` impl to `ArchiveStagingSession` / `BrowseArchiveStagingContext` that cleans up (if not already present — check)

### Fix 3: Startup reconciliation should filter out test artifacts

As a belt-and-suspenders measure, `recover_pending_archive_sessions_at_startup` could skip rows where the archive path doesn't exist AND the staging dir is under a nix-shell temp directory. But this is a workaround — fixes 1 and 2 address the root cause.

## Files

- `src/db.rs` — `pending_archive_sessions` table, `upsert_pending_archive_session`, `recover_pending_archive_sessions_at_startup`
- `src/tui/app.rs` — `ArchiveStagingSession` (creates staging dir at line 1403), `AppStartupOptions`, `AppState::new_for_test`
- `src/tui/keybindings.rs` — calls to `upsert_pending_archive_session` (lines 8973, 8993, 9168)
- `src/tui/event_loop.rs` — calls to `delete_pending_archive_session`

## Your Task

Fix the test isolation so that tests never pollute the production database or leave staging directories behind. The user should never see the archive recovery dialog unless they actually have pending archive edits from a real editing session.
