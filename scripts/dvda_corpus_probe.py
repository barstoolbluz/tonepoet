#!/usr/bin/env python3
"""DVD-Audio IFO corpus diagnostic tool.

Parses AUDIO_TS.IFO (AMG), ATS_XX_0.IFO (ATSI), and AUDIO_PP.IFO (SAMG)
from extracted fixture directories and prints structural information.

Binary layout derived from foo_input_dvda's ifo.h and dvda_zone.cpp.

Usage:
    python3 dvda_corpus_probe.py <fixture_dir> [--json]
    python3 dvda_corpus_probe.py tests/fixtures/dvda/hdad2009/
    python3 dvda_corpus_probe.py tests/fixtures/dvda/  # all subdirs
"""

import json
import struct
import sys
from pathlib import Path


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

SAMG_TRACK_SIZE = 52  # sizeof(samg_track_t)
ATS_TRACK_TIMESTAMP_SIZE = 20
ATS_TRACK_SECTOR_SIZE = 12
ATS_TITLE_IDX_SIZE = 8
ATS_TITLE_SIZE = 16
AUDIO_PGCIT_SIZE = 8
DOWNMIX_MATRICES = 14

SAMPLERATE_48K_TABLE = {0: 48000, 1: 96000, 2: 192000}
SAMPLERATE_44K_TABLE = {0: 44100, 1: 88200, 2: 176400}
BITDEPTH_TABLE = {0: 16, 1: 20, 2: 24}

# Channel assignments from foo_input_dvda's audio_stream_info.cpp
CHANNEL_ASSIGNMENTS = {
    0: "C",
    1: "L R",
    2: "L R S",
    3: "L R Ls Rs",
    4: "L R LFE",
    5: "L R LFE S",
    6: "L R LFE Ls Rs",
    7: "L R C",
    8: "L R C S",
    9: "L R C Ls Rs",
    10: "L R C LFE",
    11: "L R C LFE S",
    12: "L R C LFE Ls Rs",
    13: "L R C S + L R",
    14: "L R C Ls Rs + L R",
    15: "L R C LFE S + L R",
    16: "L R C LFE Ls Rs + L R",
    17: "L R C + L R",
    18: "L R C S + L R C",
    19: "L R C Ls Rs + L R C",
    20: "L R C LFE Ls Rs + L R C",
}


# ---------------------------------------------------------------------------
# AMG parser (AUDIO_TS.IFO)
# ---------------------------------------------------------------------------

def parse_amg(data: bytes) -> dict:
    """Parse AUDIO_TS.IFO (Audio Manager)."""
    magic = data[0:12].decode("ascii", errors="replace")
    if magic != "DVDAUDIO-AMG":
        return {"error": f"bad magic: {magic!r}"}

    amg_last_sector = struct.unpack(">I", data[0x0C:0x10])[0]
    amgi_last_sector = struct.unpack(">I", data[0x1C:0x20])[0]
    spec_version = data[0x21]
    amg_category = struct.unpack(">I", data[0x22:0x26])[0]
    nr_of_volumes = struct.unpack(">H", data[0x26:0x28])[0]
    this_volume_nr = struct.unpack(">H", data[0x28:0x2A])[0]
    disc_side = data[0x2A]
    nr_of_video_title_sets = data[0x3E]
    nr_of_audio_title_sets = data[0x3F]
    provider_id = data[0x40:0x60].decode("ascii", errors="replace").rstrip("\x00 ")

    return {
        "magic": magic,
        "amg_last_sector": amg_last_sector,
        "amgi_last_sector": amgi_last_sector,
        "spec_version": f"0x{spec_version:02x}",
        "amg_category": amg_category,
        "nr_of_volumes": nr_of_volumes,
        "this_volume_nr": this_volume_nr,
        "disc_side": disc_side,
        "nr_of_video_title_sets": nr_of_video_title_sets,
        "nr_of_audio_title_sets": nr_of_audio_title_sets,
        "provider_identifier": provider_id,
    }


# ---------------------------------------------------------------------------
# ATSI parser (ATS_XX_0.IFO)
# ---------------------------------------------------------------------------

def decode_sample_rate(coded: int) -> int:
    """Decode DVD-Audio sample rate from 4-bit field."""
    base_idx = coded & 0x07
    if base_idx > 2:
        return 0
    if coded & 0x08:
        return SAMPLERATE_44K_TABLE.get(base_idx, 0)
    else:
        return SAMPLERATE_48K_TABLE.get(base_idx, 0)


