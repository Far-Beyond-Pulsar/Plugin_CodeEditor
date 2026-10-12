//! Language-server wiring for the Mockaco editor host.
//!
//! The engine runs one rust-analyzer for the whole project. This editor never
//! starts, restarts or configures it: it attaches to the shared
//! [`RustAnalyzerManager`], keeps the server's copy of each open document in
//! step, and answers the editor view's questions (hover, completion,
//! definition) through the manager's request channel.
//!
//! * Document sync is incremental: Mockaco logs each edit as an LSP content
//!   change and [`TextEditor::lsp_sync`] forwards exactly that, so a keystroke
//!   costs a few bytes whatever the file size. Only undo/redo and wholesale
//!   replacement fall back to sending the full text.
//! * Diagnostics arrive per file ([`AnalyzerEvent::FileDiagnostics`]) and go
//!   straight to the matching view, which draws and tracks them.
//! * Requests are answered on the foreground executor as they complete; every
//!   answer names the request and document version it was asked against, and
//!   the view drops it when the user has moved on.

use super::{OpenFile, TextEditor, TextEditorEvent};
use engine_backend::services::rust_analyzer_manager::{
    path_key, path_to_uri, uri_to_path, AnalyzerEvent, AnalyzerStatus, RustAnalyzerManager,
};
use gpui::*;
use mockaco_core::{DiagnosticSeverity, TextPosition};
use mockaco_gpui::intel::{CompletionEntry, CompletionKind, DiagnosticInput, HoverContent, PositionEdit};
use mockaco_gpui::native::{EditorEvent, LspRequest, WgpuiEditorView};
use mockaco_gpui::{PendingSync, TextChange};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The editor's attachment to the shared analyzer.
pub(super) struct LspClient {
    analyzer: Entity<RustAnalyzerManager>,
    /// The raw items of the most recent completion response. A menu entry's
    /// `host_data` is its index + 1, so resolving needs no lookup table.
    completions: Option<CompletionStore>,
    /// `additionalTextEdits` already fetched while the user browsed the menu.
    resolved_edits: HashMap<u64, Vec<PositionEdit>>,
    _events: Subscription,
    _release: Subscription,
}

struct CompletionStore {
    request: u64,
    items: Arc<Vec<Value>>,
}

/// Fallback for the characters rust-analyzer registers, used until it has told us.
const SERVER_TRIGGER_CHARACTERS: [char; 4] = [':', '.', '\x27', '('];

/// Whether the language server handles `path`.
pub(super) fn is_rust(path: &PathBuf) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("rs")
}

impl TextEditor {
    /// Attaches this editor to the engine's shared analyzer. Documents that
    /// are already open are announced as soon as the server can take them.
    pub fn set_rust_analyzer(
        &mut self,
        analyzer: Entity<RustAnalyzerManager>,
        cx: &mut Context<Self>,
    ) {
        let events = cx.subscribe(
            &analyzer,
            |this: &mut Self, _manager, event: &AnalyzerEvent, cx| {
                this.on_analyzer_event(event, cx);
            },
        );
        let release = cx.on_release(|this: &mut Self, cx: &mut App| this.lsp_release(cx));
        self.lsp = Some(LspClient {
            analyzer,
            completions: None,
            resolved_edits: HashMap::new(),
            _events: events,
            _release: release,
        });
        let views: Vec<_> = self
            .open_files
            .iter()
            .filter(|file| is_rust(&file.path))
            .map(|file| file.view.clone())
            .collect();
        for view in views {
            view.update(cx, |view, cx| view.set_language_intelligence(true, cx));
        }
        self.lsp_open_pending(cx);
    }

    /// One line saying why language features are or are not available.
    pub fn lsp_status(&self, cx: &App) -> String {
        let Some(client) = &self.lsp else {
            return "language server: not attached to the engine analyzer".to_owned();
        };
        let state = match client.analyzer.read(cx).status() {
            AnalyzerStatus::Idle => "idle".to_owned(),
            AnalyzerStatus::Starting => "starting".to_owned(),
            AnalyzerStatus::Indexing { progress, .. } => {
                format!("indexing {:.0}%", progress * 100.0)
            }
            AnalyzerStatus::Ready => "ready".to_owned(),
            AnalyzerStatus::Error(error) => format!("error: {error}"),
            AnalyzerStatus::Stopped => "stopped".to_owned(),
        };
        let document = match self.current_file() {
            Some(file) if !is_rust(&file.path) => "not a Rust file",
            Some(file) if file.lsp_open => "synced",
            Some(_) => "not synced yet",
            None => "none",
        };
        format!("rust-analyzer: {state}, document {document}")
    }

