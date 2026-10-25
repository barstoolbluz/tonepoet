#!/usr/bin/env python3
"""D-03 independent FFmpeg PCM extent/offset fixture; does NOT run Rust."""
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

RATE = 44100
SAMPLES = 40013
RANGES = ((31, 17111), (17139, 35137), (35191, SAMPLES))
FRAME_BYTES = 8


def run(*args):
    process = subprocess.run(args, capture_output=True, check=True)
    return process.stdout


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    with tempfile.TemporaryDirectory(prefix="tonepoet-ip04-gapped-") as name:
        image = Path(name) / "album.flac"
        run("ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100",
            "-af", "atrim=end_sample=40013", "-ac", "2", "-c:a", "flac",
            "-sample_fmt", "s16", str(image))
        info = run("metaflac", "--show-total-samples", "--show-bps", str(image)).decode().splitlines()
        assert info == [str(SAMPLES), "16"], info
        full = run("ffmpeg", "-v", "error", "-nostdin", "-i", str(image),
                   "-f", "s32le", "-c:a", "pcm_s32le", "pipe:1")
        assert len(full) == SAMPLES * FRAME_BYTES
        data = []
        for index, (start, end) in enumerate(RANGES):
            seek = start // RATE
            anchor = start - seek * RATE
            filter_arg = f"atrim=start_sample={anchor}:end_sample={anchor+end-start},asetpts=N/SR/TB"
            output = run("ffmpeg", "-v", "error", "-hide_banner", "-nostdin", "-xerror",
                         "-accurate_seek", "-ss", str(seek), "-err_detect", "crccheck+explode",
                         "-i", str(image), "-map", "0:a:0", "-vn", "-sn", "-dn",
                         "-af", filter_arg, "-f", "s32le", "-c:a", "pcm_s32le", "pipe:1")
            expected = full[start*FRAME_BYTES:end*FRAME_BYTES]
            assert output == expected, f"incorrect selected PCM range {index+1}"
            assert len(output) == (end-start)*FRAME_BYTES
            data.append({"ordinal": index+1, "start": start, "end": end,
                         "bytes": len(output), "sha256": digest(output)})
        report = {"outcome": "PASS", "scope": "independent native test only; Rust test NOT RUN",
                  "source_flac_sha256": digest(image.read_bytes()),
                  "full_image_pcm_sha256": digest(full), "sample_count": SAMPLES,
                  "bit_depth": 16, "gaps": [31, 28, 54], "ranges": data}
        print(json.dumps(report, indent=2, sort_keys=True))

if __name__ == "__main__":
    main()
