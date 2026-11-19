//! Language-server wiring for the Mockaco editor host.
//!
//! The engine runs the language servers ([`LanguageServers`]); this editor
//! never starts, restarts or configures one. Each open file is routed to the
//! server that claims it (`server_for`: by the file's language, in the
//! root its strategy picks), and the editor keeps that server's copy of the
//! document in step and answers the view's questions (hover, completion,
//! definition) through the server's request channel. A file no server claims
//! is a plain text editor with highlighting.
//!
//! * Document sync is incremental when the server takes it: Mockaco logs each
//!   edit as an LSP content change and [`TextEditor::lsp_sync`] forwards
//!   exactly that, so a keystroke costs a few bytes whatever the file size.
//!   Undo/redo, wholesale replacement and servers that take full text only
//!   send the full text.
//! * Diagnostics come from the shared store, per file and from every source
//!   (the server's, a compiler's), and go straight to the matching view,
//!   which draws and tracks them.
//! * A file whose language has a script compiler also hands its text to the
//!   service on open, edit and save ([`TextEditor::lsp_check`]); the service
//!   debounces, compiles off the UI thread and publishes the compiler's
//!   problems (source `Pulsar`) beside the server's.
//! * Requests are answered on the foreground executor as they complete; every
//!   answer names the request and document version it was asked against, and
//!   the view drops it when the user has moved on.
//! * What only one server understands (Rust's "Expand macro") is a command
//!   the server's spec lists ([`ServerCommandSpec`]); the menu offers the
//!   commands of the open file's server.

use super::{OpenFile, TextEditor, TextEditorEvent};
use gpui::*;
use mockaco_core::{DiagnosticSeverity, TextPosition};
use mockaco_gpui::intel::{
    CompletionEntry, CompletionKind, DiagnosticInput, HoverContent, PositionEdit,
};
use mockaco_gpui::native::{EditorEvent, LspRequest, WgpuiEditorView};
use mockaco_gpui::{PendingSync, TextChange};
use pulsar_lsp::{
    path_key, path_to_uri, uri_to_path, DiagnosticsEvent,
    DiagnosticsStore, LanguageServer, LanguageServers, LanguageServersEvent, ServerEvent,
    ServerId, ServerStatus,
};
use pulsar_language::{CommandContext, CommandResult};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The editor's attachment to the engine's language servers.
pub(super) struct LspClient {
    servers: Entity<LanguageServers>,
    /// The raw items of the most recent completion response. A menu entry's
    /// `host_data` is its index + 1, so resolving needs no lookup table.
    completions: Option<CompletionStore>,
    /// `additionalTextEdits` already fetched while the user browsed the menu.
    resolved_edits: HashMap<u64, Vec<PositionEdit>>,
    _events: Vec<Subscription>,
    _release: Subscription,
}

struct CompletionStore {
    request: u64,
    items: Arc<Vec<Value>>,
}

impl TextEditor {
    /// Attaches this editor to the engine's language servers. Every open file
    /// that a server claims is announced to it as soon as it can take
    /// documents.
    pub fn set_language_servers(
        &mut self,
        servers: Entity<LanguageServers>,
        cx: &mut Context<Self>,
    ) {
        let store = servers.read(cx).diagnostics();
        let server_events = cx.subscribe(
            &servers,
            |this: &mut Self, _servers, event: &LanguageServersEvent, cx| {
                this.on_language_servers_event(event, cx);
            },
        );
        let diagnostics_events = cx.subscribe(
            &store,
            |this: &mut Self, store, event: &DiagnosticsEvent, cx| {
                this.on_diagnostics_event(&store, event, cx);
            },
        );
        let release = cx.on_release(|this: &mut Self, cx: &mut App| this.lsp_release(cx));
        self.lsp = Some(LspClient {
            servers,
            completions: None,
            resolved_edits: HashMap::new(),
            _events: vec![server_events, diagnostics_events],
            _release: release,
        });
        self.lsp_open_pending(cx);
        for index in 0..self.open_files.len() {
            self.lsp_check(index, cx);
        }
    }

    /// Hand the text of file `index` to the service's live compiler check, if
    /// its language has a compiler (the service debounces and runs it off the
    /// UI thread). Called on open, on edit and on save.
    pub(super) fn lsp_check(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(servers) = self.servers() else {
            return;
        };
        let Some(file) = self.open_files.get(index) else {
            return;
        };
        let path = file.path.clone();
        if !servers.read(cx).has_compiler_for(&path) {
            return;
        }
        let text = file.view.read(cx).text();
        servers.update(cx, |servers, cx| servers.check_source(&path, &text, cx));
    }

    /// One line saying why language features are or are not available for the
    /// current file.
    pub fn lsp_status(&self, cx: &App) -> String {
        let Some(client) = &self.lsp else {
            return "language servers: not attached to the engine".to_owned();
        };
        let Some(file) = self.current_file() else {
            return "language server: no file".to_owned();
        };
        let Some((_, spec)) = client.servers.read(cx).server_spec_for(&file.path) else {
            return "language server: none for this file".to_owned();
        };
        let Some(server) = &file.server else {
            return format!("{}: not started", spec.display_name);
        };
        let server = server.read(cx);
        let state = match server.status() {
            ServerStatus::Idle => "idle".to_owned(),
            ServerStatus::Starting => "starting".to_owned(),
            ServerStatus::Indexing { progress, .. } => {
                format!("indexing {:.0}%", progress * 100.0)
            }
            ServerStatus::Ready => "ready".to_owned(),
            ServerStatus::Error(error) => format!("error: {error}"),
            ServerStatus::Missing(message) => format!("not installed: {message}"),
            ServerStatus::Stopped => "stopped".to_owned(),
        };
        let document = if file.lsp_open { "synced" } else { "not synced yet" };
        format!("{}: {state}, document {document}", spec.display_name)
    }

