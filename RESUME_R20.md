# Resume R20 from an interrupted session

The R20 code snapshot in this source tarball already includes the source changes. No edits need to be replayed.

The 2026-10-09 **single-file overwrite with logging disabled** corrective is
also present. Read `R20_UNLOGGED_OVERWRITE_CORRECTIVE_HANDOFF_2026-10-09.md`
before running build-host acceptance or Reference requalification.

1. Read `BRIEF_LOGGING_R20_overwrite_loss_rg_survivors_logs_pills_2026-10-09.md` and `R20_ENGINEERING_HANDOFF_2026-10-09.md`.
2. Review `git log` if you cloned the companion Git bundle. Its initial commit `7ccec9e` is the uploaded R19 source baseline. The later commits implement R20 incrementally. A normal unpack of the source tarball contains no `.git` metadata.
3. Execute build-host qualification and black-box smokes described in the handoff; they were not executable in the artifact-creation container. Correct only evidenced defects.
4. On successful build-host qualification, replace all four generated v18 Reference sidecars together in the final release snapshot. Do not fabricate hashes or bypass the authority gate.
5. If re-packaging, rerun the integrity manifest and retain build logs and positive/negative smoke evidence. The local source-archive SHA-256 establishes transfer integrity, not compilation or runtime acceptance.
