"""
Whole-album byte-exact validation harness for sacd-rs vs sacd_extract.

For each track on a SACD:
  1. Parse the C-ref DSF and DFF files' footers to extract metadata
     (ID3 frames + DIIN/MARK/COMT chunks)
  2. Invoke our sacd-rs extract_track example with matching CLI args
  3. cmp the full files against the C-ref
  4. Report pass/fail per track per format

## Prerequisites

  1. Build the C reference: `~/dev/sacd-extract/build/sacd_extract`
  2. Build our extractor: `cargo build -p sacd-rs --release --example extract_track`
  3. Extract all tracks via sacd_extract (DSF + DFF):
       sacd_extract -2 -s -i $ISO -y /tmp/sacd-compare/all-tracks-c-ref-dsf/
       sacd_extract -2 -p -i $ISO -o /tmp/sacd-compare/all-tracks-c-ref-dff/
  4. Get LSN ranges via `cargo run --example dump_sacd_lsn -- $ISO`;
     paste into TRACKS table below.

## Configuration

Edit the constants below for a different SACD. The script is tuned for
Thelonious Monk's "Solo Monk" (Sony SRGS 4520, 13 tracks, stereo
uncompressed) — change ISO and TRACKS for any other uncompressed-stereo
SACD with the same sacd_extract version.

Build-environment-specific values (creation timestamp + creating_machine
string) are extracted automatically from track 1's COMT chunk and reused
for all tracks (since they're shared across the sacd_extract run).
"""
import struct, subprocess, sys, os
from pathlib import Path

# --- Configuration ---
ISO = "/home/daedalus/library/monk/Thelonious Monk - Solo Monk (1965) [ISO] {SME JSACD SRGS 4520}/THELONIOUS MONK - SOLO MONK.iso"
EX = "/home/daedalus/dev/tonepoet/target/release/examples/extract_track"
C_REF_DSF_DIR = Path("/tmp/sacd-compare/all-tracks-c-ref-dsf/SOLO MONK")
C_REF_DFF_DIR = Path("/tmp/sacd-compare/all-tracks-c-ref-dff/SOLO MONK")
OUR_DIR = Path("/tmp/sacd-compare/our-tracks")
OUR_DIR.mkdir(exist_ok=True)

# Per-track LSN ranges from `dump_sacd_lsn` (SACDTRL1, narrow range,
# pre-trimmed; no --time-filter needed).
TRACKS = [
    (1,  1260,    53205, "DINAH"),
    (2,  54884,   133607, "I SURRENDER, DEAR"),
    (3,  134031,  197205, "SWEET AND LOVELY"),
    (4,  197563,  236638, "NORTH OF THE SUNSET"),
    (5,  237542,  356511, "RUBY, MY DEAR"),
    (6,  357135,  411885, "I'M CONFESSIN' (THAT I LOVE YOU)"),
    (7,  412407,  481671, "I HADN'T ANYONE TILL YOU"),
    (8,  482416,  555301, "EVERYTHING HAPPENS TO ME"),
    (9,  556192,  604269, "MONK'S POINT"),
    (10, 605486,  646413, "I SHOULD CARE"),
    (11, 647150,  744689, "ASK ME NOW"),
    (12, 746722,  821166, "THESE FOOLISH THINGS (REMIND ME OF YOU)"),
    (13, 822173,  868220, "INTROSPECTION"),
]

def find_chunk(buf, marker, start=0, scope_end=None):
    """Walk an ID3v2.4 tag (libid3-style size encoding) to find a frame."""
    if scope_end is None: scope_end = len(buf)
    off = start
    while off + 10 <= scope_end:
        fid = buf[off:off+4]
        size = (buf[off+4] << 23) | (buf[off+5] << 15) | (buf[off+6] << 7) | buf[off+7]
        if fid == marker:
            return off, size
        off += 10 + size
    return None

def parse_dsf_id3_footer(path):
    """Read the ID3v2.4 footer from a DSF file. Returns dict of frame values."""
    data = open(path, 'rb').read()
    # DSD chunk: read total_file_size (LE u64 at 12..20) and metadata_offset (20..28)
    meta_offset = struct.unpack('<Q', data[20:28])[0]
    total_size = struct.unpack('<Q', data[12:20])[0]
    footer = data[meta_offset:total_size]
    # Walk frames; collect TIT2/TALB/TPE1/TSRC/TPOS/TCON/TYER/TDAT/TRCK
    frames = {}
    off = 10  # skip tag header
    while off + 10 <= len(footer):
        fid = footer[off:off+4]
        size = (footer[off+4] << 23) | (footer[off+5] << 15) | (footer[off+6] << 7) | footer[off+7]
        body = footer[off+10:off+10+size]
        enc = body[0]
        text = body[1:].rstrip(b'\x00').decode('latin-1' if enc == 0 else 'utf-8', errors='replace')
        frames[fid.decode('ascii')] = text
        off += 10 + size
    return frames

