# Metadata write performance implementation notes

Date: 2026-07-09

This bundle replaces the happy-path FLAC metadata writer used by the metadata editor, artwork editor, and inline browse metadata edits. The goal is to stop doing full-file backup copies and full-file Lofty rewrites for ordinary FLAC metadata edits, especially on sshfs/network filesystems.

## FLAC text-tag writes

`src/tui/probe.rs` now contains a small FLAC metadata-region writer. It detects FLAC by extension first and falls back to magic-byte sniffing, parses the FLAC metadata block chain, preserves STREAMINFO and unrelated metadata blocks, replaces the Vorbis comment block, and consumes existing PADDING when the new metadata fits.

For padded FLAC files, the writer:

- reads only the FLAC metadata region, not the audio payload;
- writes a small `.tonepoet-meta-journal` containing the original metadata region, the intended replacement metadata-region identity, target identity/check fields, and the writer owner identity;
- overwrites only the metadata region in place;
- fsyncs the write;
- removes the metadata journal after commit;
- never creates a `.tonepoet-bak` full-file backup.

The current metadata journal format is `TPFLACMJ4`. It stores PID plus stronger owner identity (Linux process start time and boot ID hash where available, plus a per-process token for same-process ownership), while retaining parser support for older `TPFLACMJ2`/`TPFLACMJ3` stale journals. Native FLAC mutations now acquire one common per-target writer claim before any tag, artwork, or overflow-rewrite mutation can proceed. The claim is stored as `<name>.tonepoet-write-lock`, created with atomic `create_new` / `O_EXCL` semantics rather than hard links, so the sshfs target does not need `link(2)` support. The lock is owner-stamped using the same strong process identity model as the journals. A live lock makes readers and competing writers return a transient "metadata/artwork write in progress" error rather than parsing or mutating the file. This is true even inside the same process for incidental read guards: while a batch-owned artwork rollback journal and common lock are live, reads are blocked rather than allowed to parse or silently skip rollback. A stale lock triggers stale metadata/artwork journal recovery, removes the lock, and retries acquisition once. Only after this common file-level claim is held does the writer install the operation-specific `.tonepoet-meta-journal`. That journal is written and fsynced as a unique temp file, then renamed into place while the common claim prevents any competing journal type from racing it. Existing live-owner metadata journals still make the new write fail; stale-owner journals are recovered/cleaned and acquisition is retried once. Read guards and competing native writes do not consume a metadata journal while the recorded owner still matches a live writer. They return a transient "metadata write in progress" error instead of parsing or restoring a possibly torn metadata region. Startup/current-directory recovery skips active-owner metadata journals and recovers only stale ones. The write path has an explicit owned-recovery mode for its own failed in-place overwrite so the owner can restore from its still-armed journal without permitting another process to race that recovery. PID existence alone is not trusted for owner liveness; a PID-reuse-style start-time or boot-ID mismatch makes the journal stale and recoverable.

The audio region starts at the same offset after a successful in-place write and is byte-identical.

## FLAC artwork writes

Artwork writes now ride the same native FLAC metadata-region machinery. The writer serializes FLAC PICTURE blocks directly, removes/replaces only the targeted picture type, preserves unrelated metadata blocks, and uses existing padding when the new PICTURE block fits.

For padded FLAC artwork insertion/removal, the happy path still touches only the metadata region and does not create a full-file `.tonepoet-bak`. If padding is exhausted, the same bounded streaming rewrite path is used and adds 1 MiB of padding for future text or artwork edits.

The artwork batch helper no longer makes unconditional full-file backups for every FLAC target. Before each native FLAC artwork mutation, it writes a small durable `.tonepoet-artwork-rollback` journal containing that file's original FLAC metadata region, original target identity, and the intended post-mutation metadata identity. Same-process failures still use the in-memory snapshot for immediate rollback, and stale rollback journals left by a process death are recovered before later FLAC reads or during startup directory recovery. Full-file backups remain only for non-FLAC Lofty fallback paths. Returned compact artwork metadata is projected from the pre-write metadata plus the requested mutation, so the artwork path no longer performs a full post-write batch reread just to rebuild UI state.