def decode_bit_depth(coded: int) -> int:
    """Decode DVD-Audio bit depth from 4-bit field."""
    return BITDEPTH_TABLE.get(coded, 0)


def parse_audio_format(data_16: bytes) -> dict:
    """Parse one ats_audio_format[i] entry (16 bytes)."""
    audio_type = struct.unpack(">H", data_16[0:2])[0]
    # channel_fmt_t is 3 bytes at offset 2
    # Bitfield layout (C low-to-high): gr2 in low nibble, gr1 in high nibble
    ch_fmt = data_16[2:5]
    gr2_bits_code = (ch_fmt[0] >> 0) & 0x0F
    gr1_bits_code = (ch_fmt[0] >> 4) & 0x0F
    gr2_freq_code = (ch_fmt[1] >> 0) & 0x0F
    gr1_freq_code = (ch_fmt[1] >> 4) & 0x0F
    ch_assignment = ch_fmt[2]

    return {
        "audio_type": audio_type,
        "audio_type_hex": f"0x{audio_type:04x}",
        "group1_sample_rate": decode_sample_rate(gr1_freq_code),
        "group1_bit_depth": decode_bit_depth(gr1_bits_code),
        "group2_sample_rate": decode_sample_rate(gr2_freq_code),
        "group2_bit_depth": decode_bit_depth(gr2_bits_code),
        "channel_assignment": ch_assignment,
        "channel_layout": CHANNEL_ASSIGNMENTS.get(ch_assignment, f"unknown({ch_assignment})"),
    }


