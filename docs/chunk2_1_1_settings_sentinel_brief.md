# Chunk 2.1.1: Full PipelineSettings Sentinel Coverage

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order).
## Prerequisites: Chunk 1 (tonepoet-pipeline crate) and Chunk 2 (orchestrator unification) are integrated and compiling.

---

## 1. What this chunk does

Prove that every field in `PipelineSettings` (the unified conversion settings type from the tonepoet-pipeline crate) survives the complete handoff chain without silent loss, reinterpretation, or fallback:

```
user/config selections → ConversionOptions.pipeline_settings → ConversionItem.pipeline_settings → PipelineRequest.settings → PlanRequest.settings
```

This is the most important correctness invariant in the system. If a field is silently dropped at any boundary, the user's conversion settings don't reach the command builders, and the output is wrong.

---

## 2. The problem being solved

### 2.1 Current risk

`PipelineSettings` has ~20 top-level fields, many with nested sub-structs (FlacSettings, Mp3Settings, AacSettings, OpusSettings, WavPackSettings, SsrcSettings, DsdSettings, MetadataSettings, VerificationSettings, ReplayGainSettings). Each sub-struct has its own fields. The total field surface is ~60+ individual values.

Today, `PipelineSettings` flows through multiple boundaries:

1. **UI/CLI → ConversionOptions**: `ConversionOptions.pipeline_settings: Option<PipelineSettings>` (added in Chunk 2)
2. **ConversionOptions → ConversionItem**: `ConversionItem.pipeline_settings: Option<PipelineSettings>` (cloned from options)
3. **ConversionItem → PipelineRequest**: `PipelineRequest.settings: PipelineSettings` (extracted in `unified_request.rs`)
4. **PipelineRequest → PlanRequest**: `PlanRequest.settings: PipelineSettings` (passed to the planner per-track)

At boundary 3, there's a legacy fallback path (`legacy_pipeline_settings_for_item()` in `unified_request.rs`) that reconstructs `PipelineSettings` from old `ConversionOptions` fields when `pipeline_settings` is `None`. This fallback is lossy — it maps a subset of fields and uses defaults for the rest. Any field not explicitly mapped is silently lost.

### 2.2 What's missing

- No test proves all fields survive the chain
- No test detects when a new field is added to `PipelineSettings` but not carried through
- No fingerprint mechanism detects settings identity for rerun/skip decisions
- The legacy fallback path has no coverage proving which fields it drops

---

## 3. Deliverables

### 3.1 Sentinel PipelineSettings fixture

Create a test helper that constructs a `PipelineSettings` value where **every field** is set to a non-default, distinctive sentinel value. No field may equal its `Default::default()` value.

Purpose: if any boundary silently resets a field to default, the sentinel test catches it.

Requirements:
- Every top-level field set to a non-default value
- Every nested sub-struct field set to a non-default value
- The fixture must compile — sentinel values must be valid (e.g., DitherType::Gesemann not DitherType::None, compression_level 8 not 5, etc.)
- If a field is an `Option`, set it to `Some(non_default_value)`
- If a field is a bool, set it to the opposite of default
- Include a doc comment listing every field and its sentinel value

### 3.2 Queue → request → plan equality assertions

Write tests that push the sentinel `PipelineSettings` through the real handoff chain and assert equality at each boundary:

**Test A: ConversionOptions → ConversionItem**
```
let options = ConversionOptions { pipeline_settings: Some(sentinel()), ... };
let item = ConversionItem::new(..., options);
assert_eq!(item.pipeline_settings.unwrap(), sentinel());
```

**Test B: ConversionItem → PipelineRequest**
```
let item = /* item with sentinel settings */;
let req = build_pipeline_request(&item);  // or unified_request construction
assert_eq!(req.settings, sentinel());
```

**Test C: PipelineRequest → PlanRequest**
```
let req = /* PipelineRequest with sentinel settings */;
let plan_req = /* construct PlanRequest for a track */;
assert_eq!(plan_req.settings, sentinel());
```

Each test must assert field-by-field equality, not just `assert_eq!` on the whole struct (so failure messages identify *which* field was lost).

### 3.3 Field drift detection

When a new field is added to `PipelineSettings` (in the tonepoet-pipeline crate), the sentinel fixture and handoff tests must fail until the new field is explicitly handled.

Approaches (implement at least one):

**Option A — Compile-time detection**: The sentinel fixture constructs `PipelineSettings` with named fields (not `..Default::default()`). Adding a new required field to the struct causes a compile error in the fixture.

**Option B — Runtime field count**: Serialize `PipelineSettings` (via serde) to a map, count fields recursively, and assert against a checked-in count. A new field changes the count and fails the test.