    fn servers(&self) -> Option<Entity<LanguageServers>> {
        self.lsp.as_ref().map(|client| client.servers.clone())
    }

    /// What the service reports: a server that stopped or restarted no longer
    /// has its documents (they are announced again when it is back); one that
    /// made progress can take documents.
    fn on_language_servers_event(&mut self, event: &LanguageServersEvent, cx: &mut Context<Self>) {
        match event {
            LanguageServersEvent::Server { id, event } => match event {
                ServerEvent::StatusChanged(
                    ServerStatus::Stopped
                    | ServerStatus::Idle
                    | ServerStatus::Error(_)
                    | ServerStatus::Missing(_),
                )
                | ServerEvent::Error(_)
                | ServerEvent::Stopped(_) => self.lsp_forget_documents(Some(id), cx),
                ServerEvent::StatusChanged(_) | ServerEvent::Progress { .. } | ServerEvent::Ready => {
                    self.lsp_open_pending(cx)
                }
            },
            LanguageServersEvent::Removed(id) => self.lsp_forget_documents(Some(id), cx),
            LanguageServersEvent::Added(_) => self.lsp_open_pending(cx),
        }
    }

    /// The store changed for one file: every source's diagnostics of it go to
    /// the open view (an empty set clears the squiggles).
    fn on_diagnostics_event(
        &mut self,
        store: &Entity<DiagnosticsStore>,
        event: &DiagnosticsEvent,
        cx: &mut Context<Self>,
    ) {
        let DiagnosticsEvent::Changed { key, path, .. } = event;
        let Some(index) = self
            .open_files
            .iter()
            .position(|file| path_key(&file.path) == *key)
        else {
            return;
        };
        let all: Vec<ui::diagnostics::Diagnostic> = store
            .read(cx)
            .for_file(path)
            .into_iter()
            .map(|(_, diagnostic)| diagnostic)
            .collect();
        self.lsp_apply_diagnostics(index, &all, cx);
    }

    /// The server `id` (every server when `None`) restarted or stopped: it no
    /// longer has our documents, so they are announced again when it is back.
    /// A file whose server is gone for good loses language intelligence.
    fn lsp_forget_documents(&mut self, id: Option<&ServerId>, cx: &mut Context<Self>) {
        for file in &mut self.open_files {
            let bound = file
                .server
                .as_ref()
                .is_some_and(|server| id.is_none_or(|id| server.read(cx).id() == id));
            if bound {
                file.lsp_open = false;
                file.server = None;
                file.view
                    .update(cx, |view, cx| view.set_language_intelligence(false, cx));
            }
        }
        if let Some(client) = &mut self.lsp {
            client.completions = None;
            client.resolved_edits.clear();
        }
        // A server that is still there (a restart) takes them again now.
        self.lsp_open_pending(cx);
    }

    /// Routes every open file that no server has yet to the server that
    /// claims it, and announces its text once the server runs.
    pub(super) fn lsp_open_pending(&mut self, cx: &mut Context<Self>) {
        let Some(servers) = self.servers() else {
            return;
        };
        for index in 0..self.open_files.len() {
            if self.open_files[index].lsp_open {
                self.lsp_refresh_triggers(index, cx);
                continue;
            }
            let path = self.open_files[index].path.clone();
            let server = match self.open_files[index].server.clone() {
                Some(server) => Some(server),
                None => servers.update(cx, |servers, cx| servers.server_for(&path, cx)),
            };
            let Some(server) = server else {
                continue;
            };
            let view = self.open_files[index].view.clone();
            let text = view.read(cx).text();
            let language_id = server.read(cx).language_id_of(&path);
            match server.read(cx).did_open_file(&path, &text, language_id) {
                Ok(()) => {
                    self.open_files[index].server = Some(server.clone());
                    self.open_files[index].lsp_open = true;
                    // The server now has (or will have) the current text; the
                    // earlier edits are already inside it.
                    view.update(cx, |view, cx| {
                        view.set_language_intelligence(true, cx);
                        let _ = view.take_pending_sync();
                    });
                    self.lsp_refresh_triggers(index, cx);
                    let cached = servers.read(cx).diagnostics().read(cx).for_file(&path);
                    if !cached.is_empty() {
                        let all: Vec<_> = cached.into_iter().map(|(_, d)| d).collect();
                        self.lsp_apply_diagnostics(index, &all, cx);
                    }
                }
                Err(error) => {
                    // Not running (yet, or any more): tried again on its next event.
                    self.open_files[index].server = Some(server);
                    tracing::debug!("language server not ready for {:?}: {error}", path.file_name());
                }
            }
        }
    }

    /// Offer completion on whatever the server registered, as VS Code and Zed
    /// do. Empty until the server has answered `initialize`.
    fn lsp_refresh_triggers(&self, index: usize, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get(index) else {
            return;
        };
        let Some(server) = &file.server else {
            return;
        };
        let triggers = server.read(cx).completion_trigger_characters();
        if !triggers.is_empty() {
            file.view
                .update(cx, |view, _| view.set_completion_triggers(triggers));
        }
    }

