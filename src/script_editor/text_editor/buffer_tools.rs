//! The UI-thread half of the chat panel's code-editor tools: requests arrive
//! from the tool thread (see `ai::tools`) and are answered against the open
//! documents here.

use super::{TextEditor, TextEditorEvent};
use crate::script_editor::ai::tools::{
    apply_line_edit, line_column, read_lines, selection_json, BufferOp, BufferRequest,
};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt as _;
use gpui::*;
use mockaco_core::DiagnosticSeverity;
use serde_json::{json, Value};

impl TextEditor {
    /// Starts the loop that answers tool requests on the UI thread.
    pub(super) fn serve_ai_requests(
        &mut self,
        mut receiver: UnboundedReceiver<BufferRequest>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(request) = receiver.next().await {
                let alive = this
                    .update(cx, |this, cx| {
                        let reply = request.reply.clone();
                        let answer = this.answer_buffer_request(request, cx);
                        reply.send(answer).ok();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        // Tools stop finding this editor's files when it goes away.
        cx.on_release(|this, _| {
            for file in &this.open_files {
                crate::script_editor::ai::tools::unregister(&file.path);
            }
        })
        .detach();
    }

    fn answer_buffer_request(
        &mut self,
        request: BufferRequest,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let BufferRequest { path, op, .. } = request;
        if let BufferOp::OpenFiles = op {
            let active = self.current_file().map(|file| file.path.clone());
            let files: Vec<Value> = self
                .open_files
                .iter()
                .map(|file| {
                    json!({
                        "path": file.path.display().to_string(),
                        "unsaved_changes": file.is_modified,
                        "active": active.as_ref() == Some(&file.path),
                        "reviewing_a_proposal": file.view.read(cx).is_reviewing(),
                    })
                })
                .collect();
            return Ok(json!({ "files": files }));
        }
        let view = self
            .view_for(&path)
            .ok_or_else(|| format!("{} is not open.", path.display()))?;
        match op {
            BufferOp::OpenFiles => unreachable!("answered above"),
            BufferOp::Read {
                start_line,
                end_line,
            } => Ok(read_lines(&view.read(cx).text(), start_line, end_line)),
            BufferOp::Selection => {
                let view = view.read(cx);
                Ok(selection_json(&view.text(), view.selection_range()))
            }
            BufferOp::Diagnostics => {
                let view = view.read(cx);
                let text = view.text();
                let items: Vec<Value> = view
                    .surface
                    .lsp_diagnostics()
                    .iter()
                    .map(|diagnostic| {
                        let (line, column) = line_column(&text, diagnostic.range.start);
                        json!({
                            "severity": match diagnostic.severity {
                                DiagnosticSeverity::Error => "error",
                                DiagnosticSeverity::Warning => "warning",
                                DiagnosticSeverity::Info => "info",
                                DiagnosticSeverity::Hint => "hint",
                            },
                            "line": line,
                            "column": column,
                            "message": diagnostic.message,
                            "source": diagnostic.source,
                        })
                    })
                    .collect();
                Ok(json!({ "count": items.len(), "diagnostics": items }))
            }
            BufferOp::ProposeEdit {
                start_line,
                end_line,
                new_text,
            } => {
                let text = view.read(cx).text();
                let proposed = apply_line_edit(&text, start_line, end_line, &new_text)?;
                self.propose(&path, &view, &proposed, cx)
            }
            BufferOp::ProposeFile { content } => self.propose(&path, &view, &content, cx),
            BufferOp::GoTo { line, column } => {
                view.update(cx, |view, cx| {
                    view.go_to(line.saturating_sub(1), column.saturating_sub(1), cx)
                });
                self.reveal(&path, line, column, cx);
                Ok(json!({ "ok": true }))
            }
        }
    }

    /// Opens `proposed` as an inline review in the document's view and brings
    /// the document to the front.
    fn propose(
        &mut self,
        path: &std::path::Path,
        view: &Entity<mockaco_gpui::native::WgpuiEditorView>,
        proposed: &str,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        if view.read(cx).is_reviewing() {
            return Err(
                "A proposal is already waiting for the user's review in this file. Wait for \
                 them to accept or reject it, then re-read the file."
                    .into(),
            );
        }
        let hunks = view.update(cx, |view, cx| view.begin_review(proposed, cx));
        if hunks == 0 {
            return Ok(json!({ "hunks": 0, "status": "no_change" }));
        }
        let first_line = view.read(cx).block_line(mockaco_gpui::native::REVIEW_BLOCK_BASE).unwrap_or(0) + 1;
        self.reveal(path, first_line, 1, cx);
        Ok(json!({
            "hunks": hunks,
            "status": "awaiting_user_review",
            "note": "The change is shown inline; the user accepts or rejects each hunk. \
                     Nothing is saved until they do.",
        }))
    }

    /// Asks the panel that owns this editor to show `path` at a line.
    fn reveal(&mut self, path: &std::path::Path, line: usize, column: usize, cx: &mut Context<Self>) {
        cx.emit(TextEditorEvent::NavigateToLocation {
            path: path.to_path_buf(),
            line: line.saturating_sub(1) as u32,
            character: column.saturating_sub(1) as u32,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_editor::ai::tools::execute_ai_tool;
    // `super::*` brings in `gpui::*`, whose `test` attribute would shadow the
    // built-in one that `#[gpui::test]` expands to, and recurse.
    use core::prelude::v1::test;
    use std::path::{Path, PathBuf};

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    const SOURCE: &str = "fn a() {}\nfn b() {}\nfn c() {}\n";

    fn open_editor<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
    ) -> (Entity<TextEditor>, PathBuf, &'a mut VisualTestContext) {
        cx.update(|cx| ui::init(cx));
        let dir = std::env::temp_dir().join(format!("pulsar_ai_tools_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.rs");
        std::fs::write(&path, SOURCE).unwrap();
        let (_host, vcx) = cx.add_window_view(|_, _| Host);
        let editor = vcx.update(|window, app| {
            let editor = app.new(|cx| TextEditor::new_with_settings(window, cx, Default::default()));
            editor.update(app, |editor, cx| editor.open_file(path.clone(), window, cx));
            editor
        });
        vcx.run_until_parked();
        (editor, path, vcx)
    }

    /// Runs a tool the way the chat does -- from another thread -- while the
    /// test pumps the UI thread that has to answer it.
    fn run_tool(
        cx: &mut VisualTestContext,
        path: &Path,
        tool: &'static str,
        args: Value,
    ) -> Result<Value, String> {
        let path = path.to_path_buf();
        let worker = std::thread::spawn(move || {
            execute_ai_tool(&path, tool, args).map_err(|error| error.to_string())
        });
        while !worker.is_finished() {
            cx.run_until_parked();
            std::thread::yield_now();
        }
        worker.join().expect("tool thread")
    }

    #[gpui::test]
    fn tools_read_the_live_buffer_and_selection(cx: &mut TestAppContext) {
        let (_editor, path, cx) = open_editor(cx, "read");
        let read = run_tool(cx, &path, "code_editor_read", json!({ "start_line": 2, "end_line": 2 }))
            .unwrap();
        assert_eq!(read["text"], "    2  fn b() {}\n");
        let selection = run_tool(cx, &path, "code_editor_get_selection", json!({})).unwrap();
        assert_eq!(selection["empty"], true);
        let files = run_tool(cx, &path, "code_editor_list_open_files", json!({})).unwrap();
        assert_eq!(files["files"].as_array().unwrap().len(), 1);
        assert_eq!(files["files"][0]["unsaved_changes"], false);
    }

    #[gpui::test]
    fn a_proposed_edit_opens_a_review_and_nothing_is_written(cx: &mut TestAppContext) {
        let (editor, path, cx) = open_editor(cx, "propose");
        let reply = run_tool(
            cx,
            &path,
            "code_editor_propose_edit",
            json!({ "start_line": 2, "end_line": 2, "new_text": "fn b() -> u8 { 1 }" }),
        )
        .unwrap();
        assert_eq!(reply["status"], "awaiting_user_review");
        let view = editor.read_with(cx, |editor, _| editor.view_for(&path).unwrap());
        assert!(view.read_with(cx, |view, _| view.is_reviewing()));
        assert_eq!(
            view.read_with(cx, |view, _| view.text()),
            "fn a() {}\nfn b() -> u8 { 1 }\nfn c() {}\n"
        );
        // The file on disk is untouched until the user decides and saves.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SOURCE);

        // One pending proposal per file.
        let again = run_tool(
            cx,
            &path,
            "code_editor_propose_edit",
            json!({ "start_line": 1, "end_line": 1, "new_text": "x" }),
        );
        assert!(again.unwrap_err().contains("already waiting"));

        // Rejecting restores the original.
        view.update(cx, |view, cx| view.reject_all_review(cx));
        assert_eq!(view.read_with(cx, |view, _| view.text()), SOURCE);
    }

    #[gpui::test]
    fn bad_proposals_are_refused_without_touching_the_buffer(cx: &mut TestAppContext) {
        let (editor, path, cx) = open_editor(cx, "bad");
        let error = run_tool(
            cx,
            &path,
            "code_editor_propose_edit",
            json!({ "start_line": 2, "end_line": 40, "new_text": "x" }),
        )
        .unwrap_err();
        assert!(error.contains("past the end"), "{error}");
        let view = editor.read_with(cx, |editor, _| editor.view_for(&path).unwrap());
        assert!(!view.read_with(cx, |view, _| view.is_reviewing()));
        assert_eq!(view.read_with(cx, |view, _| view.text()), SOURCE);
    }
}
