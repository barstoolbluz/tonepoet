# Delivery notes: metadata clipboard identity and host-publication fixes

Date: 2026-09-26
Cumulative base: `tonepoet_cumulative_cue_clipboard_metadata_paste_delivery_2026-09-26.tar.gz`
Base SHA-256: `a54ea3be4ddb47724c81691ada26af8f9e74e7295a2c5470dbd8351457d4f576`
Underlying snapshot: `main` at `6cd5b4f` (per bundle name)

## Scope

This is an iterative correction on top of the accepted cumulative CUE/menu, unified filesystem clipboard, rowless metadata FieldSet paste, and unified-CUE semantic-dimension work.

The patch is limited to metadata clipboard identity plus host publication/consumption ordering. It does not introduce a production in-process metadata/text clipboard and does not change conversion, metadata persistence, CUE save semantics, the DSD reference source lock, or true-peak code.

## Corrections

### Explicit selection preserves field identity

- Metadata copy now chooses the structured envelope from **selection intent**, not selection cardinality.
- With no explicit row selection, Ctrl+C keeps the existing `SingleField` contract for the current row. That payload can still be pasted into an arbitrary destination field.
- Any explicit metadata-row selection uses `FieldSet`, including exactly one Ctrl+clicked row and exactly one visible row selected by Ctrl+A / Alt+L.
- A stale explicit selection whose selected rows are no longer available now fails rather than silently degrading to current-row `SingleField` semantics.
- FieldSet paste continues to match by canonical field key, preserves the existing overwrite-confirmation path, and creates missing keys. The prior rowless one-field FieldSet behavior is retained.

### Host clipboard publication is ordered and observable

- Structured metadata copies now use a publication barrier: Tonepoet queues the host write, waits for the queued write stream to drain, and reports copy success only after that point.
- The write state now records only the last write error, never copied text. A failed host publication is surfaced to the metadata copy path instead of being mistaken for success.
- The consumed failure does not permanently poison later unrelated clipboard reads.
- Ordinary non-metadata clipboard publication remains asynchronous; the barrier is applied only to the structured metadata copy paths changed here.

### Ctrl+Shift+V no longer trusts a potentially stale bracketed snapshot

A terminal emulator can capture Ctrl+Shift+V while Tonepoet's preceding asynchronous copy is still publishing, then deliver the old clipboard text after Tonepoet finishes handling Ctrl+C. Waiting only inside Tonepoet's copy handler cannot prevent that external snapshot race.

For structured metadata paste targets (`MetadataRows`, whole-field detail paste, and Paste Tags):

1. the terminal-provided bracketed payload is treated as a one-operation fallback only;
2. Tonepoet waits for any prior queued clipboard writes to finish;
3. it then reads the current host clipboard;
4. when that read succeeds, the fresh host snapshot wins;
5. when native host reads are unavailable (for example an OSC 52 / SSH transport), the terminal-provided bracketed payload is used instead;
6. if a prior Tonepoet write failed or did not drain, Tonepoet does not substitute the potentially stale fallback.

The fallback string exists only inside the pending read operation and is discarded with that operation. It is not a persistent clipboard or paste authority.

Ctrl+V and Ctrl+P continue to use the ordinary host-read path. Raw Ctrl+Shift+V already does the same. Terminal-generated bracketed Ctrl+Shift+V now converges on the same structured metadata parser/application path after host reconciliation.

## Changed files relative to the cumulative base

- `src/tui/context_menu.rs`
- `src/tui/event_loop.rs`
- `src/tui/host_clipboard.rs`
- `src/tui/keybindings.rs`
- `DELIVERY_NOTES_metadata_clipboard_identity_2026-09-26.md` (this file)

## Focused regression coverage

Added/updated coverage verifies:

- Ctrl+click one metadata row -> Ctrl+C publishes a one-block `FieldSet` envelope;
- implicit current-row Ctrl+C with no explicit selection publishes `SingleField`;
- Ctrl+A and Alt+L preserve FieldSet identity even when only one field exists;
- multiline values retain the correct SingleField-versus-FieldSet identity distinction;
- a one-field FieldSet pasted with no selected row creates its missing canonical key and does not spill into the cursor's existing field;
- the existing multi-field rowless FieldSet paste behavior remains covered;
- structured metadata copy waits for a queued host publication before returning;
- host publication failure is surfaced and consumed once rather than reported as a successful copy;
- bracketed metadata reconciliation prefers a fresh host read over stale/raw terminal text, while retaining the terminal payload when native reads are unavailable;
- structured metadata bracketed targets are explicitly routed through host reconciliation;
- Ctrl+V, Ctrl+P, and reconciled Ctrl+Shift+V feed the same FieldSet parser/application semantics.

The prior regression that verifies Ctrl+V, Ctrl+P, and raw Ctrl+Shift+V all request the same `MetadataRows` host target remains present in the cumulative tree.

## Validation performed in this runner

This environment has no `nix`, `flox`, `cargo`, `rustc`, `rustfmt`, or `rust-analyzer`, so this delivery does **not** claim a compile, format, clippy, or Rust test pass here.

Validation performed:

- confirmed the base archive SHA-256 above before modification;
- audited metadata copy selection state from Ctrl+click / Select All through envelope serialization;
- audited host write coalescing, worker completion, write failure, host-read ordering, and bracketed-paste routing instead of relying on existing comments;
- audited production clipboard state to confirm this patch adds no persistent copied-text field or fallback clipboard;
- verified the previous CUE request-ownership/fingerprint and unified-CUE semantic-dimension fixes remain in the cumulative tree;
- lexical delimiter-balance scan passed for all four changed Rust files;
- `git diff --check` passed with no whitespace errors;
- generated an incremental patch from the exact cumulative base, applied it to a fresh copy, and verified all four changed Rust files byte-for-byte against the working tree.

Required handoff gate in the repository's pinned development environment:

```sh
nix develop --extra-experimental-features 'nix-command flakes'
cargo fmt --check
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

This patch does not touch the true-peak or DSD reference implementation/source lock, so their separate long-running qualification is not required by this change.