    fn analyzer(&self) -> Option<Entity<RustAnalyzerManager>> {
        self.lsp.as_ref().map(|client| client.analyzer.clone())
    }

    fn on_analyzer_event(&mut self, event: &AnalyzerEvent, cx: &mut Context<Self>) {
        match event {
            AnalyzerEvent::StatusChanged(status) => match status {
                AnalyzerStatus::Starting
                | AnalyzerStatus::Stopped
                | AnalyzerStatus::Idle
                | AnalyzerStatus::Error(_) => self.lsp_forget_documents(),
                _ => self.lsp_open_pending(cx),
            },
            AnalyzerEvent::Error(_) => self.lsp_forget_documents(),
            AnalyzerEvent::IndexingProgress { .. } | AnalyzerEvent::Ready => {
                self.lsp_open_pending(cx)
            }
            AnalyzerEvent::FileDiagnostics { key, diagnostics } => {
                if let Some(index) = self
                    .open_files
                    .iter()
                    .position(|file| file.lsp_open && path_key(&file.path) == *key)
                {
                    self.lsp_apply_diagnostics(index, diagnostics, cx);
                }
            }
            AnalyzerEvent::Diagnostics(_) => {}
        }
    }

    /// The server restarted or stopped: it no longer has our documents, so
    /// they are announced again when it is back.
    fn lsp_forget_documents(&mut self) {
        for file in &mut self.open_files {
            file.lsp_open = false;
        }
        if let Some(client) = &mut self.lsp {
            client.completions = None;
            client.resolved_edits.clear();
        }
    }

    /// Sends `didOpen` for every Rust document the server does not have yet,
    /// once the server can accept documents.
    pub(super) fn lsp_open_pending(&mut self, cx: &mut Context<Self>) {
        let Some(analyzer) = self.analyzer() else {
            return;
        };
        if !analyzer.read(cx).is_initialized() {
            return;
        }
        // Offer completion on whatever the server registered, as VS Code and Zed do.
        let triggers = analyzer.read(cx).completion_trigger_characters();
        if !triggers.is_empty() {
            for file in self.open_files.iter().filter(|file| is_rust(&file.path)) {
                let triggers = triggers.clone();
                file.view
                    .update(cx, |view, _| view.set_completion_triggers(triggers));
            }
        }
        for index in 0..self.open_files.len() {
            let file = &self.open_files[index];
            if file.lsp_open || !is_rust(&file.path) {
                continue;
            }
            let path = file.path.clone();
            let view = file.view.clone();
            let text = view.read(cx).text();
            let sent = analyzer.read(cx).did_open_file(&path, &text, "rust");
            match sent {
                Ok(()) => {
                    self.open_files[index].lsp_open = true;
                    // The server now has the current text; earlier edits
                    // are already inside it.
                    view.update(cx, |view, _| {
                        let _ = view.take_pending_sync();
                    });
                    let cached = analyzer.read(cx).diagnostics_for(&path);
                    if !cached.is_empty() {
                        self.lsp_apply_diagnostics(index, &cached, cx);
                    }
                }
                Err(error) => {
                    tracing::debug!("rust-analyzer not ready for {:?}: {}", path.file_name(), error)
                }
            }
        }
    }

    /// Forwards the edits made since the last call.
    pub(super) fn lsp_sync(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(analyzer) = self.analyzer() else {
            return;
        };
        let Some(file) = self.open_files.get(index) else {
            return;
        };
        if !file.lsp_open {
            return;
        }
        let (path, version, view) = (file.path.clone(), file.version, file.view.clone());
        let result = match view.update(cx, |view, _| view.take_pending_sync()) {
            PendingSync::None => return,
            PendingSync::Incremental(changes) => analyzer.read(cx).did_change_file_incremental(
                &path,
                version,
                changes.iter().map(content_change).collect(),
            ),
            PendingSync::Full => {
                let text = view.read(cx).text();
                analyzer.read(cx).did_change_file(&path, &text, version)
            }
        };
        if let Err(error) = result {
            tracing::debug!("didChange failed for {:?}: {}", path.file_name(), error);
        }
    }

