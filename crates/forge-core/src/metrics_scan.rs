//! Test-support scanner for `metrics`-facade emission sites.
//!
//! Used by the per-crate metric self-scan tests (see each crate's
//! `metrics` module) to harvest every `counter!`/`gauge!`/`histogram!`
//! invocation with a string-literal name from a source file, so the
//! describe lists cannot silently drift from the emission sites.
//!
//! This is not part of forge-core's public API surface; it lives here
//! (rather than being copy-pasted into four test modules) because every
//! emitting crate already depends on forge-core.

/// One facade emission site: `(macro kind, metric name)`.
///
/// `kind` is `"counter"`, `"gauge"`, or `"histogram"`.
pub type FacadeEmission = (String, String);

/// Harvest every `counter!` / `gauge!` / `histogram!` invocation with a
/// string-literal name from `source`.
///
/// - Code from the first `#[cfg(test)]` onward is ignored, so metric
///   names mentioned in test assertions are not treated as emissions.
/// - `describe_counter!` and friends do not match: the macro ident must
///   not be preceded by an identifier character.
/// - Invocations whose first argument is not a string literal (for
///   example a `const` name) are skipped; the self-scan tests assert
///   that no such invocation exists so every emission stays greppable.
pub fn facade_emissions(source: &str) -> Vec<FacadeEmission> {
    let main = source.split("#[cfg(test)]").next().unwrap_or("");
    let mut out = Vec::new();
    for kind in ["counter", "gauge", "histogram"] {
        let needle = format!("{kind}!(");
        let mut from = 0;
        while let Some(pos) = main[from..].find(&needle) {
            let at = from + pos;
            from = at + needle.len();
            if at > 0 {
                let prev = main.as_bytes()[at - 1];
                if prev == b'_' || prev.is_ascii_alphanumeric() {
                    // `describe_counter!(…)` or similar — not an emission.
                    continue;
                }
            }
            let rest = main[at + needle.len()..].trim_start();
            if let Some(name) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) {
                out.push((kind.to_string(), name.to_string()));
            }
        }
    }
    out
}

/// Harvest [`facade_emissions`] from every `.rs` file under `dir`,
/// recursively. Panics on I/O errors — this only runs in tests.
pub fn facade_emissions_in_dir(dir: &std::path::Path) -> Vec<FacadeEmission> {
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            out.extend(facade_emissions_in_dir(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            out.extend(facade_emissions(&src));
        }
    }
    out
}

/// Count invocations of the emission macros whose first argument is
/// *not* a string literal. The self-scan tests assert this is zero so
/// every emission site stays greppable and scannable.
pub fn non_literal_emissions(source: &str) -> usize {
    let main = source.split("#[cfg(test)]").next().unwrap_or("");
    let mut count = 0;
    for kind in ["counter", "gauge", "histogram"] {
        let needle = format!("{kind}!(");
        let mut from = 0;
        while let Some(pos) = main[from..].find(&needle) {
            let at = from + pos;
            from = at + needle.len();
            if at > 0 {
                let prev = main.as_bytes()[at - 1];
                if prev == b'_' || prev.is_ascii_alphanumeric() {
                    continue;
                }
            }
            if !main[at + needle.len()..].trim_start().starts_with('"') {
                count += 1;
            }
        }
    }
    count
}

/// Sum [`non_literal_emissions`] over every `.rs` file under `dir`,
/// recursively. Panics on I/O errors — this only runs in tests.
pub fn non_literal_emissions_in_dir(dir: &std::path::Path) -> usize {
    let mut count = 0;
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            count += non_literal_emissions_in_dir(&path);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            count += non_literal_emissions(&src);
        }
    }
    count
}
