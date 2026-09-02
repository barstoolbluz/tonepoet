# Implementation report — RAR write/readback corrective

Date: 2026-09-01
Starting bundle: `tonepoet_rar_write_readback_corrective_2026-09-01_bundle.tar.gz`
Brief: `BRIEF_rar_write_readback_corrective_2026-09-01.md`

## Decision

Keep RAR write support, but make the compatibility boundary explicit and fail closed:

1. Tonepoet-created replacement RARs use RARLAB `rar` store mode (`-m0`).
2. Post-write verification runs through 7-Zip, the same reader Tonepoet uses for RAR listing/extraction, rather than through `rar t`.
3. RAR write preflight requires both the RAR writer and a 7-Zip reader.

This is the smallest coherent correction that preserves the prior decision to make RAR writable without introducing a second, format-specific RAR listing/extraction/mount stack. It deliberately trades RAR compression for readback compatibility. The repository README now states that replacement RARs are written in store mode so the size consequence is not hidden.

A new `unrar`/`rar` read path was not introduced: that would touch listing, extraction, mount behavior, tool discovery, password handling, and integration tests only to preserve RAR compression, while the existing unified 7-Zip reader already works for store-mode RARs.

A legacy RAR4-output escape hatch is not available in the target writer generation: RARLAB's 7.20 release notes state that creating RAR 4.x archives is no longer supported. That leaves store mode as the measured compatibility mode without adding a second reader stack.

## Production changes

### RAR creation is explicitly 7-Zip-compatible

`src/convert/pipeline/materializer_archive.rs` now builds RAR creation arguments through `rar_repackage_create_args` and always includes `-m0`:

```text
rar a -r -m0 [-p... | -hp...] <archive> .
```

Password argument indexing remains explicit in `secret_args`; adding `-m0` before the password is covered by a regression test so redaction does not silently drift.

### Verification uses Tonepoet's actual reader

`verify_repackaged_archive` no longer runs `rar t` for RAR output. It now runs the configured/discovered 7-Zip binary as:

```text
7zz/7z t [-p...] -- <archive>
```

This fully reads/tests archive payload through the decoder Tonepoet will use after commit. If a future RAR writer/version produces an unsupported method despite the creation policy, verification fails before atomic replacement.

### RAR preflight requires the readback verifier

RAR mutation preflight now requires:

- `rar` for creation, and
- `7zz` or `7z` for readback verification.

Writer-only configurations therefore fail before an edit session begins rather than discovering at save time that Tonepoet cannot establish its own readback invariant.

## Regression coverage

### Tool-independent tests

- `rar_repackage_creation_forces_the_seven_zip_compatible_store_method`
  - locks `-m0` into plain, visible-header-encrypted, and header-encrypted creation argument construction;
  - locks the password-bearing argument index used by command redaction.
- `preflight_refuses_rar_writeback_without_the_actual_reader`
  - proves RAR writeback is not admitted with only a writer.
- The existing multi-volume preflight test now supplies both fake writer and fake reader so it continues to exercise the intended split-set refusal rather than failing earlier at tool availability.

### Real-tool tests

- `archive_materializer_extracts_real_rar_fixture_when_available`
  - generated RAR fixtures now use `-m0`, matching Tonepoet's write contract;
  - the fixture still contains PCM WAV, the content class that exposed the RAR 7.x/7-Zip interop failure.
- `repackage_archive_preserves_real_rar_password_scope_when_tools_are_available`
  - source fixtures use the same compatible store method;
  - post-repackage correctness is tested with 7-Zip rather than `rar`;
  - both visible-header and header-encrypted RARs are tested with correct and wrong passwords;
  - each fixture contains a 256 KiB highly compressible compatibility member so removing `-m0` should exercise the unsupported compressed-method regression rather than accidentally passing on tiny stored content;
  - the final replacement is extracted with 7-Zip and content is checked.

## Documentation

`README.md` now says that RAR writeback requires RARLAB `rar` and that Tonepoet writes replacement RARs in store mode for compatibility with its 7-Zip reader.

## Validation performed in this container

The supplied brief correctly describes this implementation environment: `cargo`, `rustc`, `rustfmt`, `nix`, `rar`, `7z`, and `7zz` are unavailable. The Rust/Nix gate and real archive-tool integration tests therefore could not be executed here and this report does not imply otherwise.

Performed:

- `python3 -m py_compile tools/audit_concurrent_mutation_entrypoints.py tools/audit_test_coordination_isolation.py` — passed.
- `python3 tools/audit_concurrent_mutation_entrypoints.py` — same pre-existing baseline failure documented by the prior delivery:
  - `src/convert/pipeline/materializer_archive.rs`: 3 unclassified external launches
  - `src/convert/pipeline/tool.rs`: 1 unclassified external launch
  - all other audit sections passed.
- `python3 tools/audit_test_coordination_isolation.py` — same four pre-existing unscoped permanent-delete tests in `src/tui/keybindings.rs` documented by the prior delivery.
- Static secret-flow review:
  - RAR creation password switches remain represented by `secret_args`;
  - RAR verification password switches use the existing `secret_args` path;
  - no password value was added to status/error/log text.
- Static tool-boundary review:
  - `ToolBinary::Rar` is now used only for RAR creation in `materializer_archive.rs`;
  - RAR verification uses `ToolBinary::SevenZip`.
- Modified-file delimiter counts are balanced and no trailing whitespace was introduced.

## Operator gate

Run in the repository's documented `nix develop` environment:

```sh
cargo test --workspace --no-fail-fast
```

The two real-tool regressions are especially important in the gate:

```sh
cargo test archive_materializer_extracts_real_rar_fixture_when_available -- --nocapture
cargo test repackage_archive_preserves_real_rar_password_scope_when_tools_are_available -- --nocapture
```

Then perform one manual round trip with an ordinary single-volume RAR containing a compressible WAV: edit inside the archive, save, reopen/list, and extract through Tonepoet. Repeat with one encrypted RAR if practical. A replacement RAR may be materially larger than its compressed input; that is intentional under the compatibility policy, not a regression.