    pub(super) fn lsp_saved(&mut self, index: usize, content: &str, cx: &mut Context<Self>) {
        let Some(analyzer) = self.analyzer() else {
            return;
        };
        let Some(file) = self.open_files.get(index) else {
            return;
        };
        if file.lsp_open {
            if let Err(error) = analyzer.read(cx).did_save_file(&file.path, content) {
                tracing::debug!("didSave failed for {:?}: {}", file.path.file_name(), error);
            }
        }
    }

    /// `file` has just been removed from the editor.
    pub(super) fn lsp_close(&mut self, file: &OpenFile, cx: &mut Context<Self>) {
        let Some(analyzer) = self.analyzer() else {
            return;
        };
        if file.lsp_open {
            let _ = analyzer.read(cx).did_close_file(&file.path);
        }
    }

    /// The panel is going away: release every document it held open.
    fn lsp_release(&mut self, cx: &mut App) {
        let Some(client) = &self.lsp else {
            return;
        };
        let analyzer = client.analyzer.read(cx);
        for file in &self.open_files {
            if file.lsp_open {
                let _ = analyzer.did_close_file(&file.path);
            }
        }
    }

    fn lsp_apply_diagnostics(
        &mut self,
        index: usize,
        diagnostics: &[ui::diagnostics::Diagnostic],
        cx: &mut Context<Self>,
    ) {
        let inputs: Vec<DiagnosticInput> = diagnostics.iter().map(diagnostic_input).collect();
        if let Some(file) = self.open_files.get(index) {
            file.view
                .update(cx, |view, cx| view.set_diagnostics(inputs, cx));
        }
    }

    // ---- requests from the view ------------------------------------------

