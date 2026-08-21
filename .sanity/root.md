# root — sanity assessment

9 of 9 read · 2 surprising

Each entry below is one **reading**, of a function or of a whole file. An
agent was given its name, signature, neighboring names and comments — never
its body — and wrote down what it expected to find. Then it opened the file.
The gap between the two is the finding. A file's own entry is titled `the file
itself` and asks whether the header at the top describes what is actually in
there.

`read at` is a hash of the body as it was when the reading was made. When it
stops matching the code, the reading is marked STALE and goes back in the
queue.

What this is and how to add to it: [README.md](README.md)

## build.rs

### the file itself — OBSCURE
- spec 3 · read at `85fd5a79591b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:50:11Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A cargo build script whose main() shells out to git to capture the commit hash (and maybe dirty/build-date info), then emits cargo:rustc-env directives so that version/build metadata gets compiled into the binary (used for things like a --version string or the gnudb user-agent). Likely also emits cargo:rerun-if-changed for .git/HEAD or similar so it only reruns when necessary.
- found: build.rs invokes build_native_mlp_decoder::build() (native codec build step), reads the pipeline crate's version straight out of its Cargo.toml text to embed as TONEPOET_PIPELINE_VERSION, and forwards several TONEPOET_REFERENCE_*_STORE_PATH env vars into rustc-env while registering rerun-if-env-changed for each — these look like paths to reference CLI tools (sox, ffmpeg, metaflac, wvtag, AtomicParsley) used by tests/tooling.
- predicted: none · documented: none · derivable: yes · legible: not judged · trap: no

### `main` — OBSCURE
- spec 3 · read at `f9e5c5684ddc` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:03:34Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A cargo build script that shells out to `git` to capture the current commit hash (and maybe dirty/tag state), then emits `cargo:rustc-env=GIT_HASH=...` (or similar) so it can be baked into the binary as version info, along with a `cargo:rerun-if-changed` directive for the git HEAD file.
- found: Builds a native MLP decoder, reads the pipeline crate's version out of its Cargo.toml by string-parsing (not git hash at all), embeds it as an env var, and forwards several TONEPOET_REFERENCE_*_STORE_PATH env vars into the build for use as compile-time constants.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: no peers/docs on this task meant nothing hinted at the native decoder build step or the reference-tool-store-path forwarding, both of which are specific to this project's Nix-style reproducible reference-binary setup.

## build_native_mlp_decoder.rs

### the file itself
- spec 3 · read at `6deae59da158` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:50:10Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A build-script helper (used from build.rs) that ensures a native MLP/TrueHD decoder library is available: it first checks for an existing installed library (find_existing, possibly via pkg-config flags), and if not found, builds it from bundled/vendored C source using an external build tool (run_or_panic to shell out and panic on failure), then emits cargo:rustc-link-lib/search directives so the Rust crate can link against it.
- found: A build.rs helper that (unless disabled by an env var) locates a single vendored C shim source file (native_mlp_decoder.c), compiles it directly with cc using pkg-config flags for libavcodec/libavutil/libswresample, archives it into a static lib with ar, then emits cargo link-search/link-lib directives for both the shim and the FFmpeg libs.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build`
- spec 3 · read at `cd7a3cbdd4e0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:51:08Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a cargo build.rs entry point for building a native MLP decoder library. It likely first calls find_existing() to check if a prebuilt/system library is already available, and if not, uses pkg_config_flags() to get compiler/linker flags and run_or_panic() to invoke an external build tool (make/cmake/cc) to compile the native sources. It probably ends by emitting cargo:rustc-link-lib and cargo:rustc-link-search directives so the Rust crate links against the resulting library.
- found: Checks an env var to allow disabling the native shim; locates the native_mlp_decoder.c source file (not a prebuilt lib) via find_existing; gets pkg-config flags for libavcodec/libavutil/libswresample; compiles the .c file to an object with cc using strict flags, archives it into a static lib with ar via run_or_panic; then emits cargo link-search/link-lib directives for both the static lib and the pkg-config libs.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `find_existing`
- spec 3 · read at `60694d1ea3df` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:53:06Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Iterates candidates (relative path strings), joins each to root, and returns the first one that exists() as a PathBuf, or None if none exist — a simple "find a file/lib from a list of candidate names" helper for the native MLP decoder build.
- found: Joins each candidate to root and returns the first path that exists(), else None.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `pkg_config_flags`
- spec 3 · read at `077f94a4ac86` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:10:55Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: For each package name in `packages`, runs `pkg-config --cflags --libs <package>` (likely via std::process::Command), parses the whitespace-separated output into include paths / compiler flags and library link flags, and accumulates them across all packages into a single PkgFlags struct. If pkg-config is missing or a package isn't found, it probably continues with empty flags for that package rather than panicking, since this is a build script that should degrade gracefully.
- found: Runs `pkg-config --cflags --libs <packages...>` as a single command call, panics if pkg-config fails to start or returns a non-success status (naming FFmpeg dev packages in the message), then splits stdout on whitespace into lib_dirs (-L), libs (-l), and everything else as cflags, returning a PkgFlags struct.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I expected graceful degradation on failure; the build script actually panics, which is reasonable for a build-time hard dependency.

