# TonePoet #88: independent-audit corrective delivery

**Date:** 2026-11-13 UTC
**Original rejected checkpoint:** `33461800982b3f58fb7c66991cc953b9e1ce4b0e`
**Status:** SOURCE CORRECTIONS DELIVERED; **NOT BUILD VERIFIED AND NOT ACCEPTED FOR PRODUCTION**.

This source incorporates the narrow corrections requested in the fifteen-finding audit, based on the supplied original source, brief, specification and mockups. The original Convert DSP/encoding workflow remains distinct from native extraction. No unrelated recovery subsystem or speculative concurrency framework was added.

## Finding-by-finding source assessment

| Finding | Corrective source work | Qualification / acceptance evidence still needed |
|---|---|---|
| D-01 Convert selection | Convert dispatcher uses checked program(s), independent chapter bitmap, single direct load and multi-program queue choice; none checked selects highlighted/all. | Rust build and Convert fixtures including DVD-A mixed-rate groups. |
| D-02 Managed progress | Native extraction jobs enter the existing conversion queue, worker scheduler, progress channel and cancellation token; the extraction worker does not invoke Convert DSP. | UI/progress/cancel simulation and on-disc cancellation. |
| D-03 Continuous/CUE | Continuous PCM per selected audio stream; authored order; nonadjacent chapters; exact sample-frame CUE index calculation; media and optional CUE staged privately. | **Not complete for SACD:** continuous DSF/DFF joining is explicitly blocked rather than emitting invalid DSD. CUE's 75-Hz granularity means some valid PCM boundaries are not CUE-representable and must be run without CUE. |
| D-04 Tags/artwork | Tags/fields/source choice/artwork/Keep tags controls; existing tag/art writers operate on completed staged media, not DFF. Keep tags switches chosen DFF areas to DSF. | **Partial:** disc/sidecar sources are already merged in probe results, so they cannot be separately prioritized; artwork is transferred only when an authored source image path is supplied; the existing disc-sidecar metadata editor only supports DVD-Video. Actual media tagging tests required. |
| D-05 Output naming | Template tokens, per-program subfolders, collision preflight, fail/skip/replace policy, optional extraction log. Missing tag values stay absent rather than becoming fabricated metadata. | Destination-policy fixtures and actual permission/race behavior. |
| D-06 Extract eligibility | DVD-A split-rate channel-group detection and DVD-Video interleaved-cell detection; disabled extraction rows do not participate in check-all/ranges; Convert remains accessible. | Authored equal/mixed-rate and seamless-branching discs. |
| D-07 Protection/probes | Blu-ray probe protection facts plus per-presentation asynchronous bounded source eligibility probes and pending state; failed program only is excluded; cancelling/closes invalidate probes. SACD is not gated by protection. | **Partial:** DVD-Video CSS preflight samples representative chapter starts, not all sectors; it is not an authoritative whole-disc protection verdict. Protected Blu-ray paths, CPPM and unknown-depth fixtures still required. |
| D-08 Large intermediates | Extraction-only RF64 intermediate WAVs for the affected DVD-Video, Blu-ray LPCM and DVD-Audio MLP routes; Blu-ray carrier inspection is bounded; Convert path retains previous RIFF behavior. | >4 GiB native source fixtures, 4-GiB boundary and actual decoder tests. |
| D-09 Speaker mapping | Nonzero observed WAV channel mask takes priority; only recognized **explicit** layout descriptions can supply a missing mask; no count-derived 5.1 guess. | **Partial:** when DVD-Video carrier reports mask zero and no authoritative source speaker assignment is available, output still has unknown layout. Exact DVD-Video/DVD-A 5.1/7.1 assignments require channel-labelled fixtures. |
| D-10 Responsive Output | Wide two-column Output, medium focused-row scroll, compact tab navigation; output-row hitbox registration retained. | 150x40, 140x36, 96x30, 64x20, 63x19 rendered UI and mouse QA. |
| D-11 Source labels | DVD-Video/Blu-ray language/audio-role labeling, nonadjacent playlist grouping, two-line SACD/DVD-A groups, wide start times; SACD conditional authored ISRC/composer/etc details. | Media-specific visual fixture and real-text validation. |
| D-12 Authored angles | One selected angle across each title/playlist's streams; `[`, `]` and mouse arrows; source options passed to materializers. | Distinct-payload two-angle authored sources and chapter identity checks. |
| D-13 More / mouse actions | More routes to existing tags editor where supported, existing MusicBrainz command and Analyze; DVD-A group presets; row click focuses vs checkbox click toggles; Shift-range; popup key ownership. | Metadata editor and MusicBrainz routing must be verified in real TUI. Other disc formats lack an equivalent existing disc-sidecar editor. |
| D-14 Classified folders | Cached-disc classification consulted for directory context menu; no synchronous directory probe. | Pending/known classification and all disc folder variants. |
| D-15 Atomic publication | Private work directory outside the destination but on its filesystem; configured scratch used when suitable, otherwise target-volume sibling; complete files published via same-filesystem rename; no hard links. | exFAT, separate scratch/destination volumes, cancellation and late failure. A destination equal to an entire filesystem mountpoint may have no outside-destination same-volume staging location; reject rather than expose partial files. |

## Qualification boundary

This environment does **not** have `cargo`, `rustc` or `rustfmt`, nor the optical-disc fixtures needed for end-to-end testing. **No Rust compile, Rust test, TUI integration test, SACD frame comparison, bit-perfect PCM comparison, or disc playback passed here.** Independent Python/FFprobe reference-container checks and the archive/Git integrity checks do not substitute for those tests.

Before calling the issue complete, run `verification/verify_on_build_host.sh`, review any compile/test failures, exercise the real-disc/viewport matrix in `docs/issue88/DELIVERY_STATUS.md`, and resolve or explicitly re-scope the four qualified gaps above (D-03, D-04, D-07, D-09). Do not add broad protective abstractions to compensate for missing fixtures; test the narrow paths and correct only observed failures.

## Integrity and resume

The archive root includes `START.md`, `notes/CORRECTIVE_RESUME.md`, `VERIFY_RETURN.sh`, complete Git objects and an exportable series `verification/ISSUE88_commits.mbox`. All patches are against the original import base `30d4d7c15793998f7edc7bf581a975a7e4c7b132`. The source is a correction checkpoint suitable for a Rust build-host continuation, not a certified release.