    pub(super) fn on_lsp_event(&mut self, index: usize, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::HoverRequested(request) => self.lsp_hover(index, *request, cx),
            EditorEvent::CompletionRequested { request, trigger } => {
                self.lsp_completion(index, *request, *trigger, cx)
            }
            EditorEvent::DefinitionRequested(request) => self.lsp_definition(index, *request, cx),
            EditorEvent::CompletionResolveRequested { request, host_data } => {
                self.lsp_resolve(index, *request, *host_data, false, cx)
            }
            EditorEvent::CompletionCommitted { host_data } => {
                self.lsp_commit(index, *host_data, cx)
            }
            EditorEvent::Changed | EditorEvent::SaveRequested => {}
        }
    }

    /// The analyzer, the view and the document URI for a request, provided the
    /// server has the document.
    fn lsp_target(
        &self,
        index: usize,
        cx: &Context<Self>,
    ) -> Option<(Entity<RustAnalyzerManager>, Entity<WgpuiEditorView>, String)> {
        let analyzer = self.analyzer()?;
        let file = self.open_files.get(index)?;
        if !file.lsp_open || !analyzer.read(cx).is_running() {
            return None;
        }
        Some((analyzer, file.view.clone(), path_to_uri(&file.path)))
    }

    fn lsp_hover(&mut self, index: usize, request: LspRequest, cx: &mut Context<Self>) {
        let Some((analyzer, view, uri)) = self.lsp_target(index, cx) else {
            tracing::info!("hover requested but the language server has no such document open (or is not running)");
            return;
        };
        let log_uri = uri.clone();
        let params = position_params(&uri, request.position);
        let Ok(response) = analyzer
            .read(cx)
            .send_request_async("textDocument/hover", params)
        else {
            return;
        };
        cx.spawn(async move |_this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            let raw = response.to_string();
            tracing::info!(
                "hover {log_uri} @ {}:{} -> {}",
                request.position.line,
                request.position.column,
                raw.chars().take(300).collect::<String>()
            );
            let content = hover_markdown(&response).map(|markdown| HoverContent::from_markdown(&markdown, None));
            let _ = view.update(cx, |view, cx| {
                view.show_hover(request.id, request.version, content, cx)
            });
        })
        .detach();
    }

    fn lsp_completion(
        &mut self,
        index: usize,
        request: LspRequest,
        trigger: Option<char>,
        cx: &mut Context<Self>,
    ) {
        let Some((analyzer, view, uri)) = self.lsp_target(index, cx) else {
            tracing::info!("completion requested but the language server has no such document open (or is not running)");
            return;
        };
        let mut server_triggers = analyzer.read(cx).completion_trigger_characters();
        if server_triggers.is_empty() {
            server_triggers = SERVER_TRIGGER_CHARACTERS.to_vec();
        }
        let mut params = position_params(&uri, request.position);
        // Only the characters the server registered go as trigger characters;
        // the rest (space, comma, brackets...) are ordinary invocations.
        params["context"] = match trigger.filter(|c| server_triggers.contains(c)) {
            Some(character) => json!({ "triggerKind": 2, "triggerCharacter": character.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        let Ok(first) = analyzer
            .read(cx)
            .send_request_async("textDocument/completion", params.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let mut pending = first;
            let mut attempts = 0;
            // rust-analyzer cancels a request that races a fresh edit and
            // says so with ContentModified/ServerCancelled; asking again
            // gets the answer for the current text.
            let response = loop {
                let Ok(response) = pending.recv_async().await else {
                    return;
                };
                let cancelled = matches!(error_code(&response), Some(-32801 | -32802));
                if !cancelled || attempts >= 3 {
                    break response;
                }
                attempts += 1;
                let params = params.clone();
                let analyzer = analyzer.clone();
                let Ok(Some(next)) = this.update(cx, |_, cx| {
                    analyzer
                        .read(cx)
                        .send_request_async("textDocument/completion", params)
                        .ok()
                }) else {
                    return;
                };
                pending = next;
            };
            // A busy response holds thousands of items; turn them into menu
            // entries off the UI thread.
            let head: String = response.to_string().chars().take(200).collect();
            let parsed = cx
                .background_executor()
                .spawn(async move { parse_completions(response) })
                .await;
            tracing::info!(
                "completion {uri} @ {}:{} -> {} items ({head})",
                request.position.line,
                request.position.column,
                parsed.as_ref().map_or(0, |(_, entries)| entries.len())
            );
            let Some((items, entries)) = parsed else {
                return;
            };
            let _ = this.update(cx, |this, _| {
                if let Some(client) = &mut this.lsp {
                    client.completions = Some(CompletionStore {
                        request: request.id,
                        items,
                    });
                    client.resolved_edits.clear();
                }
            });
            let _ = view.update(cx, |view, cx| {
                view.show_completions(request.id, request.version, entries, cx)
            });
        })
        .detach();
    }

    /// Fetches documentation, detail and extra edits for one completion item.
    /// With `commit`, the extra edits (an added `use`) are applied on arrival.
    fn lsp_resolve(
        &mut self,
        index: usize,
        request: u64,
        host_data: u64,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((analyzer, view, _uri)) = self.lsp_target(index, cx) else {
            return;
        };
        // `host_data` is index + 1; 0 wraps to a miss.
        let Some(item) = self
            .lsp
            .as_ref()
            .and_then(|client| client.completions.as_ref())
            .filter(|store| store.request == request)
            .and_then(|store| store.items.get((host_data as usize).wrapping_sub(1)).cloned())
        else {
            return;
        };
        let Ok(response) = analyzer
            .read(cx)
            .send_request_async("completionItem/resolve", item)
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            let result = response.get("result");
            let documentation = result
                .and_then(|result| result.get("documentation"))
                .and_then(marked);
            let detail = result
                .and_then(|result| result.get("detail"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let edits = result
                .and_then(|result| result.get("additionalTextEdits"))
                .map(position_edits)
                .unwrap_or_default();
            let _ = this.update(cx, |this, _| {
                if let Some(client) = &mut this.lsp {
                    client.resolved_edits.insert(host_data, edits.clone());
                }
            });
            let _ = view.update(cx, |view, cx| {
                view.resolve_completion(request, host_data, documentation, detail, cx);
                if commit {
                    view.apply_position_edits(edits, cx);
                }
            });
        })
        .detach();
    }

    /// A completion was accepted: add the edits it needs elsewhere in the file.
    fn lsp_commit(&mut self, index: usize, host_data: u64, cx: &mut Context<Self>) {
        let cached = self
            .lsp
            .as_mut()
            .and_then(|client| client.resolved_edits.remove(&host_data));
        match cached {
            Some(edits) => {
                if let Some(file) = self.open_files.get(index) {
                    file.view
                        .update(cx, |view, cx| view.apply_position_edits(edits, cx));
                }
            }
            None => {
                let request = self
                    .lsp
                    .as_ref()
                    .and_then(|client| client.completions.as_ref())
                    .map(|store| store.request);
                if let Some(request) = request {
                    self.lsp_resolve(index, request, host_data, true, cx);
                }
            }
        }
    }

    fn lsp_definition(&mut self, index: usize, request: LspRequest, cx: &mut Context<Self>) {
        let Some((analyzer, _view, uri)) = self.lsp_target(index, cx) else {
            return;
        };
        let params = position_params(&uri, request.position);
        let Ok(response) = analyzer
            .read(cx)
            .send_request_async("textDocument/definition", params)
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            let Some((target, line, character)) = response.get("result").and_then(first_location)
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.navigate_to(uri_to_path(&target), line, character, cx)
            });
        })
        .detach();
    }

    /// Shows a definition: in this editor when the file is already open here,
    /// otherwise by asking the owning panel to open it.
    fn navigate_to(&mut self, path: PathBuf, line: u32, character: u32, cx: &mut Context<Self>) {
        let key = path_key(&path);
        match self
            .open_files
            .iter()
            .position(|file| path_key(&file.path) == key)
        {
            Some(index) => {
                self.current_file_index = Some(index);
                self.open_files[index].view.update(cx, |view, cx| {
                    view.go_to(line as usize, character as usize, cx)
                });
                cx.notify();
            }
            None => cx.emit(TextEditorEvent::NavigateToLocation {
                path,
                line,
                character,
            }),
        }
    }
}

