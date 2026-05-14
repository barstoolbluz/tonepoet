#!/usr/bin/env python3
"""
Whole-album byte-exact validation harness for sacd-rs vs sacd_extract.

For each configured SACD track this script:

1. Parses the sacd_extract DSF/DFF reference files for ID3 and DFF footer metadata.
2. Invokes the sacd-rs `extract_track` example with matching CLI metadata arguments.
3. Compares the full output files byte-for-byte against the reference files.
4. Reports per-track and per-format pass/fail counts.

Prerequisites:

    cargo build -p sacd-rs --release --example extract_track
    sacd_extract -2 -s -i "$ISO" -y /tmp/sacd-compare/all-tracks-c-ref-dsf/
    sacd_extract -2 -p -i "$ISO" -o /tmp/sacd-compare/all-tracks-c-ref-dff/

Select a profile with SACD_VERIFY_PROFILE. PR 2's DST gate is:

    SACD_VERIFY_PROFILE=al_jarreau_all_i_got \
    SACD_VERIFY_ISO="/path/to/AL JARREAU - ALL I GOT.iso" \
    SACD_VERIFY_TRACKS_JSON="/path/to/all_i_got_lsn.json" \
    crates/sacd-rs/scripts/verify_all_tracks.py

The JSON override is only needed until the Al Jarreau LSNs from the local
read-only ISO have been pasted into TRACKS_AL_JARREAU_ALL_I_GOT. It accepts
either objects with track_idx/start_lsn/end_lsn/title keys or four-element
arrays in that order.
"""

from __future__ import annotations

import json
import os
import struct
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


@dataclass(frozen=True)
class Track:
    idx: int
    start_lsn: int | None
    end_lsn: int | None
    title: str
    time_filter_start: str | None = None
    time_filter_duration: str | None = None


# Per-track LSN ranges from `dump_sacd_lsn` for Thelonious Monk, Solo Monk.
TRACKS_SOLO_MONK = [
    Track(1, 1260, 53205, "DINAH"),
    Track(2, 54884, 133607, "I SURRENDER, DEAR"),
    Track(3, 134031, 197205, "SWEET AND LOVELY"),
    Track(4, 197563, 236638, "NORTH OF THE SUNSET"),
    Track(5, 237542, 356511, "RUBY, MY DEAR"),
    Track(6, 357135, 411885, "I'M CONFESSIN' (THAT I LOVE YOU)"),
    Track(7, 412407, 481671, "I HADN'T ANYONE TILL YOU"),
    Track(8, 482416, 555301, "EVERYTHING HAPPENS TO ME"),
    Track(9, 556192, 604269, "MONK'S POINT"),
    Track(10, 605486, 646413, "I SHOULD CARE"),
    Track(11, 647150, 744689, "ASK ME NOW"),
    Track(12, 746722, 821166, "THESE FOOLISH THINGS (REMIND ME OF YOU)"),
    Track(13, 822173, 868220, "INTROSPECTION"),
]


# Al Jarreau, All I Got (2002 SACD), stereo DST area. Paste the local
# `dump_sacd_lsn` narrow ranges here when available. The titles/durations are
# pinned so JSON overrides can be checked against the expected 11-track album.
TRACKS_AL_JARREAU_ALL_I_GOT = [
    Track(1, None, None, "Random Act Of Love"),
    Track(2, None, None, "Life Is"),
    Track(3, None, None, "Never Too Late"),
    Track(4, None, None, "Feels Like Heaven"),
    Track(5, None, None, "Lost And Found"),
    Track(6, None, None, "Secrets Of Love"),
    Track(7, None, None, "All I Got"),
    Track(8, None, None, "Until You Love Me"),
    Track(9, None, None, "Oasis"),
    Track(10, None, None, "Jacaranda Bougainvillea"),
    Track(11, None, None, "Route 66"),
]


