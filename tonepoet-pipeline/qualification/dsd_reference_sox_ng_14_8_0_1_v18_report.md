# DSD Reference policy v18 qualification-basis report

Policy v18 is an unpromoted qualification candidate. This source-controlled report records the policy delta and the evidence the real-tool gate must produce; it is not execution evidence and does not promote the candidate.

## Append-only corrections from v17

- Admit SACD track sources through the production `sacd-rs` TOC/extraction/materialization path. The production executor re-reads the TOC, binds the selected area/track geometry, hashes the ISO before and after extraction, and validates the extracted DSD carrier before Reference reconstruction. SACD DSD64 stereo plus five- and six-channel areas are represented in the qualified source matrix.
- Expand the Reference channel domain from one/two channels to one through six channels. The common certified true-peak observer and gain path are channel-generic; the historical 16x SoX analyzer matrix remains audit context and is not expanded or treated as active gain authority.
- Admit DSD64/DST across the one-through-six-channel Reference domain using the existing DSDIFF/DST production decoder/materialization path. Pinned predictive oracles cover stereo and six-channel decoding; standards-literal uncompressed DST covers the remaining channel geometries without pretending to provide independent predictive evidence. SACD/DST uses the same commissioned decoder after bounded per-track extraction.
- Add target-limited B4T reconstruction profiles for DSD128/DSD256 to 88.2 kHz and 96 kHz: 30 kHz passband with 14 kHz/18 kHz transitions and 44 kHz/48 kHz stopband edges respectively.
- Admit Int16 using SoX-ng plain triangular TPDF at the terminal. The deterministic bound charges two signed-16 LSB peak error (`2^-14` FS) once, producing Q1.63 ceiling `562949953421312` and a safe pre-terminal ceiling of `-1.010595538 dBTP`, including the existing 0.010 dB post-final reporting reserve.
- Retire P0-025 as an active admission rule. The common execution model uses path-backed W64 reconstruction/terminal carriers; the old unseekable streamed-WAV RIFF32 capacity model remains only as historical evidence. Qualification adds a sparse Wave64 probe with a logical PCM payload greater than 4 GiB, validates its exact 64-bit structure, and proves v18 planner admission without performing a redundant multi-gigabyte full decode.
- Expand exact Wave64 characterization to 300 cells: ten rates × six channels × five terminal depths.

## Qualification contract

`complete_p0_reference_qualification_report` must execute the shared production lowerer/executor, SACD DSD and DST extraction fixtures including multichannel, the full lossless package matrix, the 300-cell exact-Wave64 characterization, complete-reader checks, certified gain/true-peak checks, workspace regression, timeout/cancellation/resource checks, and paired performance/resource characterization. The new B4T response cells and Int16 terminal bound are measured under the exact pinned toolchain.

The generated common-model report, certification, and evidence JSON are release evidence. They are written only by the gated qualification path; they are not hand-authored by this policy derivation. Historical v17 report, certification, and evidence remain unchanged.

## Status

`not_run`. Production remains fail-closed until the operator executes the exact pinned gate and installs a matching passed v18 report/certification/evidence set.
