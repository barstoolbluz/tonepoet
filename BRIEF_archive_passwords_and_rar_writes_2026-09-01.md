# BRIEF — Archive passwords, RAR writes, and prompt consistency

**Date:** 2026-09-01
**Base:** `main` @ `3714ac1`

Three problems reported from field use, each described with its evidence.

The user has decided *what* is wanted for the first two — cached passwords should be tried
before the user is prompted, and RAR should become writable. How to achieve any of it is
deliberately left open, as are the questions raised under each section.

## A. Only one stored archive password is ever tried

### What the user sees

A password that has worked for years on a particular family of archives stops being used.
Config shows three entries under **Archive Passwords**, and the behaviour looks as though the
list has acquired a ranking in which only the top entry matters. Opening such an archive
prompts for a password even though the correct one is already stored.

The user's intent for this feature is that Tonepoet tries the known/cached passwords against
an archive before prompting. That is not what the code does.

### Mechanism

Resolution ends at `src/tui/app.rs:1035`:

```rust
app.keychain
    .ensure_loaded()
    .map_err(|error| format!("cannot resolve stored archive passwords: {error}"))?;
Ok(app.keychain.passwords.first().cloned())
```

`stored_archive_password` returns the **first** stored password and nothing else. It has
exactly two callers, `archive_password_for_path` (`app.rs:1038`) and
`archive_preview_password_for_path` (`:1050`), both of which check a per-path session map
first and otherwise return whatever that single value is.

The CLI path is the same shape. `resolve_cli_archive_password` (`src/main.rs:2103`) ends:

```rust
load_mru()
    .map(|passwords| passwords.into_iter().next())
```

Across the whole tree there are five uses of `keychain.passwords` outside tests. Four are
Config-screen presentation — `draw.rs:382` (`is_empty`), `:401` (`len`), `:428` (indexing the
selected row), and `keybindings.rs:597` (`len`, for list navigation). The fifth is the
`.first()` above. Nothing anywhere iterates the stored passwords against an archive.

`looks_like_archive_password_error` has four call sites (`event_loop.rs:2806`, `:2954`,
`:3080`, `:8072`), and each of them decides whether to *prompt* the user. None advances to
another stored password.

### Why a working password appears to "reset"

The store is an MRU. `add_password_locked` (`src/tui/keychain.rs:346`) walks the existing
references, pulls out a matching one if present, and then in both branches inserts at the
front:

```rust
match existing {
    Some(reference) => {
        retained.insert(0, reference);      // existing password promoted to front
        ...
    }
    None => {
        let reference = crate::secret_store::allocate_reference();
        retained.insert(0, reference.clone());   // new password inserted at front
```

Combined with `.first()`, entering any password makes it the only password subsequently tried
automatically, and demotes whatever was working before. The previous password is still in the
keychain and still visible in Config; it is simply never reached.

That also explains an earlier report that entering a known-good password appeared to fail.
The password was cached the whole time and was never tried. Two other hypotheses were checked
and eliminated: the entered password *is* inserted into the per-path session map before the
keychain write (`keybindings.rs:47428`), and the `-p` switch position in
`archive_listing.rs:303` is not a problem — listing a header-encrypted 7z succeeds with the
switch either before or after the archive path (verified, four entries listed both ways).

### A workaround that works today

Re-entering the desired password promotes it back to position 0 through the `existing` branch
above, after which it is tried again automatically. This does not fix anything; it is noted
because it costs the user nothing and the underlying behaviour is unchanged.

### What is and is not remembered today

- `app.archive_passwords` is a real per-archive association,
  `HashMap<PathBuf, String>` (`src/tui/app.rs:12984`). It has four uses and no save/load path,
  so the association exists only for the current session and is gone on the next launch.
- The keychain is `passwords: Vec<String>` (`src/tui/keychain.rs:24`) — a flat MRU list with
  no archive association whatsoever.
- So across runs, Tonepoet remembers *which passwords exist* but never *which archive each one
  opened*.
- A separate persistence mechanism does exist, but for a different thing: the conversion
  queue stores a per-item archive password reference through
  `queue::prepare_archive_passwords_for_persistence` and
  `restore_archive_passwords_after_load` (`src/convert/queue.rs:685`, `:660`). That is queued
  jobs, not Browse, and is noted only so it is not mistaken for the Browse association.

### What the user wants

Stated directly: Tonepoet should **try the known/cached passwords against the archive and use
the one that works, prompting only if none of them does.** That is the behaviour the feature
was intended to have, and it is a requirement of this round rather than an option.

The related question of whether a successful match should then be *remembered* for that
archive across sessions is open — the per-archive map exists but is session-only, so the
information is currently rediscovered every run.

### Things worth establishing before designing a cycling scheme

