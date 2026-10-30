//! What the editor asks the model, and how it reads the answer back.
//!
//! Everything here is plain text in, plain text out, so the contract with the
//! model (the region markers, the reply format, how a reply is spliced into
//! the file) is unit-tested without a window or a provider.

use plugin_editor_api::llm::{LlmMessage, LlmRequest};
use std::ops::Range;

/// What the user wants done with the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistKind {
    /// Rewrite the region as instructed.
    Edit,
    /// Fix the diagnostics (or the problem described) in the region.
    Fix,
    /// Answer a question about the region; changes nothing.
    Ask,
    /// Explain what the region does; changes nothing.
    Explain,
    /// Add documentation comments to the region.
    Document,
    /// Write tests for the region.
    Tests,
}

impl AssistKind {
    pub const ALL: [AssistKind; 6] = [
        AssistKind::Edit,
        AssistKind::Fix,
        AssistKind::Ask,
        AssistKind::Explain,
        AssistKind::Document,
        AssistKind::Tests,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AssistKind::Edit => "Edit",
            AssistKind::Fix => "Fix",
            AssistKind::Ask => "Ask",
            AssistKind::Explain => "Explain",
            AssistKind::Document => "Document",
            AssistKind::Tests => "Tests",
        }
    }

    /// Whether the answer is a rewrite of the region (reviewed as a diff)
    /// rather than prose.
    pub fn edits(self) -> bool {
        !matches!(self, AssistKind::Ask | AssistKind::Explain)
    }

    /// Whether the instruction field may stay empty.
    pub fn instruction_optional(self) -> bool {
        !matches!(self, AssistKind::Edit | AssistKind::Ask)
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            AssistKind::Edit => "Describe the change, e.g. \"use early returns\"",
            AssistKind::Fix => "What is wrong? (optional - diagnostics are included)",
            AssistKind::Ask => "Ask about this code",
            AssistKind::Explain => "Anything specific to explain? (optional)",
            AssistKind::Document => "Documentation style or focus (optional)",
            AssistKind::Tests => "What should the tests cover? (optional)",
        }
    }
}

/// A diagnostic as the model sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticNote {
    /// One-based line.
    pub line: usize,
    pub severity: &'static str,
    pub message: String,
}

/// Lines of the file the request is about, zero-based and end-exclusive.
pub type LineRegion = Range<usize>;

/// The lines `selection` (a byte range) touches. An empty selection is the
/// caret's line. A selection ending at the very start of a line does not
/// include that line.
pub fn region_lines(text: &str, selection: Range<usize>) -> LineRegion {
    let start = selection.start.min(text.len());
    let mut end = selection.end.clamp(start, text.len());
    if end > start && text.as_bytes().get(end - 1) == Some(&b'\n') {
        end -= 1;
    }
    let first = text[..start].matches('\n').count();
    let last = text[..end].matches('\n').count();
    first..last + 1
}

/// The byte range of lines `region` including each line's terminator.
pub fn region_bytes(text: &str, region: &LineRegion) -> Range<usize> {
    let mut starts = std::iter::once(0).chain(text.match_indices('\n').map(|(index, _)| index + 1));
    let start = starts.clone().nth(region.start).unwrap_or(text.len());
    let end = starts.nth(region.end).unwrap_or(text.len());
    start..end
}

/// `text` with lines `region` replaced by `replacement`. The replacement's
/// own line ending is normalised: the region's trailing newline (or lack of
/// one at the end of the file) is kept.
pub fn splice(text: &str, region: &LineRegion, replacement: &str) -> String {
    let bytes = region_bytes(text, region);
    let had_newline = text[bytes.clone()].ends_with('\n');
    let mut replacement = replacement.replace("\r\n", "\n");
    while replacement.ends_with('\n') {
        replacement.pop();
    }
    if had_newline {
        replacement.push('\n');
    }
    let mut out = String::with_capacity(text.len() + replacement.len());
    out.push_str(&text[..bytes.start]);
    out.push_str(&replacement);
    out.push_str(&text[bytes.end..]);
    out
}

/// Everything the prompt is built from.
#[derive(Debug, Clone)]
pub struct AssistContext {
    pub path: String,
    pub language: &'static str,
    pub text: String,
    pub region: LineRegion,
    pub diagnostics: Vec<DiagnosticNote>,
}

/// How much of the file around the region goes into a prompt.
const CONTEXT_CHARS: usize = 12_000;

fn lines_text(lines: &[&str], range: Range<usize>) -> String {
    lines[range.start.min(lines.len())..range.end.min(lines.len())].join("\n")
}