def parse_dff_dff_metadata(path):
    """Extract DFF footer metadata (DIIN+COMT+ID3) from a DFF file.
    Returns (mark_dur_minutes, mark_dur_seconds, mark_dur_frames,
             disc_or_album_title, creation_y/m/d/h/m, creating_machine)."""
    data = open(path, 'rb').read()
    # Find audio end via DSD-data chunk size at offset 132..144
    dsd_data_size = struct.unpack('>Q', data[136:144])[0]
    audio_end = 144 + dsd_data_size
    footer = data[audio_end:]
    # Walk top-level chunks
    off = 0
    chunks = {}
    while off + 12 <= len(footer):
        cid = footer[off:off+4]
        size = struct.unpack('>Q', footer[off+4:off+12])[0]
        chunks[cid] = (off, size, footer[off+12:off+12+size])
        # advance: 12-byte header + body, padded to even
        off += 12 + size + (size % 2)
    # Parse DIIN children for MARK
    diin = chunks.get(b'DIIN')
    assert diin, "DIIN chunk missing"
    diin_body = diin[2]
    co = 0
    mark = None
    while co + 12 <= len(diin_body):
        cid = diin_body[co:co+4]
        size = struct.unpack('>Q', diin_body[co+4:co+12])[0]
        body = diin_body[co+12:co+12+size]
        if cid == b'MARK':
            mark = body
        co += 12 + size + (size % 2)
    assert mark, "MARK missing in DIIN"
    # MARK fields
    hours = struct.unpack('>H', mark[0:2])[0]
    minutes = mark[2]
    seconds = mark[3]
    samples = struct.unpack('>L', mark[4:8])[0]
    duration_frames = samples // (588 * 64)
    duration_minutes_total = hours * 60 + minutes
    # Parse COMT
    comt = chunks.get(b'COMT')
    assert comt, "COMT missing"
    comt_body = comt[2]
    numcomments = struct.unpack('>H', comt_body[0:2])[0]
    # Walk comments
    co = 2
    comments = []
    for _ in range(numcomments):
        y = struct.unpack('>H', comt_body[co:co+2])[0]
        m, d, h, mi = comt_body[co+2], comt_body[co+3], comt_body[co+4], comt_body[co+5]
        c_type = struct.unpack('>H', comt_body[co+6:co+8])[0]
        c_ref = struct.unpack('>H', comt_body[co+8:co+10])[0]
        c_count = struct.unpack('>L', comt_body[co+10:co+14])[0]
        c_text = comt_body[co+14:co+14+c_count].decode('latin-1', errors='replace')
        comments.append((y, m, d, h, mi, c_type, c_ref, c_text))
        co += 14 + c_count + (c_count % 2)
    # Comment 1 has "Material ripped from SACD: <title>"; extract title
    c1_text = comments[0][7]
    prefix = "Material ripped from SACD: "
    assert c1_text.startswith(prefix), c1_text
    disc_or_album_title = c1_text[len(prefix):]
    # Comment 2: timestamp + creating_machine
    c2 = comments[1]
    return {
        'duration_minutes_total': duration_minutes_total,
        'duration_seconds': seconds,
        'duration_frames': duration_frames,
        'disc_or_album_title': disc_or_album_title,
        'creation_year': c2[0],
        'creation_month_0_indexed': c2[1],
        'creation_day': c2[2],
        'creation_hour': c2[3],
        'creation_minute': c2[4],
        'creating_machine': c2[7],
        'mark_hours': hours,
        'disc_date_y': comments[0][0],
        'disc_date_m': comments[0][1],
        'disc_date_d': comments[0][2],
    }

def cmp_files(a, b):
    """Return True if files are byte-identical."""
    return subprocess.run(['cmp', a, b], capture_output=True).returncode == 0

# Get the DFF metadata that's CONSTANT across all tracks: the creating_machine
# and creation timestamp (sacd_extract was called once). Extract from track 1.
print("Extracting shared DFF metadata from C-ref track 1...")
t1_dff_meta = parse_dff_dff_metadata(str(C_REF_DFF_DIR / "01 - DINAH.dff"))
shared_creating_machine = t1_dff_meta['creating_machine']
shared_creation = (
    t1_dff_meta['creation_year'],
    t1_dff_meta['creation_month_0_indexed'],
    t1_dff_meta['creation_day'],
    t1_dff_meta['creation_hour'],
    t1_dff_meta['creation_minute'],
)
shared_disc_date = (
    t1_dff_meta['disc_date_y'],
    t1_dff_meta['disc_date_m'],
    t1_dff_meta['disc_date_d'],
)
print(f"  shared creation: {shared_creation}")
print(f"  shared disc_date: {shared_disc_date}")
print(f"  shared creating_machine: {shared_creating_machine!r}")
print()

