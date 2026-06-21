use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn build() {
    println!("cargo:rerun-if-env-changed=TONEPOET_DISABLE_NATIVE_MLP_DECODER");
    if env::var_os("TONEPOET_DISABLE_NATIVE_MLP_DECODER").is_some() {
        println!("cargo:warning=native MLP decoder shim disabled by TONEPOET_DISABLE_NATIVE_MLP_DECODER");
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let shim = find_existing(&manifest_dir, &[
        "src/convert/pipeline/native_mlp_decoder.c",
        "tonepoet_dvda_source/pipeline/native_mlp_decoder.c",
    ]).unwrap_or_else(|| {
        panic!(
            "native MLP decoder shim source not found under {}; expected src/convert/pipeline/native_mlp_decoder.c",
            manifest_dir.display()
        )
    });
    let header = shim.with_file_name("native_mlp_decoder.h");
    println!("cargo:rerun-if-changed={}", shim.display());
    println!("cargo:rerun-if-changed={}", header.display());

    let pkg = pkg_config_flags(&["libavcodec", "libavutil", "libswresample"]);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let object = out_dir.join("native_mlp_decoder.o");
    let archive = out_dir.join("libtonepoet_native_mlp_decoder.a");

    let cc = env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let mut compile = Command::new(&cc);
    compile
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-O2")
        .arg("-fPIC")
        .arg("-c")
        .arg(&shim)
        .arg("-o")
        .arg(&object);
    for flag in &pkg.cflags {
        compile.arg(flag);
    }
    run_or_panic(compile, "compile native MLP decoder shim");

    let ar = env::var_os("AR").unwrap_or_else(|| OsString::from("ar"));
    let mut archive_cmd = Command::new(ar);
    archive_cmd.arg("crs").arg(&archive).arg(&object);
    run_or_panic(archive_cmd, "archive native MLP decoder shim");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=tonepoet_native_mlp_decoder");
    for lib_dir in pkg.lib_dirs {
        println!("cargo:rustc-link-search=native={lib_dir}");
    }
    for lib in pkg.libs {
        println!("cargo:rustc-link-lib={lib}");
    }
}

struct PkgFlags {
    cflags: Vec<String>,
    lib_dirs: Vec<String>,
    libs: Vec<String>,
}

fn find_existing(root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|candidate| root.join(candidate))
        .find(|path| path.exists())
}

fn pkg_config_flags(packages: &[&str]) -> PkgFlags {
    let output = Command::new("pkg-config")
        .arg("--cflags")
        .arg("--libs")
        .args(packages)
        .output()
        .unwrap_or_else(|err| panic!("pkg-config failed to start: {err}"));
    if !output.status.success() {
        panic!(
            "pkg-config could not locate FFmpeg development packages {:?}: {}",
            packages,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut cflags = Vec::new();
    let mut lib_dirs = Vec::new();
    let mut libs = Vec::new();
    for token in stdout.split_whitespace() {
        if let Some(path) = token.strip_prefix("-L") {
            lib_dirs.push(path.to_string());
        } else if let Some(lib) = token.strip_prefix("-l") {
            libs.push(lib.to_string());
        } else {
            cflags.push(token.to_string());
        }
    }
    PkgFlags { cflags, lib_dirs, libs }
}

fn run_or_panic(mut command: Command, action: &str) {
    let output = command.output().unwrap_or_else(|err| panic!("failed to {action}: {err}"));
    if !output.status.success() {
        panic!(
            "failed to {action}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
