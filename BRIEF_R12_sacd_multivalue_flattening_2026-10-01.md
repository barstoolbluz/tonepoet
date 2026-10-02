# Brief R12 — SACD sidecar multi-value metadata arrives as one delimited string

Date: 2026-10-02 (supersedes the 2026-10-01 draft)
Base: the supplied `tonepoet-src.tar.gz` is our `main` at gate 7237/0.

## What the user sees

Reported: converting an SACD ISO that has an XML sidecar produces FLAC files
whose metadata editor shows backslashes in multi-value fields —

```
La Petite Bande, Dir. Sigiswald Kuijken\; Sophie Karthäuser - Soprano\;
Petra Noskaiová - Alto\; Christoph Genz - Tenor\; Dominik Wörner - Baryton
```

— in `PERFORMER`, `ARTIST` and `GENRE`, and that conversions from DSF, PCM and
CUE sources do not show them. Only SACD.

## What the sidecars contain

Measured across the SACD metabase sidecars on this machine — 13 distinct files,
539 tracks (22 files were found; 9 are byte-identical copies present in both
`~/livetorrents` and `~/torrents`):

| field | tracks | with `; ` |
|---|---|---|
| ARTIST | 539 | 526 |
| ALBUMARTIST | 491 | 49 |
| COMPOSER | 530 | 4 |
| TITLE | 539 | 0 |
| ALBUM | 539 | 0 |
| GENRE | 178 | 0 |

No `<track>` in the corpus repeats a `<meta name=…>`. `PERFORMER` does not
appear at all.

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

Caveat on the corpus: 11 of the 13 are one Bach cantata series from one label,
and every `; ` instance above comes from that series. The two others — a
Steely Dan and an Analogue Productions reissue — use no `; ` anywhere. Four
sidecars are included in this bundle under `sidecars/`, covering both cases.

## What a conversion produces

Converting track 1 of Bach Vol. 12 (sidecar included in this bundle) and
reading the result with `metaflac`:

| field | sidecar, track 1 | converted FLAC |
|---|---|---|
| `ARTIST` | `Johann Sebastian Bach; La Petite Bande; …` (7 names) | same, one value |
| `ALBUMARTIST` | `Johann Sebastian Bach` | the 7-name `ARTIST` string |
| `COMPOSER` | `Johann Sebastian Bach` | absent |
| `PERFORMER` | absent | the 7-name `ARTIST` string |
| `GENRE` | `Classical` | `Classical` |

The sidecar's own `ALBUMARTIST` and `COMPOSER` values survive only under
`TONEPOET_TRACK_*` and `TONEPOET_ALBUM_*` keys.

`PERFORMER` is one of the fields the escaping was reported in, and it does not
appear anywhere in the sidecar corpus.

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

Gate 7237 passed / 0 failed across 63 targets, zero warnings (run today).
`tonepoet-true-peak` is gated separately and was not run today; CLAUDE.md
records it at 160/0. Reference qualification current as of 2026-10-02;
`tests/reference_qualification_freshness.rs` fails the gate if a locked source
drifts from it.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The SACD and metadata
paths live in the root `tonepoet` crate, which peaks at 6.24 GB in one `rustc`
and will OOM under a 4 GB ceiling; write those changes uncompiled and say so.
