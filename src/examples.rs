//! The plugin's example files: varied sample files for each language the
//! code editor registers (Rust, JavaScript, TypeScript, Python, Lua, TOML,
//! Markdown), provided read-only into every project under the plugin's
//! folder at `examples/`.

use std::sync::Arc;

use engine_fs::{EmbeddedFile, FolderSource, MemoryFolder, PluginFolder};

/// Every embedded example, at its path inside the plugin's folder.
pub const EXAMPLES: &[EmbeddedFile] = engine_fs::embedded_files!("examples_assets":
    "examples/README.md",
    "examples/rust/inventory.rs",
    "examples/rust/fibonacci.rs",
    "examples/javascript/debounce.js",
    "examples/javascript/event_emitter.js",
    "examples/typescript/result.ts",
    "examples/typescript/state_machine.ts",
    "examples/python/wordcount.py",
    "examples/python/grid_path.py",
    "examples/lua/timers.lua",
    "examples/lua/vector.lua",
    "examples/toml/project.toml",
    "examples/toml/manifest.toml",
    "examples/markdown/design_notes.md",
    "examples/markdown/changelog.md",
);

fn open(folder: PluginFolder) -> Arc<dyn FolderSource> {
    Arc::new(MemoryFolder::with_files(folder, EXAMPLES))
}

/// Register the plugin's file provider (the plugin's `build` calls this).
pub(crate) fn build(p: &mut pulsar_plugins::PluginBuilder) {
    p.add(engine_fs::FileProvider {
        origin: p.origin(),
        open,
        // Embedded in the plugin; a game serves the packager's snapshot.
        in_game: engine_fs::InGame::Snapshot,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(file: &EmbeddedFile) -> &'static str {
        std::str::from_utf8(file.bytes).unwrap_or_else(|e| panic!("{}: {e}", file.path))
    }

    /// Brackets balance outside strings and comments, a cheap check for the
    /// languages with no parser in this crate's dependencies.
    fn brackets_balance(source: &str, line_comment: &str) -> bool {
        let mut stack = Vec::new();
        for line in source.lines() {
            let line = line.split(line_comment).next().unwrap_or("");
            let mut quote: Option<char> = None;
            for c in line.chars() {
                match quote {
                    Some(q) if c == q => quote = None,
                    Some(_) => {}
                    None => match c {
                        '"' | '\'' | '`' => quote = Some(c),
                        '(' | '[' | '{' => stack.push(c),
                        ')' | ']' | '}' => {
                            let open = stack.pop();
                            if open != Some(match c { ')' => '(', ']' => '[', _ => '{' }) {
                                return false;
                            }
                        }
                        _ => {}
                    },
                }
            }
        }
        stack.is_empty()
    }

    #[test]
    fn every_example_is_valid_for_its_language() {
        for file in EXAMPLES {
            let source = text(file);
            assert!(!source.trim().is_empty(), "{} is empty", file.path);
            match file.path.rsplit('.').next().unwrap() {
                "rs" => {
                    syn::parse_file(source)
                        .unwrap_or_else(|e| panic!("{}: not Rust: {e}", file.path));
                }
                "toml" => {
                    source
                        .parse::<toml::Table>()
                        .unwrap_or_else(|e| panic!("{}: not TOML: {e}", file.path));
                }
                "js" | "ts" => assert!(brackets_balance(source, "//"), "{}", file.path),
                "lua" => assert!(brackets_balance(source, "--"), "{}", file.path),
                "py" => assert!(brackets_balance(source, "#"), "{}", file.path),
                "md" => assert!(source.starts_with('#'), "{}: a title", file.path),
                other => panic!("{}: unexpected extension {other}", file.path),
            }
        }
    }

    #[test]
    fn each_registered_language_has_at_least_two_examples() {
        for ext in ["rs", "js", "ts", "py", "lua", "toml", "md"] {
            let count = EXAMPLES
                .iter()
                .filter(|f| f.path.ends_with(&format!(".{ext}")) && !f.path.ends_with("README.md"))
                .count();
            assert!(count >= 2, "{ext}: {count}");
        }
    }
}
