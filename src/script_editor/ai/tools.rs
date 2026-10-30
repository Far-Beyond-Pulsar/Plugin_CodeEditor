//! The editor's tools for the agent chat panel.
//!
//! The chat runs tools on its own worker thread, but the open documents live
//! in GPUI entities that only the UI thread may touch. Each [`TextEditor`]
//! registers a request channel here under the paths of its open files; a tool
//! sends a [`BufferRequest`], the editor answers it on the UI thread, and the
//! tool waits for the reply.
//!
//! Tools read what the user sees (unsaved edits included) and never write to
//! disk or to the buffer directly: an edit is a *proposal* that lands as
//! Mockaco's inline review, where the user accepts or rejects each hunk.
//!
//! [`TextEditor`]: crate::script_editor::TextEditor

use super::prompt::{region_lines, splice};
use anyhow::{anyhow, bail, Result};
use futures::channel::mpsc::UnboundedSender;
use plugin_editor_api::{AiToolDefinition, PluginError};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;
use tool_registry::{DynamicTool, ToolContext, ToolRegistry};

/// How long a tool waits for the UI thread before giving up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
/// The most lines `code_editor_read` returns at once.
const MAX_READ_LINES: usize = 2000;

/// What a tool asks of an open document.
#[derive(Debug)]
pub(crate) enum BufferOp {
    OpenFiles,
    Read {
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    Selection,
    Diagnostics,
    /// Replace 1-based inclusive lines `start_line..=end_line` (an
    /// `end_line` one short of `start_line` inserts before `start_line`).
    ProposeEdit {
        start_line: usize,
        end_line: usize,
        new_text: String,
    },
    ProposeFile {
        content: String,
    },
    GoTo {
        line: usize,
        column: usize,
    },
}

pub(crate) struct BufferRequest {
    pub path: PathBuf,
    pub op: BufferOp,
    pub reply: mpsc::Sender<Result<Value, String>>,
}

type Senders = HashMap<String, (PathBuf, UnboundedSender<BufferRequest>)>;

fn senders() -> &'static Mutex<Senders> {
    static SENDERS: OnceLock<Mutex<Senders>> = OnceLock::new();
    SENDERS.get_or_init(Default::default)
}

/// Paths compare equal across `\` / `/`, the Windows verbatim prefix and, on
/// Windows, case.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_owned()
    }
}

/// Routes tool requests for `path` to `sender` until [`unregister`].
pub(crate) fn register(path: &Path, sender: UnboundedSender<BufferRequest>) {
    if let Ok(mut map) = senders().lock() {
        map.insert(key(path), (path.to_path_buf(), sender));
    }
}

pub(crate) fn unregister(path: &Path) {
    if let Ok(mut map) = senders().lock() {
        map.remove(&key(path));
    }
}

/// Sends `op` to the editor holding `path` and waits for its answer.
fn call(path: &Path, op: BufferOp) -> Result<Value> {
    let sender = senders()
        .lock()
        .map_err(|_| anyhow!("The editor registry is unavailable."))?
        .get(&key(path))
        .map(|(_, sender)| sender.clone())
        .ok_or_else(|| {
            anyhow!(
                "{} is not open in the code editor. Use code_editor_list_open_files to see what is.",
                path.display()
            )
        })?;
    let (reply, answer) = mpsc::channel();
    sender
        .unbounded_send(BufferRequest {
            path: path.to_path_buf(),
            op,
            reply,
        })
        .map_err(|_| anyhow!("The editor holding {} was closed.", path.display()))?;
    answer
        .recv_timeout(REPLY_TIMEOUT)
        .map_err(|_| anyhow!("The editor did not answer in time."))?
        .map_err(|message| anyhow!(message))
}

/// The file a call is about: its `path` argument, else the file the chat is
/// bound to, else the only open file.
fn target(ctx: &ToolContext, args: &Value) -> Result<PathBuf> {
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = &ctx.current_file {
        if senders()
            .lock()
            .is_ok_and(|map| map.contains_key(&key(path)))
        {
            return Ok(path.clone());
        }
    }
    let map = senders().lock().map_err(|_| anyhow!("The editor registry is unavailable."))?;
    let mut paths = map.values().map(|(path, _)| path);
    match (paths.next(), paths.next()) {
        (Some(only), None) => Ok(only.clone()),
        (None, _) => bail!("No file is open in the code editor."),
        _ => bail!("Several files are open; pass `path` (see code_editor_list_open_files)."),
    }
}

