#!/usr/bin/env python3
"""Commission Fast066V2 against the unchanged Fast066V1 baseline.

This driver deliberately lives outside the production API. It verifies and
extracts the exact V1 baseline archive, runs A/B/C/D against one carrier, applies
the C compatibility-ablation stop/go rule, and writes one provenance-rich JSON manifest. Hashing and
compiler/source metadata collection happen outside every benchmark's internal
wall-time interval.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from typing import Any, Iterable

REQUIRED_V1_ARCHIVE_SHA256 = "604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0"
FAST_TARGET_SECONDS_PER_PROGRAMME_MINUTE = 0.66
EDGE_POLICY = "RepeatEndpoints"
BENCH_EXAMPLE = "bench_ceiling_f64le"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source_tree_sha256(root: Path) -> str:
    """Hash source/configuration bytes while excluding build/cache products."""
    ignored_parts = {
        ".git", "target", "__pycache__", ".pytest_cache", ".mypy_cache", "Cargo.lock"
    }
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        relative = path.relative_to(root)
        if any(part in ignored_parts for part in relative.parts):
            continue
        digest.update(relative.as_posix().encode("utf-8"))
        digest.update(b"\0")
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        digest.update(b"\0")
    return digest.hexdigest()


def copy_source_tree(source: Path, destination: Path) -> Path:
    """Copy source into disposable commissioning storage without build caches."""
    ignored = shutil.ignore_patterns(
        ".git", "target", "__pycache__", ".pytest_cache", ".mypy_cache"
    )
    shutil.copytree(source, destination, ignore=ignored)
    return destination


def lockfile_sha256(crate_root: Path) -> str | None:
    lockfile = crate_root / "Cargo.lock"
    return sha256_file(lockfile) if lockfile.is_file() else None


def finalize_source_provenance(
    manifest: dict[str, Any],
    *,
    v1_crate: Path,
    v2_run_crate: Path,
    v2_original_crate: Path,
    v1_source_sha256: str,
    v2_source_sha256: str,
) -> None:
    sources = manifest["sources"]
    sources["build_lockfiles"] = {
        "v1_cargo_lock_sha256": lockfile_sha256(v1_crate),
        "v2_cargo_lock_sha256": lockfile_sha256(v2_run_crate),
    }
    after = {
        "v1_source_tree_sha256_after": source_tree_sha256(v1_crate),
        "v2_run_source_tree_sha256_after": source_tree_sha256(v2_run_crate),
        "v2_original_source_tree_sha256_after": source_tree_sha256(v2_original_crate),
    }
    sources.update(after)
    sources["source_trees_unchanged"] = (
        after["v1_source_tree_sha256_after"] == v1_source_sha256
        and after["v2_run_source_tree_sha256_after"] == v2_source_sha256
        and after["v2_original_source_tree_sha256_after"] == v2_source_sha256
    )
    if not sources["source_trees_unchanged"]:
        raise RuntimeError("commissioning changed source bytes outside excluded build products")


def find_crate_root(root: Path) -> Path:
    direct = root / "crates" / "tonepoet-true-peak" / "Cargo.toml"
    if direct.is_file():
        return direct.parent
    matches = sorted(root.rglob("crates/tonepoet-true-peak/Cargo.toml"))
    if len(matches) != 1:
        raise RuntimeError(
            f"expected exactly one tonepoet-true-peak Cargo.toml below {root}, found {len(matches)}"
        )
    return matches[0].parent


def extract_verified_v1(archive: Path, destination: Path) -> Path:
    actual = sha256_file(archive)
    if actual != REQUIRED_V1_ARCHIVE_SHA256:
        raise RuntimeError(
            "V1 archive SHA-256 mismatch: "
            f"expected {REQUIRED_V1_ARCHIVE_SHA256}, got {actual}"
        )
    if not tarfile.is_tarfile(archive):
        raise RuntimeError(f"required V1 archive is not a tar archive: {archive}")
    with tarfile.open(archive, "r:*") as handle:
        # The exact archive digest is pinned above. Python's data filter also
        # rejects path traversal, device nodes, and unsafe link targets.
        handle.extractall(destination, filter="data")
    return find_crate_root(destination)


def command_output(command: list[str], env: dict[str, str]) -> str:
    process = subprocess.run(
        command,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if process.returncode != 0:
        raise RuntimeError(
            f"command failed ({process.returncode}): {' '.join(command)}\n{process.stderr.strip()}"
        )
    return process.stdout.strip()


def parse_benchmark_json(stdout: str) -> dict[str, Any]:
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and "total_wall_seconds" in value:
            return value
    raise RuntimeError("benchmark did not emit a parseable JSON result")


def seconds_per_programme_minute(total_wall_seconds: float, programme_seconds: float) -> float:
    if programme_seconds <= 0.0:
        raise ValueError("programme duration must be positive")
    return total_wall_seconds * 60.0 / programme_seconds


def c_allows_d(total_wall_seconds: float, programme_seconds: float) -> bool:
    return (
        seconds_per_programme_minute(total_wall_seconds, programme_seconds)
        < FAST_TARGET_SECONDS_PER_PROGRAMME_MINUTE
    )


def normalized_point_dbtp(value: Any) -> float | None:
    if isinstance(value, (int, float)):
        return float(value)
    return None


def active_simd_backend(result: dict[str, Any]) -> str:
    certificate = result.get("certificate", {})
    active = certificate.get("accelerated_same_graph_avx_prefix_active")
    if active is True:
        return "avx"
    if active is False:
        return "scalar"
    raise RuntimeError("benchmark certificate did not identify the active SIMD backend")


def v2_work_counters(result: dict[str, Any]) -> dict[str, Any]:
    """Extract the work diagnostics required by the commissioning record."""
    certificate = result.get("certificate")
    if not isinstance(certificate, dict):
        raise RuntimeError("benchmark result did not contain certificate diagnostics")
    names = (
        "tiles_processed",
        "groups_rejected",
        "groups_expanded",
        "candidate_cells",
        "refined_cells",
        "phase_evaluations",
        "authoritative_coarse_values",
        "authoritative_coarse_groups",
        "accelerated_l1_groups_tested",
        "accelerated_l1_groups_rejected",
        "accelerated_curvature_roots",
        "strict_coarse_evaluations",
        "dense_regions",
        "dense_intermediate_cells",
        "dense_complete_regions",
        "dense_phase_evaluations",
        "direct_rescore_evaluations",
        "work_credits_consumed",
        "work_limited_tiles",
        "time_bounded_prefix_blocks_skipped",
        "time_limited_tiles",
        "fast_survey_knots",
        "fast_flat_groups",
        "fast_input_sample_peak_linear",
        "fast_hq4_peak_linear",
        "max_evaluation_error_linear",
        "unresolved_upper_linear",
    )
    return {name: certificate.get(name) for name in names}


def rustflags_tokens(env: dict[str, str]) -> list[str]:
    """Return operator rustflags, preferring Cargo's encoded form when present."""
    encoded = env.get("CARGO_ENCODED_RUSTFLAGS")
    if encoded:
        return [token for token in encoded.split("\x1f") if token]
    raw = env.get("RUSTFLAGS", "")
    return shlex.split(raw) if raw else []