The FLAC artwork rollback journal is per file, not a whole-library transaction coordinator. Its current `TPFLACAJ3` format stores the owner PID plus stronger owner identity (Linux process start time and boot ID hash where available, plus a per-process token for same-process ownership), canonical path, original file length, original metadata length/checksum, original audio offset, filesystem device/inode where available, mtime/ctime where available, and the intended post-write metadata length/checksum plus intended file length. Artwork rollback-journal acquisition happens while the same common `<name>.tonepoet-write-lock` is held, so a tag write and an artwork write cannot mutate the same FLAC metadata region concurrently under separate recovery artifacts. The rollback journal is written and synced as a temp journal, then renamed into place under that common claim. Existing live-owner rollback journals make a competing artwork operation fail instead of overwriting the recovery record; stale-owner rollback journals are recovered/cleaned and acquisition retries once. Legacy `TPFLACAJ1`/`TPFLACAJ2` records are still parsed, but only AJ3 can prove live ownership strongly enough to suppress stale rollback during incidental reads. Recovery suppresses rollback only when the recorded owner still matches the same live process identity; PID existence alone is not trusted because PID reuse can otherwise suppress recovery indefinitely.

A batch is considered committed only after every successfully written FLAC target has had its rollback journal removed. If journal removal succeeds but the parent directory fsync cannot be confirmed, the artwork mutation is committed with a durability warning rather than reported as an ordinary failed batch; the artwork write result carries those warnings so the metadata editor can surface them in status instead of hiding them or converting them into rollback failures. If journal removal itself fails, the rollback journal remains armed and the batch is not reported as cleanly committed because later stale-journal recovery may roll the file back. Same-process rollback paths now follow the same rule: if restoring the saved FLAC metadata snapshot fails, the rollback journal is deliberately left in place and the error says that the journal remains armed for later recovery or inspection; cleanup is attempted only after the restore has definitely succeeded or recovery has determined the target is already back in the original state. If the process dies after one FLAC artwork target succeeds but before the batch commits, later recovery restores that target's saved metadata region only when the current target still matches either the journaled original identity or the intended artwork mutation identity. If the file has been externally replaced or modified so it matches neither identity, recovery refuses rather than overwriting unknown content with stale metadata. Artwork rollback recovery is itself idempotent: if recovery crashes during an in-place restore and leaves an unparsable metadata region, the next recovery uses the journaled identity and saved metadata bytes to retry the restore instead of refusing merely because parsing failed; if an overflow-style rollback restore has already committed by rename but the journal cleanup did not finish, the next recovery recognizes the original metadata bytes as an already-rolled-back state and removes the stale rollback journal without requiring the old inode/ctime to match. While the owning process is provably still alive, incidental reads do not consume the active rollback journal; ordinary same-process failure/cancellation paths perform rollback explicitly.

## Padding exhaustion / overflow rewrite

When the rewritten metadata does not fit the existing metadata region plus padding, the writer performs the unavoidable rewrite by streaming:

- a same-directory temporary file is created with restrictive permissions (`0600` on Unix) so there is no window where the replacement is broader than the source;
- new FLAC metadata is written with 1 MiB of padding for future edits;
- the old audio payload is copied with a bounded 1 MiB buffer;
- the temp file is flushed and synced;
- ordinary file metadata from the source is applied to the temp;
- immediately before rename, the source path is revalidated against the identity captured when streaming began;
- the temp is atomically renamed over the original only if that revalidation succeeds, and the parent directory is synced; if the rename has already committed but the parent-directory sync cannot be confirmed, the write returns a committed-with-durability-warning outcome rather than an ordinary failure.

The overflow path preserves mode/permissions and, on Unix where permitted, owner and group. Owner/group and mode preservation are security-relevant: if the replacement would not retain the source owner/group or mode, the rewrite aborts before rename and the original file remains in place. Access and modification timestamps are restored on Unix as a non-security property; if the platform/filesystem refuses timestamp restoration, the write still succeeds.

On Linux, extended attributes are copied when the filesystem supports them, and captured xattrs are verified on the rewrite temp file before rename. Whole-filesystem xattr absence (`ENOTSUP`, `EOPNOTSUPP`, or `ENOSYS` while listing attributes) is an explicit unsupported-capability downgrade. It is not treated the same as a failed preservation attempt. If xattr listing succeeds but an existing attribute cannot be read, or if a captured attribute cannot be restored and verified on the replacement, the overflow rewrite aborts before rename so the original file remains intact. POSIX ACL storage xattrs are not copied through the generic xattr path; they are handled by the ACL path below to avoid corrupting ACL encoding.