fn line_arg(args: &Value, name: &str) -> Result<Option<usize>> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|&line| line >= 1)
            .map(|line| Some(line as usize))
            .ok_or_else(|| anyhow!("`{name}` must be a line number, counting from 1.")),
    }
}

fn path_schema() -> Value {
    json!({
        "type": "string",
        "description": "File to act on. Defaults to the file the chat is about, or the only open file."
    })
}

fn registry() -> &'static ToolRegistry {
    static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = ToolRegistry::new();
        let mut add = |tool: DynamicTool| registry.register(Arc::new(tool));

        add(DynamicTool::builder("code_editor_list_open_files")
            .description(
                "List the files open in the code editor, with whether each has unsaved edits. \
                 The first step when the user refers to \"the file\" or \"this code\".",
            )
            .category("code_editor")
            .parameters(json!({ "type": "object", "properties": {} }))
            .handler(|_, ctx| {
                let path = {
                    let map = senders()
                        .lock()
                        .map_err(|_| anyhow!("The editor registry is unavailable."))?;
                    ctx.current_file
                        .clone()
                        .filter(|path| map.contains_key(&key(path)))
                        .or_else(|| map.values().next().map(|(path, _)| path.clone()))
                        .ok_or_else(|| anyhow!("No file is open in the code editor."))?
                };
                call(&path, BufferOp::OpenFiles)
            })
            .build());

        add(DynamicTool::builder("code_editor_read")
            .description(
                "Read a file as the user sees it in the editor, unsaved edits included. Lines \
                 are returned numbered from 1. Pass start_line/end_line to read a range; at most \
                 2000 lines come back at once.",
            )
            .category("code_editor")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": path_schema(),
                    "start_line": { "type": "integer", "description": "First line to return (1-based)." },
                    "end_line": { "type": "integer", "description": "Last line to return, inclusive." }
                }
            }))
            .handler(|args, ctx| {
                let path = target(ctx, &args)?;
                call(
                    &path,
                    BufferOp::Read {
                        start_line: line_arg(&args, "start_line")?,
                        end_line: line_arg(&args, "end_line")?,
                    },
                )
            })
            .build());

        add(DynamicTool::builder("code_editor_get_selection")
            .description(
                "The user's current selection and caret in a file: the selected text, its line \
                 range, and the caret position. Use it when the user says \"this\" or \"the selected code\".",
            )
            .category("code_editor")
            .parameters(json!({ "type": "object", "properties": { "path": path_schema() } }))
            .handler(|args, ctx| call(&target(ctx, &args)?, BufferOp::Selection))
            .build());

        add(DynamicTool::builder("code_editor_get_diagnostics")
            .description(
                "The language server's current errors and warnings for a file, with line numbers.",
            )
            .category("code_editor")
            .parameters(json!({ "type": "object", "properties": { "path": path_schema() } }))
            .handler(|args, ctx| call(&target(ctx, &args)?, BufferOp::Diagnostics))
            .build());

        add(DynamicTool::builder("code_editor_propose_edit")
            .description(
                "Propose replacing lines start_line..=end_line of a file with new_text. Nothing \
                 is changed yet: the edit appears in the editor as an inline diff and the user \
                 accepts or rejects each change. Prefer small, targeted edits over rewriting the \
                 whole file. To insert without replacing, pass end_line = start_line - 1. Line \
                 numbers refer to the file as code_editor_read shows it now; after a proposal is \
                 accepted they shift, so re-read before the next edit. Only one proposal can be \
                 pending per file.",
            )
            .category("code_editor")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": path_schema(),
                    "start_line": { "type": "integer", "description": "First line to replace (1-based)." },
                    "end_line": { "type": "integer", "description": "Last line to replace, inclusive." },
                    "new_text": { "type": "string", "description": "The replacement lines, without line numbers." }
                },
                "required": ["start_line", "end_line", "new_text"]
            }))
            .handler(|args, ctx| {
                let path = target(ctx, &args)?;
                let start_line = line_arg(&args, "start_line")?
                    .ok_or_else(|| anyhow!("`start_line` is required."))?;
                let end_line = args
                    .get("end_line")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("`end_line` is required."))?
                    as usize;
                if end_line + 1 < start_line {
                    bail!("`end_line` must be at least start_line - 1.");
                }
                let new_text = args
                    .get("new_text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`new_text` is required."))?
                    .to_owned();
                call(
                    &path,
                    BufferOp::ProposeEdit {
                        start_line,
                        end_line,
                        new_text,
                    },
                )
            })
            .build());

        add(DynamicTool::builder("code_editor_replace_file")
            .description(
                "Propose a new full content for a file. Like code_editor_propose_edit, the user \
                 reviews it as an inline diff. Use only when most of the file changes; otherwise \
                 propose targeted edits.",
            )
            .category("code_editor")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": path_schema(),
                    "content": { "type": "string", "description": "The complete new file." }
                },
                "required": ["content"]
            }))
            .handler(|args, ctx| {
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`content` is required."))?
                    .to_owned();
                call(&target(ctx, &args)?, BufferOp::ProposeFile { content })
            })
            .build());

        add(DynamicTool::builder("code_editor_go_to")
            .description("Scroll the editor to a line and put the caret there.")
            .category("code_editor")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": path_schema(),
                    "line": { "type": "integer", "description": "Line (1-based)." },
                    "column": { "type": "integer", "description": "Column (1-based). Default 1." }
                },
                "required": ["line"]
            }))
            .handler(|args, ctx| {
                let line = line_arg(&args, "line")?.ok_or_else(|| anyhow!("`line` is required."))?;
                let column = line_arg(&args, "column")?.unwrap_or(1);
                call(&target(ctx, &args)?, BufferOp::GoTo { line, column })
            })
            .build());

        registry
    })
}

