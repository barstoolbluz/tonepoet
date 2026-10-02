# Brief R12 — SACD sidecar multi-value metadata arrives as one delimited string

Date: 2026-10-02 (supersedes the 2026-10-01 draft)
Base: the supplied `tonepoet-src.tar.gz` is our `main` at gate 7237/0.

## What the user sees

Converting an SACD ISO that has an XML sidecar produces FLAC files whose
metadata editor shows backslashes in multi-value fields:

```
La Petite Bande, Dir. Sigiswald Kuijken\; Sophie Karthäuser - Soprano\;
Petra Noskaiová - Alto\; Christoph Genz - Tenor\; Dominik Wörner - Baryton
```

Conversions from DSF, PCM and CUE sources do not show them. Only SACD.

The converted file holds one value containing semicolons. A DSF-sourced
conversion of a comparable album holds several distinct values.

## What the sidecars contain

Measured across the 22 SACD metabase sidecars on this machine, 961 tracks:

- No `<track>` ever repeats a `<meta name=…>`.
- `ARTIST` contains `; ` in 948 of 961 tracks.
- `COMPOSER` in 8 of 952. `ALBUMARTIST` in some.
- `TITLE`, `ALBUM`, `DATE`, `ISRC`, `CATALOGNUMBER`, `TRACKNUMBER`: never.

A representative value:

```xml
<meta name="ARTIST" value="Johann Sebastian Bach; La Petite Bande; Sigiswald
Kuijken; Gerlinde Sämann; Petra Noskaiová; Christoph Genz; Jan Van der Crabben"/>
```

and one that is less tidy:

```xml
<meta name="ALBUMARTIST" value="La Petite Band, Sigiswald Kuijken; Sämann,
Noskaiova, Genz, Van der Crabben"/>
```

## The outcome we want

Multi-value metadata from an SACD sidecar reaches the converted files as
multiple values, the way it does from every other source. A listener's tagger,
a library scanner and tonepoet's own editor should see the same structure for
the same album regardless of which source it came from.

Where a field genuinely holds one value that happens to contain a semicolon,
it stays one value.

Writing a sidecar back should reproduce the source's own convention rather
than inventing a new one.

## State

Gate 7237 passed / 0 failed across 63 targets, zero warnings.
`tonepoet-true-peak` 160/0. Reference qualification current as of 2026-10-02;
`tests/reference_qualification_freshness.rs` fails the gate if a locked source
drifts from it.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The SACD and metadata
paths live in the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc`
and will OOM under a 4 GB ceiling; write those changes uncompiled and say so.