REPO_ROOT = Path(os.environ.get("TONEPOET_ROOT", "/home/daedalus/dev/tonepoet"))
COMPARE_ROOT = Path(os.environ.get("SACD_COMPARE_ROOT", "/tmp/sacd-compare"))
EX = os.environ.get(
    "SACD_RS_EXTRACT_TRACK",
    str(REPO_ROOT / "target" / "release" / "examples" / "extract_track"),
)
OUR_DIR = Path(os.environ.get("SACD_VERIFY_OUR_DIR", str(COMPARE_ROOT / "our-tracks")))
OUR_DIR.mkdir(parents=True, exist_ok=True)

PROFILES = {
    "solo_monk": {
        "iso": os.environ.get(
            "SACD_VERIFY_ISO",
            "/home/daedalus/library/monk/Thelonious Monk - Solo Monk (1965) [ISO] {SME JSACD SRGS 4520}/THELONIOUS MONK - SOLO MONK.iso",
        ),
        "album_dir": "SOLO MONK",
        "tracks": TRACKS_SOLO_MONK,
        "channels": 2,
    },
    "al_jarreau_all_i_got": {
        "iso": os.environ.get(
            "SACD_VERIFY_ISO",
            "/home/daedalus/library/jarreau/Al Jarreau - All I Got (2002) [ISO]/AL JARREAU - ALL I GOT.iso",
        ),
        "album_dir": "ALL I GOT",
        "tracks": TRACKS_AL_JARREAU_ALL_I_GOT,
        "channels": 2,
    },
}


def read_syncsafe_u32(buf: bytes) -> int:
    return (buf[0] << 21) | (buf[1] << 14) | (buf[2] << 7) | buf[3]


def decode_id3_text(body: bytes) -> str:
    if not body:
        return ""
    encoding = body[0]
    payload = body[1:]
    if encoding == 0:
        return payload.decode("latin-1", errors="replace").rstrip("\x00")
    if encoding == 1:
        return payload.decode("utf-16", errors="replace").rstrip("\x00")
    if encoding == 2:
        return payload.decode("utf-16-be", errors="replace").rstrip("\x00")
    return payload.decode("utf-8", errors="replace").rstrip("\x00")


def parse_dsf_id3_footer(path: str) -> dict[str, str]:
    data = Path(path).read_bytes()
    if len(data) < 28:
        raise ValueError(f"DSF too short: {path}")
    metadata_offset = struct.unpack_from("<Q", data, 20)[0]
    if metadata_offset == 0:
        raise ValueError(f"DSF has no ID3 metadata footer: {path}")

    tag = data[metadata_offset:]
    if tag[:3] != b"ID3":
        raise ValueError(f"metadata footer is not ID3v2: {path}")

    tag_size = read_syncsafe_u32(tag[6:10])
    off = 10
    end = min(len(tag), 10 + tag_size)
    frames: dict[str, str] = {}
    while off + 10 <= end:
        frame_id = tag[off:off + 4]
        if frame_id == b"\x00\x00\x00\x00":
            break
        size = read_syncsafe_u32(tag[off + 4:off + 8])
        body = tag[off + 10:off + 10 + size]
        fid = frame_id.decode("latin-1")
        if fid.startswith("T") and fid != "TXXX":
            frames[fid] = decode_id3_text(body)
        elif fid == "TXXX" and len(body) >= 2:
            # body: <encoding><description>\0<value>\0
            encoding = body[0]
            rest = body[1:]
            sep = rest.find(b"\x00")
            if sep > 0:
                desc = rest[:sep].decode("utf-8" if encoding == 3 else "latin-1", errors="replace")
                val = rest[sep + 1:].rstrip(b"\x00").decode(
                    "utf-8" if encoding == 3 else "latin-1", errors="replace")
                frames[f"TXXX:{desc}"] = val
        off += 10 + size

    return frames


def walk_dff_chunks(buf: bytes, start: int = 0, end: int | None = None) -> Iterable[tuple[bytes, int, bytes]]:
    if end is None:
        end = len(buf)
    off = start
    while off + 12 <= end:
        chunk_id = buf[off:off + 4]
        size = struct.unpack_from(">Q", buf, off + 4)[0]
        body_start = off + 12
        body_end = body_start + size
        if body_end > len(buf):
            break
        yield chunk_id, off, buf[body_start:body_end]
        off = body_end + (size & 1)