ACLs are also no longer silent-best-effort. When `getfacl` is available and reports extended ACL entries, `setfacl` must be available and must restore them successfully before rename; failure to capture or restore an existing extended ACL aborts the overflow rewrite. If `getfacl` reports only mode-equivalent base ACL entries, preserving mode bits is sufficient. If ACL tools or ACL support are unavailable and no existing POSIX ACL xattr is detected, the rewrite proceeds with an explicit unsupported-capability downgrade and preserved mode bits. If a POSIX ACL xattr is detected but `getfacl` is unavailable, the rewrite refuses to proceed because an existing ACL could not be captured. Tests cover supported ACL preservation, injected ACL capture/restore failure, and deterministic skips when tools/filesystem support are absent.

Hardlinks and symlinks are handled conservatively. Native FLAC writes on Unix now refuse any target whose link count is greater than one, even when the edit would fit in existing padding and preserve the inode. In-place hardlink writes would mutate every alias while creating a path-local recovery journal beside only one alias; a crash followed by opening another hardlink name could miss the journal. Rather than pretending path-local journals are inode-aware, the native writer returns an actionable error asking the caller to remove hardlinks or edit a de-hardlinked copy. Padding-exhaustion overflow rewrites also refuse hardlinked files because they would replace the inode and silently sever hardlink identity. Native FLAC writes through symlink paths are refused for both padded in-place edits and overflow rewrites. Although an in-place write would mutate the symlink target inode, the native crash-recovery artifacts are path-local sidecar journals; placing recovery state beside the symlink while mutating a different target path would break recovery locality if the symlink is later removed or the target is later opened directly. The caller must rewrite the canonical target path instead. This same policy applies to native FLAC artwork rollback journals.

Overflow commit also protects against concurrent source changes. On Unix, the writer captures the source path's device/inode, length, hardlink count, mode, owner/group, mtime, and ctime after opening the file and before streaming. Immediately before the atomic rename, it re-stats the path, refuses if the path has become a symlink, and refuses if any captured identity or metadata field changed. On non-Unix platforms, it performs a best-effort revalidation using file length, modified time, and readonly status. A revalidation failure aborts before rename, leaves the externally modified source in place, and cleans the same-process temp file. This avoids overwriting another process's replacement or in-place modification with a temp file built from stale bytes.

Overflow stream rewrites are serialized process-wide inside the native FLAC writer. Metadata-region writes remain cheap and can run with the normal batch parallelism, but the expensive bounded-copy path is limited to one active stream rewrite at a time so four parallel metadata-editor workers cannot accidentally launch four full audio-payload copies over sshfs or another network filesystem.

This still avoids the old 4x-network-I/O pattern. It does not create a full-file `.tonepoet-bak` sibling and it does not buffer the audio payload in memory. The inode replacement tradeoff is now limited to non-hardlinked padding-exhausted FLAC rewrites; allowed non-hardlinked padded in-place edits keep the original inode and touch only the metadata region.

## Crash safety

For in-place FLAC writes, the `.tonepoet-meta-journal` is committed before the metadata region is touched. The journal stores the original metadata region, original audio offset, original file length, original metadata checksum, intended replacement metadata length/checksum, canonical path bytes, and filesystem identity where the platform exposes it. Recovery refuses to apply a journal if the target file identity or file length no longer matches the saved record. If the current metadata region already matches the original checksum, recovery removes the journal as a no-op. If it matches the intended replacement checksum, recovery treats the synced metadata overwrite as committed and removes the journal. Otherwise, recovery restores the original metadata region even when the torn current metadata is syntactically parseable but reports a different audio offset.

Recovery is now reached before reads/probes as well as before later writes. The startup path scans the current browse directory for `.tonepoet-write-lock`, `.tonepoet-meta-journal`, and `.tonepoet-artwork-rollback` siblings. Active common write locks are skipped and cause read guards to report a transient in-progress condition; stale common locks trigger stale journal recovery and cleanup before later writes retry. Tests that simulate process death now make both the operation journal and the common write lock stale or absent; marking only the operation journal stale while leaving a live common lock is intentionally treated as an active writer, not as a recoverable crash. The tag/probe entry points recover a path-local journal before handing a FLAC file to Lofty or ffmpeg. Read guards also resolve symlink paths and recover the canonical target-local FLAC metadata and artwork rollback journals before parsing, even though native writes through symlinks remain refused. That keeps the conservative write policy from missing a real target journal when the user later opens the same file through a symlinked library path. Direct read helpers outside the main probe path, including embedded-CUESHEET preview, MusicBrainz duplicate-release detection, and selected-path tag sorting, now route through the same recovery guard before calling Lofty. This prevents a half-written metadata block chain from being parsed before recovery has a chance to run.

