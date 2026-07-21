#!/usr/bin/env python3
"""Verify the preserved append-only policy-v9 W64 admission artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

EXPECTED = {
    "dsd_reference_sox_ng_14_8_0_1_v9.json": "9b6b924d4164aaf9907edc91edbbd5ccee7479d7bbc9d85857e4756911de9ea6",
    "dsd_reference_sox_ng_14_8_0_1_v9_candidate.json": "9b6b924d4164aaf9907edc91edbbd5ccee7479d7bbc9d85857e4756911de9ea6",
    "dsd_reference_sox_ng_14_8_0_1_v9_certification.json": "e792ce06704d988f50c40adbea8462b71d86bff49e9dd42774b032c2b4f15ad3",
    "dsd_reference_sox_ng_14_8_0_1_v9_report.md": "860d72b571e063797a245ec5e95c5da55391481dd94efafaebbf155e70a36fbc",
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(root: Path) -> None:
    q = root / "tonepoet-pipeline/qualification"
    for name, expected in EXPECTED.items():
        actual = sha256(q / name)
        if actual != expected:
            raise AssertionError(f"preserved v9 artifact changed: {name}: {actual}")
    current = (q / "dsd_reference_sox_ng_14_8_0_1_v9.json").read_bytes()
    candidate = (q / "dsd_reference_sox_ng_14_8_0_1_v9_candidate.json").read_bytes()
    if current != candidate:
        raise AssertionError("preserved v9 current/candidate bytes differ")
    v9 = json.loads(current)
    if v9.get("schema_version") != 9 or v9.get("policy") != "sox_ng_14_8_0_1_v9":
        raise AssertionError("preserved v9 identity is noncanonical")
    if v9.get("status") != "qualification_candidate":
        raise AssertionError("preserved v9 snapshot is not the candidate authority")
    mutation = v9["sample_identity"]["metadata_mutation"]
    if mutation != {
        "wav_w64": "error:DSD-REF-P0-024",
        "qualified_post_metadata_targets": [
            "flac_native", "wav_riff", "wav_rf64", "aiff_native", "wavpack_native", "alac_m4a"
        ],
        "w64_non_8_aligned_int24_mono_probe": "known_muxer_defect_phantom_sample",
        "riff_odd_byte_int24_mono_probe": "qualified_sample_exact",
    }:
        raise AssertionError("preserved v9 metadata-admission authority changed")

    planner = (root / "tonepoet-pipeline/src/dsd_reference.rs").read_text()
    stages = (root / "src/convert/pipeline/stages.rs").read_text()
    finding = (root / "docs/findings_dsd_reference_p0_admission_round.md").read_text()
    for marker in (
        'pub const DSD_REFERENCE_POLICY_V9_KEY: &str = "sox_ng_14_8_0_1_v9";',
        "SoxNg14801V9",
        "W64MetadataMutationUnqualified",
        "DSD-REF-P0-024",
    ):
        if marker not in planner:
            raise AssertionError(f"compiled append-only policy omitted historical v9 marker {marker!r}")
    for marker in ('"w64" =>', "MetadataError::PolicyRejected", "W64MetadataMutationUnqualified"):
        if marker not in stages:
            raise AssertionError(f"production metadata stage omitted v9 admission marker {marker!r}")
    if "### F5 resolution (policy v9 candidate" not in finding:
        raise AssertionError("findings no longer preserve the v9 F5 resolution")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    verify(args.repository_root.resolve())
    print("DSD Reference policy v9 preserved metadata-admission verification passed")


if __name__ == "__main__":
    main()
