# BRIEF — HDCD dither does not revert, and RF64 `.wav` files fail tag reading

Last full gate: **7500 passed / 1 failed**, the failure being source-lock
freshness. Reference was requalified on 2026-10-15 and freshness is now 3 of 3;
the full suite has not been re-run since. Two items.

## 1. Automatic TPDF survives turning HDCD Decode off

Observed in the field:

1. HDCD Decode on. Bit depth switches to 24-bit automatically.
2. Select 16-bit. TPDF dither switches on automatically.
3. Turn HDCD Decode off. **TPDF stays on.**

The TPDF in step 2 was path-dependent, not chosen. It should go when the
condition that produced it goes.

**What we want:** an automatic TPDF selection that came from HDCD plus 16-bit
reverts when HDCD Decode is turned off, while a dither the user chose is left
alone.

What we could not explain: the revert path appears to exist already. All three
toggle routes reach `record_manual_hdcd_choice`, which calls
`recompute_auto_hdcd`, which calls `apply_auto_dither`. That function falls
through to `ordinary_auto_dither` when neither auto-TPDF condition holds, and
for a 16-bit source to an Int16 target that returns `None`. It returns early
only on `dither_overridden`, so that flag is where we would look first — but we
have not reproduced this in a test and the cause is not ours to assert.

## 2. Issue #62 — an RF64 `.wav` fails tag and artwork reading

One track of a ten-track album reports:

```
A1 Reel Around The Fountain: tags/artwork: failed to read Wav: WAV file doesn't
contain a RIFF chunk
```

That file begins with the magic `RF64`, not `RIFF`; the other nine begin with
`RIFF`. All ten are named `.wav`. The file is 2,187,972,872 bytes, which is why
it is RF64 — the format exists to carry what will not fit in a 32-bit RIFF
size field.

**What we want:** RF64 `.wav` files read their tags and artwork like any other
WAV. Only this read path refuses — `ffprobe` reads the file without complaint,
`.rf64` is already an accepted input extension, and three places in the tree
already accept either magic rather than trusting the name.

The full entry is `## 62` in `docs/OUTSTANDING_ISSUES.md`.

## Ledger

`docs/OUTSTANDING_ISSUES.md` now carries verified status for #70, #71, #72,
#74, #75 and #76. #73 is unchanged and still deliberately deferred.
#71, #72 and #74 are resolved. #75 is implemented but has never been run
against a real SACD, and #76 is only addressed for the reported instance — the
configured metadata target order is still not the single authority it asks to
be. Neither is closed.