/// The region and as much of its surroundings as fits.
fn excerpt(context: &AssistContext) -> (String, String, String) {
    let mut lines: Vec<&str> = context.text.split('\n').collect();
    // A final newline ends the last line; it does not start another.
    if context.text.ends_with('\n') {
        lines.pop();
    }
    let region = context.region.start.min(lines.len())..context.region.end.min(lines.len());
    let selected = lines_text(&lines, region.clone());
    let side = CONTEXT_CHARS.saturating_sub(selected.len()) / 2;
    let mut budget = side;
    let mut before = region.start;
    while before > 0 && budget >= lines[before - 1].len() + 1 {
        before -= 1;
        budget -= lines[before].len() + 1;
    }
    let mut budget = side;
    let mut after = region.end;
    while after < lines.len() && budget >= lines[after].len() + 1 {
        budget -= lines[after].len() + 1;
        after += 1;
    }
    (
        lines_text(&lines, before..region.start),
        selected,
        lines_text(&lines, region.end..after),
    )
}

const EDIT_SYSTEM: &str = "You are the code-editing engine inside a game engine's code editor. \
You are given a file excerpt with one marked region and an instruction. \
Reply with the complete new text of the marked region only -- exactly as it should appear in the file, \
keeping the file's indentation and style -- inside a single fenced code block. \
Do not repeat the surrounding code. Do not add commentary before or after the block. \
If the region needs no change, reply with the region unchanged.";

const ANSWER_SYSTEM: &str = "You are a senior engineer answering questions about code in a game engine's \
code editor. Be direct and specific; refer to line numbers when it helps. Use short markdown, with \
fenced code blocks only for code worth copying. Do not restate the question.";

fn describe_diagnostics(notes: &[DiagnosticNote]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nDiagnostics reported by the language server:\n");
    for note in notes.iter().take(20) {
        out.push_str(&format!("- line {} ({}): {}\n", note.line, note.severity, note.message));
    }
    out
}

fn default_instruction(kind: AssistKind) -> &'static str {
    match kind {
        AssistKind::Edit => "Improve this code.",
        AssistKind::Fix => "Fix the problems in this code.",
        AssistKind::Ask => "Explain this code.",
        AssistKind::Explain => "Explain what this code does, how, and anything surprising in it.",
        AssistKind::Document => {
            "Add documentation comments in the language's idiomatic style to every item and any \
             non-obvious logic. Do not change behaviour."
        }
        AssistKind::Tests => {
            "Write unit tests for this code using the language's idiomatic test framework. Reply \
             with the region unchanged followed by the new tests."
        }
    }
}

/// The request for `kind` over `context`; `instruction` is the user's own
/// words and may be empty.
pub fn build_request(kind: AssistKind, instruction: &str, context: &AssistContext) -> LlmRequest {
    let (before, selected, after) = excerpt(context);
    let instruction = match instruction.trim() {
        "" => default_instruction(kind).to_owned(),
        text if matches!(kind, AssistKind::Document | AssistKind::Tests | AssistKind::Fix) => {
            format!("{} Additional guidance: {text}", default_instruction(kind))
        }
        text => text.to_owned(),
    };
    let region_first = context.region.start + 1;
    let region_last = context.region.end.max(region_first);
    let mut body = format!(
        "File: {} ({})\nTotal lines: {}\n\n",
        context.path,
        context.language,
        context.text.split('\n').count()
    );
    if !before.is_empty() {
        body.push_str("<before_region>\n");
        body.push_str(&before);
        body.push_str("\n</before_region>\n");
    }
    body.push_str(&format!("<region lines=\"{region_first}-{region_last}\">\n"));
    body.push_str(&selected);
    body.push_str("\n</region>\n");
    if !after.is_empty() {
        body.push_str("<after_region>\n");
        body.push_str(&after);
        body.push_str("\n</after_region>\n");
    }
    body.push_str(&describe_diagnostics(&context.diagnostics));
    body.push_str(&format!("\nInstruction: {instruction}\n"));
    LlmRequest {
        system: if kind.edits() { EDIT_SYSTEM } else { ANSWER_SYSTEM }.to_owned(),
        messages: vec![LlmMessage::user(body)],
        temperature: Some(if kind.edits() { 0.1 } else { 0.3 }),
        max_tokens: Some(if kind.edits() { 8192 } else { 4096 }),
    }
}

