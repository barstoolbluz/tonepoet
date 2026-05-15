I treated phase0_audit_prompt.md as the task spec: audit the three named functions at barstoolbluz/tonepoet commit 644ac50, with the 40K-album / 400K-track archival workload as the decision frame. I inspected the pinned source statically; I did not run conversions or the test suite.

Verdicts
Function	Verdict	Main reason
process_item	Refactor	Good queue-facing skeleton, but it mixes planning, dispatch, conversion, progress, logging, and completion semantics in one place.
extract_and_convert_7z	Rebuild	Too much archival-critical behavior lives in one ~1,900-line function, including extraction, normalization, conversion, merge, tags, ReplayGain, logging, final moves, and cleanup.
extract_single_image_tracks	Refactor	The CUE boundary model is useful, but the extractor returns only temp paths, drops metadata, blocks synchronously, and lacks sample-count validation.
1. process_item — Refactor

process_item has a reasonable top-level shape: it receives a ConversionItem, emits progress, dispatches by format, calls the backend conversion path for normal audio files, and returns (item_id, ConversionStatus) for queue orchestration. That skeleton is worth preserving. The queue code already treats this function as the worker-unit boundary, spawning it per item and converting errors into failed statuses at the queue layer.

The problem is that process_item is not just a worker boundary. In src/convert/processor.rs:1167-1428, it also performs input validation, output path planning, progress phase semantics, archive dispatch, copy-mode dispatch, backend adapter setup, ReplayGain messaging, completion signaling, and conversion-log writing. That concentration makes it a poor foundation for the next pipeline, where one source can intentionally produce N tracks. The current repo describes a batch conversion architecture, but this function still assumes a mostly one-input/one-output control flow except for its archive escape hatch.

Specific issues:

Failure semantics are inconsistent. Missing input returns Ok((id, Failed { ... })) at processor.rs:1174-1179, while backend/archive errors return Err(...) and rely on the queue wrapper to normalize them. That is workable today, but for resumable archival runs it creates two classes of failure: “status failure” and “control-flow failure.”
Output planning happens too early and too simply. process_item calls determine_output_path(&item) at processor.rs:1194-1196; that helper starts from input_path.with_extension(output_ext) and checks collisions. It does not model album output directories, per-track outputs, custom output roots, or a source-to-tracks manifest. For 7z, the computed output_path becomes a proxy used later by the archive function, not the real final output.
Completion and logging are entangled. The function sends a completed progress update before it writes conversion logs. In a weeks-long batch, “completed” should mean all required final side effects succeeded or that nonfatal side effects have been explicitly marked nonfatal.
It is not source-cardinality aware. SACD ISO and CUE+FLAC both need a first-class “materialize source into tracks” stage. process_item currently branches by FileFormat::SevenZip versus “normal audio,” which will push the new code toward more special cases rather than a stable source pipeline.
Keep

Preserve the worker boundary, progress channel integration, backend error mapping, and FLAC copy-mode fast path. Those are real strengths.

PR boundary
Add a PipelinePlan / PreparedSource model: one input item can produce one or more PreparedTracks.
Move output planning out of process_item.
Convert process_item into a thin orchestrator: preflight → plan → materialize → convert → finalize → report.
Normalize all internal failures as errors and convert to ConversionStatus::Failed only at the queue/reporting boundary.
Move final completion signaling after all required finalization steps.
2. extract_and_convert_7z — Rebuild

This function should not become the template for SACD ISO or CUE+FLAC. It contains many useful behaviors, but its current shape is unsafe as a long-term foundation.

The function at src/convert/processor.rs:1974-3889 performs archive extraction, output directory choice, stale workdir deletion, audio discovery, folder renaming, file renaming, tag updates, optional lineage-tag injection, source metadata capture, optimized merge conversion, copy-mode handling, parallel conversion, post-conversion renaming, old merge conversion, feature logging, album ReplayGain, post-conversion hooks, custom-output moves, backup/rollback behavior, and cleanup. The processor file itself is a large central module, not a narrow archive materializer.