def parse_atsi(data: bytes) -> dict:
    """Parse ATS_XX_0.IFO (Audio Title Set Information)."""
    magic = data[0:12].decode("ascii", errors="replace")
    if magic != "DVDAUDIO-ATS":
        return {"error": f"bad magic: {magic!r}"}

    ats_last_sector = struct.unpack(">I", data[0x0C:0x10])[0]
    atsi_last_sector = struct.unpack(">I", data[0x1C:0x20])[0]
    spec_version = data[0x21]
    atstt_vobs = struct.unpack(">I", data[0xC4:0xC8])[0]

    # Audio format entries: 8 entries of 16 bytes each starting at a
    # known offset in atsi_mat_t. From ifo.h, ats_audio_format[8] follows
    # the sector pointer fields. The offset depends on the structure layout.
    # In foo_input_dvda, ats_audio_format starts after zero_13[24] at the end
    # of the fixed header. Let's compute:
    # The atsi_mat_t fixed fields end at offset 0xC0 (from structure size
    # analysis), then audio_format[8] begins. Each audio_format_t is 16 bytes.
    # Actually from dvda_zone.cpp, the audio formats are read directly from
    # the atsi_mat_t struct which is read as a whole. Let me use the offset
    # from the struct layout.
    #
    # atsi_mat_t layout up to audio formats:
    #   0x00: ats_identifier[12]
    #   0x0C: ats_last_sector (4)
    #   0x10: zero_1[12]
    #   0x1C: atsi_last_sector (4)
    #   0x20: zero_2 (1)
    #   0x21: specification_version (1)
    #   0x22: ats_category (4)
    #   0x26: zero_3 (2)
    #   0x28: zero_4 (2)
    #   0x2A: zero_5 (1)
    #   0x2B: zero_6[19]
    #   0x3E: zero_7 (2)
    #   0x40: zero_8[32]
    #   0x60: zero_9 (8)
    #   0x68: zero_10[24]
    #   0x80: atsi_last_byte (4)
    #   0x84: zero_11 (4)
    #   0x88: zero_12[56]
    #   0xC0: atsm_vobs (4)
    #   0xC4: atstt_vobs (4)
    #   0xC8: ats_ptt_srpt (4)
    #   0xCC: ats_pgcit (4)
    #   0xD0: atsm_pgci_ut (4)
    #   0xD4: ats_tmapt (4)
    #   0xD8: atsm_c_adt (4)
    #   0xDC: atsm_vobu_admap (4)
    #   0xE0: ats_c_adt (4)
    #   0xE4: ats_vobu_admap (4)
    #   0xE8: zero_13[24]
    #   0x100: ats_audio_format[8]  (8 * 16 = 128 bytes)
    #   0x180: ats_downmix_matrices[14] (14 * 18 = 252 bytes)
    audio_format_offset = 0x100
    audio_formats = []
    for i in range(8):
        off = audio_format_offset + i * 16
        fmt = parse_audio_format(data[off:off + 16])
        if fmt["audio_type"] != 0:
            fmt["index"] = i
            audio_formats.append(fmt)

    # Detect audio vs video titleset
    atsm_vobs = struct.unpack(">I", data[0xC0:0xC4])[0]
    is_audio_ts = atsm_vobs == 0

    # Parse audio_pgcit at offset 0x800
    titles = []
    if len(data) >= 0x808:
        pgcit_data = data[0x800:]
        nr_of_titles = struct.unpack(">H", pgcit_data[0:2])[0]
        pgcit_last_byte = struct.unpack(">I", pgcit_data[4:8])[0]
        pgcit_end = min(len(pgcit_data), pgcit_last_byte + 1)

        # Title index entries start at AUDIO_PGCIT_SIZE (8)
        for i in range(nr_of_titles):
            idx_off = AUDIO_PGCIT_SIZE + i * ATS_TITLE_IDX_SIZE
            if idx_off + ATS_TITLE_IDX_SIZE > pgcit_end:
                break
            title_nr = pgcit_data[idx_off]
            title_table_offset = struct.unpack(">I", pgcit_data[idx_off + 4:idx_off + 8])[0]

            title_off = title_table_offset
            if title_off + ATS_TITLE_SIZE > pgcit_end:
                break
            track_count = pgcit_data[title_off + 2]
            index_count = pgcit_data[title_off + 3]
            title_len_pts = struct.unpack(">I", pgcit_data[title_off + 4:title_off + 8])[0]
            track_sector_table_offset = struct.unpack(">H", pgcit_data[title_off + 12:title_off + 14])[0]

            # Parse track timestamps
            tracks = []
            ts_off = title_off + ATS_TITLE_SIZE
            for j in range(track_count):
                t_off = ts_off + j * ATS_TRACK_TIMESTAMP_SIZE
                if t_off + ATS_TRACK_TIMESTAMP_SIZE > pgcit_end:
                    break
                track_type = pgcit_data[t_off]
                downmix_matrix = pgcit_data[t_off + 1]
                n = pgcit_data[t_off + 4]
                first_pts = struct.unpack(">I", pgcit_data[t_off + 6:t_off + 10])[0]
                len_in_pts = struct.unpack(">I", pgcit_data[t_off + 10:t_off + 14])[0]
                tracks.append({
                    "track_number": j + 1,
                    "track_type": track_type,
                    "downmix_matrix": downmix_matrix if downmix_matrix < DOWNMIX_MATRICES else -1,
                    "index_start": n,
                    "first_pts": first_pts,
                    "len_in_pts": len_in_pts,
                    "duration_seconds": round(len_in_pts / 90000.0, 3),
                })

            # Parse sector pointers (indexes)
            sectors = []
            sec_off = title_off + track_sector_table_offset
            for j in range(index_count):
                s_off = sec_off + j * ATS_TRACK_SECTOR_SIZE
                if s_off + ATS_TRACK_SECTOR_SIZE > pgcit_end:
                    break
                first_sector = struct.unpack(">I", pgcit_data[s_off + 4:s_off + 8])[0]
                last_sector = struct.unpack(">I", pgcit_data[s_off + 8:s_off + 12])[0]
                sectors.append({
                    "index": j + 1,
                    "first_sector": first_sector,
                    "last_sector": last_sector,
                })

            # Assign sector pointers to tracks (matching dvda_zone.cpp logic)
            for j, track in enumerate(tracks):
                track_idx = track["index_start"]
                next_idx = tracks[j + 1]["index_start"] if j + 1 < len(tracks) else 0
                track_sectors = []
                for sec in sectors:
                    sec_idx = sec["index"]
                    if sec_idx >= track_idx and (sec_idx < next_idx or next_idx == 0):
                        track_sectors.append(sec)
                track["sector_pointers"] = track_sectors
                if track_sectors:
                    track["first_sector"] = track_sectors[0]["first_sector"]
                    track["last_sector"] = track_sectors[-1]["last_sector"]

            titles.append({
                "title_number": title_nr,
                "track_count": track_count,
                "index_count": index_count,
                "len_in_pts": title_len_pts,
                "duration_seconds": round(title_len_pts / 90000.0, 3),
                "tracks": tracks,
                "sector_pointers": sectors,
            })

    return {
        "magic": magic,
        "ats_last_sector": ats_last_sector,
        "atsi_last_sector": atsi_last_sector,
        "spec_version": f"0x{spec_version:02x}",
        "is_audio_titleset": is_audio_ts,
        "atstt_vobs_sector": atstt_vobs,
        "audio_formats": audio_formats,
        "titles": titles,
    }