    /// Forwards the edits made since the last call.
    pub(super) fn lsp_sync(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get(index) else {
            return;
        };
        let Some(server) = file.server.clone().filter(|_| file.lsp_open) else {
            return;
        };
        let (path, version, view) = (file.path.clone(), file.version, file.view.clone());
        let full = |view: &Entity<WgpuiEditorView>, cx: &mut Context<Self>| {
            let text = view.read(cx).text();
            server.read(cx).did_change_file(&path, &text, version)
        };
        let result = match view.update(cx, |view, _| view.take_pending_sync()) {
            PendingSync::None => return,
            PendingSync::Incremental(changes) => {
                let sent = server.read(cx).did_change_file_incremental(
                    &path,
                    version,
                    changes.iter().map(content_change).collect(),
                );
                // A server that takes full text only (or has not been told of
                // the document yet) gets the whole text.
                match sent {
                    Ok(()) => Ok(()),
                    Err(_) => full(&view, cx),
                }
            }
            PendingSync::Full => full(&view, cx),
        };
        if let Err(error) = result {
            tracing::debug!("didChange failed for {:?}: {}", path.file_name(), error);
        }
    }

    pub(super) fn lsp_saved(&mut self, index: usize, content: &str, cx: &mut Context<Self>) {
        let Some(file) = self.open_files.get(index) else {
            return;
        };
        if let (true, Some(server)) = (file.lsp_open, &file.server) {
            if let Err(error) = server.read(cx).did_save_file(&file.path, content) {
                tracing::debug!("didSave failed for {:?}: {}", file.path.file_name(), error);
            }
        }
        self.lsp_check(index, cx);
    }

    /// `file` has just been removed from the editor.
    pub(super) fn lsp_close(&mut self, file: &OpenFile, cx: &mut Context<Self>) {
        if let (true, Some(server)) = (file.lsp_open, &file.server) {
            let _ = server.read(cx).did_close_file(&file.path);
        }
        if let Some(servers) = self.servers() {
            servers.update(cx, |servers, cx| servers.forget_source(&file.path, cx));
        }
    }

