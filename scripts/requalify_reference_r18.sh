#!/usr/bin/env bash
# Requalify the FINAL R18 source on the configured build host. Not a qualification bypass.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"
qualification="$repo/tonepoet-pipeline/qualification"
prefix='dsd_reference_common_v18'
parts=(report certification evidence source_lock)

fail() { printf 'R18 Reference requalification: %s\n' "$*" >&2; exit 1; }
[[ $# -le 1 ]] || fail 'usage: scripts/requalify_reference_r18.sh [real-SACD.iso]'
command -v cargo >/dev/null 2>&1 || fail 'cargo is missing: use the configured Rust/Nix build host'
command -v python3 >/dev/null 2>&1 || fail 'python3 is required to check generated evidence coherence'
command -v install >/dev/null 2>&1 || fail 'the install utility is required for sidecar installation'
[[ ${TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE:-0} != 1 ]] || fail 'unqualified Reference override must NOT be active'
[[ -z ${TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT:-} && -z ${TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH:-} || -n ${TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT:-} && -n ${TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH:-} ]] || fail 'runtime-closure root and manifest must be set together (staged package) or both absent (Nix)'
[[ -f "$qualification/${prefix}_candidate.json" ]] || fail 'not at the TonePoet repository root'

# Preserve qualified executable/rooting configuration from the host. Enforce the
# real-tool gate and direct all four *new* outputs to an otherwise empty directory.
work="$(mktemp -d "${TMPDIR:-/tmp}/tonepoet-r18-reference.XXXXXXXX")"
mkdir "$work/generated" "$work/old"
install_started=0
install_dir=''
cleanup() {
    result=$?
    trap - EXIT
    if [[ $result -ne 0 && $install_started -eq 1 ]]; then
        printf 'Restoring the original four sidecars after unsuccessful installation.\n' >&2
        for part in "${parts[@]}"; do
            cp -p "$work/old/${prefix}_${part}.json" "$qualification/${prefix}_${part}.json" || true
        done
    fi
    if [[ -n $install_dir && -d $install_dir ]]; then
        rm -rf -- "$install_dir"
    fi
    rm -rf -- "$work"
    exit "$result"
}
trap cleanup EXIT

printf 'Running the existing gated real-tool Reference v18 qualification...\n'
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$work/generated/${prefix}_report.json" \
TONEPOET_DSD_REFERENCE_CERTIFICATION_PATH="$work/generated/${prefix}_certification.json" \
TONEPOET_DSD_REFERENCE_EVIDENCE_PATH="$work/generated/${prefix}_evidence.json" \
TONEPOET_DSD_REFERENCE_SOURCE_LOCK_PATH="$work/generated/${prefix}_source_lock.json" \
cargo test --release --test dsd_reference_qualification \
    complete_p0_reference_qualification_report -- --exact --nocapture
unset TONEPOET_REQUIRE_TOOLS

# A passing Cargo exit alone is not enough: the test can be skipped without
# the explicit gate, and inherited output artifacts could otherwise be reused.
for part in "${parts[@]}"; do
    [[ -s "$work/generated/${prefix}_${part}.json" ]] || fail "missing freshly generated ${part} sidecar; qualification not installed"
done

# Confirm that the four generated files represent ONE qualifying execution,
# with the report bytes certified and source hashes matching this checkout.
python3 - "$repo" "$work/generated" "$prefix" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
generated = pathlib.Path(sys.argv[2])
prefix = sys.argv[3]
raw = {part: (generated / f"{prefix}_{part}.json").read_bytes()
       for part in ('report', 'certification', 'evidence', 'source_lock')}
items = {key: json.loads(value) for key, value in raw.items()}
report, cert, evidence, lock = (items[name] for name in
                                 ('report', 'certification', 'evidence', 'source_lock'))
candidate_digest = hashlib.sha256(
    (root / 'tonepoet-pipeline/qualification/dsd_reference_common_v18_candidate.json').read_bytes()
).hexdigest()
assert report['status'] == cert['status'] == evidence['status'] == 'passed'
assert cert['outcome'] == 'qualified'
assert report['positive_case_count'] > 0 and report['expected_negative_case_count'] > 0
assert report['gates'] and cert['gates'] and evidence['release_gates']
assert cert['qualification_report_sha256'] == hashlib.sha256(raw['report']).hexdigest()
assert report['candidate_manifest_sha256'] == cert['candidate_manifest_sha256'] == evidence['candidate_manifest_sha256'] == candidate_digest
assert report['runtime_closure_fingerprint_sha256'] == cert['runtime_closure_fingerprint_sha256'] == evidence['runtime_closure_fingerprint_sha256'] == lock['runtime_closure_fingerprint_sha256']
assert report['metadata_mutation_closure_fingerprint_sha256'] == cert['metadata_mutation_closure_fingerprint_sha256'] == evidence['metadata_mutation_closure_fingerprint_sha256']
for path, expected in lock['locked_source_sha256'].items():
    if path in ('Cargo.toml', 'Cargo.lock'):
        continue  # Exact project canonicalization is checked by the Rust freshness gate below.
    assert hashlib.sha256((root / path).read_bytes()).hexdigest() == expected, path
print('Generated report, certification, evidence and source lock are coherent.')
PY

# Only after qualification and coherence checks may the installed set change.
install_dir="$(mktemp -d "$qualification/.r18-qualification-install.XXXXXXXX")"
for part in "${parts[@]}"; do
    file="${prefix}_${part}.json"
    cp -p "$qualification/$file" "$work/old/$file"
    install -m 644 "$work/generated/$file" "$install_dir/$file"
done
install_started=1
for part in "${parts[@]}"; do
    file="${prefix}_${part}.json"
    mv -f "$install_dir/$file" "$qualification/$file"
done
rmdir "$install_dir"

# The canonical gate validates all locked source paths, including the
# special Cargo.toml/Cargo.lock version canonicalization, and sacd-rs assets.
cargo test --test reference_qualification_freshness
install_started=0
printf 'Reference qualification installed and freshness gate passed.\n'

# Functional checks follow the R18 bounce and are not part of the sidecar writer.
for test in \
    issue_65_postconversion_destination_inventory_covers_source_routes_and_log_consent \
    issue_65_reference_execution_evidence_does_not_force_unrequested_manifest \
    issue_65_reference_execution_still_validates_sample_identity_without_manifest \
    issue_65_independent_folder_album_publish_has_no_extra_entries_with_log_on_or_off \
    terminal_delivery_evidence_reflects_independent_request_and_result \
    human_log_flag_does_not_request_machine_evidence \
    independently_requested_machine_evidence_survives_prebuilt_request; do
    cargo test --lib "$test"
done
cargo test --test chunk_2_1_2_manifest_publication
cargo test --lib album_gain
cargo test --workspace
cargo build --workspace

if [[ $# -eq 1 ]]; then
    [[ -f "$1" ]] || fail "not a real SACD ISO: $1"
    iso="$(realpath "$1")"
    mkdir -p "$work/smoke-output"
    printf 'Converting real SACD track through qualified Reference with manifest consent OFF...\n'
    cargo run --release -- convert "$iso" --track 1 --area stereo \
        --dsd-path reference --format flac --output "$work/smoke-output"
    tracks="$(find "$work/smoke-output" -type f -iname '*.flac' | wc -l)"
    [[ "$tracks" -eq 1 ]] || fail "expected one published FLAC, got $tracks"
    [[ -z "$(find "$work/smoke-output" -name '.tonepoet-manifest.json' -print -quit)" ]] || fail 'unrequested manifest published'
    printf 'Real-SACD positive smoke passed: one FLAC, no manifest.\n'
else
    printf 'Real-SACD positive smoke NOT RUN: rerun with a real SACD ISO path before release.\n' >&2
fi
printf 'Complete the negative runtime-mismatch smoke in an isolated checkout before release; see R18_REFERENCE_REQUALIFICATION_HANDOFF.md.\n'
