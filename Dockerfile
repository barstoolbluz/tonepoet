# tonepoet — nix-free build
#
# Debian trixie is the base because it ships ffmpeg 7.1, which is what
# ffmpeg-next 7.1 and the native MLP decoder shim link against. Bookworm
# (ffmpeg 5) and Ubuntu 24.04 (ffmpeg 6) will NOT work.
#
#   docker build -t tonepoet .
#   docker run --rm -it -v "$PWD/audio:/audio" tonepoet tui
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

# bindgen (ffmpeg-sys-next, libbluray-sys) needs libclang.so on the path.
RUN echo "export LIBCLANG_PATH=$(dirname $(find /usr/lib -name 'libclang.so*' | head -1))" \
      > /etc/profile.d/libclang.sh
ENV LIBCLANG_PATH=/usr/lib/llvm-19/lib

# Rust toolchain (stable; the workspace needs >= 1.82, system Rust is too old).
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /build

COPY . .

# The nix build bakes reference-tool store paths into the binary via
# option_env!(). Outside nix there is no store, so point them at the
# distro prefix; track_executor falls back to PATH lookup when unset.
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
# Debian also packages. Three of the flake's tools have no trixie package, so
# `check-tools` reports them missing — none of them block conversion:
#   ssrc          - brick-wall resampling; sox/ffmpeg resamplers still work
#   sox_ng        - stock sox is installed instead (no Gesemann dither, no DSD)
#   monkeys-audio - APE decoding still works via ffmpeg
# Debian's `7zip` provides /usr/bin/7z but no `7zz`; tonepoet accepts 7z.
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
      libbluray2 libudfread0 libaacs0 \
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