    /// The panel is going away: release every document it held open.
    fn lsp_release(&mut self, cx: &mut App) {
        for file in &self.open_files {
            if let (true, Some(server)) = (file.lsp_open, &file.server) {
                let _ = server.read(cx).did_close_file(&file.path);
            }
            if let Some(client) = &self.lsp {
                client
                    .servers
                    .update(cx, |servers, cx| servers.forget_source(&file.path, cx));
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

    pub(super) fn on_lsp_event(
        &mut self,
        index: usize,
        event: &EditorEvent,
        cx: &mut Context<Self>,
    ) {
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
            // The panel that shows this document answers the AI events.
            EditorEvent::Changed
            | EditorEvent::SaveRequested
            | EditorEvent::SelectionChanged
            | EditorEvent::InlineAssistRequested
            | EditorEvent::QuickFixRequested
            | EditorEvent::ReviewUpdated { .. }
            | EditorEvent::ReviewFinished { .. } => {}
        }
    }

    /// The server, the view and the document URI for a request, provided the
    /// file's server has the document and can answer.
    fn lsp_target(
        &self,
        index: usize,
        cx: &Context<Self>,
    ) -> Option<(Entity<LanguageServer>, Entity<WgpuiEditorView>, String)> {
        let file = self.open_files.get(index)?;
        let server = file.server.clone()?;
        if !file.lsp_open || !server.read(cx).is_initialized() {
            return None;
        }
        Some((server, file.view.clone(), path_to_uri(&file.path)))
    }

    fn lsp_hover(&mut self, index: usize, request: LspRequest, cx: &mut Context<Self>) {
        let Some((server, view, uri)) = self.lsp_target(index, cx) else {
            tracing::info!("hover requested but the language server has no such document open (or is not running)");
            return;
        };
        let log_uri = uri.clone();
        let params = position_params(&uri, request.position);
        let Ok(response) = server
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
            let content = hover_markdown(&response)
                .map(|markdown| HoverContent::from_markdown(&markdown, None));
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
        let Some((server, view, uri)) = self.lsp_target(index, cx) else {
            tracing::info!("completion requested but the language server has no such document open (or is not running)");
            return;
        };
        let server_triggers = server.read(cx).completion_trigger_characters();
        let mut params = position_params(&uri, request.position);
        // Only the characters the server registered go as trigger characters;
        // the rest (space, comma, brackets...) are ordinary invocations.
        params["context"] = match trigger.filter(|c| server_triggers.contains(c)) {
            Some(character) => {
                json!({ "triggerKind": 2, "triggerCharacter": character.to_string() })
            }
            None => json!({ "triggerKind": 1 }),
        };
        let Ok(first) = server
            .read(cx)
            .send_request_async("textDocument/completion", params.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let mut pending = first;
            let mut attempts = 0;
            // A server cancels a request that races a fresh edit and says so
            // with ContentModified/ServerCancelled; asking again
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
                let server = server.clone();
                let Ok(Some(next)) = this.update(cx, |_, cx| {
                    server
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
        let Some((server, view, _uri)) = self.lsp_target(index, cx) else {
            return;
        };
        // `host_data` is index + 1; 0 wraps to a miss.
        let Some(item) = self
            .lsp
            .as_ref()
            .and_then(|client| client.completions.as_ref())
            .filter(|store| store.request == request)
            .and_then(|store| {
                store
                    .items
                    .get((host_data as usize).wrapping_sub(1))
                    .cloned()
            })
        else {
            return;
        };
        let Ok(response) = server
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
        let Some((server, _view, uri)) = self.lsp_target(index, cx) else {
            return;
        };
        let params = position_params(&uri, request.position);
        let Ok(response) = server
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

impl TextEditor {
    /// The places Go to References or Go to Symbol last found.
    pub fn locations(&self) -> Option<&LocationList> {
        self.locations.as_ref()
    }

    pub fn clear_locations(&mut self, cx: &mut Context<Self>) {
        if self.locations.take().is_some() {
            cx.notify();
        }
    }

    /// Shows `item`: in this editor when its file is open here, otherwise by
    /// asking the owning panel to open it.
    pub fn open_location(&mut self, item: &LocationItem, cx: &mut Context<Self>) {
        self.navigate_to(item.path.clone(), item.line, item.character, cx);
    }

    /// The index of `path` and the request target, or a reason there is none.
    fn lsp_target_for(
        &self,
        path: &std::path::Path,
        cx: &Context<Self>,
    ) -> Result<
        (
            usize,
            Entity<LanguageServer>,
            Entity<WgpuiEditorView>,
            String,
        ),
        &'static str,
    > {
        let index = self
            .open_files
            .iter()
            .position(|file| file.path == path)
            .ok_or("The file is not open")?;
        if self.lsp.is_none() || self.open_files[index].server.is_none() {
            return Err("No language server for this file");
        }
        let (server, view, uri) = self
            .lsp_target(index, cx)
            .ok_or("The language server is not ready")?;
        Ok((index, server, view, uri))
    }

    /// Asks the server for the definition under the caret of `path`.
    pub fn go_to_definition(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if let Ok((_, _, view, _)) = self.lsp_target_for(path, cx) {
            view.update(cx, |view, cx| view.request_definition_at_caret(cx));
        }
    }

    /// Lists every reference to the symbol under the caret of `path`.
    pub fn find_references(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let title = "References";
        let (server, view, uri) = match self.lsp_target_for(path, cx) {
            Ok((_, server, view, uri)) => (server, view, uri),
            Err(reason) => return self.show_locations(path, title, Err(reason), cx),
        };
        let Some(request) = view.update(cx, |view, _| view.lsp_request_at_caret()) else {
            return self.show_locations(path, title, Err("No caret"), cx);
        };
        let mut params = position_params(&uri, request.position);
        params["context"] = json!({ "includeDeclaration": true });
        self.request_locations(
            path,
            title,
            server,
            "textDocument/references",
            params,
            cx,
            |result, _| {
                all_locations(result)
                    .into_iter()
                    .map(|(uri, line, character)| LocationItem {
                        path: uri_to_path(&uri),
                        line,
                        character,
                        label: String::new(),
                        detail: String::new(),
                    })
                    .collect()
            },
        );
    }

    /// Lists the symbols declared in `path`, outermost first.
    pub fn document_symbols(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let title = "Symbols";
        let (server, uri) = match self.lsp_target_for(path, cx) {
            Ok((_, server, _, uri)) => (server, uri),
            Err(reason) => return self.show_locations(path, title, Err(reason), cx),
        };
        let params = json!({ "textDocument": { "uri": uri } });
        self.request_locations(
            path,
            title,
            server,
            "textDocument/documentSymbol",
            params,
            cx,
            |result, path| document_symbols(result, path),
        );
    }

    /// Sends a request whose answer is a list of places, and shows the list
    /// (empty until the answer arrives) for `path`'s panel.
    #[allow(clippy::too_many_arguments)]
    fn request_locations(
        &mut self,
        path: &std::path::Path,
        title: &'static str,
        server: Entity<LanguageServer>,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
        parse: fn(&Value, &std::path::Path) -> Vec<LocationItem>,
    ) {
        let Ok(response) = server.read(cx).send_request_async(method, params) else {
            return self.show_locations(
                path,
                title,
                Err("The language server did not take the request"),
                cx,
            );
        };
        self.show_locations(path, title, Ok(None), cx);
        let origin = path.to_path_buf();
        let request = self.locations_request;
        cx.spawn(async move |this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                // Only the latest request's answer, while its list is shown.
                let still_asked = this.locations_request == request
                    && this.locations.as_ref().is_some_and(|list| {
                        list.origin == origin && list.title == title && list.items.is_none()
                    });
                if !still_asked {
                    return;
                }
                let result = response.get("result").cloned().unwrap_or(Value::Null);
                let mut items = parse(&result, &origin);
                this.fill_previews(&mut items, cx);
                this.show_locations(&origin, title, Ok(Some(items)), cx);
            });
        })
        .detach();
    }

    fn show_locations(
        &mut self,
        path: &std::path::Path,
        title: &'static str,
        items: Result<Option<Vec<LocationItem>>, &'static str>,
        cx: &mut Context<Self>,
    ) {
        let (items, message) = match items {
            Ok(items) => (items, None),
            Err(reason) => (Some(Vec::new()), Some(reason.into())),
        };
        if items.is_none() {
            self.locations_request += 1;
        }
        self.locations = Some(LocationList {
            origin: path.to_path_buf(),
            title: title.into(),
            items,
            message,
        });
        cx.notify();
    }

    /// Labels each reference with its file and line, and previews the line:
    /// from the open document when it is open here, else from disk.
    fn fill_previews(&self, items: &mut [LocationItem], cx: &App) {
        let mut disk: HashMap<PathBuf, Option<String>> = HashMap::new();
        for item in items.iter_mut() {
            if !item.label.is_empty() {
                continue;
            }
            let name = item
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            item.label = format!("{name}:{}", item.line + 1);
            let key = path_key(&item.path);
            let open = self
                .open_files
                .iter()
                .find(|file| path_key(&file.path) == key);
            item.detail = match open {
                Some(file) => file
                    .view
                    .read(cx)
                    .surface
                    .document()
                    .text()
                    .line(item.line as usize)
                    .map(|line| line.trim().to_owned())
                    .unwrap_or_default(),
                None => disk
                    .entry(item.path.clone())
                    .or_insert_with(|| engine_fs::virtual_fs::read_to_string(&item.path).ok())
                    .as_deref()
                    .and_then(|text| text.lines().nth(item.line as usize))
                    .map(|line| line.trim().to_owned())
                    .unwrap_or_default(),
            };
        }
    }

    /// Formats `path` with the language server, as one edit. An answer for a
    /// document that changed meanwhile is dropped.
    pub fn format_document(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.format_lines(path, None, cx);
    }

    /// Formats the lines the selection of `path` touches (the whole
    /// document without a selection). Servers differ in range formatting
    /// (some need a nightly formatter for it), so this asks for the whole
    /// document and applies the edits that fall within those lines: the
    /// rest of the file is left as it is.
    pub fn format_selection(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let Ok((_, _, view, _)) = self.lsp_target_for(path, cx) else {
            return;
        };
        let lines = view
            .read(cx)
            .surface
            .editor()
            .selections()
            .primary()
            .and_then(|selection| {
                let range = selection.ordered_range();
                if range.is_empty() {
                    return None;
                }
                let text = view.read(cx).surface.document().text().clone();
                let first = text.byte_to_line(range.start).ok()?;
                // A selection ending at a line's start does not take that line.
                let last = text
                    .byte_to_line(range.end.saturating_sub(1).max(range.start))
                    .ok()?;
                Some(first..=last)
            });
        self.format_lines(path, lines, cx);
    }

    fn format_lines(
        &mut self,
        path: &std::path::Path,
        lines: Option<std::ops::RangeInclusive<usize>>,
        cx: &mut Context<Self>,
    ) {
        let Ok((_, server, view, uri)) = self.lsp_target_for(path, cx) else {
            return;
        };
        let version = view.read(cx).document_version();
        let params = json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true },
        });
        let Ok(response) = server
            .read(cx)
            .send_request_async("textDocument/formatting", params)
        else {
            return;
        };
        cx.spawn(async move |_this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            let mut edits = position_edits(response.get("result").unwrap_or(&Value::Null));
            if let Some(lines) = &lines {
                edits.retain(|edit| edit_within_lines(edit, lines));
            }
            let _ = view.update(cx, |view, cx| {
                if view.document_version() == version {
                    view.apply_position_edits(edits, cx);
                }
            });
        })
        .detach();
    }

