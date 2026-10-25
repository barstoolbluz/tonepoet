# IP-05 conformance matrix

| Requirement / cell | Authored implementation | Executed evidence | Current status |
| --- | --- | --- | --- |
| Default OFF; explicit exact `=1` opt-in | Hidden gate; S1 untouched on false | Source review / static verifier | Static only |
| Whole authored album / single physical FILE | Full CUE sheet count equals selected source/scheduler count; group count and typed preflight | Added real materializer-shape 12-authored/2-selected Rust regression **NOT RUN** | Pending host |
| Non-FLAC / rate / depth / partial / multiple FILE | Group-atomic prelaunch decline; complete Range/Set selection permitted | Named Rust rejection tests **NOT RUN** | Pending host |
| HDCD/de-emphasis/effects/active gain | Registered effect and active gain gates; exact typed planner | Host excluded-mode matrix **NOT RUN** | Pending host |
| Non-Unix runner | Explicit reject before worker creation | Cross-platform build **NOT RUN** | Pending host |
| 16-bit, 3 ranges | Exact FFmpeg FLAC simulation | Native quick PCM PASS (3/3) | **Native only** |
| 24-bit, 3 ranges | Exact FFmpeg FLAC simulation | Native quick PCM PASS (3/3) | **Native only** |
| 16-bit, 12 ranges | Exact FFmpeg FLAC simulation | Native full PCM PASS (12/12) | **Native only** |
| 24-bit, 30 ranges | Exact FFmpeg FLAC simulation | Native full PCM PASS (30/30) | **Native only** |
| INDEX gaps / noncontiguous ranges | Existing IP-02 exact atrim + group identities | Native gap PCM PASS (3/3) | **Native only** |
| 16/24 FLAC S3 vs S1 end-to-end | Named real-worker parity tests | `cargo` **NOT RUN** | Pending host |
| RG Off/Track/Album/Both; `IfMissing` | Existing post-output native RG | Rust real worker tests + full host run **NOT RUN** | Pending host |
| Source tags, multivalues, cover art | Existing shared album postprocess | Full host conversion **NOT RUN** | Pending host |
| Provenance, two child records, exact extent | IP-04 worker returns typed ordered outputs | Rust named test **NOT RUN** | Pending host |
| Shared permits, contention, cancel/reaping | IP-03 semaphores with cancellation-aware wait | New native-shell Rust test **NOT RUN** | Pending host |
| Child/metadata/RG/verification failure; partial | Existing album failure handling | Fault-injection host matrix **NOT RUN** | Pending host |
| Atomic and cross-device publication | Existing album publisher untouched | Host conversion cross-FS **NOT RUN** | Pending host |
| Strict visible destination file set | Publisher untouched; independent exact-set checker | Checker self-test PASS; actual TonePoet conversion **NOT RUN** | Pending host |
| Full Rust format, filtered and workspace suites | Host gate commands provided | `cargo` absent | NOT RUN |
| No inherited build caches in source archive | Delete inherited Python bytecode; reject caches in static checker | Archive inventory and source-manifest audit | Static only |

**No row marks integrated Rust/audio semantics accepted in this environment.**