For padding-exhaustion rewrites, the original file is unchanged until the temporary file is fully written, synced, has source metadata applied, and is renamed. A crash or injected failure before rename leaves the original file intact and at most an orphan temp file. Same-process failures and cooperative cancellations before rename clean their temp immediately; startup-directory recovery also removes same-directory `.tonepoet-flac-rewrite-*` temporary files left by interrupted stream rewrites.

For FLAC artwork batches, stale `.tonepoet-artwork-rollback` journals are also recovered before reads and during startup directory recovery. If a process dies after a FLAC artwork target has been mutated but before the batch-level cleanup commits, the journal restores that file's pre-batch metadata region. Recovery first validates the canonical path and target identity. If the current metadata already matches the original journaled region and the original file identity still matches, cleanup is a no-op. If the current metadata/file length match the intended artwork mutation, recovery restores the original metadata region. If the target no longer matches either state, recovery refuses and leaves the journal for explicit inspection instead of writing old metadata into an externally changed file. This is deliberately metadata-scoped: it restores FLAC tags and PICTURE blocks without copying or rewriting the audio payload as a full-file backup.

Same-directory marker cleanup is explicit. A `.tonepoet-write-lock`, `.tonepoet-meta-journal`, `.tonepoet-artwork-rollback`, `.tonepoet-flac-rewrite-*` temp, or legacy `.tonepoet-bak` created beside user media must be removed on ordinary completion, recovered or retired by stale recovery after a crash, or surfaced as an actionable warning/error if cleanup cannot be confirmed. Native FLAC success paths now explicitly release the common write lock and carry cleanup/durability warnings in the write report instead of relying only on `Drop`; `Drop` remains a best-effort last resort for unwinding paths. Artwork batch cleanup likewise releases the common lock after rollback-journal commit/rollback cleanup and reports any cleanup warning.

Directory-entry durability is now explicit. After committing the FLAC metadata journal, after removing the journal, and after atomically renaming an overflow rewrite into place, the writer attempts to fsync the parent directory. On Unix, known unsupported cases such as directory fsync returning `EINVAL`, `ENOTSUP`, `EOPNOTSUPP`, `ENOSYS`, or permission denial on filesystems that do not expose directory fsync are treated as an explicit durability downgrade: file contents have been synced, but directory-entry persistence follows the filesystem's own crash semantics. Before an audio mutation starts, a supported parent-sync failure while committing the metadata journal remains a hard error because the recovery journal cannot be claimed durable. After an audio mutation has committed, cleanup and parent-sync failures are not returned as ordinary write failures: in-place journal-removal file deletion failures, in-place journal-removal sync failures, overflow-rewrite rename sync failures, and FLAC artwork rollback-journal parent-sync failures after successful journal removal become committed-with-durability-warning results. FLAC artwork rollback-journal deletion failure is different: because the rollback journal remains armed, the artwork batch is not reported as cleanly committed and may later be rolled back by stale-journal recovery. The metadata editor treats those results as saved for semantic follow-up work, including single-image CUE sidecar writeback, and surfaces the durability/cleanup warning separately. If journal file deletion itself fails after commit, the stale recovery journal remains as an explicit remediation artifact; later startup/read recovery may retry cleanup, but the just-committed audio mutation is not misreported as failed. This avoids the false state where flat FLAC tags and embedded CUESHEET have already changed but the sidecar gate is skipped because a post-commit fsync warning was misclassified as save failure. On non-Unix platforms, parent-directory fsync is treated as unsupported unless a platform-specific implementation is added.

Non-FLAC formats keep the existing conservative Lofty writer and full-file backup rollback path.

## Batch writes and progress

`apply_audio_tag_changes_with_save_blocks` now plans diffs deterministically, then runs independent file writes with bounded parallelism capped at four workers. Results are returned in the original path order so metadata-editor reduction and CUE sidecar gating remain stable. If duplicate target paths are detected, the function serializes the batch to avoid concurrent writes to the same file. Native FLAC overflow stream rewrites have a separate internal throttle and are serialized process-wide; cheap padded metadata-region writes still use the normal worker parallelism.

The TUI save path passes a progress callback that emits `AppMessage::StatusMessage` after each file completes.