pub fn ai_tools() -> Vec<AiToolDefinition> {
    registry()
        .definitions()
        .into_iter()
        .map(|def| {
            let tool = AiToolDefinition::new(def.name, def.description, def.parameters_schema);
            match def.category {
                Some(category) => tool.with_category(category),
                None => tool,
            }
        })
        .collect()
}

/// The tools for `file_path`: all of them for the file types the editor opens.
pub fn capabilities_for_file(file_path: &Path) -> Vec<String> {
    let supported = file_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension, "rs" | "js" | "ts" | "py" | "lua" | "toml" | "md")
        });
    if !supported {
        return Vec::new();
    }
    registry().names().into_iter().map(str::to_owned).collect()
}

pub fn execute_ai_tool(
    file_path: &Path,
    tool_name: &str,
    tool_args: Value,
) -> Result<Value, PluginError> {
    let ctx = ToolContext::new().with_current_file(file_path);
    registry()
        .execute(tool_name, tool_args, &ctx)
        .map_err(|error| PluginError::Other {
            message: error.to_string(),
        })
}

// ---- UI-thread side -----------------------------------------------------------


/// Numbered lines `start..=end` (1-based) of `text`.
pub(crate) fn read_lines(text: &str, start: Option<usize>, end: Option<usize>) -> Value {
    let lines: Vec<&str> = text.split('\n').collect();
    // A final newline is not a line of its own.
    let total = if text.ends_with('\n') { lines.len() - 1 } else { lines.len() };
    let start = start.unwrap_or(1).max(1);
    let end = end.unwrap_or(total).min(total);
    let capped = end.min(start.saturating_add(MAX_READ_LINES - 1));
    let body: String = (start..=capped)
        .map(|number| format!("{number:>5}  {}\n", lines[number - 1]))
        .collect();
    json!({
        "total_lines": total,
        "start_line": start,
        "end_line": capped,
        "truncated": capped < end,
        "text": body,
    })
}