This is the load-bearing problem: archival reliability needs clear transaction boundaries. This function has too many side effects to reason about as one unit.

Specific issues:

It reports completion too early. The function sends finalization and Completed { output_path: output_dir } around processor.rs:3567-3583, but album ReplayGain, post-conversion features, and the custom output move happen later around processor.rs:3662-3861. A later move failure can return an error after the UI or queue listener already saw completion. For intermittent batch runs, that is a serious trust problem.
Partial track failure can still produce album success. In the parallel conversion path, individual task failures increment a counter, but the function only returns an error when all tracks fail. A 12-track album with one failed track can continue toward finalization. The log records a failed feature status, but the item-level result can still complete. For a personal archive, the default should be “any missing track fails the album,” with an explicit opt-in for partial output.
Per-track error data gets lost. The conversion log construction marks failed per-track results but leaves error_message: None. At 400K conversions, the log has to answer “what failed and why?” without rerunning the job.
Tool path handling is inconsistent. The function receives _tool_paths but ignores it for archive extraction and calls tools such as 7z, ffmpeg, loudgain, metaflac, and opustags through ad hoc command construction. ConversionItem already carries archive-specific data such as an optional archive password, so the data model is in place, but the tool runner layer is not.
Long-running child processes have no timeout or cancellation policy. A stuck 7z, ffmpeg, or loudgain can strand a batch. The extraction progress loop is time-estimated, not child-output-driven, and it will run until the process exits.
Temporary-output policy is not a real transaction. The .extract_* folder doubles as both workdir and, in the no-custom-output case, final album output. That makes retry behavior and crash recovery harder than they need to be. A reliable archival pipeline should write into a job-scoped staging directory, validate results, then publish to the final path.
Debug residue can ship into outputs. The optimized merge path writes a metadata_debug marker next to the merged output. That is acceptable while diagnosing one album; it is not acceptable in a production archival path.
Keep

There is valuable code here. Preserve these behaviors, but move them into smaller stages:

archive extraction with multithreaded 7z;
scratch-directory support;
cross-filesystem final move;
backup/rollback for existing destination folders;
pre-conversion source metadata capture;
semaphore-limited parallel conversion;
optimized merge-before-final-encode;
nonfatal tagging/ReGain behavior where the user explicitly chooses that policy.
PR boundary

Treat this as a rebuild of the archive pipeline, not an edit-in-place patch.

Archive materializer PR: extract_archive() returns a staged source tree plus manifest; no conversion.
Track discovery PR: discover_album_tracks() returns ordered PreparedTracks with source metadata and intended output paths.
Conversion PR: convert_tracks() applies backend conversion with structured per-track success/error records.
Merge PR: merge_tracks() becomes a separate optional stage with its own validation and cleanup.
Finalize PR: publish_album_output() performs final move/rollback and only then emits item completion.
Tool runner PR: one async child-process wrapper with configured binary paths, timeout, stderr capture, cancellation, and command logging.
Failure-policy PR: default archival policy should fail the album on any track failure; partial output should require an explicit option.
3. extract_single_image_tracks — Refactor

This function is much smaller and easier to salvage than extract_and_convert_7z. The surrounding parser already distinguishes single-image CUE layouts from track-per-file layouts by requiring multiple tracks, INDEX 01 on each track, and the same referenced audio file. That detection logic is a useful starting point. The GitHub view shows extract_single_image_tracks creating per-track FLACs with ffmpeg, using wvunpack as a fallback when the source cannot be read directly.

The function should not ship as the new archival CUE pipeline without refactoring.

Specific issues:

It returns only Vec<PathBuf>. The parser has track titles, performers, file references, and track numbers, but extraction throws away that structured metadata. The new pipeline needs PreparedTrack, not just temp FLAC paths.
It uses blocking process calls. std::process::Command blocks the calling thread for wvunpack and every ffmpeg invocation. That is tolerable in a small TUI operation, but not as part of a long batch pipeline.
It discards useful error output. The ffmpeg command pipes stderr but uses .status(), so the returned error says only that extraction failed for a track. Bad source files, codec issues, bad seeks, and permission errors need captured stderr in the job log.
It does not validate outputs. After ffmpeg succeeds, the function does not check that the output file exists, has nonzero size, or has the expected sample count/duration.
Splitting by seconds risks sample drift. The boundary model is sample-based, but the command converts boundaries into floating-point -ss and -t values. For CUE archival work, sample-count validation should be mandatory. A better extractor should either cut by sample-aware filters where practical or verify each result after extraction.
Partial failure cleanup is absent. A failed track leaves earlier temp tracks and the optional _image.wav decode in place. A job-scoped cleanup guard would make retries safer.
Keep

Preserve the single-image detection criteria, sample-boundary calculation, one-time WavPack decode fallback, and low-compression FLAC temp output. Those choices fit a materialization stage.

PR boundary
Replace Vec<PathBuf> with Vec<PreparedTrack>.
Move command execution to the same async tool runner proposed above.
Capture stderr/stdout metadata in structured errors.
Add per-track sample-count validation.
Add cleanup guards for failed extraction.
Add fixtures for ordinary CUE+FLAC, WavPack v4, malformed CUE, pregap/index cases, Unicode paths, high sample-rate sources, and intentional ffmpeg failure.
Cross-cutting recommendation

Do not mirror these three functions directly into the SACD/CUE pipeline. Use them as source material for a staged architecture:

Input item
  -> Source materializer
  -> Ordered PreparedTrack manifest
  -> Per-track conversion
  -> Optional album merge
  -> Metadata / ReplayGain / feature stages
  -> Atomic publish
  -> Completion event + durable log

The recent crates/sacd-rs work is a better model: it keeps the SACD extraction/parsing concern narrow and leaves higher-level orchestration to callers, with a staged roadmap visible in its module documentation.

The immediate priority is not more format support. It is to create the transaction boundary that all multi-track sources share: materialize tracks, validate them, convert them, publish them, and only then mark the album complete.

FOLLOW UP:

I broadly concur. I would keep the verdicts exactly as reported:

Function	My validated verdict
process_item	Refactor
extract_and_convert_7z	Rebuild
extract_single_image_tracks	Refactor

I validated against the uploaded prompt’s scope: three named functions, pinned commit 644ac50, and the 40K-album / 400K-track archival reliability frame. I also checked the repo at 644ac50; the README describes exactly the kind of archive extraction, conversion, ReplayGain, metadata, renaming, CUE, and logging responsibilities that show up in these functions.

Where I concur
1. process_item — Refactor

Yes. The reported critique is fair.

The queue-facing boundary is worth preserving: the queue worker calls process_item, then converts returned Err values into ConversionStatus::Failed. But inside process_item, missing input returns Ok((id, Failed { ... })), while later failures use ? and become Err. That confirms the “two failure channels” criticism.

I also agree that output planning happens too early and too simply. determine_output_path() just uses input_path.with_extension(output_ext) and collision suffixes. It does not model album directories, track manifests, custom roots, or one-source-to-N-track output. That makes it a poor base for SACD ISO or CUE+FLAC materialization.

The “completion before logging” concern is also valid: process_item sends Completed before it writes the conversion log. I would phrase this as a status-contract problem rather than an absolute bug, because the current code treats log-writing failure as nonfatal. But for archival work, the pipeline should explicitly decide which finalization steps are required, which are optional, and what “complete” means.

Extra note: the hardcoded UI pacing sleep and hardcoded feature worker_count: 8 inside this worker are additional signs that orchestration, UX signaling, and runtime policy need separation.

2. extract_and_convert_7z — Rebuild

Yes. This is the strongest conclusion in the report.

The function spans roughly 1,900 lines and acts as extractor, normalizer, metadata editor, converter, merger, ReplayGain runner, logger, publisher, rollback handler, and cleanup routine. That is too much authority for one function in a weeks-long archival workflow.

The report’s most important finding is correct: it sends Completed { output_path: output_dir } before album ReplayGain, post-conversion feature generation, and custom-output publishing. A later move failure can still return an error after listeners have already seen completion. For batch archival use, that is a serious trust problem.

