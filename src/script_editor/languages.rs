//! Central language registry.
//!
//! Single source of truth mapping file extensions to editor language ids
//! (as understood by `InputState::code_editor`), display names, and comment
//! syntax. Previously this mapping was duplicated inconsistently across
//! several match statements in `text_editor.rs`.

use std::path::Path;

/// Static metadata for one supported language.
pub struct LanguageSpec {
    /// Language id passed to `InputState::code_editor(...)`.
    pub id: &'static str,
    /// Human-readable name for status bars / menus.
    pub display_name: &'static str,
    /// Line comment token, if the language has one.
    pub comment_line: Option<&'static str>,
    /// Block comment (open, close) tokens, if the language has them.
    pub comment_block: Option<(&'static str, &'static str)>,
}

/// Fallback for unknown extensions: editable, no highlighting.
pub const PLAINTEXT: &LanguageSpec = &LanguageSpec {
    id: "text",
    display_name: "Plain Text",
    comment_line: None,
    comment_block: None,
};

/// Files above this many lines render without syntax highlighting.
pub const HIGHLIGHT_DISABLE_LINES: usize = 50_000;
/// Files above this many lines do not soft-wrap.
pub const SOFT_WRAP_DISABLE_LINES: usize = 5_000;
/// Files above this many bytes do not soft-wrap.
pub const SOFT_WRAP_DISABLE_BYTES: usize = 500_000;

const LANGUAGES: &[&LanguageSpec] = &[
    &LanguageSpec { id: "rust", display_name: "Rust", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "javascript", display_name: "JavaScript", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "typescript", display_name: "TypeScript", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "python", display_name: "Python", comment_line: Some("#"), comment_block: None },
    &LanguageSpec { id: "lua", display_name: "Lua", comment_line: Some("--"), comment_block: Some(("--[[", "]]")) },
    &LanguageSpec { id: "toml", display_name: "TOML", comment_line: Some("#"), comment_block: None },
    &LanguageSpec { id: "json", display_name: "JSON", comment_line: None, comment_block: None },
    &LanguageSpec { id: "markdown", display_name: "Markdown", comment_line: None, comment_block: Some(("<!--", "-->")) },
    &LanguageSpec { id: "html", display_name: "HTML", comment_line: None, comment_block: Some(("<!--", "-->")) },
    &LanguageSpec { id: "css", display_name: "CSS", comment_line: None, comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "go", display_name: "Go", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "ruby", display_name: "Ruby", comment_line: Some("#"), comment_block: None },
    &LanguageSpec { id: "sql", display_name: "SQL", comment_line: Some("--"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "yaml", display_name: "YAML", comment_line: Some("#"), comment_block: None },
    &LanguageSpec { id: "xml", display_name: "XML", comment_line: None, comment_block: Some(("<!--", "-->")) },
    &LanguageSpec { id: "c", display_name: "C", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "cpp", display_name: "C++", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "java", display_name: "Java", comment_line: Some("//"), comment_block: Some(("/*", "*/")) },
    &LanguageSpec { id: "bash", display_name: "Shell", comment_line: Some("#"), comment_block: None },
];

/// Look up a language by file extension (without the leading dot).
pub fn language_for_extension(ext: &str) -> Option<&'static LanguageSpec> {
    let ext = ext.to_ascii_lowercase();
    let matched = match ext.as_str() {
        "rs" => "rust",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "py" => "python",
        "lua" => "lua",
        "toml" => "toml",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "go" => "go",
        "rb" => "ruby",
        "sql" => "sql",
        "yaml" | "yml" => "yaml",
        "xml" => "xml",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "java" => "java",
        "sh" | "bash" => "bash",
        _ => return None,
    };
    LANGUAGES.iter().copied().find(|spec| spec.id == matched)
}

/// Look up a language by path, falling back to [`PLAINTEXT`].
pub fn language_for_path(path: &Path) -> &'static LanguageSpec {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(language_for_extension)
        .unwrap_or(PLAINTEXT)
}

/// Whether syntax highlighting should be enabled for a file of this size.
pub fn highlighting_enabled(lines_count: usize) -> bool {
    lines_count <= HIGHLIGHT_DISABLE_LINES
}

/// Whether soft wrap should be enabled for a file of this size.
pub fn should_soft_wrap(lines_count: usize, size_bytes: usize) -> bool {
    lines_count < SOFT_WRAP_DISABLE_LINES && size_bytes < SOFT_WRAP_DISABLE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_extensions() {
        assert_eq!(language_for_extension("rs").unwrap().id, "rust");
        assert_eq!(language_for_extension("yml").unwrap().id, "yaml");
        assert_eq!(language_for_extension("TS").unwrap().id, "typescript");
        assert_eq!(language_for_extension("hpp").unwrap().id, "cpp");
        assert_eq!(language_for_extension("sh").unwrap().id, "bash");
        assert!(language_for_extension("xyzzy").is_none());
    }

    #[test]
    fn falls_back_to_plaintext() {
        assert_eq!(language_for_path(Path::new("/tmp/mystery.bin")).id, "text");
        assert_eq!(
            language_for_path(Path::new("thing.TS")).id,
            "typescript"
        );
    }

    #[test]
    fn size_guards_match_documented_thresholds() {
        assert!(highlighting_enabled(HIGHLIGHT_DISABLE_LINES));
        assert!(!highlighting_enabled(HIGHLIGHT_DISABLE_LINES + 1));
        assert!(should_soft_wrap(SOFT_WRAP_DISABLE_LINES - 1, SOFT_WRAP_DISABLE_BYTES - 1));
        assert!(!should_soft_wrap(SOFT_WRAP_DISABLE_LINES, 0));
        assert!(!should_soft_wrap(0, SOFT_WRAP_DISABLE_BYTES));
    }
}