/// The whole-file text after replacing 1-based inclusive lines.
pub(crate) fn apply_line_edit(
    text: &str,
    start_line: usize,
    end_line: usize,
    new_text: &str,
) -> Result<String, String> {
    let total = if text.ends_with('\n') {
        text.split('\n').count() - 1
    } else {
        text.split('\n').count()
    };
    if start_line > total + 1 {
        return Err(format!(
            "start_line {start_line} is past the end of the file ({total} lines)."
        ));
    }
    if end_line > total {
        return Err(format!(
            "end_line {end_line} is past the end of the file ({total} lines)."
        ));
    }
    // An insertion is an empty region before `start_line`.
    if end_line + 1 == start_line {
        let region = start_line - 1..start_line - 1;
        let mut insertion = new_text.replace("\r\n", "\n");
        if !insertion.ends_with('\n') && !insertion.is_empty() {
            insertion.push('\n');
        }
        let at = super::prompt::region_bytes(text, &region).start;
        return Ok(format!("{}{insertion}{}", &text[..at], &text[at..]));
    }
    Ok(splice(text, &(start_line - 1..end_line), new_text))
}

/// 1-based `(line, column)` of byte `offset` in `text`; columns count chars.
pub(crate) fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count())
        + 1;
    (line, column)
}

/// The selection as the tool reports it.
pub(crate) fn selection_json(text: &str, selection: std::ops::Range<usize>) -> Value {
    let (start_line, start_column) = line_column(text, selection.start);
    let (end_line, end_column) = line_column(text, selection.end);
    let lines = region_lines(text, selection.clone());
    json!({
        "empty": selection.is_empty(),
        "start": { "line": start_line, "column": start_column },
        "end": { "line": end_line, "column": end_column },
        "lines": { "first": lines.start + 1, "last": lines.end },
        "text": text.get(selection).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_numbered_and_bounded() {
        let result = read_lines("a\nb\nc\n", Some(2), None);
        assert_eq!(result["total_lines"], 3);
        assert_eq!(result["text"], "    2  b\n    3  c\n");
        assert_eq!(result["truncated"], false);
        let long: String = (0..3000).map(|n| format!("{n}\n")).collect();
        let result = read_lines(&long, None, None);
        assert_eq!(result["end_line"], 2000);
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn line_edits_replace_insert_and_reject_bad_ranges() {
        let text = "a\nb\nc\n";
        assert_eq!(apply_line_edit(text, 2, 2, "B").unwrap(), "a\nB\nc\n");
        assert_eq!(apply_line_edit(text, 2, 3, "x\ny\nz").unwrap(), "a\nx\ny\nz\n");
        // Insert before line 2 (end_line = start_line - 1).
        assert_eq!(apply_line_edit(text, 2, 1, "new").unwrap(), "a\nnew\nb\nc\n");
        // Append after the last line.
        assert_eq!(apply_line_edit(text, 4, 3, "d").unwrap(), "a\nb\nc\nd\n");
        // Delete.
        assert_eq!(apply_line_edit(text, 1, 1, "").unwrap(), "\nb\nc\n");
        assert!(apply_line_edit(text, 9, 9, "x").is_err());
        assert!(apply_line_edit(text, 2, 9, "x").is_err());
    }

    #[test]
    fn positions_are_one_based_and_count_characters() {
        let text = "fn é() {}\nlet x = 1;\n";
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, text.find('x').unwrap()), (2, 5));
        let selection = selection_json(text, 11..17);
        assert_eq!(selection["start"]["line"], 2);
        assert_eq!(selection["text"], "let x ");
        assert_eq!(selection["lines"]["first"], 2);
        assert_eq!(selection["lines"]["last"], 2);
    }

    #[test]
    fn paths_match_across_separators_and_case_on_windows() {
        assert_eq!(
            key(Path::new(r"\\?\C:\Proj\src\lib.rs")),
            key(Path::new("C:/Proj/src/lib.rs"))
        );
        if cfg!(windows) {
            assert_eq!(key(Path::new("C:/A/B.rs")), key(Path::new("c:/a/b.rs")));
        }
    }

    #[test]
    fn tools_fail_clearly_without_an_editor() {
        let error = execute_ai_tool(
            Path::new("/nowhere/never_opened.rs"),
            "code_editor_read",
            json!({ "path": "/nowhere/never_opened.rs" }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not open"), "{error}");
        assert!(ai_tools().iter().any(|tool| tool.name == "code_editor_propose_edit"));
        assert!(capabilities_for_file(Path::new("x.rs")).len() >= 7);
        assert!(capabilities_for_file(Path::new("x.png")).is_empty());
    }
}