The partial-failure finding is also correct. The parallel conversion path increments failure counts, logs failures, but continues unless all tracks fail. So an album can succeed at the item level with missing tracks. That policy might suit casual batch conversion, but it should not be the default for archival conversion.

The per-track error-data point is also accurate: the conversion log records failed status but leaves error_message: None with a TODO. At 400K conversions, that makes the log much less useful.

The tool-runner critique is fair too. The function receives _tool_paths but uses direct command construction for tools such as 7z, ffmpeg, loudgain, and related utilities. The README itself says the target design wants configurable tools and no hardcoded values, so a central async tool runner would align with the project direction.

One calibration: “rebuild” should mean retire this as the orchestrator, not discard every useful behavior. The report correctly identifies valuable pieces to salvage: 7z extraction, scratch directory support, cross-filesystem move, backup/rollback, source metadata capture, semaphore-limited conversion, optimized merge, and optional nonfatal post-processing.

3. extract_single_image_tracks — Refactor

Yes. The function is salvageable, but not as the final archival CUE pipeline.

The detection model is useful: it requires multiple tracks, INDEX 01 timestamps, a single shared FILE, probes sample count/rate, and computes sample boundaries.

But the extractor returns only Vec<PathBuf>, even though the parser retains album and track metadata such as title, performer, track number, and file references. The function also uses blocking std::process::Command, decodes WavPack through wvunpack when needed, shells out to ffmpeg -ss/-t, pipes stderr but uses .status() rather than capturing it, and returns only generic track-failure messages.

I also agree with the sample-validation concern. The model starts with sample boundaries, but extraction converts them to floating-point seconds. Even if that usually works for common CUE rates, archival code should verify that each output exists, has nonzero size, and matches the expected duration/sample count within a defined tolerance.

Minor disagreements or refinements

I do not materially disagree with the findings. I would only refine three points:

First, not every post-conversion side effect must be fatal. Log writing, CUE generation, ReplayGain, lineage tags, and metadata repair can each be required or optional depending on user policy. The issue is that the current code does not make that policy explicit and can signal completion before later side effects finish.

Second, extract_and_convert_7z deserves “rebuild” because of its orchestration shape, not because every internal algorithm is bad. Several pieces are useful and should move into smaller tested stages.

Third, the extract_single_image_tracks sample-drift concern should become a validation requirement, not necessarily a mandate to avoid ffmpeg entirely. A practical first step is: cut with the current approach, then prove the resulting FLACs match the manifest.

Additional feedback I would add

I would make the next PRs revolve around one shared invariant:

A source item becomes a durable manifest of expected tracks before conversion starts, and completion can only fire after the manifest has been satisfied or explicitly marked partial by policy.

That implies:

Add a PreparedSource / PreparedTrack model with source path, intended output path, track number, title, artist, sample rate, expected sample count or duration, source metadata, and extraction provenance.
Add a failure policy enum, with archival default set to fail the album if any expected track fails. Partial output should require an explicit option.
Add one async ToolRunner for 7z, ffmpeg, ffprobe, loudgain, metaflac, opustags, wvunpack, etc. It should own configured binary paths, timeout, cancellation, stdout/stderr capture, command logging, and redaction of passwords.
Publish atomically from a job-scoped staging directory. The current .extract_* folder sometimes acts as both workdir and final output; that makes retries and crash recovery harder than necessary.
Treat logs as durable job records, not best-effort decoration. At minimum, each track record should preserve success/failure, exact error text or stderr summary, command/pipeline info, source file, output file, sizes, and timing.
Add tests specifically for completion semantics: one failed track, a failed custom-output move, loudgain failure, tool timeout, stale staging directory, Unicode paths, pregap/index cases, and proof that no debug marker files ship into final output.

So yes: I concur with the reported audit. The strongest recommendation is to avoid copying these functions into the SACD/CUE work. Use process_item as the worker boundary, use extract_and_convert_7z as a source of behaviors to split apart, and use extract_single_image_tracks as a prototype materializer that needs metadata, async process handling, validation, and cleanup before it becomes archival-grade.