    /// Lists the symbols of the whole workspace whose names match `query`
    /// (the server's own matching, which is fuzzy). Each call replaces the
    /// previous one's answer; an empty query lists nothing.
    pub fn workspace_symbols(
        &mut self,
        path: &std::path::Path,
        query: &str,
        cx: &mut Context<Self>,
    ) {
        let title = "Workspace Symbols";
        let server = match self.lsp_target_for(path, cx) {
            Ok((_, server, _, _)) => server,
            Err(reason) => return self.show_locations(path, title, Err(reason), cx),
        };
        if query.trim().is_empty() {
            return self.show_locations(
                path,
                title,
                Err("Type a name to search the workspace"),
                cx,
            );
        }
        self.request_locations(
            path,
            title,
            server,
            "workspace/symbol",
            json!({ "query": query.trim() }),
            cx,
            |result, _| workspace_symbols(result),
        );
    }

    /// The commands the server of the open file `path` offers (its spec's
    /// [`ServerCommandSpec`]s, such as Rust's "Expand Macro Recursively"):
    /// `(id, label)`, for the menu. Empty when no server has the file.
    pub fn language_commands(
        &self,
        path: &std::path::Path,
        cx: &App,
    ) -> Vec<(&'static str, &'static str)> {
        self.open_files
            .iter()
            .find(|file| file.path == path)
            .and_then(|file| file.server.as_ref())
            .map(|server| {
                server
                    .read(cx)
                    .commands()
                    .iter()
                    .map(|command| (command.id, command.label))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Runs the server command `id` for the open file `path`: asks the
    /// server, then shows what the command's spec says its answer is (a
    /// location to open, or text to show in a scratch file).
    pub fn run_language_command(
        &mut self,
        path: &std::path::Path,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.open_files.iter().position(|file| file.path == path) else {
            return;
        };
        let Some(server) = self.open_files[index].server.clone() else {
            return;
        };
        let Some(command) = server
            .read(cx)
            .commands()
            .iter()
            .find(|command| command.id == id)
            .copied()
        else {
            return;
        };
        if !server.read(cx).is_initialized() {
            return;
        }
        let view = self.open_files[index].view.clone();
        let (line, column) = if command.needs_position {
            match view.update(cx, |view, _| view.lsp_request_at_caret()) {
                Some(request) => (request.position.line, request.position.column),
                None => return,
            }
        } else {
            (0, 0)
        };
        let context = CommandContext {
            uri: path_to_uri(path),
            line,
            column,
        };
        let params = (command.params)(&context);
        let Ok(response) = server.read(cx).send_request_async(command.method, params) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            let result = response.get("result").cloned().unwrap_or(Value::Null);
            match command.result {
                CommandResult::OpenLocation => {
                    let Some((target, line, character)) = first_location(&result) else {
                        return;
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.navigate_to(uri_to_path(&target), line, character, cx)
                    });
                }
                CommandResult::ShowText => {
                    let Some((name, text)) = (command.text)(&result) else {
                        return;
                    };
                    let header = format!("// {}: {name}\n{text}\n", command.label);
                    let file = std::env::temp_dir()
                        .join("pulsar_language_server_output")
                        .join(format!(
                            "{}.{}",
                            sanitize_file_name(&name),
                            command.scratch_extension
                        ));
                    let written = file
                        .parent()
                        .map(engine_fs::virtual_fs::create_dir_all)
                        .unwrap_or(Ok(()))
                        .and_then(|_| engine_fs::virtual_fs::write_file(&file, header.as_bytes()));
                    if let Err(error) = written {
                        tracing::warn!("Could not write the output of {}: {error}", command.label);
                        return;
                    }
                    let _ = this.update(cx, |this, cx| {
                        // A scratch file shown before is replaced by the new text.
                        if let Some(index) = this.open_files.iter().position(|open| open.path == file) {
                            this.open_files[index]
                                .view
                                .update(cx, |view, cx| view.replace_text(header.clone(), cx));
                        }
                        this.navigate_to(file, 0, 0, cx)
                    });
                }
                CommandResult::None => {}
            }
        })
        .detach();
    }