# Loop
dsf_pass = 0
dff_pass = 0
dsf_fail = []
dff_fail = []

for track_idx, start_lsn, end_lsn, title in TRACKS:
    # Find C-ref files for this track
    c_dsf = list(C_REF_DSF_DIR.glob(f"{track_idx:02d} *.dsf"))[0]
    c_dff = list(C_REF_DFF_DIR.glob(f"{track_idx:02d} *.dff"))[0]
    print(f"=== Track {track_idx}: {title} ===")
    # Extract per-track metadata from C-ref DSF
    id3 = parse_dsf_id3_footer(str(c_dsf))
    dff = parse_dff_dff_metadata(str(c_dff))
    # ----- DSF -----
    our_dsf = OUR_DIR / f"{track_idx:02d}.dsf"
    # Parse TPOS, TRCK as "n/m" pairs
    tpos_n, tpos_m = id3['TPOS'].split('/')
    trck_n, trck_m = id3['TRCK'].split('/')
    args = [
        EX, "--iso", ISO,
        "--start-lsn", str(start_lsn), "--end-lsn", str(end_lsn),
        "--channels", "2", "--format", "dsf",
        "--id3-title", id3['TIT2'], "--id3-album", id3['TALB'],
        "--id3-artist", id3['TPE1'], "--id3-isrc", id3.get('TSRC', ''),
        "--id3-disc", f"{tpos_n}/{tpos_m}", "--id3-genre", id3['TCON'],
        "--id3-year", id3['TYER'], "--id3-date", id3['TDAT'],
        "--id3-track", f"{trck_n}/{trck_m}",
        "-o", str(our_dsf),
    ]
    subprocess.run(args, check=True, capture_output=True)
    if cmp_files(str(c_dsf), str(our_dsf)):
        print(f"  DSF: ✓ byte-exact ({c_dsf.stat().st_size:,} bytes)")
        dsf_pass += 1
    else:
        print(f"  DSF: ✗ DIVERGED")
        dsf_fail.append(track_idx)
    # ----- DFF -----
    our_dff = OUR_DIR / f"{track_idx:02d}.dff"
    args = [
        EX, "--iso", ISO,
        "--start-lsn", str(start_lsn), "--end-lsn", str(end_lsn),
        "--channels", "2", "--format", "dff",
        "--id3-title", id3['TIT2'], "--id3-album", id3['TALB'],
        "--id3-artist", id3['TPE1'], "--id3-isrc", id3.get('TSRC', ''),
        "--id3-disc", f"{tpos_n}/{tpos_m}", "--id3-genre", id3['TCON'],
        "--id3-year", id3['TYER'], "--id3-date", id3['TDAT'],
        "--id3-track", f"{trck_n}/{trck_m}",
        "--dff-diar", id3['TPE1'],  # = disc_artist (TPE1 fallback)
        "--dff-diti", id3['TIT2'],  # = track title
        "--dff-duration-minutes", str(dff['duration_minutes_total']),
        "--dff-duration-seconds", str(dff['duration_seconds']),
        "--dff-duration-frames", str(dff['duration_frames']),
        "--dff-disc-date", f"{shared_disc_date[0]}-{shared_disc_date[1]}-{shared_disc_date[2]}",
        "--dff-title", dff['disc_or_album_title'],
        "--dff-creation-time",
            f"{shared_creation[0]}-{shared_creation[1]}-{shared_creation[2]}-{shared_creation[3]}:{shared_creation[4]}",
        "--dff-creating-machine", shared_creating_machine,
        "-o", str(our_dff),
    ]
    subprocess.run(args, check=True, capture_output=True)
    if cmp_files(str(c_dff), str(our_dff)):
        print(f"  DFF: ✓ byte-exact ({c_dff.stat().st_size:,} bytes)")
        dff_pass += 1
    else:
        print(f"  DFF: ✗ DIVERGED")
        dff_fail.append(track_idx)

print()
print(f"=== SUMMARY ===")
print(f"DSF: {dsf_pass}/13 byte-exact" + (f"  FAILED: {dsf_fail}" if dsf_fail else ""))
print(f"DFF: {dff_pass}/13 byte-exact" + (f"  FAILED: {dff_fail}" if dff_fail else ""))