- **Cost per attempt.** Trying N passwords means N attempts before success or prompt. For 7z
  and ZIP a listing is a header read and cheap; what it costs on a large archive over the
  user's sshfs mount has not been measured, and the locality work in `653cb1e` showed that
  small repeated operations on that mount behave very differently from local ones.
- **Distinguishing a wrong password from an unusable archive.**
  `looks_like_archive_password_error` (`src/tui/app.rs:11772`) is a substring match over
  `"password"`, `"passphrase"`, `"encrypted"`, `"encryption"`, `"wrong password"`,
  `"requires password"`, `"unsupported encryption"`. Note that `"unsupported encryption"` and
  the bare `"encrypted"`/`"encryption"` needles all match, so this predicate cannot currently
  separate "this password was wrong, try the next one" from "this archive's encryption is not
  supported at all, stop." Anything that loops on this signal inherits that ambiguity.
- **Whether a successful match should be remembered per archive**, given that the per-archive
  map described above is session-only. Persisting it would make the second and later opens of
  an archive free rather than re-cycling every time; not persisting it keeps one less secret
  association on disk.
- **Whether MRU promotion should still happen** when a password is found by cycling rather
  than typed, and what that does to the ordering other archives depend on.
- **Lockout and rate-limiting.** Repeated failed attempts against some formats or tools may be
  slow or may be undesirable; no view is taken here.

### Outcomes wanted

- A password already known to Tonepoet should be tried automatically; the user should be
  prompted only when none of the known passwords opens the archive.
- Adding a password for one archive should not stop a different, still-correct password from
  being used for other archives.

## B. RAR archives cannot be written back, and the refusal is only a status line

The user has decided this round should ship RAR write support. The evidence below records why
it is currently refused and what that refusal looks like.

### What exists today

RAR is readable. 7-Zip 25.01 carries its own `Rar`/`Rar5` codecs; listing and decoding were
both verified against a real 59-volume, 2.9 GB set, and `7z x -so` streamed payload out of it.

RAR is not writable by anything currently available. `7z a -trar` fails outright — 7-Zip has
never been able to create RAR archives. The only writer that exists is RARLAB's proprietary
`rar`.

The code already anticipates that tool. `repackage_tool_path(tool_paths, &["rar"])` is used
for RAR repackaging, and when it is unavailable
`preflight_archive_repackage_capability` refuses with:

> RAR archive creation requires the `rar` executable; install rar or convert the archive to
> 7z before editing metadata

That refusal fires **before extraction**, not at save time, at four production call sites in
`src/tui/keybindings.rs` (`:34674`, `:35478`, `:59705`, `:60227`), so the user does not wait
through a multi-gigabyte extraction before being told. That part already behaves well.

### The gap

The four sites cover the four mutating operations, and each prefixes the same underlying
error. Three set a status line directly:

```rust
app.set_status(format!("metadata: archive cannot be edited: {err}"));   // :34674
app.set_status(format!("delete: archive cannot be edited: {err}"));     // :35478
app.set_status(message.clone());                                        // :59705, "rename: ..."
```

The fourth (`:60227`, create) instead propagates the formatted string to its caller with
`.map_err(|err| format!("create: archive cannot be edited: {err}"))?`.

What none of them does is open a dialog. The outcome in every case is text — a status line, or
an error string that becomes one upstream — with no acknowledgement step and nothing that
distinguishes "this archive format can never be written" from any other transient status
message. A user who has just tried to rename something inside a RAR gets one line at the
bottom of the screen.

The user's request is a warning box explaining why the action cannot be performed.

### On making RAR writable

This is a live option rather than a licensing dead end. `nixpkgs#rar` exists
(`meta.description` = "Utility for RAR archives") and is unfree (`meta.license.free` =
`false`) — but `flake.nix` already sets `config.allowUnfree = true` (`:31`) and already
depends on an unfree package, `pkgs.ffmpeg_7-full.override { withUnfree = true; }` (`:74`).
So adding a RAR writer would not introduce a new class of dependency.

**The user has decided to ship RAR write support**, on the basis that the project already
accepts unfree packages. So RAR should become writable rather than remaining read-only.

That decision does not settle everything. Whether the `rar` binary is a hard build
requirement or an optional capability that upgrades RAR from read-only to writable when
present is still open — the refusal path described above already exists and would remain the
behaviour wherever the writer is absent, which matters for anyone building without it.

The third possibility the user raised earlier — writing the result as a different container
when RAR cannot be written — was deliberately refused in a previous round because it silently
changes the user's file format. That reasoning is recorded here rather than re-litigated.

### Outcomes wanted