def parse_dff_dff_metadata(path: str) -> dict[str, object]:
    data = Path(path).read_bytes()
    if data[:4] != b"FRM8":
        raise ValueError(f"not a DSDIFF/FRM8 file: {path}")

    top = {chunk_id: body for chunk_id, _off, body in walk_dff_chunks(data, start=16)}
    diin_body = top.get(b"DIIN")
    comt_body = top.get(b"COMT")
    if diin_body is None or comt_body is None:
        raise ValueError(f"DFF metadata footer missing DIIN/COMT chunks: {path}")

    mark = None
    for chunk_id, _off, body in walk_dff_chunks(diin_body, start=0, end=len(diin_body)):
        if chunk_id == b"MARK":
            mark = body
            break
    if mark is None:
        raise ValueError(f"DFF DIIN chunk has no MARK: {path}")

    hours = struct.unpack_from(">H", mark, 0)[0]
    minutes = mark[2]
    seconds = mark[3]
    samples = struct.unpack_from(">L", mark, 4)[0]
    duration_frames = samples // (588 * 64)
    duration_minutes_total = hours * 60 + minutes

    num_comments = struct.unpack_from(">H", comt_body, 0)[0]
    comments = []
    off = 2
    for _ in range(num_comments):
        year = struct.unpack_from(">H", comt_body, off)[0]
        month = comt_body[off + 2]
        day = comt_body[off + 3]
        hour = comt_body[off + 4]
        minute = comt_body[off + 5]
        c_type = struct.unpack_from(">H", comt_body, off + 6)[0]
        c_ref = struct.unpack_from(">H", comt_body, off + 8)[0]
        c_count = struct.unpack_from(">L", comt_body, off + 10)[0]
        text_start = off + 14
        text = comt_body[text_start:text_start + c_count].decode("latin-1", errors="replace")
        comments.append((year, month, day, hour, minute, c_type, c_ref, text))
        off = text_start + c_count + (c_count & 1)

    if len(comments) < 2:
        raise ValueError(f"DFF COMT chunk has too few comments: {path}")
    prefix = "Material ripped from SACD: "
    c1_text = comments[0][7]
    if not c1_text.startswith(prefix):
        raise ValueError(f"unexpected DFF album comment: {c1_text!r}")
    c2 = comments[1]

    return {
        "duration_minutes_total": duration_minutes_total,
        "duration_seconds": seconds,
        "duration_frames": duration_frames,
        "disc_or_album_title": c1_text[len(prefix):],
        "creation_year": c2[0],
        "creation_month_0_indexed": c2[1],
        "creation_day": c2[2],
        "creation_hour": c2[3],
        "creation_minute": c2[4],
        "creating_machine": c2[7],
        "disc_date_y": comments[0][0],
        "disc_date_m": comments[0][1],
        "disc_date_d": comments[0][2],
    }


def cmp_files(a: str, b: str) -> bool:
    return subprocess.run(["cmp", a, b], capture_output=True).returncode == 0


def load_json_tracks(path: Path) -> list[Track]:
    raw = json.loads(path.read_text())
    tracks = []
    for item in raw:
        if isinstance(item, dict):
            tracks.append(Track(
                int(item["track_idx"]),
                int(item["start_lsn"]),
                int(item["end_lsn"]),
                str(item["title"]),
                item.get("time_filter_start"),
                item.get("time_filter_duration"),
            ))
        else:
            idx, start_lsn, end_lsn, title, *rest = item
            tfs = rest[0] if len(rest) >= 1 else None
            tfd = rest[1] if len(rest) >= 2 else None
            tracks.append(Track(int(idx), int(start_lsn), int(end_lsn), str(title), tfs, tfd))
    return tracks


