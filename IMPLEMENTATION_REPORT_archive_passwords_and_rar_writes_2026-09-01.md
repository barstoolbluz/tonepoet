# Implementation report — archive passwords, RAR writes, and prompt consistency

Date: 2026-09-01
Starting bundle: `tonepoet_archive_passwords_and_rar_writes_2026-09-01_bundle.tar.gz`
Brief: `BRIEF_archive_passwords_and_rar_writes_2026-09-01.md`

## Scope delivered

### A. Cached archive passwords are actually tried

- Replaced the single-MRU-entry resolution boundary with ordered password candidate resolution.
- CLI explicit-password behavior is unchanged. In the TUI, explicit configured passwords/references retain their previous single-value semantics when no path-local session association exists; when one does exist, the session value is tried first and the configured value follows as a fallback candidate.
- A path-local session password is tried first, then configured/keychain candidates follow in their existing order with duplicate values removed. Session state is an ordering hint, not exclusive proof: a mistype or an archive re-passworded in place cannot suppress another known working password.
- Browse archive listing now cycles cached candidates in MRU order and stops at the first verified success.
- ZIP/RAR listings with plaintext headers are not treated as proof of a password: one encrypted payload member is tested with `7z t`, then remaining candidates test that member without re-reading the directory listing.
- Header-encrypted archives cycle passworded listings.
- Unsupported encryption, damaged archives, timeouts, cancellation, and other non-password failures stop the cycle immediately instead of trying every cached secret.
- Successful matches are remembered only in the existing process-local `archive_passwords` map. No new persistent archive-to-secret association was added, and cycling does not reorder the global MRU.
- Convert archive preview uses the same candidate behavior when multiple cached passwords exist while preserving the previous one-password fast path.
- Direct Browse bulk/folder queue expansion authenticates archive paths asynchronously before queue admission. A session value is probed first whenever another cached candidate exists; only a literal session-only candidate keeps the previous zero-probe fast path. Successfully authenticated values travel through async messages as redacted `SecretString`s.
- CLI queue admission cycles multiple cached candidates before publishing queue items and does not hold the queue write lock while external probes run.

### B. RAR writes are shipped where the writer exists

- The existing RAR creation/repackage implementation remains unchanged; this round makes its required `rar` executable part of the Nix runtime on systems where nixpkgs marks the package available.
- The flake already permits unfree packages, so this does not introduce a new dependency-policy class.
- PATH/tool-path discovery remains intact, so non-Nix or unsupported-flake hosts can still gain RAR write capability by supplying `rar`.
- Existing preflight remains before extraction.
- Split/multi-volume RAR sets are deliberately refused before extraction. The existing transaction replaces one archive pathname; silently repackaging a multi-volume input to one file would risk replacing only part of a set. Supporting transactional multi-file RAR volume recreation is a separate design problem and was not fabricated in this round.
- Multi-volume detection is bounded: modern `.partNN.rar` is recognized lexically; old-style `.rar` + `.r00` probes at most the deterministic `.r00`/`.R00` siblings and does not scan the directory.

### C. Refusals/prompts are visible and long text remains reachable

- Archive mutation preflight failures for metadata edit, delete, rename, and create now open an acknowledgement-required `Notice` overlay while preserving their existing status strings.
- `ErrorDetail` is content-sized up to terminal bounds and scrollable with Up/Down/PageUp/PageDown/Home/End rather than silently dropping long wrapped diagnostics.
- The archive-password `TextEdit` now uses prompt-specific chrome, explanatory text, and content-derived height instead of looking like a file rename box.
- Shared geometry helpers are used by rendering and mouse hit-testing for the new scrollable surfaces.
- Existing confirmation prompts retain their current behavior; when terminal height clips content, their title explicitly says to resize rather than silently implying all text is visible.

## Focused regression coverage added/updated

- Full MRU order is preserved by TUI and CLI password resolution.
- A stale/session password is first but non-exclusive: session `wrong` plus MRU `good`, `older` yields `wrong`, `good`, `older`; duplicate session/MRU values are tried only once.
- A working session-only password remains usable when the configured/secret backend is unavailable.
- A stale session candidate followed by a working cached candidate is authenticated through the fake archive-tool harness, and successful `ArchiveListingComplete` publication replaces the old process-local association.
- Bulk/folder admission likewise advances from stale session `wrong` to cached `good`; backend failure preserves session-only admission and the single-candidate fast path performs no archive probe.
- Non-password archive formats do not touch the secret backend.
- ZIP/RAR plaintext-header listing cannot falsely authenticate the first cached password.
- Header-encrypted archives advance to later passwords correctly.
- No-password encrypted payload listing returns the normal prompt signal.
- Plaintext archives do not acquire arbitrary cached-password associations.
- Cycling stops at first success, exhausts all wrong candidates before the prompt signal, and stops immediately on unsupported encryption.
- Password-bearing tool output is redacted before it can enter returned diagnostics.
- Fake archive-tool tests inject a child executable directly; they do not mutate process-global `PATH`.
- RAR multi-volume detection covers modern and old naming and preflight refuses it even with a writer configured.
- Archive edit refusal opens the blocking notice while preserving the existing status contract.
- Scroll offsets clamp correctly and long error detail can render its final canary text.
- Archive password prompt rendering uses the new prompt chrome/hint.
- Cancellation after bulk expansion now retires any already-created synthetic CUE artifacts before the result is discarded.

## Validation performed in this container

The brief explicitly states that this implementation container has no Rust toolchain, Nix, or archive tools. That remains true here: `cargo`, `rustc`, `rustfmt`, `nix`, `rar`, `7z`, and `7zz` are unavailable. Therefore the Rust/Nix build and test gate was **not** run and this report does not imply otherwise.

Performed instead:

- `python3 -m py_compile tools/audit_concurrent_mutation_entrypoints.py tools/audit_test_coordination_isolation.py` — passed.
- Reference/static consistency review across renamed password helpers, async message constructors, `ErrorDetail`/`Notice` constructors, and new `BrowseConvertExpansion` secret state.
- Added-line secret leak review for status/log formatting — no password value is interpolated into added log/status text.
- `python3 tools/audit_concurrent_mutation_entrypoints.py` was run on both the corrected tree and the untouched starting tree. Both fail identically on the same pre-existing inventory gap:
  - `src/convert/pipeline/materializer_archive.rs`: 3 unclassified external launches
  - `src/convert/pipeline/tool.rs`: 1 unclassified external launch
  The other four audit sections pass in both trees. This round did not create that baseline failure.
- `python3 tools/audit_test_coordination_isolation.py` was also run on both trees. Both fail identically on the same four pre-existing unscoped permanent-delete tests in `src/tui/keybindings.rs`; the new child-tool password tests do not add a process-global-state isolation failure.

## Operator gate

Run inside the repository's documented `nix develop` environment:

```sh
cargo test --workspace
```

Then exercise at least one real encrypted 7z/ZIP/RAR with the correct password below MRU position zero, one unsupported-encryption failure, one ordinary single-volume RAR metadata/structural save using RARLAB `rar`, and one missing-writer RAR preflight to verify the notice surface.