### `run_or_panic`
- spec 3 · read at `8fd4e377ec52` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:52:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Runs the given Command (e.g. via .status() or .output()), and if it fails to spawn or returns a non-success exit status, panics with a message that includes `action` and the failure details — a small helper to make build-script command failures fatal with context.
- found: Exactly as predicted: uses .output(), panics with action+error on spawn failure, panics with action+stdout+stderr on non-success exit.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## flake.nix

### the file itself
- spec 3 · read at `eda76a7a750f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:50:25Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A Nix flake defining inputs (nixpkgs, likely a Rust toolchain overlay like rust-overlay or fenix, and flake-utils for multi-system support) and a single `outputs` function that produces a devShell (with Rust toolchain, cargo, and native build deps like audio/codec libraries) and possibly a `packages.default` derivation that builds the tonepoet binary via cargo/naersk/crane.
- found: Nix flake with nixpkgs/flake-utils/rust-overlay plus three custom-built audio tool flake inputs (sox_ng, opustags, ssrc); outputs a per-system buildRustPackage derivation that wraps the tonepoet binary with a PATH of many reference audio CLI tools (sox, ffmpeg, opustags, ssrc, loudgain, flac, wavpack, etc.) and sets TONEPOET_REFERENCE_* env vars pinning exact store paths, plus a matching devShell and an apps.default runner.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `outputs`
- spec 3 · read at `4437a13d0490` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:32:48Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Uses flake-utils.lib.eachDefaultSystem to iterate supported systems, imports nixpkgs with the rust-overlay overlay applied to get a pinned Rust toolchain, and pulls in sox_ng, opustags, and ssrc as flake inputs providing external CLI tools this audio-conversion project shells out to. Defines packages.default (building the Rust crate), a devShells.default with the toolchain plus those audio tools on PATH, and possibly apps.default for nix run.
- found: eachDefaultSystem with rust-overlay for the toolchain; patches opus-tools and pins loudgain to ffmpeg_6; defines singular "policy-owned" reference sox/ffmpeg bindings; packages.default builds tonepoet via buildRustPackage with reference-tool store paths baked in as env vars and wraps the binary's PATH/env vars via wrapProgram at install time; devShells.default sets the same reference env vars plus LD_LIBRARY_PATH for dlopen'd libaacs; apps.default runs the built binary.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The TONEPOET_REFERENCE_*_STORE_PATH vs TONEPOET_REFERENCE_*_PATH distinction (compiled-in store path vs runtime binary path) and the "policy-owned" comment about singular bindings are load-bearing details a predictor from signature alone would miss entirely.