// ---- protocol conversions ---------------------------------------------------

fn position_json(position: TextPosition) -> Value {
    json!({ "line": position.line, "character": position.column })
}

fn position_params(uri: &str, position: TextPosition) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "position": position_json(position),
    })
}

/// One LSP `TextDocumentContentChangeEvent` for an editor edit.
fn content_change(change: &TextChange) -> Value {
    json!({
        "range": { "start": position_json(change.start), "end": position_json(change.end) },
        "text": change.text,
    })
}

fn diagnostic_input(diagnostic: &ui::diagnostics::Diagnostic) -> DiagnosticInput {
    // The analyzer reports 1-based line/column; LSP positions are 0-based.
    let start = TextPosition {
        line: diagnostic.line.saturating_sub(1),
        column: diagnostic.column.saturating_sub(1),
    };
    let end = match (diagnostic.end_line, diagnostic.end_column) {
        (Some(line), Some(column)) => TextPosition {
            line: line.saturating_sub(1),
            column: column.saturating_sub(1),
        },
        _ => start,
    };
    let severity = match diagnostic.severity {
        ui::diagnostics::DiagnosticSeverity::Error => DiagnosticSeverity::Error,
        ui::diagnostics::DiagnosticSeverity::Warning => DiagnosticSeverity::Warning,
        ui::diagnostics::DiagnosticSeverity::Information => DiagnosticSeverity::Info,
        ui::diagnostics::DiagnosticSeverity::Hint => DiagnosticSeverity::Hint,
    };
    let source = match (&diagnostic.source, &diagnostic.code) {
        (Some(source), Some(code)) => Some(format!("{source}({code})")),
        (Some(source), None) => Some(source.clone()),
        (None, Some(code)) => Some(code.clone()),
        (None, None) => None,
    };
    DiagnosticInput {
        start,
        end,
        severity,
        message: diagnostic.message.clone(),
        source,
    }
}

/// Markdown out of any LSP "MarkedString"/"MarkupContent" shape.
fn marked(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(marked).collect();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        Value::Object(map) => {
            let text = map.get("value")?.as_str()?;
            match map.get("language").and_then(Value::as_str) {
                Some(language) => Some(format!("```{language}\n{text}\n```")),
                None => Some(text.to_owned()),
            }
        }
        _ => None,
    }
}

