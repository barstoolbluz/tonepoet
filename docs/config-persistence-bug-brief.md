# Bug: TUI settings don't persist across rebuild+restart — find the root cause and fix it

## The problem (symptom)

Settings changed in the TUI and explicitly saved do **not** survive a quit + rebuild + restart.
The user builds and runs with `cargo run --release tui` (a fresh binary each rebuild, reading the
same on-disk config at `~/.config/tonepoet/config.toml`).

Concrete reproduction:
1. Launch the TUI. On the Browse screen, press `.` to hide hidden files, and via
   **Options → Layout** hide the explorer pane.
2. **Options → "Save layout as default."** (Status: "browse layout saved".)
3. Quit, rebuild, relaunch with `cargo run --release tui`.
4. **Observed:** hidden files are shown again and the explorer pane is back — the saved layout is
   gone. The user has to redo this **every** rebuild.

Separately (same root area, possibly same cause): an `aggregate_metadata_target_priority` value that
was present in `config.toml` reverted to the default order after a session — i.e. a save wrote the
default over a non-default on-disk value.

At the point the reset was observed, `config.toml` on disk held **defaults** for the affected fields
(`show_hidden = true`, `layout_explore = "open"`, `aggregate_metadata_target_priority = [sidecar-cue,
embedded-cue, individual-files]`) — so the file itself was overwritten with defaults, not merely
mis-applied at startup.

## Desired outcome (the contract)

- A setting changed in the TUI and saved (Options → "Save layout as default", and every other
  `config.save()` path) **persists across quit + rebuild + `cargo run --release tui` restart** —
  specifically hide-hidden, hide-explorer-pane, and `aggregate_metadata_target_priority`.
- **No save path ever writes a default/stale value over a user's existing on-disk setting.** A save
  triggered to persist setting A must not silently reset unrelated setting B to default.
- The full round-trip (change → save → quit → relaunch → observe) is lossless and idempotent for
  every persisted field.

## What the applying side already ruled out — CONTEXT ONLY, not conclusions; verify or discard freely

From static reading the round-trip *looks* correct, which is why the bug is subtle. Treat the notes
below as hints, not answers — the real fault may be in an area these notes call "fine."

- `TonepoetConfig::save` → `save_to_path_with_outcome_impl` (config.rs) serializes a **clone of the
  whole in-memory config**, mutating only the two archive-password fields. It does not strip or reset
  `browsing` or `conversion.aggregate_metadata_target_priority`.
- Startup load: `main.rs` does `require_startup_config(TonepoetConfig::load())?`. `load` →
  `load_from_locked_path` parses the full config via serde; on a parse/IO **error**,
  `require_startup_config` **propagates** it (the app refuses to start) — it does **not** fall back to
  defaults-and-resave. **One caveat, NOT fully ruled out:** `load_from_locked_path` returns
  `Ok(Self::default())` when the config file **does not exist** (config.rs:1690) — a silent default
  with no error. Normal saves write a temp file and rename (the file is never momentarily absent), so
  this *should* only fire on first run or external deletion — but a file-absence / first-run /
  write-race / wrong-path angle is therefore a live candidate; do not discount it.
- Config path is `dirs::config_dir()/tonepoet/config.toml` (`config.rs::config_path`) — stable, and
  the file that actually gets overwritten.
- `config.browsing` **is** applied at startup: `AppState::new` → `BrowseState::new_with_config(&config.browsing)`
  (src/tui/app.rs), and `capture_browsing_config` (browse.rs) ↔ `new_with_config` /
  `apply_browsing_config` look symmetric on the toggles (`show_hidden`, `explore_enabled`,
  `explore_collapsed`, etc.).

So load, save, path, and startup-apply each look individually correct — yet the file ends up with
defaults. Likely suspects to investigate (not exhaustive, not prescriptive): a **clobbering save**
that runs with a partially-initialized / default in-memory config (there are ~15 `app.config.save()`
call sites across keybindings.rs/app.rs/command.rs/context_menu.rs — does any fire before the config
is fully loaded/applied, or from a state that reset a field?); an **asymmetry** between
`capture_browsing_config` and `new_with_config`/`apply_browsing_config` or inside
`BrowsingConfig::normalized()`; a save that serializes something other than the live user state; or a
timing/ordering issue between load, browse-state construction, and the first save.

## Guardrails / constraints

- Single-user desktop TUI. **Smallest correct fix** in the surrounding style. No new config
  subsystem, migration framework, journal, or versioning layer.
- Do not regress the workspace suite (~5440 tests, green). The suite currently **misses** this — add
  a **deterministic regression test** that sets a non-default `browsing` (hidden off, explorer
  hidden) **and** a non-default `aggregate_metadata_target_priority`, saves to a temp path, reloads,
  and asserts every field is preserved (there is a `TEST_CONFIG_PATH_OVERRIDE` hook in
  `config.rs::config_path` for exactly this). If the true bug is a runtime save-ordering issue that a
  pure config round-trip test can't catch, add the tightest test that does catch it and say so.
- Version stays 0.4.5. byobu-safe input rules unchanged. No compiler on your side — the applying
  side compile-fixes and runs `cargo test --workspace --no-fail-fast`.

## Relevant code (in the bundle, at repo-relative paths under tree/)

- `src/config.rs` — `TonepoetConfig`, `BrowsingConfig` (+ `normalized()`), `UiConfig`,
  `ConversionSettings`, `load_from_locked_path`, `save_to_path_with_outcome_impl`, `config_path`,
  defaults, `TEST_CONFIG_PATH_OVERRIDE`.
- `src/tui/browse.rs` — `capture_browsing_config`, `new_with_config`, `apply_browsing_config[_with_search]`.
- `src/tui/keybindings.rs` — `persist_browse_config`, `save_browse_layout`, and the many
  `app.config.save()` sites.
- `src/tui/app.rs` — `AppState::new` (the `new_with_config` call), its `config.save()` site.
- `src/main.rs` — startup config load + `AppState::new`.
- `src/tui/context_menu.rs`, `src/tui/draw_browse.rs` — the Options-menu "Save layout as default"
  trigger and other `persist_browse_config` callers.

## Ask

Find the root cause and fix it so the desired outcome holds. Add the regression test. Keep it small.