# ---------------------------------------------------------------------------
# SAMG parser (AUDIO_PP.IFO)
# ---------------------------------------------------------------------------

def parse_samg(data: bytes) -> dict:
    """Parse AUDIO_PP.IFO (Simple Audio Manager)."""
    magic = data[0:12].decode("ascii", errors="replace")
    if magic != "DVDAUDIOSAPP":
        return {"error": f"bad magic: {magic!r}"}

    nr_of_tracks = struct.unpack(">H", data[0x0C:0x0E])[0]
    spec_version = data[0x0F]

    tracks = []
    for i in range(nr_of_tracks):
        off = 0x10 + i * SAMG_TRACK_SIZE
        if off + SAMG_TRACK_SIZE > len(data):
            break

        group_nr = data[off + 2]
        track_nr = data[off + 3]
        first_pts = struct.unpack(">I", data[off + 4:off + 8])[0]
        len_in_pts = struct.unpack(">I", data[off + 8:off + 12])[0]
        zone_byte = data[off + 16]
        zone = (zone_byte >> 5) & 1  # bit 5 = zone (0=AOB, 1=VOB)

        # channel_fmt_t at off+17 (3 bytes)
        # Bitfield layout: gr2 in low nibble, gr1 in high nibble
        ch_fmt = data[off + 17:off + 20]
        gr2_bits_code = ch_fmt[0] & 0x0F
        gr1_bits_code = (ch_fmt[0] >> 4) & 0x0F
        gr2_freq_code = ch_fmt[1] & 0x0F
        gr1_freq_code = (ch_fmt[1] >> 4) & 0x0F
        ch_assignment = ch_fmt[2]

        abs_first_sect = struct.unpack(">I", data[off + 40:off + 44])[0]
        abs_first_sect_dup = struct.unpack(">I", data[off + 44:off + 48])[0]
        abs_last_sect = struct.unpack(">I", data[off + 48:off + 52])[0]

        track_info = {
            "entry": i + 1,
            "group": group_nr,
            "track": track_nr,
            "first_pts": first_pts,
            "len_in_pts": len_in_pts,
            "duration_seconds": round(len_in_pts / 90000.0, 3),
            "zone": "AOB" if zone == 0 else "VOB",
            "group1_sample_rate": decode_sample_rate(gr1_freq_code),
            "group1_bit_depth": decode_bit_depth(gr1_bits_code),
            "group2_sample_rate": decode_sample_rate(gr2_freq_code),
            "group2_bit_depth": decode_bit_depth(gr2_bits_code),
            "channel_assignment": ch_assignment,
            "channel_layout": CHANNEL_ASSIGNMENTS.get(ch_assignment, f"unknown({ch_assignment})"),
            "abs_first_sector": abs_first_sect,
            "abs_last_sector": abs_last_sect,
        }
        # Skip empty/zero tracks
        if len_in_pts > 0 or abs_first_sect > 0:
            tracks.append(track_info)

    return {
        "magic": magic,
        "nr_of_tracks": nr_of_tracks,
        "spec_version": f"0x{spec_version:02x}",
        "tracks": tracks,
    }


# ---------------------------------------------------------------------------
# Disc-level probe
# ---------------------------------------------------------------------------

def probe_disc(fixture_dir: Path) -> dict:
    """Probe all IFO files in a fixture directory."""
    result = {"directory": str(fixture_dir)}

    # AMG
    amg_path = fixture_dir / "AUDIO_TS.IFO"
    if amg_path.exists():
        result["amg"] = parse_amg(amg_path.read_bytes())

    # SAMG
    samg_path = fixture_dir / "AUDIO_PP.IFO"
    if samg_path.exists():
        result["samg"] = parse_samg(samg_path.read_bytes())

    # ATSI (one per title set)
    atsi_entries = {}
    for f in sorted(fixture_dir.glob("ATS_*_0.IFO")):
        ts_num = f.name[4:6]  # "01", "02", etc.
        atsi_entries[f"ATS_{ts_num}"] = parse_atsi(f.read_bytes())
    if atsi_entries:
        result["atsi"] = atsi_entries

    # CPPM
    mkb_path = fixture_dir / "DVDAUDIO.MKB"
    result["cppm"] = {
        "mkb_present": mkb_path.exists(),
        "mkb_size": mkb_path.stat().st_size if mkb_path.exists() else 0,
    }

    # File inventory
    files = {}
    for f in sorted(fixture_dir.iterdir()):
        if f.is_file():
            files[f.name] = f.stat().st_size
    result["files"] = files

    return result


