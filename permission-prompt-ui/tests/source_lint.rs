//! Backstop for the rendering-safety and environment-mutation encapsulation.
//!
//! These are not the mechanism — the mechanism is that `Escaped` is the only text the dialog
//! builder accepts, and that the pre-GTK setup module is the only place that touches `environ`.
//! This test exists so a future edit cannot quietly reintroduce either hazard.

use std::path::{Path, PathBuf};

struct Rule {
    /// Substrings that must not appear...
    tokens: &'static [&'static str],
    /// ...outside these paths, given relative to the workspace root.
    allowed_in: &'static [&'static str],
    why: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        tokens: &["set_markup", "use_markup", "set_use_underline", "markup="],
        allowed_in: &["permission-prompt-ui/src/text.rs"],
        why: "caller text must never be interpreted as Pango markup or as a mnemonic",
    },
    Rule {
        tokens: &["Label::new", "set_text(", "set_label(", "TextView", "gtk::Entry"],
        allowed_in: &["permission-prompt-ui/src/text.rs"],
        why: "only the audited text module may construct or write to a text widget",
    },
    Rule {
        tokens: &["set_var", "remove_var", "clearenv"],
        allowed_in: &["sudo-prompt/src/envsetup.rs"],
        why: "the gate's own environment is set up once, before GTK init and any threads",
    },
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn workspace_sources_respect_the_encapsulation() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(files.len() > 5, "found only {} sources; the walk is broken", files.len());

    let mut violations = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        // This file names every forbidden token by definition.
        if rel.ends_with("tests/source_lint.rs") {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read source");
        for rule in RULES {
            if rule.allowed_in.contains(&rel.as_str()) {
                continue;
            }
            for token in rule.tokens {
                for (n, line) in text.lines().enumerate() {
                    if line.contains(token) {
                        violations.push(format!(
                            "{}:{}: `{}` — {} (allowed only in {:?})",
                            rel,
                            n + 1,
                            token,
                            rule.why,
                            rule.allowed_in
                        ));
                    }
                }
            }
        }
    }
    assert!(violations.is_empty(), "source lint failed:\n{}", violations.join("\n"));
}