/// The code in a model reply: the first fenced block, or the whole reply when
/// it has none. `None` when the reply is empty.
pub fn extract_code(reply: &str) -> Option<String> {
    let reply = reply.trim_matches(|c| c == '\r' || c == '\n');
    let code = match reply.find("```") {
        Some(open) => {
            let after = &reply[open + 3..];
            // The rest of the opening line is the language tag.
            let body = after.split_once('\n').map_or("", |(_, body)| body);
            match body.rfind("\n```") {
                Some(close) => &body[..close],
                None => body.trim_end_matches("```").trim_end_matches('\n'),
            }
        }
        None => reply,
    };
    (!code.trim().is_empty()).then(|| code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "fn a() {}\nfn b() {\n    1\n}\nfn c() {}\n";

    #[test]
    fn regions_cover_whole_lines() {
        // Caret inside line 1.
        assert_eq!(region_lines(FILE, 13..13), 1..2);
        // Selection across lines 1..=2.
        assert_eq!(region_lines(FILE, 13..22), 1..3);
        // Selection ending at the start of the next line excludes it.
        let end_of_b = FILE.find("fn c").unwrap();
        assert_eq!(region_lines(FILE, 10..end_of_b), 1..4);
        // Past the end clamps.
        assert_eq!(region_lines(FILE, 999..999), 5..6);
    }

    #[test]
    fn region_bytes_include_terminators() {
        assert_eq!(&FILE[region_bytes(FILE, &(1..2))], "fn b() {\n");
        assert_eq!(&FILE[region_bytes(FILE, &(4..5))], "fn c() {}\n");
        assert_eq!(&FILE[region_bytes(FILE, &(1..4))], "fn b() {\n    1\n}\n");
        assert_eq!(region_bytes("a\nb", &(1..2)), 2..3);
    }

    #[test]
    fn splice_keeps_the_region_terminator() {
        assert_eq!(
            splice(FILE, &(1..4), "fn b() -> i32 {\n    2\n}"),
            "fn a() {}\nfn b() -> i32 {\n    2\n}\nfn c() {}\n"
        );
        // A reply with trailing blank lines does not grow the file.
        assert_eq!(splice("a\nb\n", &(0..1), "x\n\n"), "x\nb\n");
        // The last line has no newline to keep.
        assert_eq!(splice("a\nb", &(1..2), "z"), "a\nz");
        // Deleting a region.
        assert_eq!(splice("a\nb\nc\n", &(1..2), ""), "a\n\nc\n");
    }

    #[test]
    fn replies_yield_their_code() {
        assert_eq!(extract_code("```rust\nfn x() {}\n```").as_deref(), Some("fn x() {}"));
        assert_eq!(
            extract_code("Here you go:\n```\na\nb\n```\nDone.").as_deref(),
            Some("a\nb")
        );
        assert_eq!(extract_code("plain\ntext").as_deref(), Some("plain\ntext"));
        // An unterminated fence (a cut-off reply) still gives the code so far.
        assert_eq!(extract_code("```rust\nfn x() {}").as_deref(), Some("fn x() {}"));
        assert_eq!(extract_code("   \n"), None);
        assert_eq!(extract_code("```\n```"), None);
    }

    fn context() -> AssistContext {
        AssistContext {
            path: "src/lib.rs".into(),
            language: "rust",
            text: FILE.into(),
            region: 1..4,
            diagnostics: vec![DiagnosticNote {
                line: 3,
                severity: "error",
                message: "mismatched types".into(),
            }],
        }
    }

    #[test]
    fn edit_requests_mark_the_region_and_ask_for_one_block() {
        let request = build_request(AssistKind::Edit, "return an i32", &context());
        assert!(request.system.contains("single fenced code block"));
        let body = &request.messages[0].content;
        assert!(body.contains("<region lines=\"2-4\">\nfn b() {\n    1\n}\n</region>"));
        assert!(body.contains("<before_region>\nfn a() {}\n</before_region>"));
        assert!(body.contains("<after_region>\nfn c() {}\n</after_region>"));
        assert!(body.contains("- line 3 (error): mismatched types"));
        assert!(body.ends_with("Instruction: return an i32\n"));
    }

    #[test]
    fn empty_instructions_fall_back_and_extras_are_appended() {
        let fix = build_request(AssistKind::Fix, "", &context());
        assert!(fix.messages[0].content.contains("Instruction: Fix the problems"));
        let tests = build_request(AssistKind::Tests, "cover overflow", &context());
        assert!(tests.messages[0]
            .content
            .contains("Additional guidance: cover overflow"));
        let explain = build_request(AssistKind::Explain, "", &context());
        assert!(!explain.system.contains("single fenced code block"));
    }

    #[test]
    fn long_files_are_trimmed_around_the_region() {
        let text: String = (0..5000).map(|n| format!("line {n}\n")).collect();
        let context = AssistContext {
            path: "big.rs".into(),
            language: "rust",
            text,
            region: 2500..2502,
            diagnostics: Vec::new(),
        };
        let request = build_request(AssistKind::Edit, "x", &context);
        let body = &request.messages[0].content;
        assert!(body.len() < CONTEXT_CHARS * 2, "{}", body.len());
        assert!(body.contains("line 2500") && body.contains("line 2499"));
        assert!(!body.contains("line 0\n"));
    }
}
