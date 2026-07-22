# Metadata persistence carrier fixtures

These are tiny, valid audio carriers used by production-path metadata round-trip tests in
`src/tui/probe.rs`. Tests copy each file into a temporary directory before mutation; the
checked-in fixtures are never modified.

They were generated from 50 ms of mono digital silence with Debian FFmpeg 7.1.3:

```sh
ffmpeg -f lavfi -i anullsrc=r=8000:cl=mono -t 0.05 -c:a libvorbis -q:a 0 -map_metadata -1 vorbis.ogg
ffmpeg -f lavfi -i anullsrc=r=8000:cl=mono -t 0.05 -c:a libmp3lame -b:a 16k -map_metadata -1 -id3v2_version 3 -write_id3v1 0 id3v2.mp3
ffmpeg -f lavfi -i anullsrc=r=8000:cl=mono -t 0.05 -c:a wavpack -map_metadata -1 ape.wv
ffmpeg -f lavfi -i anullsrc=r=8000:cl=mono -t 0.05 -c:a aac -b:a 16k -map_metadata -1 -movflags +faststart mp4.m4a
```

The tests depend only on the bytes in this directory and Lofty pinned by `Cargo.lock`; FFmpeg
is not required at test runtime.