def print_human_readable(disc: dict):
    """Print a human-readable summary of a disc probe."""
    print(f"\n{'='*72}")
    print(f"  {disc['directory']}")
    print(f"{'='*72}")

    if "amg" in disc:
        amg = disc["amg"]
        if "error" in amg:
            print(f"  AMG: ERROR - {amg['error']}")
        else:
            print(f"  AMG: {amg['magic']}")
            print(f"    Audio title sets: {amg['nr_of_audio_title_sets']}")
            print(f"    Video title sets: {amg['nr_of_video_title_sets']}")
            print(f"    Provider: {amg['provider_identifier']!r}")
            print(f"    Spec version: {amg['spec_version']}")

    cppm = disc.get("cppm", {})
    print(f"  CPPM: {'YES (' + str(cppm['mkb_size']) + ' bytes)' if cppm.get('mkb_present') else 'No'}")

    if "atsi" in disc:
        for ts_name, atsi in disc["atsi"].items():
            if "error" in atsi:
                print(f"\n  {ts_name}: ERROR - {atsi['error']}")
                continue
            print(f"\n  {ts_name}: {atsi['magic']}")
            print(f"    Type: {'Audio' if atsi['is_audio_titleset'] else 'Video'}")
            print(f"    AOB start sector: {atsi['atstt_vobs_sector']}")

            if atsi["audio_formats"]:
                for fmt in atsi["audio_formats"]:
                    print(f"    Audio format [{fmt['index']}]:")
                    print(f"      Type: 0x{fmt['audio_type']:04x}")
                    g1 = f"{fmt['group1_sample_rate']/1000:.1f}kHz/{fmt['group1_bit_depth']}bit"
                    print(f"      Group 1: {g1}")
                    if fmt["group2_sample_rate"] > 0:
                        g2 = f"{fmt['group2_sample_rate']/1000:.1f}kHz/{fmt['group2_bit_depth']}bit"
                        print(f"      Group 2: {g2}")
                    print(f"      Channels: {fmt['channel_layout']} (assignment {fmt['channel_assignment']})")

            for title in atsi.get("titles", []):
                print(f"    Title {title['title_number']}: {title['track_count']} tracks, "
                      f"{title['index_count']} indexes, "
                      f"{title['duration_seconds']:.1f}s")
                for track in title["tracks"]:
                    sec_info = ""
                    if "first_sector" in track:
                        sec_info = f" sectors {track['first_sector']}-{track['last_sector']}"
                    print(f"      Track {track['track_number']:2d}: "
                          f"{track['duration_seconds']:7.3f}s "
                          f"(PTS {track['first_pts']}-+{track['len_in_pts']})"
                          f"{sec_info}")

    if "samg" in disc:
        samg = disc["samg"]
        if "error" in samg:
            print(f"\n  SAMG: ERROR - {samg['error']}")
        else:
            print(f"\n  SAMG: {samg['magic']} ({samg['nr_of_tracks']} entries)")
            for t in samg["tracks"]:
                g1 = f"{t['group1_sample_rate']/1000:.1f}kHz/{t['group1_bit_depth']}bit"
                print(f"    Group {t['group']:2d} Track {t['track']:2d}: "
                      f"{t['duration_seconds']:7.3f}s  {t['zone']}  {g1}  "
                      f"{t['channel_layout']}  "
                      f"sectors {t['abs_first_sector']}-{t['abs_last_sector']}")

    print()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <fixture_dir> [--json]", file=sys.stderr)
        sys.exit(1)

    target = Path(sys.argv[1])
    output_json = "--json" in sys.argv

    # If target contains IFO files directly, probe it
    # If target contains subdirectories with IFO files, probe each
    dirs_to_probe = []
    if (target / "AUDIO_TS.IFO").exists():
        dirs_to_probe.append(target)
    else:
        for d in sorted(target.iterdir()):
            if d.is_dir() and (d / "AUDIO_TS.IFO").exists():
                dirs_to_probe.append(d)

    if not dirs_to_probe:
        print(f"No IFO fixtures found in {target}", file=sys.stderr)
        sys.exit(1)

    results = []
    for d in dirs_to_probe:
        disc = probe_disc(d)
        results.append(disc)
        if not output_json:
            print_human_readable(disc)

    if output_json:
        print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