def loop_vectorizer_disabled(env: dict[str, str]) -> bool:
    tokens = rustflags_tokens(env)
    for index, token in enumerate(tokens):
        if "--vectorize-loops=false" in token:
            return True
        if token == "-C" and index + 1 < len(tokens) and "--vectorize-loops=false" in tokens[index + 1]:
            return True
    return False


def rustc_invocations(verbose_stderr: str) -> list[str]:
    """Keep the actual crate/example rustc command lines emitted by `cargo -vv`."""
    selected: list[str] = []
    for line in verbose_stderr.splitlines():
        stripped = line.strip()
        if "Running `" not in stripped:
            continue
        if (
            "--crate-name tonepoet_true_peak" in stripped
            or f"--crate-name {BENCH_EXAMPLE}" in stripped
        ):
            selected.append(stripped)
    return selected


def prebuild_benchmark(
    *,
    build_id: str,
    crate_root: Path,
    cargo: str,
    env: dict[str, str],
    instrumented: bool,
) -> dict[str, Any]:
    """Build outside the measured wall interval and capture effective rustc invocations."""
    command = [
        cargo,
        "build",
        "--release",
        "--manifest-path",
        str(crate_root / "Cargo.toml"),
        "-vv",
    ]
    if instrumented:
        command += ["--features", "fast-stage-timing"]
    command += ["--example", BENCH_EXAMPLE]
    process = subprocess.run(
        command,
        cwd=crate_root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if process.returncode != 0:
        raise RuntimeError(
            f"prebuild {build_id} failed ({process.returncode})\n"
            f"stdout:\n{process.stdout}\nstderr:\n{process.stderr}"
        )
    invocations = rustc_invocations(process.stderr)
    if not invocations:
        raise RuntimeError(
            f"prebuild {build_id} did not expose the crate/example rustc command under cargo -vv"
        )
    return {
        "build_id": build_id,
        "instrumented": instrumented,
        "cargo_command": command,
        "cargo_target_dir": env.get("CARGO_TARGET_DIR"),
        "rustflags_tokens": rustflags_tokens(env),
        "loop_vectorizer_disabled": loop_vectorizer_disabled(env),
        "rustc_invocations": invocations,
    }


def validate_benchmark_identity(
    benchmark: dict[str, Any],
    *,
    carrier_bytes: int,
    sample_rate_hz: int,
    channels: int,
    configuration: str,
    instrumented: bool,
    expected_algorithm_revision: str,
) -> None:
    frame_bytes = channels * 8
    expected_frames = carrier_bytes // frame_bytes
    expected_configuration = {
        "fast": "production",
        "fast-survey": "survey-bounds-only",
        "fast-nominate": "survey-bounds-only-compat",
    }[configuration]
    expected = {
        "tier": "Fast",
        "fast_algorithm_revision": expected_algorithm_revision,
        "fast_configuration": expected_configuration,
        "bytes_read": carrier_bytes,
        "frames": expected_frames,
        "sample_rate_hz": sample_rate_hz,
        "channels": channels,
        "binding_fast_wall_gate": not instrumented,
    }
    for key, value in expected.items():
        if benchmark.get(key) != value:
            raise RuntimeError(
                f"benchmark identity mismatch for {key}: expected {value!r}, "
                f"got {benchmark.get(key)!r}"
            )
    build = benchmark.get("build")
    if not isinstance(build, dict) or build.get("fast_stage_timing_feature") is not instrumented:
        raise RuntimeError("benchmark build metadata does not match requested instrumentation")


def run_benchmark(
    *,
    label: str,
    crate_root: Path,
    carrier: Path,
    carrier_sha256: str,
    sample_rate_hz: int,
    channels: int,
    configuration: str,
    cargo: str,
    env: dict[str, str],
    instrumented: bool,
    expected_algorithm_revision: str,
    source_role: str,
    source_tree_sha256: str,
    build_id: str,
    allow_binding_gate_failure: bool = False,
) -> dict[str, Any]:
    command = [
        cargo,
        "run",
        "--quiet",
        "--release",
        "--manifest-path",
        str(crate_root / "Cargo.toml"),
    ]
    if instrumented:
        command += ["--features", "fast-stage-timing"]
    command += [
        "--example",
        BENCH_EXAMPLE,
        "--",
        str(carrier),
        str(sample_rate_hz),
        str(channels),
        configuration,
    ]
    process = subprocess.run(
        command,
        cwd=crate_root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    try:
        benchmark = parse_benchmark_json(process.stdout)
    except RuntimeError as error:
        raise RuntimeError(
            f"{label} benchmark failed before producing JSON; return code={process.returncode}\n"
            f"stdout:\n{process.stdout}\nstderr:\n{process.stderr}"
        ) from error

    validate_benchmark_identity(
        benchmark,
        carrier_bytes=carrier.stat().st_size,
        sample_rate_hz=sample_rate_hz,
        channels=channels,
        configuration=configuration,
        instrumented=instrumented,
        expected_algorithm_revision=expected_algorithm_revision,
    )

    if process.returncode != 0:
        expected_gate_failure = (
            allow_binding_gate_failure
            and benchmark.get("binding_fast_wall_gate") is True
            and (
                benchmark.get("fast_wall_target_met") is False
                or benchmark.get("fast_accuracy_target_met") is False
            )
        )
        if not expected_gate_failure:
            raise RuntimeError(
                f"{label} benchmark returned {process.returncode}\n"
                f"stdout:\n{process.stdout}\nstderr:\n{process.stderr}"
            )

    programme_seconds = float(benchmark["programme_seconds"])
    rate = seconds_per_programme_minute(float(benchmark["total_wall_seconds"]), programme_seconds)
    record = {
        "label": label,
        "configuration_label": label.split("-", 1)[0],
        "benchmark_configuration": configuration,
        "instrumented": instrumented,
        "binding": not instrumented,
        "edge_policy": EDGE_POLICY,
        "carrier_sha256": carrier_sha256,
        "source_role": source_role,
        "source_tree_sha256": source_tree_sha256,
        "build_id": build_id,
        "algorithm_revision": benchmark.get("fast_algorithm_revision"),
        "cargo_command": command,
        "process_returncode": process.returncode,
        "stderr": process.stderr.strip(),
        "seconds_per_programme_minute": rate,
        "active_simd_backend": active_simd_backend(benchmark),
        "benchmark": benchmark,
    }
    if expected_algorithm_revision == "Fast066V2":
        record["v2_work_counters"] = v2_work_counters(benchmark)
    return record


def annotate_accuracy(
    record: dict[str, Any],
    expected_point_dbtp: float | None,
) -> None:
    measured = normalized_point_dbtp(record["benchmark"].get("point_dbtp"))
    record["measured_point_dbtp"] = measured
    if measured is None or expected_point_dbtp is None:
        record["signed_point_error_db"] = None
        record["absolute_point_error_db"] = None
    else:
        signed = measured - expected_point_dbtp
        record["signed_point_error_db"] = signed
        record["absolute_point_error_db"] = abs(signed)


def write_manifest(path: Path, manifest: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def build_environment(args: argparse.Namespace, target_dir: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    if args.rustflags is not None:
        env["RUSTFLAGS"] = args.rustflags
        env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    return env


def release_profile_metadata(crate_root: Path) -> dict[str, Any]:
    with (crate_root / "Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    profile = manifest.get("profile", {}).get("release", {})
    if not isinstance(profile, dict):
        raise RuntimeError(f"invalid [profile.release] in {crate_root / 'Cargo.toml'}")
    return profile


def compiler_metadata(
    args: argparse.Namespace,
    env: dict[str, str],
    v1_crate: Path,
    v2_crate: Path,
) -> dict[str, Any]:
    cargo_env = {
        key: value
        for key, value in sorted(env.items())
        if key.startswith("CARGO_PROFILE_RELEASE_")
        or key in {"CARGO_BUILD_RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"}
        or (key.startswith("CARGO_TARGET_") and key.endswith("_RUSTFLAGS"))
    }
    return {
        "rustc_vV": command_output([args.rustc, "-vV"], env),
        "cargo_version": command_output([args.cargo, "--version"], env),
        "cargo_profile": "release",
        "operator_rustflags": env.get("RUSTFLAGS", ""),
        "operator_encoded_rustflags": env.get("CARGO_ENCODED_RUSTFLAGS", ""),
        "rustflags_tokens": rustflags_tokens(env),
        "loop_vectorizer_disabled": loop_vectorizer_disabled(env),
        "cargo_codegen_environment": cargo_env,
        "v1_release_profile": release_profile_metadata(v1_crate),
        "v2_release_profile": release_profile_metadata(v2_crate),
    }


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--v1-archive", type=Path, required=True)
    parser.add_argument("--carrier", type=Path, required=True)
    parser.add_argument("--sample-rate", type=int, required=True)
    parser.add_argument("--channels", type=int, required=True)
    parser.add_argument(
        "--v2-crate",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Fast066V2 crate root (default: crate containing this driver)",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument(
        "--rustflags",
        default=None,
        help="Exact RUSTFLAGS for all builds. If omitted, inherit the current environment.",
    )
    parser.add_argument(
        "--expected-point-dbtp",
        type=float,
        default=None,
        help="Revalidated dense finite-HQ point for signed/absolute error reporting.",
    )
    parser.add_argument(
        "--max-abs-point-error-db",
        type=float,
        default=0.01,
        help="Point-accuracy gate for this carrier when --expected-point-dbtp is supplied (default: 0.01 dB).",
    )
    return parser.parse_args(argv)


def main(argv: Iterable[str] | None = None) -> int:
    args = parse_args(argv)
    v1_archive = args.v1_archive.resolve()
    carrier = args.carrier.resolve()
    v2_crate = args.v2_crate.resolve()
    output = args.output.resolve()

    if not v1_archive.is_file():
        raise RuntimeError(f"V1 archive not found: {v1_archive}")
    if not carrier.is_file():
        raise RuntimeError(f"carrier not found: {carrier}")
    if not (v2_crate / "Cargo.toml").is_file():
        raise RuntimeError(f"V2 crate Cargo.toml not found: {v2_crate / 'Cargo.toml'}")
    if args.sample_rate <= 0:
        raise RuntimeError("sample rate must be positive")
    if args.channels <= 0:
        raise RuntimeError("channel count must be positive")
    if args.max_abs_point_error_db < 0.0:
        raise RuntimeError("maximum absolute point error must be nonnegative")

    frame_bytes = args.channels * 8
    carrier_bytes = carrier.stat().st_size
    if carrier_bytes == 0 or carrier_bytes % frame_bytes != 0:
        raise RuntimeError("carrier must contain a positive whole number of interleaved f64 frames")
    frames = carrier_bytes // frame_bytes
    programme_seconds = frames / args.sample_rate

    # Provenance is collected before any timed executable starts.
    carrier_sha256 = sha256_file(carrier)
    v1_archive_sha256 = sha256_file(v1_archive)
    if v1_archive_sha256 != REQUIRED_V1_ARCHIVE_SHA256:
        raise RuntimeError(
            f"V1 archive SHA-256 mismatch: expected {REQUIRED_V1_ARCHIVE_SHA256}, got {v1_archive_sha256}"
        )
    v2_source_sha256_before = source_tree_sha256(v2_crate)

    with tempfile.TemporaryDirectory(prefix="fast066-v2-commission-") as temporary_directory:
        work = Path(temporary_directory)
        v1_extract = work / "v1"
        v1_extract.mkdir()
        v1_crate = extract_verified_v1(v1_archive, v1_extract)
        v1_source_sha256_before = source_tree_sha256(v1_crate)
        v2_run_crate = copy_source_tree(v2_crate, work / "v2")
        if source_tree_sha256(v2_run_crate) != v2_source_sha256_before:
            raise RuntimeError("disposable V2 source copy does not match commissioned source identity")

        # Keep instrumented and binding artifacts physically separate so a
        # stale feature-enabled executable cannot be mistaken for shipping D.
        env_v1_instrumented = build_environment(args, work / "cargo-target-v1-instrumented")
        env_v2_instrumented = build_environment(args, work / "cargo-target-v2-instrumented")
        env_v2_binding = build_environment(args, work / "cargo-target-v2-binding")
        compiler = compiler_metadata(args, env_v2_binding, v1_crate, v2_run_crate)

        builds = {
            "A-instrumented": prebuild_benchmark(
                build_id="A-instrumented",
                crate_root=v1_crate,
                cargo=args.cargo,
                env=env_v1_instrumented,
                instrumented=True,
            ),
            "V2-instrumented": prebuild_benchmark(
                build_id="V2-instrumented",
                crate_root=v2_run_crate,
                cargo=args.cargo,
                env=env_v2_instrumented,
                instrumented=True,
            ),
        }
        records: list[dict[str, Any]] = []

        manifest: dict[str, Any] = {
            "schema": "tonepoet.fast066.commissioning.v2",
            "target_seconds_per_programme_minute": FAST_TARGET_SECONDS_PER_PROGRAMME_MINUTE,
            "edge_policy": EDGE_POLICY,
            "carrier": {
                "path": str(carrier),
                "sha256": carrier_sha256,
                "sample_format": "interleaved-f64le",
                "bytes": carrier_bytes,
                "frames": frames,
                "sample_rate_hz": args.sample_rate,
                "channels": args.channels,
                "programme_seconds": programme_seconds,
            },
            "sources": {
                "v1_archive_sha256": v1_archive_sha256,
                "v1_required_archive_sha256": REQUIRED_V1_ARCHIVE_SHA256,
                "v1_source_tree_sha256": v1_source_sha256_before,
                "v2_source_tree_sha256": v2_source_sha256_before,
                "source_tree_digest_excludes": [
                    ".git",
                    "target",
                    "__pycache__",
                    ".pytest_cache",
                    ".mypy_cache",
                    "Cargo.lock (recorded separately as a build input)",
                ],
                "build_lockfiles": None,
            },
            "compiler": compiler,
            "builds": builds,
            "accuracy_reference": {
                "expected_point_dbtp": args.expected_point_dbtp,
                "max_abs_point_error_db": args.max_abs_point_error_db,
            },
            "records": records,
            "stop_go": None,
            "release_gate": {
                "speed": None,
                "certificate_width": None,
                "point_accuracy": None,
                "accuracy": None,
                "ready": False,
            },
        }

        # A: exact V1 source, with instrumentation only so A/B/C/D share the
        # same nonbinding measurement posture. The source archive itself is
        # never modified and is re-hashed before extraction above.
        a = run_benchmark(
            label="A",
            crate_root=v1_crate,
            carrier=carrier,
            carrier_sha256=carrier_sha256,
            sample_rate_hz=args.sample_rate,
            channels=args.channels,
            configuration="fast",
            cargo=args.cargo,
            env=env_v1_instrumented,
            instrumented=True,
            expected_algorithm_revision="Fast066V1",
            source_role="v1-baseline",
            source_tree_sha256=v1_source_sha256_before,
            build_id="A-instrumented",
        )
        annotate_accuracy(a, args.expected_point_dbtp)
        records.append(a)

        b = run_benchmark(
            label="B",
            crate_root=v2_run_crate,
            carrier=carrier,
            carrier_sha256=carrier_sha256,
            sample_rate_hz=args.sample_rate,
            channels=args.channels,
            configuration="fast-survey",
            cargo=args.cargo,
            env=env_v2_instrumented,
            instrumented=True,
            expected_algorithm_revision="Fast066V2",
            source_role="v2-candidate",
            source_tree_sha256=v2_source_sha256_before,
            build_id="V2-instrumented",
        )
        annotate_accuracy(b, args.expected_point_dbtp)
        records.append(b)

        # C preserves the frozen NominationOnly commissioning entry point. The
        # nomination stage has been retired, so C is intentionally equivalent
        # to B and is reported under a compatibility label by the benchmark.
        c = run_benchmark(
            label="C",
            crate_root=v2_run_crate,
            carrier=carrier,
            carrier_sha256=carrier_sha256,
            sample_rate_hz=args.sample_rate,
            channels=args.channels,
            configuration="fast-nominate",
            cargo=args.cargo,
            env=env_v2_instrumented,
            instrumented=True,
            expected_algorithm_revision="Fast066V2",
            source_role="v2-candidate",
            source_tree_sha256=v2_source_sha256_before,
            build_id="V2-instrumented",
        )
        annotate_accuracy(c, args.expected_point_dbtp)
        records.append(c)

        go = c_allows_d(float(c["benchmark"]["total_wall_seconds"]), programme_seconds)
        manifest["stop_go"] = {
            "configuration": "C",
            "seconds_per_programme_minute": c["seconds_per_programme_minute"],
            "threshold_seconds_per_programme_minute": FAST_TARGET_SECONDS_PER_PROGRAMME_MINUTE,
            "run_D": go,
            "profile_mandatory_path": not go,
            "reason": (
                "C compatibility ablation is below the 0.66 s/min mandatory-path threshold"
                if go
                else "C compatibility ablation is at or above 0.66 s/min; stop and profile the mandatory path"
            ),
        }

        if not go:
            manifest["release_gate"]["speed"] = None
            manifest["release_gate"]["speed_status"] = "not_run_c_stop"
            finalize_source_provenance(
                manifest,
                v1_crate=v1_crate,
                v2_run_crate=v2_run_crate,
                v2_original_crate=v2_crate,
                v1_source_sha256=v1_source_sha256_before,
                v2_source_sha256=v2_source_sha256_before,
            )
            write_manifest(output, manifest)
            print(output)
            return 2

        builds["D-binding"] = prebuild_benchmark(
            build_id="D-binding",
            crate_root=v2_run_crate,
            cargo=args.cargo,
            env=env_v2_binding,
            instrumented=False,
        )

        d_instrumented = run_benchmark(
            label="D-instrumented",
            crate_root=v2_run_crate,
            carrier=carrier,
            carrier_sha256=carrier_sha256,
            sample_rate_hz=args.sample_rate,
            channels=args.channels,
            configuration="fast",
            cargo=args.cargo,
            env=env_v2_instrumented,
            instrumented=True,
            expected_algorithm_revision="Fast066V2",
            source_role="v2-candidate",
            source_tree_sha256=v2_source_sha256_before,
            build_id="V2-instrumented",
        )
        annotate_accuracy(d_instrumented, args.expected_point_dbtp)
        records.append(d_instrumented)

        d_binding = run_benchmark(
            label="D-binding",
            crate_root=v2_run_crate,
            carrier=carrier,
            carrier_sha256=carrier_sha256,
            sample_rate_hz=args.sample_rate,
            channels=args.channels,
            configuration="fast",
            cargo=args.cargo,
            env=env_v2_binding,
            instrumented=False,
            expected_algorithm_revision="Fast066V2",
            source_role="v2-candidate",
            source_tree_sha256=v2_source_sha256_before,
            build_id="D-binding",
            allow_binding_gate_failure=True,
        )
        annotate_accuracy(d_binding, args.expected_point_dbtp)
        records.append(d_binding)

        speed_pass = d_binding["benchmark"].get("fast_wall_target_met") is True
        certificate_width_pass = d_binding["benchmark"].get("fast_accuracy_target_met") is True
        if args.expected_point_dbtp is None:
            point_accuracy_pass: bool | None = None
            accuracy_pass: bool | None = None
        else:
            error = d_binding["absolute_point_error_db"]
            point_accuracy_pass = error is not None and error <= args.max_abs_point_error_db
            accuracy_pass = certificate_width_pass and point_accuracy_pass
        manifest["release_gate"] = {
            "speed": speed_pass,
            "certificate_width": certificate_width_pass,
            "point_accuracy": point_accuracy_pass,
            "accuracy": accuracy_pass,
            "ready": speed_pass and accuracy_pass is True,
        }

        finalize_source_provenance(
            manifest,
            v1_crate=v1_crate,
            v2_run_crate=v2_run_crate,
            v2_original_crate=v2_crate,
            v1_source_sha256=v1_source_sha256_before,
            v2_source_sha256=v2_source_sha256_before,
        )

        write_manifest(output, manifest)
        print(output)
        return 0 if speed_pass else 2


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"commission_fast066_v2: {error}", file=sys.stderr)
        raise SystemExit(1)