    /// Renames the symbol under the caret of `path` to `new_name`: the server
    /// answers with a `WorkspaceEdit`, applied across the open buffers (one
    /// undo group each) and the files on disk.
    pub fn rename_symbol(
        &mut self,
        path: &std::path::Path,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let Ok((_, server, view, uri)) = self.lsp_target_for(path, cx) else {
            return;
        };
        let Some(request) = view.update(cx, |view, _| view.lsp_request_at_caret()) else {
            return;
        };
        let mut params = position_params(&uri, request.position);
        params["newName"] = Value::String(new_name);
        let Ok(response) = server.read(cx).send_request_async("textDocument/rename", params) else {
            return;
        };
        cx.spawn(async move |_this, cx| {
            let Ok(response) = response.recv_async().await else {
                return;
            };
            if let Some(error) = response.get("error") {
                tracing::warn!("rename refused: {error}");
                return;
            }
            let Some(edit) = response.get("result").filter(|result| !result.is_null()) else {
                return;
            };
            let outcome = pulsar_lsp::WorkspaceEdit::from_value(edit)
                .and_then(|edit| cx.update(|cx| pulsar_lsp::edit::apply_workspace_edit_in_app(&edit, cx)));
            match outcome {
                Ok(applied) => tracing::info!(
                    "renamed: {} buffers, {} files, {} resource operations",
                    applied.edited_buffers.len(),
                    applied.written_files.len(),
                    applied.resource_operations
                ),
                Err(error) => tracing::warn!("rename failed: {error}"),
            }
        })
        .detach();
    }
}

/// What Go to References or Go to Symbol found for one file's panel.
#[derive(Clone, Debug, PartialEq)]
pub struct LocationList {
    /// The file it was asked from; its panel shows the list.
    pub origin: PathBuf,
    pub title: SharedString,
    /// `None` while the server is answering.
    pub items: Option<Vec<LocationItem>>,
    /// Why there is nothing to list, when the request could not be made.
    pub message: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocationItem {
    pub path: PathBuf,
    /// Zero-based line and UTF-16 column.
    pub line: u32,
    pub character: u32,
    pub label: String,
    pub detail: String,
}

// ---- workspace edits ---------------------------------------------------------

/// The text editors that exist, so an edit from a server (`workspace/applyEdit`,
/// rename, a refactor) reaches the buffers open in any of them.
#[derive(Default)]
struct OpenTextEditors(Vec<WeakEntity<TextEditor>>);

impl Global for OpenTextEditors {}

/// Make `editor` reachable by workspace edits, and install the host that
/// applies them (once).
pub(super) fn register_editor(editor: WeakEntity<TextEditor>, cx: &mut App) {
    cx.default_global::<OpenTextEditors>().0.push(editor);
    if !cx.has_global::<pulsar_lsp::EditHost>() {
        cx.set_global(pulsar_lsp::EditHost(Arc::new(EditorBuffers)));
    }
}

fn live_editors(cx: &App) -> Vec<Entity<TextEditor>> {
    cx.try_global::<OpenTextEditors>()
        .map(|editors| editors.0.iter().filter_map(WeakEntity::upgrade).collect())
        .unwrap_or_default()
}

/// The view that has `path` open, in any editor.
fn view_of(path: &std::path::Path, cx: &App) -> Option<Entity<WgpuiEditorView>> {
    let key = path_key(path);
    live_editors(cx).into_iter().find_map(|editor| {
        editor
            .read(cx)
            .open_files
            .iter()
            .find(|file| path_key(&file.path) == key)
            .map(|file| file.view.clone())
    })
}

/// The open buffers, as the [`pulsar_lsp::BufferEdits`] the workspace edit
/// applier edits through: one undo group per file.
struct EditorBuffers;

impl pulsar_lsp::BufferEdits<App> for EditorBuffers {
    fn text(&self, path: &std::path::Path, cx: &mut App) -> Option<String> {
        view_of(path, cx).map(|view| view.read(cx).text())
    }