Metadata-editor saves and artwork writes now carry a cooperative cancellation flag. Closing the metadata editor while a tag save or artwork write is active requests cancellation and leaves the editor open until the worker reports completion. Cancellation is checked only at safe points: before starting a file, before entering the non-FLAC full-file fallback, before each artwork target, while waiting for another FLAC overflow rewrite to finish, between bounded 1 MiB chunks during FLAC overflow streaming, and immediately before committing the overflow temp-file rename. The writer deliberately does not interrupt the middle of an in-place metadata overwrite or journal-removal sequence; once a recovery-critical fixed-region write has begun, it finishes that small atomic sequence so the journal model remains valid. Cancelled files are reported as skipped in the existing per-file result order. Artwork cancellation rolls back already-written targets before returning, and stale per-file FLAC artwork rollback journals cover process death before the batch commits.

## Inline metadata edits

The browse inline metadata editor no longer copies a backup on the TUI thread. It also no longer records native FLAC edits in the DB metadata journal with a fake `.tonepoet-bak` path. Native FLAC inline edits are represented only by the sidecar `.tonepoet-meta-journal` created by the blocking writer. Non-FLAC inline edits still use the legacy DB/full-file-backup journal until those formats receive native metadata-region writers. Startup recovery runs both explicit models: FLAC sidecar journals first, then legacy DB backup journals.

## Tests added

The bundle adds regression coverage for:

- padded FLAC tag-only updates not creating `.tonepoet-bak` and preserving the audio region byte-for-byte;
- an injected fast-path hook proving no `.tonepoet-bak` exists during the in-place write, not merely after it;
- an injected metadata-write-size hook proving in-place writes are bounded to the metadata region;
- padding-exhaustion rewrites streaming the audio and adding enough padding for the next save to stay in place;
- overflow replacement preserving mode and Unix timestamps;
- Linux user-xattr preservation when writable xattrs are supported, with deterministic skip on unsupported filesystems;
- xattr capture and restore failures aborting overflow rewrite before replacement and without leaving a temp file;
- ACL preservation when `getfacl`/`setfacl` and filesystem ACLs are available, with deterministic skip otherwise;
- ACL capture and restore failures aborting overflow rewrite before replacement and cleaning any same-process temp file;
- hardlinked FLAC overflow rewrites being refused before temp creation so hardlink identity is not silently broken;
- padded in-place FLAC writes on hardlinked files being refused before metadata-journal creation so path-local crash recovery is not claimed for multi-alias inodes;
- native FLAC artwork writes on hardlinked files being refused before artwork rollback-journal creation;
- post-commit parent-directory fsync failures after overflow commit becoming committed-with-durability-warning outcomes rather than ordinary failed writes;
- post-commit metadata-journal removal failures and parent-directory fsync failures after in-place metadata-journal removal becoming committed-with-durability-warning outcomes rather than ordinary failed writes;
- single-image CUE sidecar writeback still running when the audio image save committed with durability warnings;
- symlinked FLAC overflow rewrites being refused before temp creation so the symlink is not replaced by a regular file;
- symlinked padded FLAC tag writes and native artwork writes being refused before path-local metadata/artwork journals are created, preserving recovery locality;
- read guards through symlink paths recovering canonical target-local FLAC metadata and artwork rollback journals before Lofty/probe parsing;
- cooperative FLAC overflow cancellation before commit and between stream chunks leaving the original file intact and cleaning the same-process rewrite temp;
- cooperative non-FLAC fallback cancellation before `.tonepoet-bak` creation;
- injected overflow preservation/commit failure leaving the original file intact and cleaning the same-process rewrite temp;
- pre-commit overflow source revalidation refusing to overwrite a concurrently replaced FLAC path and cleaning the same-process rewrite temp;
- kill-point recovery for journal-created-only, partial metadata-region overwrite, fully synced metadata overwrite before journal removal, interrupted stream rewrite temp cleanup, and rename-committed stream rewrite validity;
- parseable-but-wrong-audio-offset torn metadata recovery, including a real-FLAC/Lofty readback regression when ffmpeg is available;
- stale FLAC metadata journals being recovered before later reads/writes, including the selected-path sorting helper before it attempts its Lofty tag read;
- active-owner FLAC metadata journals not being consumed by read guards or competing writes while the owning writer is still live;
- common per-FLAC write-lock acquisition refusing concurrent metadata/artwork mutations, including tag-vs-artwork races, before either operation can create a separate recovery journal;
- stale common write locks being recovered before a native FLAC write retries;
- active common write locks causing read guards to return a transient write-in-progress error before parsing;
- metadata-journal acquisition under the common write lock refusing to overwrite an active writer's journal and recovering a stale writer's journal before retrying once;
- parseable torn metadata under an active metadata journal not being restored concurrently by a reader;
- PID-reuse-style metadata-journal owner mismatches not suppressing stale recovery merely because `/proc/<pid>` exists;
- native/Lofty semantic readback through real FLAC parser paths, and through `metaflac` when available;
- a FLAC metadata-layout matrix covering existing PICTURE blocks, APPLICATION, SEEKTABLE, CUESHEET, multiple padding blocks, missing Vorbis comments, huge existing comments, reserved/unknown blocks, and byte-identical survival of every non-target block;
- native FLAC artwork replacement preserving unrelated metadata blocks and non-target PICTURE blocks byte-identical;
- an ignored/explicit large-fixture acceptance test that uses ffmpeg to generate a real FLAC of at least 100 MiB, seeds padding, performs a padded fast-path edit, verifies no full backup existed during the write, checks bounded metadata-region writes, and verifies the audio region checksum plus semantic title readback;
- padded FLAC artwork insertion/removal using metadata-region writes without full-file backup;
- same-process artwork batch rollback restoring successful FLAC metadata-region edits when a later target fails;
- durable per-file FLAC artwork rollback journals restoring pre-batch metadata after a simulated process death before batch commit;
- artwork rollback recovery retrying idempotently after a crash during in-place restore leaves an unparsable FLAC metadata region;
- artwork rollback cleanup retrying idempotently after an overflow-style rollback restore has already committed but journal removal did not finish;
- stale FLAC artwork rollback recovery refusing to restore into an externally replaced file whose current metadata no longer matches either the original or intended journal identity;
- FLAC artwork rollback-journal removal parent-sync failure being classified as a committed durability warning rather than an armed rollback failure;
- same-process artwork rollback failures leaving the FLAC artwork rollback journal armed instead of deleting the only durable recovery artifact;
- PID-reuse-style artwork rollback owner mismatches not suppressing stale recovery merely because `/proc/<pid>` exists;
- active same-process FLAC artwork rollback journals not being consumed by incidental protected reads while the owning process identity still matches;
- artwork rollback journal acquisition under the common per-FLAC write lock refusing to overwrite an active writer's rollback journal and recovering a stale rollback journal before retrying once;
- native FLAC overflow stream rewrites being serialized even when callers run in parallel;
- native FLAC tag/artwork writer refusals returning explicit errors without entering the full-file Lofty backup path.


