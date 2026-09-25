use sha2::{Digest, Sha256};
use std::io;

/// Source files whose exact contents define the common Reference runtime source
/// closure. The root package version is intentionally identity-only and is
/// canonicalized in both Cargo.toml and Cargo.lock before hashing; every other
/// byte in these files remains bound.
pub const REFERENCE_COMMON_SOURCE_PATHS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "reference_source_lock.rs",
    "tonepoet-pipeline/Cargo.toml",
    "tonepoet-pipeline/src/semantic_plan.rs",
    "tonepoet-pipeline/src/dsd_reference.rs",
    "tonepoet-pipeline/src/dsd_album_gain.rs",
    "tonepoet-pipeline/src/plan.rs",
    "tonepoet-pipeline/src/settings.rs",
    "tonepoet-pipeline/src/enums.rs",
    "tonepoet-pipeline/src/source.rs",
    "tonepoet-pipeline/src/tools.rs",
    "tonepoet-pipeline/src/fingerprint.rs",
    "tonepoet-pipeline/src/qualification_schema.rs",
    "tonepoet-pipeline/src/plugins.rs",
    "tonepoet-pipeline/src/w64.rs",
    "src/convert/pipeline/plan_bridge.rs",
    "src/convert/pipeline/track_executor.rs",
    "src/convert/pipeline/stages.rs",
    "src/convert/pipeline/manifest_builder.rs",
    "src/convert/replaygain.rs",
];

const ROOT_PACKAGE_VERSION_SENTINEL: &str = "\"<reference-package-version-ignored>\"";

fn canonicalize_assignment_value(line: &str, key: &str, sentinel: &str) -> Option<String> {
    let (body, newline) = line
        .strip_suffix('\n')
        .map_or((line, ""), |body| (body, "\n"));
    let body_no_cr = body.strip_suffix('\r').unwrap_or(body);
    let cr = if body.ends_with('\r') { "\r" } else { "" };
    let equal = body_no_cr.find('=')?;
    if body_no_cr[..equal].trim() != key {
        return None;
    }

    let rhs = &body_no_cr[equal + 1..];
    let leading_ws_len = rhs.len() - rhs.trim_start().len();
    let leading_ws = &rhs[..leading_ws_len];
    let value_and_suffix = &rhs[leading_ws_len..];
    let value_end = value_and_suffix.find('#').unwrap_or(value_and_suffix.len());
    let before_comment = &value_and_suffix[..value_end];
    let trailing_ws_start = before_comment.trim_end().len();
    let suffix = &value_and_suffix[trailing_ws_start..];
    let comment = &value_and_suffix[value_end..];

    Some(format!(
        "{}{}{}{}{}{}{}",
        &body_no_cr[..=equal],
        leading_ws,
        sentinel,
        suffix,
        comment,
        cr,
        newline,
    ))
}

fn canonicalize_root_lock_package_version(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Cargo.lock is not UTF-8: {error}"),
        )
    })?;

    let mut output = String::with_capacity(text.len() + ROOT_PACKAGE_VERSION_SENTINEL.len());
    let mut package_block = String::new();
    let mut in_package = false;
    let mut replacements = 0usize;

    let flush_package = |block: &mut String,
                         output: &mut String,
                         replacements: &mut usize|
     -> io::Result<()> {
        if block.is_empty() {
            return Ok(());
        }
        let is_root_tonepoet = block.lines().any(|line| line.trim() == "name = \"tonepoet\"")
            && !block.lines().any(|line| line.trim_start().starts_with("source ="));
        if !is_root_tonepoet {
            output.push_str(block);
            block.clear();
            return Ok(());
        }

        let mut replaced = false;
        for line in block.split_inclusive('\n') {
            if !replaced {
                if let Some(canonical) =
                    canonicalize_assignment_value(line, "version", ROOT_PACKAGE_VERSION_SENTINEL)
                {
                    output.push_str(&canonical);
                    replaced = true;
                    continue;
                }
            }
            output.push_str(line);
        }
        if !replaced {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source-less root tonepoet Cargo.lock package has no version assignment",
            ));
        }
        *replacements += 1;
        block.clear();
        Ok(())
    };

    for line in text.split_inclusive('\n') {
        if line.trim() == "[[package]]" {
            if in_package {
                flush_package(&mut package_block, &mut output, &mut replacements)?;
            }
            in_package = true;
            package_block.push_str(line);
        } else if in_package {
            package_block.push_str(line);
        } else {
            output.push_str(line);
        }
    }
    if in_package {
        flush_package(&mut package_block, &mut output, &mut replacements)?;
    }

    if replacements != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected exactly one source-less root tonepoet package in Cargo.lock, found {replacements}"
            ),
        ));
    }
    Ok(output.into_bytes())
}

/// Return the bytes that participate in the Reference source-lock hash.
///
/// Only the value of `[package].version` in the workspace root Cargo.toml and
/// the corresponding source-less root package version in Cargo.lock are
/// canonicalized. Whitespace, comments, section placement, dependency versions,
/// and every other source byte remain source-lock inputs.
pub fn reference_source_bytes_for_hash(path: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    if path == "Cargo.lock" {
        return canonicalize_root_lock_package_version(bytes);
    }
    if path != "Cargo.toml" {
        return Ok(bytes.to_vec());
    }

    let text = std::str::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("root Cargo.toml is not UTF-8: {error}"),
        )
    })?;

    let mut in_package = false;
    let mut replaced = false;
    let mut output = String::with_capacity(text.len() + ROOT_PACKAGE_VERSION_SENTINEL.len());

    for line in text.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |body| (body, "\n"));
        let body_no_cr = body.strip_suffix('\r').unwrap_or(body);
        let trimmed = body_no_cr.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
        }

        if in_package && !replaced {
            if let Some(equal) = body_no_cr.find('=') {
                let key = body_no_cr[..equal].trim();
                if key == "version" {
                    // Keep all formatting before '=' and all whitespace/comment
                    // bytes after the TOML value so only the package-version
                    // value itself is excluded from identity.
                    output.push_str(
                        &canonicalize_assignment_value(
                            line,
                            "version",
                            ROOT_PACKAGE_VERSION_SENTINEL,
                        )
                        .expect("matching version assignment canonicalizes"),
                    );
                    replaced = true;
                    continue;
                }
            }
        }

        output.push_str(body);
        output.push_str(newline);
    }

    if !replaced {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "root Cargo.toml has no [package].version assignment to canonicalize",
        ));
    }

    Ok(output.into_bytes())
}

pub fn hash_reference_source_files_with_reader<F>(
    paths: &[&str],
    mut read: F,
) -> io::Result<String>
where
    F: FnMut(&str) -> io::Result<Vec<u8>>,
{
    let mut hasher = Sha256::new();
    for path in paths {
        let raw = read(path)?;
        let bytes = reference_source_bytes_for_hash(path, &raw)?;
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