    fn apply(
        &self,
        path: &std::path::Path,
        edits: &[pulsar_lsp::LspTextEdit],
        cx: &mut App,
    ) -> Result<(), String> {
        let view = view_of(path, cx).ok_or("the file is no longer open")?;
        let edits = edits
            .iter()
            .map(|edit| PositionEdit {
                start: TextPosition {
                    line: edit.start.0 as usize,
                    column: edit.start.1 as usize,
                },
                end: TextPosition {
                    line: edit.end.0 as usize,
                    column: edit.end.1 as usize,
                },
                new_text: edit.new_text.clone(),
            })
            .collect();
        view.update(cx, |view, cx| view.apply_position_edits(edits, cx));
        Ok(())
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
    // The server reports 1-based line/column; LSP positions are 0-based.
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
    let uri = item
        .get("targetUri")
        .or_else(|| item.get("uri"))?
        .as_str()?;
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

/// Every `(uri, line, character)` in a `Location[] | LocationLink[]` result.
fn all_locations(result: &Value) -> Vec<(String, u32, u32)> {
    match result {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| first_location(item))
            .collect(),
        Value::Object(_) => first_location(result).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The symbols of a `DocumentSymbol[]` (nested, indented by depth) or
/// `SymbolInformation[]` result, in document order.
fn document_symbols(result: &Value, path: &std::path::Path) -> Vec<LocationItem> {
    fn walk(symbol: &Value, depth: usize, path: &std::path::Path, out: &mut Vec<LocationItem>) {
        let Some(name) = symbol.get("name").and_then(Value::as_str) else {
            return;
        };
        let range = symbol
            .get("selectionRange")
            .or_else(|| {
                symbol
                    .get("location")
                    .and_then(|location| location.get("range"))
            })
            .or_else(|| symbol.get("range"));
        let Some(start) = range
            .and_then(|range| range.get("start"))
            .and_then(position_from)
        else {
            return;
        };
        let target = symbol
            .get("location")
            .and_then(|location| location.get("uri"))
            .and_then(Value::as_str)
            .map(uri_to_path)
            .unwrap_or_else(|| path.to_path_buf());
        let kind = symbol_kind_name(symbol.get("kind").and_then(Value::as_u64).unwrap_or(0));
        let detail = match symbol.get("detail").and_then(Value::as_str) {
            Some(detail) if !detail.is_empty() => format!("{kind} · {detail}"),
            _ => kind.to_owned(),
        };
        out.push(LocationItem {
            path: target,
            line: start.line as u32,
            character: start.column as u32,
            label: format!("{}{name}", "  ".repeat(depth)),
            detail,
        });
        for child in symbol
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk(child, depth + 1, path, out);
        }
    }
    let mut out = Vec::new();
    for symbol in result.as_array().into_iter().flatten() {
        walk(symbol, 0, path, &mut out);
    }
    out
}

/// Whether `edit` lies within `lines` (an edit ending at the start of the
/// next line, as one replacing a line's end does, counts as within).
fn edit_within_lines(edit: &PositionEdit, lines: &std::ops::RangeInclusive<usize>) -> bool {
    let last = *lines.end();
    let end_inside = edit.end.line <= last || (edit.end.line == last + 1 && edit.end.column == 0);
    edit.start.line >= *lines.start() && end_inside
}

/// The symbols of a `workspace/symbol` answer (`SymbolInformation[]` or
/// `WorkspaceSymbol[]`, whose location may name only the file), labelled by
/// name, with kind, container and file as detail.
fn workspace_symbols(result: &Value) -> Vec<LocationItem> {
    result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| {
            let name = symbol.get("name")?.as_str()?;
            let location = symbol.get("location")?;
            let path = uri_to_path(location.get("uri")?.as_str()?);
            let start = location
                .get("range")
                .and_then(|range| range.get("start"))
                .and_then(position_from)
                .unwrap_or(TextPosition { line: 0, column: 0 });
            let kind = symbol_kind_name(symbol.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let detail = match symbol.get("containerName").and_then(Value::as_str) {
                Some(container) if !container.is_empty() => {
                    format!("{kind} · {container} · {file}:{}", start.line + 1)
                }
                _ => format!("{kind} · {file}:{}", start.line + 1),
            };
            Some(LocationItem {
                path,
                line: start.line as u32,
                character: start.column as u32,
                label: name.to_owned(),
                detail,
            })
        })
        .collect()
}

/// LSP's `SymbolKind` by name.
fn symbol_kind_name(kind: u64) -> &'static str {
    const NAMES: [&str; 26] = [
        "file",
        "module",
        "namespace",
        "package",
        "class",
        "method",
        "property",
        "field",
        "constructor",
        "enum",
        "interface",
        "function",
        "variable",
        "constant",
        "string",
        "number",
        "boolean",
        "array",
        "object",
        "key",
        "null",
        "enum member",
        "struct",
        "event",
        "operator",
        "type parameter",
    ];
    kind.checked_sub(1)
        .and_then(|index| NAMES.get(index as usize))
        .copied()
        .unwrap_or("symbol")
}

fn sanitize_file_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "macro".into()
    } else {
        name
    }
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
    entry.kind =
        CompletionKind::from_lsp(item.get("kind").and_then(Value::as_u64).unwrap_or(1) as u32);
    entry.detail = item
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // A server's signature hint lives in labelDetails.
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
    entry.preselect = item
        .get("preselect")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    entry.deprecated = item
        .get("deprecated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || item
            .get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_u64() == Some(1)));
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `super::*` brings in `gpui::*`, whose `test` attribute would shadow the
    // built-in one that `#[gpui::test]` expands to, and recurse.
    use core::prelude::v1::test;
    use plugin_editor_api::{EditorServices, EditorSettingsSnapshot};

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn status_of(settings: EditorSettingsSnapshot, cx: &mut TestAppContext) -> (bool, String) {
        let (_host, cx) = cx.add_window_view(|_, _| Host);
        let editor = cx.update(|window, app| {
            app.new(|cx| TextEditor::new_with_settings(window, cx, settings))
        });
        editor.read_with(cx, |editor, cx| {
            (editor.lsp.is_some(), editor.lsp_status(cx))
        })
    }

    /// The regression this module exists for: an editor that was handed a
    /// language server by its context must be attached to it, and say so.
    #[gpui::test]
    fn an_editor_attaches_to_the_language_servers_in_its_context(cx: &mut TestAppContext) {
        let (_host, vcx) = cx.add_window_view(|_, _| Host);
        let servers = vcx.update(|_, app| app.new(LanguageServers::new));
        let settings = EditorSettingsSnapshot::default().with_services(EditorServices {
            language_servers: Some(servers),
            ..Default::default()
        });
        let (attached, status) = status_of(settings, cx);
        assert!(attached, "the handle in the context was ignored");
        assert!(
            status.starts_with("language server: no file"),
            "unexpected status: {status}"
        );
    }

    #[gpui::test]
    fn an_editor_without_a_language_server_says_so(cx: &mut TestAppContext) {
        let (attached, status) = status_of(EditorSettingsSnapshot::default(), cx);
        assert!(!attached);
        assert!(
            status.contains("not attached"),
            "unexpected status: {status}"
        );
    }

    #[test]
    fn hover_contents_come_in_every_lsp_shape() {
        let markup = json!({ "result": { "contents": { "kind": "markdown", "value": "docs" } } });
        assert_eq!(hover_markdown(&markup).as_deref(), Some("docs"));
        let marked_string =
            json!({ "result": { "contents": { "language": "rust", "value": "fn a()" } } });
        assert_eq!(
            hover_markdown(&marked_string).as_deref(),
            Some("```rust\nfn a()\n```")
        );
        let array =
            json!({ "result": { "contents": ["a", { "language": "rust", "value": "b" }] } });
        assert_eq!(
            hover_markdown(&array).as_deref(),
            Some("a\n\n```rust\nb\n```")
        );
        assert_eq!(hover_markdown(&json!({ "result": null })), None);
        assert_eq!(
            hover_markdown(&json!({ "result": { "contents": "  " } })),
            None
        );
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
    fn workspace_symbols_take_both_answer_shapes() {
        let result = json!([
            {
                "name": "Player", "kind": 23, "containerName": "game",
                "location": { "uri": "file:///p/src/player.rs",
                    "range": { "start": { "line": 4, "character": 11 }, "end": { "line": 4, "character": 17 } } }
            },
            { "name": "spawn", "kind": 12, "location": { "uri": "file:///p/src/lib.rs" } },
            { "kind": 12, "location": { "uri": "file:///p/src/lib.rs" } }
        ]);
        let items = workspace_symbols(&result);
        assert_eq!(items.len(), 2, "a symbol without a name is skipped");
        assert_eq!(items[0].label, "Player");
        assert_eq!((items[0].line, items[0].character), (4, 11));
        assert_eq!(items[0].detail, "struct · game · player.rs:5");
        assert_eq!(items[1].label, "spawn");
        assert_eq!(items[1].line, 0, "a WorkspaceSymbol may name only its file");
        assert_eq!(items[1].detail, "function · lib.rs:1");
    }

    #[test]
    fn format_selection_keeps_the_edits_within_its_lines() {
        let edit = |start: (usize, usize), end: (usize, usize)| PositionEdit {
            start: TextPosition {
                line: start.0,
                column: start.1,
            },
            end: TextPosition {
                line: end.0,
                column: end.1,
            },
            new_text: String::new(),
        };
        let lines = 3..=5;
        assert!(edit_within_lines(&edit((3, 0), (3, 4)), &lines));
        assert!(
            edit_within_lines(&edit((5, 2), (6, 0)), &lines),
            "the last line's end"
        );
        assert!(
            !edit_within_lines(&edit((2, 9), (3, 0)), &lines),
            "starts above"
        );
        assert!(
            !edit_within_lines(&edit((5, 0), (6, 1)), &lines),
            "reaches below"
        );
        assert!(!edit_within_lines(&edit((7, 0), (7, 1)), &lines));
    }

    #[test]
    fn reference_results_list_every_location() {
        let result = json!([
            { "uri": "file:///a.rs", "range": { "start": { "line": 1, "character": 2 }, "end": {} } },
            { "uri": "file:///b.rs", "range": { "start": { "line": 7, "character": 0 }, "end": {} } },
        ]);
        assert_eq!(
            all_locations(&result),
            vec![("file:///a.rs".into(), 1, 2), ("file:///b.rs".into(), 7, 0)]
        );
        assert!(all_locations(&Value::Null).is_empty());
    }

    #[test]
    fn document_symbols_flatten_nested_and_flat_answers() {
        let path = std::path::Path::new("/p/lib.rs");
        let range = |line: u32| json!({ "start": { "line": line, "character": 4 }, "end": {} });
        let nested = json!([{
            "name": "Thing", "kind": 23, "range": range(0), "selectionRange": range(0),
            "children": [{ "name": "new", "kind": 6, "detail": "fn() -> Self",
                "range": range(2), "selectionRange": range(2) }],
        }]);
        let items = document_symbols(&nested, path);
        assert_eq!(items.len(), 2);
        assert_eq!(
            (items[0].label.as_str(), items[0].detail.as_str()),
            ("Thing", "struct")
        );
        assert_eq!(items[1].label, "  new");
        assert_eq!(items[1].detail, "method · fn() -> Self");
        assert_eq!((items[1].line, items[1].character), (2, 4));
        assert_eq!(items[1].path, path);

        let flat = json!([{ "name": "run", "kind": 12,
            "location": { "uri": "file:///p/main.rs", "range": range(5) } }]);
        let items = document_symbols(&flat, path);
        assert_eq!(items[0].label, "run");
        assert_eq!(items[0].detail, "function");
        assert_eq!(items[0].line, 5);
        assert_eq!(items[0].path, uri_to_path("file:///p/main.rs"));
        assert_eq!(symbol_kind_name(0), "symbol");
    }

    #[test]
    fn scratch_file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("my::mac"), "my__mac");
        assert_eq!(sanitize_file_name(""), "macro");
    }

    #[test]
    fn definition_results_resolve_to_a_target() {
        let location = json!({ "uri": "file:///a.rs", "range": { "start": { "line": 3, "character": 4 }, "end": {} } });
        assert_eq!(
            first_location(&location),
            Some(("file:///a.rs".into(), 3, 4))
        );
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