def resolved_tracks(profile_name: str, profile: dict[str, object]) -> list[Track]:
    override = os.environ.get("SACD_VERIFY_TRACKS_JSON")
    if override:
        return load_json_tracks(Path(override))

    tracks = list(profile["tracks"])
    missing = [t.idx for t in tracks if t.start_lsn is None or t.end_lsn is None or t.end_lsn <= t.start_lsn]
    if missing:
        raise SystemExit(
            f"profile {profile_name!r} needs LSN ranges for tracks {missing}. "
            "Paste dump_sacd_lsn values into the TRACKS table or set SACD_VERIFY_TRACKS_JSON."
        )
    return tracks


def first_matching_file(directory: Path, track_idx: int, suffix: str) -> Path:
    matches = sorted(directory.glob(f"{track_idx:02d} *.{suffix}"))
    if not matches:
        raise FileNotFoundError(f"no reference {suffix.upper()} for track {track_idx:02d} in {directory}")
    return matches[0]


def split_pair(value: str, default_total: int) -> tuple[str, str]:
    if "/" in value:
        left, right = value.split("/", 1)
        return left, right
    return value, str(default_total)


def main() -> int:
    profile_name = os.environ.get("SACD_VERIFY_PROFILE", "al_jarreau_all_i_got")
    if profile_name not in PROFILES:
        raise SystemExit(f"unknown SACD_VERIFY_PROFILE={profile_name!r}; choose one of {sorted(PROFILES)}")

    profile = PROFILES[profile_name]
    iso = str(profile["iso"])
    album_dir = str(profile["album_dir"])
    channels = int(profile["channels"])
    tracks = resolved_tracks(profile_name, profile)
    track_total = len(tracks)

    c_ref_dsf_dir = Path(os.environ.get("SACD_VERIFY_C_REF_DSF_DIR", str(COMPARE_ROOT / "all-tracks-c-ref-dsf" / album_dir)))
    c_ref_dff_dir = Path(os.environ.get("SACD_VERIFY_C_REF_DFF_DIR", str(COMPARE_ROOT / "all-tracks-c-ref-dff" / album_dir)))

    print(f"profile: {profile_name}")
    print(f"iso: {iso}")
    print(f"reference DSF: {c_ref_dsf_dir}")
    print(f"reference DFF: {c_ref_dff_dir}")
    print()

    first_dff = first_matching_file(c_ref_dff_dir, tracks[0].idx, "dff")
    print(f"Extracting shared DFF metadata from C-ref track {tracks[0].idx}...")
    t1_dff_meta = parse_dff_dff_metadata(str(first_dff))
    shared_creating_machine = str(t1_dff_meta["creating_machine"])
    shared_creation = (
        t1_dff_meta["creation_year"],
        t1_dff_meta["creation_month_0_indexed"],
        t1_dff_meta["creation_day"],
        t1_dff_meta["creation_hour"],
        t1_dff_meta["creation_minute"],
    )
    shared_disc_date = (
        t1_dff_meta["disc_date_y"],
        t1_dff_meta["disc_date_m"],
        t1_dff_meta["disc_date_d"],
    )
    print(f"  shared creation: {shared_creation}")
    print(f"  shared disc_date: {shared_disc_date}")
    print(f"  shared creating_machine: {shared_creating_machine!r}")
    print()

    dsf_pass = 0
    dff_pass = 0
    dsf_fail: list[int] = []
    dff_fail: list[int] = []

    for track in tracks:
        c_dsf = first_matching_file(c_ref_dsf_dir, track.idx, "dsf")
        c_dff = first_matching_file(c_ref_dff_dir, track.idx, "dff")
        print(f"=== Track {track.idx}: {track.title} ===")

        id3 = parse_dsf_id3_footer(str(c_dsf))
        dff = parse_dff_dff_metadata(str(c_dff))
        tpos_n, tpos_m = split_pair(id3.get("TPOS", f"1/1"), 1)
        trck_n, trck_m = split_pair(id3.get("TRCK", f"{track.idx}/{track_total}"), track_total)

        common_id3_args = [
            "--id3-title", id3.get("TIT2", track.title),
            "--id3-album", id3.get("TALB", album_dir),
            "--id3-artist", id3.get("TPE1", ""),
            "--id3-isrc", id3.get("TSRC", ""),
            "--id3-disc", f"{tpos_n}/{tpos_m}",
            "--id3-genre", id3.get("TCON", ""),
            "--id3-year", id3.get("TYER", id3.get("TDRC", "")[:4]),
            "--id3-date", id3.get("TDAT", ""),
            "--id3-track", f"{trck_n}/{trck_m}",
        ]
        # Pass optional ID3 frames only when present in the C-ref (avoid empty-string mismatch).
        for cli_flag, frame_key in [
            ("--id3-album-artist", "TPE2"),
            ("--id3-performer", "TXXX:PERFORMER"),
            ("--id3-publisher", "TPUB"),
            ("--id3-copyright", "TCOP"),
        ]:
            val = id3.get(frame_key)
            if val:
                common_id3_args += [cli_flag, val]

        # Pass time-filter args when the track has them (Al Jarreau-style DST tracks).
        time_filter_args: list[str] = []
        if track.time_filter_start and track.time_filter_duration:
            time_filter_args = [
                "--time-filter-start", track.time_filter_start,
                "--time-filter-duration", track.time_filter_duration,
            ]

        our_dsf = OUR_DIR / f"{profile_name}-{track.idx:02d}.dsf"
        args = [
            EX, "--iso", iso,
            "--start-lsn", str(track.start_lsn), "--end-lsn", str(track.end_lsn),
            "--channels", str(channels), "--format", "dsf",
            *time_filter_args,
            *common_id3_args,
            "-o", str(our_dsf),
        ]
        subprocess.run(args, check=True, capture_output=True)
        if cmp_files(str(c_dsf), str(our_dsf)):
            print(f"  DSF: byte-exact ({c_dsf.stat().st_size:,} bytes)")
            dsf_pass += 1
        else:
            print("  DSF: DIVERGED")
            dsf_fail.append(track.idx)

        our_dff = OUR_DIR / f"{profile_name}-{track.idx:02d}.dff"
        args = [
            EX, "--iso", iso,
            "--start-lsn", str(track.start_lsn), "--end-lsn", str(track.end_lsn),
            "--channels", str(channels), "--format", "dff",
            *time_filter_args,
            *common_id3_args,
            "--dff-diar", id3.get("TPE1", ""),
            "--dff-diti", id3.get("TIT2", track.title),
            "--dff-duration-minutes", str(dff["duration_minutes_total"]),
            "--dff-duration-seconds", str(dff["duration_seconds"]),
            "--dff-duration-frames", str(dff["duration_frames"]),
            "--dff-disc-date", f"{shared_disc_date[0]}-{shared_disc_date[1]}-{shared_disc_date[2]}",
            "--dff-title", str(dff["disc_or_album_title"]),
            # Use per-track creation_time: sacd_extract crosses minute boundaries mid-album.
            "--dff-creation-time", f"{dff['creation_year']}-{dff['creation_month_0_indexed']}-{dff['creation_day']}-{dff['creation_hour']}:{dff['creation_minute']}",
            "--dff-creating-machine", shared_creating_machine,
            "-o", str(our_dff),
        ]
        subprocess.run(args, check=True, capture_output=True)
        if cmp_files(str(c_dff), str(our_dff)):
            print(f"  DFF: byte-exact ({c_dff.stat().st_size:,} bytes)")
            dff_pass += 1
        else:
            print("  DFF: DIVERGED")
            dff_fail.append(track.idx)

    print()
    print("=== SUMMARY ===")
    print(f"DSF: {dsf_pass}/{track_total} byte-exact" + (f"  FAILED: {dsf_fail}" if dsf_fail else ""))
    print(f"DFF: {dff_pass}/{track_total} byte-exact" + (f"  FAILED: {dff_fail}" if dff_fail else ""))
    return 0 if not dsf_fail and not dff_fail else 1


if __name__ == "__main__":
    sys.exit(main())
