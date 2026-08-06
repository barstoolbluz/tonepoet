# syntax=docker/dockerfile:1
# tonepoet — nix-free build
#
# Requires BuildKit (default in Docker 23+; otherwise DOCKER_BUILDKIT=1) for
# the RUN --mount cache mounts below.
#
# Debian trixie is the base because it ships ffmpeg 7.1, which is what
# ffmpeg-next 7.1 and the native MLP decoder shim link against. Bookworm
# (ffmpeg 5) and Ubuntu 24.04 (ffmpeg 6) will NOT work.
#
#   docker build -t tonepoet .
#   docker run --rm -it \
#     -v "$PWD/audio:/audio" \
#     -v tonepoet-state:/root/.config/tonepoet \
#     tonepoet tui
#
# The tonepoet-state volume persists config, presets, and the copy/move
# recovery journal (which restores interrupted file operations on the next
# start). Without it, `--rm` discards all of that with the container.
# Optionally add `-v tonepoet-cache:/root/.cache/tonepoet` for the
# conversion queue and probe cache.
#
# ---------------------------------------------------------------------------
# Stage 1: build
# ---------------------------------------------------------------------------
FROM debian:trixie-slim AS builder

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl \
      build-essential pkg-config \
      clang libclang-dev llvm-dev \
      libavcodec-dev libavdevice-dev libavfilter-dev libavformat-dev \
      libavutil-dev libpostproc-dev libswresample-dev libswscale-dev \
      libssl-dev \
      libbluray-dev libudfread-dev \
      libdbus-1-dev \
    && rm -rf /var/lib/apt/lists/*

# bindgen (ffmpeg-sys-next, libbluray-sys) needs libclang.so. Resolve the
# real llvm directory at build time and expose it behind a stable symlink so
# a trixie point-release moving past llvm-19 cannot silently break the build.
RUN ln -s "$(dirname "$(find /usr/lib -name 'libclang.so*' | head -1)")" /opt/libclang
ENV LIBCLANG_PATH=/opt/libclang

# Rust toolchain. Pinned for reproducible rebuilds — an unpinned `stable`
# floats with upstream releases. NB: Cargo.toml's rust-version = "1.82"
# understates the real requirement; 1.82 cannot resolve this workspace's
# dependency tree (see CLAUDE.md). 1.93.1 matches the nix dev shell.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.93.1

WORKDIR /build

COPY . .

# The nix build bakes reference-tool store paths into the binary via
# option_env!(). Outside nix there is no store, so point them at the
# distro prefix; track_executor falls back to PATH lookup when unset.
#
# Reference mode COMPARES these: the compiled {STORE_PATH}/bin/<tool>, the
# runtime TONEPOET_REFERENCE_*_PATH, and the exec'd path must all
# canonicalize to the same executable. /usr + /usr/bin/<tool> (runtime
# stage below) satisfy that by construction. Unlike a nix store path,
# /usr/bin tools can change under a running container (apt upgrade); the
# executor re-hashes the tool and fails closed on drift, so that surfaces
# as an honest error rather than silent divergence.
ENV TONEPOET_REFERENCE_SOX_STORE_PATH=/usr \
    TONEPOET_REFERENCE_FFMPEG_STORE_PATH=/usr \
    TONEPOET_REFERENCE_METAFLAC_STORE_PATH=/usr \
    TONEPOET_REFERENCE_WVTAG_STORE_PATH=/usr \
    TONEPOET_REFERENCE_ATOMIC_PARSLEY_STORE_PATH=/usr

# Registry and target dirs are BuildKit cache mounts, so a rebuild after a
# source edit reuses compiled dependencies. Cache mounts don't persist into
# the image layer, so the binary is copied out inside the same RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked \
    && cp target/release/tonepoet /usr/local/bin/tonepoet

# ---------------------------------------------------------------------------
# Stage 2: runtime
# ---------------------------------------------------------------------------
FROM debian:trixie-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

# Audio tools tonepoet shells out to. Everything the nix flake provides that
# Debian also packages. Known gaps vs the flake — none block conversion:
#   ssrc          - no trixie package; `check-tools` reports it MISSING.
#                   Brick-wall resampling unavailable; sox/ffmpeg still work.
#   7zz           - `check-tools` reports it MISSING: Debian's `7zip` ships
#                   /usr/bin/7z but no `7zz`; tonepoet falls back to 7z.
#   sox_ng        - stock sox substituted (no Gesemann dither, no DSD path).
#                   Not probed by name, so check-tools does NOT flag it.
#   monkeys-audio - no trixie package; APE decoding still works via ffmpeg.
#                   Not probed by name, so check-tools does NOT flag it.
# `acl` supplies getfacl/setfacl, which FLAC metadata rewrites use to
# preserve POSIX ACLs (hard error when required and absent).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
      ffmpeg \
      sox libsox-fmt-all \
      flac \
      lame \
      opus-tools opustags \
      wavpack \
      atomicparsley \
      7zip \
      loudgain \
      acl \
      libbluray2 libudfread0 libaacs0 libbdplus0 \
      libdbus-1-3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/tonepoet /usr/local/bin/tonepoet

ENV TONEPOET_REFERENCE_SOX_PATH=/usr/bin/sox \
    TONEPOET_REFERENCE_FFMPEG_PATH=/usr/bin/ffmpeg \
    TONEPOET_REFERENCE_METAFLAC_PATH=/usr/bin/metaflac \
    TONEPOET_REFERENCE_WVTAG_PATH=/usr/bin/wvtag \
    TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH=/usr/bin/AtomicParsley

# TUI needs a sane terminal when run with -it.
ENV TERM=xterm-256color

WORKDIR /audio
ENTRYPOINT ["tonepoet"]
CMD ["check-tools"]