- RAR archives should be editable and writable the way 7z and ZIP already are.
- Where an archive genuinely cannot be written — a build without the writer, or any future
  read-only format — the user should be told clearly, in a surface they cannot miss, before
  they invest any work. A status line is not that surface.
- Making RAR writable should not change the behaviour of any format that already works.

## C. Prompt overlays do not share a sizing or style policy

### What prompted this

The consent prompt built for writing Tonepoet-specific CUE metadata as `REM` fields
established a look and a sizing behaviour. The archive-password prompt does not use it, and
the RAR warning in section B would need to come from somewhere. The user's question is
whether these should be consistent.

### What the overlays actually do

Every popup in `src/tui/draw_overlays.rs` sizes itself independently:

| Overlay | Width | Height |
|---|---|---|
| `draw_confirmation` (`:1601`) | 66 consent / 50, widened to fit its footer, capped to `area.width - 2` | `wrapped_row_count(message)` + chrome, clamped to `[min, area.height - 2]` |
| `draw_batch_list` (`:1385`) | `area.width - 8`, clamped 40..100 | `area.height - 6`, clamped 10..30 |
| `draw_format_settings` (`:2041`) | `area.width - 4`, capped to a per-kind `min_width` | `match kind { .. }` |
| `draw_text_edit` (`:1974`) | `area.width - 4`, capped 80 | **7** |
| `draw_file_operation_settings` (`:1265`) | **68** | **13** |
| `draw_error_detail` (`:1718`) | **60** | **12** |
| `draw_item_info` (`:1743`) | **70** | **16** |
| `draw_file_input` (`:1915`) | **60** | **7** |

Four of the eight take the terminal size into account at all; four are bare constants. Only
`draw_confirmation` derives either dimension from the content it has to display — its width
grows to fit its footer, and its height comes from the wrapped message — while
`draw_batch_list` sizes to the terminal and `draw_format_settings` to a per-kind constant,
which are different things. Fixed widths across the set are 50, 60, 66, 68, 70 and 80.

`wrapped_row_count` (`:1532`) is the helper that makes content-derived sizing possible. It
has one production caller, `draw_confirmation` at `:1596`.

### A consequence that is not cosmetic

`draw_error_detail` renders arbitrary text with `Wrap { trim: true }` into a hardcoded 12-row
popup and never calls `.scroll(..)`. Anything longer than roughly ten wrapped lines at that
width is silently lost, with no indication that more exists.

Its content is the `error` string from `ConversionStatus::Failed`, reached from
`keybindings.rs:9532` and `context_menu.rs:4114` — so this is where a user goes to find out
why a conversion failed, which is exactly the text that tends to be long and to carry paths
and tool output. (Archive editing errors do not reach this overlay; they go to the status
line, as described in section B.)

This is the same defect class that was fixed in `draw_confirmation` under
`OUTSTANDING_ISSUES.md` #2. The fix was not generalized.

### The password prompt specifically

The archive-password prompt is `ActiveOverlay::TextEdit` with `label: "archive password"`
(`event_loop.rs:2825`, `:3092`, `command.rs:4985`). It therefore renders through
`draw_text_edit`, whose block title is `" Edit {label} "` — the same chrome used for renaming
a file. It is a fixed 7-row box containing a one-line hint, a one-line input and a one-line
help row, with no room for explanatory text.

Note that a password prompt and a consent prompt are not the same widget: one collects input,
the other collects a decision. The consent prompt already handles more than a bare yes/no —
it carries a "remember my choice" affordance — so the boundary between the two is not
self-evident, and where it should fall is part of what needs deciding.

### Outcomes wanted

- Prompts that ask the user something should look like each other, and like the CUE-consent
  prompt the user already regards as the reference.
- No prompt or error surface should silently truncate the text it exists to show.

Whether that means a shared helper, a shared policy applied per overlay, or promoting some of
these to a common widget is open. So is whether the RAR warning in section B and the password
prompt should use the same surface as each other.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the archive tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `src/convert/pipeline/mod.rs:13` carries `#![deny(unsafe_code)]`. Files beneath it that need
  `unsafe` use a narrowly scoped `#[allow(unsafe_code)]` with a justifying comment;
  `tool.rs:261` and `progress/streaming.rs:175` are the established examples.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. `Alt+L` is taken by the
  metadata editor's select-all, which exists because tmux users have `Ctrl+A` bound. No emoji
  or decorative unicode in UI text.
- Passwords must not reach logs, status text, or sanitized command records. The existing
  external-tool invocation path indexes password-bearing arguments in `secret_args` for that
  reason.
- Tests that mutate process-global state have caused repeated flakes here; a recent round ran
  a `PATH`-dependent test in a child process with a controlled environment instead, and that
  pattern is worth preserving.
- `OUTSTANDING_ISSUES.md` #22 through #26 are open in this area and are not in scope.