fn hover_markdown(response: &Value) -> Option<String> {
    let text = marked(response.get("result")?.get("contents")?)?;
    (!text.trim().is_empty()).then_some(text)
}

fn position_from(value: &Value) -> Option<TextPosition> {
    Some(TextPosition {
        line: value.get("line")?.as_u64()? as usize,
        column: value.get("character")?.as_u64()? as usize,
    })
}

fn position_edits(edits: &Value) -> Vec<PositionEdit> {
    edits
        .as_array()
        .map(|edits| {
            edits
                .iter()
                .filter_map(|edit| {
                    let range = edit.get("range")?;
                    Some(PositionEdit {
                        start: position_from(range.get("start")?)?,
                        end: position_from(range.get("end")?)?,
                        new_text: edit.get("newText")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The JSON-RPC error code of a response, if it is an error.
fn error_code(response: &Value) -> Option<i64> {
    response.get("error")?.get("code")?.as_i64()
}

/// `(uri, line, character)` of the first definition in a
/// `Location | Location[] | LocationLink[]` result.
fn first_location(result: &Value) -> Option<(String, u32, u32)> {
    let item = match result {
        Value::Array(items) => items.first()?,
        Value::Object(_) => result,
        _ => return None,
    };
    let uri = item.get("targetUri").or_else(|| item.get("uri"))?.as_str()?;
    let range = item
        .get("targetSelectionRange")
        .or_else(|| item.get("range"))?;
    let start = range.get("start")?;
    Some((
        uri.to_owned(),
        start.get("line")?.as_u64()? as u32,
        start.get("character")?.as_u64()? as u32,
    ))
}

/// The items of a completion response and the menu entries built from them.
fn parse_completions(mut response: Value) -> Option<(Arc<Vec<Value>>, Vec<CompletionEntry>)> {
    let items = match response.get_mut("result")?.take() {
        Value::Array(items) => items,
        Value::Object(mut list) => match list.remove("items") {
            Some(Value::Array(items)) => items,
            _ => return None,
        },
        _ => return None,
    };
    let entries = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| completion_entry(item, index as u64 + 1))
        .collect();
    Some((Arc::new(items), entries))
}

fn completion_entry(item: &Value, host_data: u64) -> Option<CompletionEntry> {
    let label = item.get("label")?.as_str()?;
    let mut entry = CompletionEntry::new(label);
    entry.host_data = host_data;
    entry.kind = CompletionKind::from_lsp(
        item.get("kind").and_then(Value::as_u64).unwrap_or(1) as u32,
    );
    entry.detail = item
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // rust-analyzer's signature hint lives in labelDetails.
    entry.label_detail = item.get("labelDetails").and_then(|details| {
        let parts: Vec<&str> = ["detail", "description"]
            .iter()
            .filter_map(|key| details.get(key).and_then(Value::as_str))
            .filter(|part| !part.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    });
    entry.documentation = item.get("documentation").and_then(marked);
    entry.insert_text = item
        .get("textEdit")
        .and_then(|edit| edit.get("newText"))
        .or_else(|| item.get("insertText"))
        .and_then(Value::as_str)
        .unwrap_or(label)
        .to_owned();
    entry.is_snippet = item.get("insertTextFormat").and_then(Value::as_u64) == Some(2);
    entry.filter_text = item
        .get("filterText")
        .and_then(Value::as_str)
        .map(str::to_owned);
    entry.sort_key = item
        .get("sortText")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    entry.preselect = item.get("preselect").and_then(Value::as_bool).unwrap_or(false);
    entry.deprecated = item.get("deprecated").and_then(Value::as_bool).unwrap_or(false)
        || item
            .get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_u64() == Some(1)));
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_backend::services::rust_analyzer_manager::RustAnalyzerManager;
    use plugin_editor_api::{EditorServices, EditorSettingsSnapshot};

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn status_of(
        settings: EditorSettingsSnapshot,
        cx: &mut TestAppContext,
    ) -> (bool, String) {
        let (_host, cx) = cx.add_window_view(|_, _| Host);
        let editor = cx.update(|window, app| {
            app.new(|cx| TextEditor::new_with_settings(window, cx, settings))
        });
        editor.read_with(cx, |editor, cx| (editor.lsp.is_some(), editor.lsp_status(cx)))
    }

    /// The regression this module exists for: an editor that was handed a
    /// language server by its context must be attached to it, and say so.
    #[gpui::test]
    fn an_editor_attaches_to_the_language_server_in_its_context(cx: &mut TestAppContext) {
        let (_host, vcx) = cx.add_window_view(|_, _| Host);
        let analyzer = vcx.update(|window, app| app.new(|cx| RustAnalyzerManager::new(window, cx)));
        let settings = EditorSettingsSnapshot::default().with_services(EditorServices {
            language_server: Some(analyzer),
        });
        let (attached, status) = status_of(settings, cx);
        assert!(attached, "the handle in the context was ignored");
        assert!(status.starts_with("rust-analyzer:"), "unexpected status: {status}");
    }

    #[gpui::test]
    fn an_editor_without_a_language_server_says_so(cx: &mut TestAppContext) {
        let (attached, status) = status_of(EditorSettingsSnapshot::default(), cx);
        assert!(!attached);
        assert!(status.contains("not attached"), "unexpected status: {status}");
    }

    #[test]
    fn hover_contents_come_in_every_lsp_shape() {
        let markup = json!({ "result": { "contents": { "kind": "markdown", "value": "docs" } } });
        assert_eq!(hover_markdown(&markup).as_deref(), Some("docs"));
        let marked_string = json!({ "result": { "contents": { "language": "rust", "value": "fn a()" } } });
        assert_eq!(hover_markdown(&marked_string).as_deref(), Some("```rust\nfn a()\n```"));
        let array = json!({ "result": { "contents": ["a", { "language": "rust", "value": "b" }] } });
        assert_eq!(hover_markdown(&array).as_deref(), Some("a\n\n```rust\nb\n```"));
        assert_eq!(hover_markdown(&json!({ "result": null })), None);
        assert_eq!(hover_markdown(&json!({ "result": { "contents": "  " } })), None);
    }

    #[test]
    fn completions_become_entries_tagged_for_resolving() {
        let response = json!({ "result": { "isIncomplete": false, "items": [
            { "label": "push", "kind": 2, "sortText": "0001", "filterText": "push",
              "labelDetails": { "detail": "(…)", "description": "()" },
              "textEdit": { "newText": "push($0)", "range": {} }, "insertTextFormat": 2,
              "tags": [1], "preselect": true },
            { "label": "len" },
            { "nolabel": true },
        ] } });
        let (raw, entries) = parse_completions(response).unwrap();
        assert_eq!(raw.len(), 3);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].host_data, 1);
        assert_eq!(entries[0].insert_text, "push($0)");
        assert!(entries[0].is_snippet && entries[0].deprecated && entries[0].preselect);
        assert_eq!(entries[0].label_detail.as_deref(), Some("(…) ()"));
        assert_eq!(entries[1].host_data, 2);
        assert_eq!(entries[1].insert_text, "len");
        assert!(parse_completions(json!({ "result": null })).is_none());
    }

    #[test]
    fn definition_results_resolve_to_a_target() {
        let location = json!({ "uri": "file:///a.rs", "range": { "start": { "line": 3, "character": 4 }, "end": {} } });
        assert_eq!(first_location(&location), Some(("file:///a.rs".into(), 3, 4)));
        let link = json!([{ "targetUri": "file:///b.rs", "targetRange": {},
            "targetSelectionRange": { "start": { "line": 9, "character": 1 }, "end": {} } }]);
        assert_eq!(first_location(&link), Some(("file:///b.rs".into(), 9, 1)));
        assert_eq!(first_location(&json!([])), None);
    }

    #[test]
    fn edits_convert_to_positions() {
        let edits = json!([{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "use a::B;\n" }]);
        let converted = position_edits(&edits);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].new_text, "use a::B;\n");
        assert_eq!(converted[0].start, TextPosition { line: 0, column: 0 });
    }

    #[test]
    fn editor_edits_become_content_changes() {
        let change = TextChange {
            start: TextPosition { line: 1, column: 2 },
            end: TextPosition { line: 1, column: 5 },
            text: "x".into(),
        };
        assert_eq!(
            content_change(&change),
            json!({ "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 5 } }, "text": "x" })
        );
    }
}