**Option C — Fingerprint coverage**: The settings fingerprint (3.4) includes every field. A new field that isn't added to the fingerprint function causes a test failure because the fingerprint doesn't change when that field is mutated.

Option A is the strongest (compile-time). Option C is the most useful (also serves rerun decisions). Implement both if practical.

### 3.4 Stable settings fingerprint

Add a deterministic fingerprint function:

```rust
pub fn settings_fingerprint(settings: &PipelineSettings) -> SettingsFingerprint
```

Requirements:
- **Include every conversion-affecting field.** If changing a field would change the conversion output, it must change the fingerprint.
- **Exclude display-only fields** (if any exist — currently all fields are conversion-affecting).
- **Stable field ordering.** The fingerprint must not depend on HashMap iteration order or struct field declaration order changes.
- **Deterministic.** Same settings → same fingerprint, always.
- **Use a content hash** (e.g., SHA-256 of a canonical serialization, or a purpose-built hasher). The exact algorithm is a design choice for the reasoning model.

Tests:
- **Per-field mutation test**: For each conversion-affecting field, construct sentinel settings, mutate that one field, and assert the fingerprint changes.
- **Stability test**: Same settings produces the same fingerprint across multiple calls.
- **Default vs sentinel**: `settings_fingerprint(default)` ≠ `settings_fingerprint(sentinel)`.

The fingerprint will be used in:
- Conversion manifests (Chunk 2.1.2)
- Rerun skip/redo decisions (Chunk 2.1.2)
- Conversion logs

### 3.5 Ban production PipelineSettings::default() at orchestration boundaries

Add a static test (grep-based or similar) that scans production conversion code for patterns that construct default settings silently:

Denylist patterns:
```
PipelineSettings::default()
..Default::default()  (when applied to PipelineSettings)
```

Allowed locations:
- Test fixtures
- The sentinel helper itself
- UI/CLI code where defaults are the explicit user-facing starting point
- The legacy fallback path (with a named exception comment)

This prevents future code from silently constructing empty settings at an orchestration boundary.

---

## 4. Design constraints

1. **The tonepoet-pipeline crate may receive a small fingerprint module addition** (`src/fingerprint.rs`). No existing types, traits, or functions are modified. All other work is in the main crate's test suite.
2. **PipelineSettings derives PartialEq** (it already does). Use this for equality assertions.
3. **PipelineSettings has optional serde support** (`#[cfg_attr(feature = "serde", ...)]`). The fingerprint can use serde serialization if the feature is enabled, or a manual approach if not.
4. **The sentinel fixture is a test helper, not production code.** It lives in a test module or test file.
5. **The fingerprint function IS production code.** It should live in the tonepoet-pipeline crate (preferred, if we allow a small addition) or in the main crate's pipeline bridge module.

---

## 5. Where to put things

| Deliverable | Location |
|-------------|----------|
| Sentinel fixture | `tests/settings_sentinel.rs` or `src/convert/pipeline/tests/` |
| Handoff chain tests | Same test file |
| Field drift detection | Same test file (compile-time via struct construction) |
| Settings fingerprint function | `tonepoet-pipeline/src/fingerprint.rs` (preferred) or `src/convert/pipeline/settings_fingerprint.rs` |
| Fingerprint tests | Adjacent to the fingerprint function |
| Static denylist test | `tests/settings_sentinel.rs` or a dedicated `tests/static_audit.rs` |

---

## 6. Code files the reasoning model needs

To implement these deliverables, the model needs to see:

1. **PipelineSettings and all its nested types** — to construct the sentinel fixture
2. **ConversionOptions** — to see the `pipeline_settings: Option<PipelineSettings>` field
3. **ConversionItem** — to see how `pipeline_settings` is carried on queue items
4. **PipelineRequest** — to see the `settings: PipelineSettings` field
5. **unified_request.rs** — to see the handoff boundary and the legacy fallback
6. **PlanRequest** — to see the final destination of settings in the planner
7. **ConversionItem::new() and new_with_pipeline_settings()** — to see how items are constructed

---

## 7. Acceptance criteria

- [ ] A sentinel `PipelineSettings` exists where no field equals its default
- [ ] Tests prove every field survives ConversionOptions → ConversionItem → PipelineRequest → PlanRequest
- [ ] Adding a new field to `PipelineSettings` causes a compile error or test failure in the sentinel fixture
- [ ] A `settings_fingerprint()` function exists that is deterministic and stable
- [ ] Mutating any single conversion-affecting field changes the fingerprint
- [ ] Production orchestration code does not silently construct `PipelineSettings::default()` (enforced by test)
- [ ] All tests pass, clippy clean, no new warnings