## FLAC native-writer refusal and fallback policy

Native FLAC writes do not silently fall back to Lofty's full-file writer. If the
native metadata-region writer refuses a FLAC tag, picture, recovery, or overflow
operation, the operation returns a clear error and leaves non-FLAC fallback logic
unused. This is intentional: the Lofty fallback creates a `.tonepoet-bak` full
copy and rewrites the entire FLAC, which is exactly the network-filesystem cost
this architecture avoids. Operators who intentionally want a full-file repair or
normalization pass should run an explicit repair/full-rewrite workflow; ordinary
metadata saves must not hide that cost behind apparent success.

Non-FLAC formats still use the conservative Lofty backup-and-save path until they
receive native metadata-region writers.

## v23 hardening: common-claim token binding

The native FLAC common write lock now carries a per-acquisition claim token in addition to process identity. Metadata journals and artwork rollback journals created under that lock copy the same token into their journal records.

This matters for same-process recovery ambiguity. A live artwork rollback journal is allowed to coexist with the writer only when the current thread holds the exact common write claim that created that journal. A later native write in the same process receives a different claim token and must not treat an old rollback journal as belonging to its operation. It fails with a write-in-progress / different-claim error and leaves the rollback journal armed for recovery or explicit cleanup.

The same rule applies to metadata journals during owned recovery: an active metadata journal may be recovered by its owner only when the current common write claim token matches the journal token. Other read guards and competing writes, including later writes in the same process, do not consume or restore an active journal.

The on-disk formats are advanced as follows:

- `.tonepoet-write-lock`: `TPFLACWL2`, owner identity plus claim token plus canonical target path.
- `.tonepoet-meta-journal`: `TPFLACMJ5`, previous metadata-journal fields plus claim token.
- `.tonepoet-artwork-rollback`: `TPFLACAJ4`, previous artwork-rollback fields plus claim token.

Legacy WL1/MJ2-MJ4/AJ1-AJ3 records remain parseable. Legacy active journals without claim tokens are treated conservatively: they can block while the owner appears live, but they cannot authorize same-process same-thread bypass for a new lock instance.
