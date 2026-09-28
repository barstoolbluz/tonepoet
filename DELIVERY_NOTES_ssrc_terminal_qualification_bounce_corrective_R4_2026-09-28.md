# Delivery notes: SSRC terminal qualification bounce corrective R4

## Scope

This is a corrective continuation of the R3 response to `BRIEF_ssrc_terminal_qualification_bounce_2026-09-28.md`. It keeps the correction tightly inside the reported SSRC qualification/runtime surfaces and one direct typed-planner ownership branch. The long-name implementation is untouched.

## What changed

### Qualification fixture

The old fixture alternated sample sign and accidentally created a source-Nyquist square wave. The replacement is a deterministic 16,384-frame periodic low-band multitone using integer bins 37, 149, and 509. Cross-channel discrimination is carried by channel-specific phase, polarity, and level, not by sample-to-sample sign inversion. The highest component is below the tightest destination-Nyquist boundary in the commissioned Binary64 rate grid.

### Wave64 compatibility

The generic Wave64 parser remains strict. A dedicated SSRC validator accepts one observed SSRC 2.4.2 quirk only: exactly two trailing zero bytes after the final data chunk, outside the data chunk's declared payload. Payload length, exact frame count, format, root extent, and all other structure stay checked. The certified SSRC terminal is the only production call site using this compatibility validator.

### Rate-aware dither capability

The qualification harness probes the exact Binary64-authorized SSRC executable for the finite native ID set used by Tonepoet before production cells are enumerated. Only SSRC's exact destination-rate-unavailable diagnostic removes a candidate ID; any unrelated probe failure aborts qualification. Floating-point cells remain no-dither. Runtime mapping also treats IDs 98/99 as destination-rate dependent, so active dither at 176.4/352.8/384 kHz is refused before invalid command construction. Global `None` remains switchless and valid.

### R4 planner correction

The continuation pass found that R3's low-level command rejection could still be bypassed by typed planning: a direct SSRC WAV integer-terminal request with unavailable dither could become an undithered Float64 SSRC split and allow a later terminal to own dither. That was a real acceptance defect, not a hardening request.

R4 changes only that direct-terminal branch. It now returns `ssrc_terminal_dither_unavailable` with the destination rate and mapping reason. A paired regression proves global `None` at 176.4 kHz remains an SSRC-owned Int16 terminal with no dither/PDF switches.

## Verification performed in this environment

- Qualification/promotion Python tests: **17/17 PASS**.
- Expanded source/adversarial audit: **30/30 PASS**.
- Pinned-capability production-grid derivation: **4,368 cells**.
- Active-dither cells at 176.4 kHz: **0**.
- Float terminal cells retained: **504**.
- Supplied failed report preserved byte-for-byte: SHA-256 `10965530548724afdb6edc53e9319d3c2a7b59b45800e24e9f8e534e9054a784`.
- Commissioned registry preserved byte-for-byte and empty: SHA-256 `9aa87c0beb7ba7e8595c133c768e903d7538ac18190bd6214a2a7ff4e01b9d8d`.
- Long-name `types.rs` and `plan_bridge.rs` remain byte-identical to the supplied bounce.
- No `target/`, `__pycache__`, or `.pyc` material retained.

## Gates not claimed here

The container has no Rust/Cargo/Rustfmt, Nix, or SSRC executable and no bundled toolchain. Consequently this delivery does not claim Rust compilation/tests, Nix gates, a fresh physical 4,368-cell execution run, promotion, Reference requalification, or Journey/Bach field conversion. Those remain operator acceptance and are listed in the R4 handoff.

The commissioned registry intentionally remains empty until a successful operator-machine execution report is reviewed and promoted.
