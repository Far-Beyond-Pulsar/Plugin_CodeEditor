mod autocomplete_integration;
mod file_explorer;
pub mod languages;
pub mod text_editor;
mod workspace_panels;

pub use autocomplete_integration::*;
pub use file_explorer::FileExplorer;
pub use languages::*;
pub use text_editor::{TextEditor, TextEditorEvent};
pub use workspace_panels::*;

use gpui::*;
use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use ui::{
    ActiveTheme, StyledExt,
    dock::{Panel, PanelEvent},
    h_flex,
    resizable::{ResizableState, h_resizable, resizable_panel},
    v_flex,
};

use engine_backend::services::rust_analyzer_manager::RustAnalyzerManager;

actions!(script_editor, [SaveCurrentFile, CloseCurrentFile]);

/// Mode for the script editor
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScriptEditorMode {
    /// Normal mode with single editor
    Normal,
    /// Diff mode with side-by-side editors
    Diff,
}

/// Entry for diff mode file list
#[derive(Clone)]
pub struct DiffFileEntry {
    pub path: String,
    pub before_content: String,
    pub after_content: String,
}

pub struct ScriptEditor {
    focus_handle: FocusHandle,
    file_explorer: Entity<FileExplorer>,
    text_editor: Entity<TextEditor>,
    /// Second editor for diff mode
    diff_editor: Entity<TextEditor>,
    /// Resizable state for diff mode split
    diff_resizable_state: Entity<ResizableState>,
    horizontal_resizable_state: Entity<ResizableState>,
    /// Global rust analyzer for LSP support
    rust_analyzer: Option<Entity<RustAnalyzerManager>>,
    /// Current mode (normal or diff)
    mode: ScriptEditorMode,
    /// Diff mode file list with before/after content
    diff_files: Vec<DiffFileEntry>,
    /// Currently selected diff file index
    selected_diff_index: Option<usize>,
    /// Last known scroll offset for left editor (for sync)
    last_left_scroll: Option<Point<Pixels>>,
    /// Last known scroll offset for right editor (for sync)
    last_right_scroll: Option<Point<Pixels>>,
}

impl ScriptEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_settings(
            window,
            cx,
            plugin_editor_api::EditorSettingsSnapshot::default(),
        )
    }

    pub fn new_with_settings(
        window: &mut Window,
        cx: &mut Context<Self>,
        settings: plugin_editor_api::EditorSettingsSnapshot,
    ) -> Self {
        cx.bind_keys([
            KeyBinding::new("ctrl-s", SaveCurrentFile, Some("ScriptEditor")),
            KeyBinding::new("ctrl-w", CloseCurrentFile, Some("ScriptEditor")),
        ]);

        let horizontal_resizable_state = ResizableState::new(cx);
        let diff_resizable_state = ResizableState::new(cx);

        let file_explorer = cx.new(|cx| FileExplorer::new(window, cx));
        // The host's context carries the engine's language server. The working
        // editor attaches to it; the diff view shows historical text under real
        // file names and must never be announced to the server.
        let analyzer = settings.services().language_server.clone();
        let diff_settings = settings
            .clone()
            .with_services(plugin_editor_api::EditorServices::default());
        let text_editor = cx.new(|cx| TextEditor::new_with_settings(window, cx, settings.clone()));
        let diff_editor = cx.new(|cx| TextEditor::new_with_settings(window, cx, diff_settings));

        // Forward text editor events; a go-to-definition into another file
        // opens that file in this panel.
        cx.subscribe_in(
            &text_editor,
            window,
            |this: &mut Self, _editor, event: &TextEditorEvent, window, cx| {
                if let TextEditorEvent::NavigateToLocation {
                    path,
                    line,
                    character,
                } = event
                {
                    this.open_file(path.clone(), window, cx);
                    this.go_to_line(*line as usize + 1, *character as usize + 1, window, cx);
                }
                cx.emit(event.clone());
            },
        )
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            file_explorer,
            text_editor,
            diff_editor,
            diff_resizable_state,
            horizontal_resizable_state,
            rust_analyzer: analyzer,
            mode: ScriptEditorMode::Normal,
            diff_files: Vec::new(),
            selected_diff_index: None,
            last_left_scroll: None,
            last_right_scroll: None,
        }
    }

    /// Attaches the engine's shared rust-analyzer. The editor never starts, stops
    /// or re-roots it: where it runs is the engine's decision. The text editor owns the
    /// document sync, diagnostics and requests from here on.
    pub fn set_rust_analyzer(
        &mut self,
        analyzer: Entity<RustAnalyzerManager>,
        cx: &mut Context<Self>,
    ) {
        self.rust_analyzer = Some(analyzer.clone());
        self.text_editor.update(cx, |editor, cx| {
            editor.set_rust_analyzer(analyzer, cx);
        });
    }

    /// Set the project path and load it in the file explorer
    pub fn set_project_path(
        &mut self,
        project_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(
            "📁 ScriptEditor::set_project_path called with: {:?}",
            project_path
        );
        self.file_explorer.update(cx, |explorer, cx| {
            explorer.open_project(project_path, window, cx);
        });
    }

    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.text_editor.update(cx, |editor, cx| {
            editor.open_file(path, window, cx);
        });
    }

    pub fn get_file_explorer(&self) -> &Entity<FileExplorer> {
        &self.file_explorer
    }

    pub fn get_text_editor(&self) -> &Entity<TextEditor> {
        &self.text_editor
    }

    /// Navigate to a specific line and column in the currently open file
    pub fn go_to_line(
        &mut self,
        line: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_editor.update(cx, |editor, cx| {
            editor.go_to_line(line, column, window, cx);
        });
    }

    fn save_current_file(
        &mut self,
        _action: &SaveCurrentFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_editor.update(cx, |editor, cx| {
            editor.save_current_file(window, cx);
        });
    }

    fn close_current_file(
        &mut self,
        _action: &CloseCurrentFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_editor.update(cx, |editor, cx| {
            editor.close_current_file(window, cx);
        });
    }

    /// Enter diff mode with a list of files and their before/after content
    pub fn enter_diff_mode(
        &mut self,
        files: Vec<DiffFileEntry>,
        project_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mode = ScriptEditorMode::Diff;
        self.diff_files = files;
        self.selected_diff_index = if !self.diff_files.is_empty() {
            Some(0)
        } else {
            None
        };

        // Set up the file explorer to highlight diff files
        self.setup_diff_file_explorer(project_root, window, cx);

        // Load the first file if available
        if let Some(0) = self.selected_diff_index {
            self.load_diff_file(0, window, cx);
        }

        cx.notify();
    }

    /// Set up the file explorer with a flat list of diff files
    fn setup_diff_file_explorer(
        &mut self,
        project_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use std::collections::HashSet;

        // Build a HashSet of diff file paths (full paths)
        let diff_paths: HashSet<PathBuf> = self
            .diff_files
            .iter()
            .map(|entry| project_root.join(&entry.path))
            .collect();

        // Open the real project root and set diff files
        self.file_explorer.update(cx, |explorer, cx| {
            explorer.open_project(project_root, window, cx);
            explorer.set_diff_files(diff_paths, cx);
        });
    }

    /// Exit diff mode and return to normal mode
    pub fn exit_diff_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = ScriptEditorMode::Normal;
        self.diff_files.clear();
        self.selected_diff_index = None;

        // Clear diff files in explorer
        self.file_explorer.update(cx, |explorer, cx| {
            explorer.clear_diff_files(cx);
        });

        cx.notify();
    }

    /// Load a diff file by index
    fn load_diff_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.diff_files.get(index) {
            tracing::debug!(
                "load_diff_file: Loading file {} at index {}",
                file.path,
                index
            );

            let before_content = file.before_content.clone();
            let after_content = file.after_content.clone();
            let path = PathBuf::from(&file.path);

            // Load "before" content into main editor with diff highlighting
            self.text_editor.update(cx, |editor, cx| {
                editor.load_content_with_diff_highlight(
                    path.clone(),
                    before_content.clone(),
                    Some((after_content.clone(), true)), // Compare with after, this is before
                    window,
                    cx,
                );
            });

            // Load "after" content into diff editor with diff highlighting
            self.diff_editor.update(cx, |editor, cx| {
                editor.load_content_with_diff_highlight(
                    path,
                    after_content,
                    Some((before_content, false)), // Compare with before, this is after
                    window,
                    cx,
                );
            });

            self.selected_diff_index = Some(index);
            cx.notify();
            tracing::debug!(
                "load_diff_file: Successfully loaded file at index {}",
                index
            );
        } else {
            tracing::error!("load_diff_file: No file found at index {}", index);
        }
    }

    /// Get the project root from the file explorer
    pub fn get_project_root(&self, cx: &mut Context<Self>) -> Option<PathBuf> {
        self.file_explorer
            .update(cx, |explorer, _cx| explorer.get_project_root().cloned())
    }

    /// Select a diff file by path (called when clicking in file explorer)
    fn select_diff_file_by_path(
        &mut self,
        full_path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!("select_diff_file_by_path: full_path = {:?}", full_path);

        // Get the project root from the file explorer
        let project_root = self
            .file_explorer
            .update(cx, |explorer, _cx| explorer.get_project_root().cloned());

        if let Some(root) = project_root {
            tracing::debug!("Project root: {:?}", root);

            // Convert full path to relative path
            if let Ok(relative_path) = full_path.strip_prefix(&root) {
                let relative_str = relative_path.to_string_lossy().replace("\\", "/");
                tracing::debug!("Relative path: {}", relative_str);

                // Find the diff file with this relative path
                if let Some(index) = self.diff_files.iter().position(|f| {
                    // Normalize both paths for comparison
                    let f_normalized = f.path.replace("\\", "/");
                    tracing::debug!("Comparing '{}' with '{}'", f_normalized, relative_str);
                    f_normalized == relative_str
                }) {
                    tracing::debug!("Found diff file at index {}", index);
                    self.selected_diff_index = Some(index);
                    self.load_diff_file(index, window, cx);
                } else {
                    tracing::warn!("Diff file not found for path: {}", relative_str);
                    tracing::warn!(
                        "Available diff files: {:?}",
                        self.diff_files.iter().map(|f| &f.path).collect::<Vec<_>>()
                    );
                }
            } else {
                tracing::error!(
                    "Failed to strip prefix. Root: {:?}, Full: {:?}",
                    root,
                    full_path
                );
            }
        } else {
            tracing::error!("No project root available");
        }
    }

    /// Update the after_content of a diff file (used when receiving file content from network)
    pub fn update_diff_file_after_content(
        &mut self,
        file_path: &str,
        content: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = self.diff_files.iter_mut().find(|f| f.path == file_path) {
            file.after_content = content.clone();

            // If this is the currently selected file, reload it
            if let Some(idx) = self.selected_diff_index {
                if let Some(selected_file) = self.diff_files.get(idx) {
                    if selected_file.path == file_path {
                        self.load_diff_file(idx, window, cx);
                    }
                }
            }
        }
    }

    /// Update the before_content of a diff file
    pub fn update_diff_file_before_content(
        &mut self,
        file_path: &str,
        content: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = self.diff_files.iter_mut().find(|f| f.path == file_path) {
            file.before_content = content.clone();

            // If this is the currently selected file, reload it
            if let Some(idx) = self.selected_diff_index {
                if let Some(selected_file) = self.diff_files.get(idx) {
                    if selected_file.path == file_path {
                        self.load_diff_file(idx, window, cx);
                    }
                }
            }
        }
    }
}

impl Panel for ScriptEditor {
    fn panel_name(&self) -> &'static str {
        "Script Editor"
    }

    fn panel_file_path(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.text_editor.read(cx).current_file_path()
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        div()
            .child(t!("CodeEditor.ScriptEditor").to_string())
            .into_any_element()
    }

    fn dump(&self, _cx: &App) -> ui::dock::PanelState {
        ui::dock::PanelState {
            panel_name: self.panel_name().to_string(),
            ..Default::default()
        }
    }
}

// Plugin-related methods (called by ScriptEditorWrapper)
impl ScriptEditor {
    pub fn plugin_save(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), plugin_editor_api::PluginError> {
        // Delegate save to the text editor
        self.text_editor.update(cx, |editor, cx| {
            editor.save_current_file(window, cx);
        });
        Ok(())
    }

    pub fn plugin_reload(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), plugin_editor_api::PluginError> {
        // Get current file path from text editor and reload it
        let current_file = self
            .text_editor
            .update(cx, |editor, _cx| editor.current_file_path());

        if let Some(file_path) = current_file {
            self.text_editor.update(cx, |editor, cx| {
                editor.open_file(file_path, window, cx);
            });
            Ok(())
        } else {
            Err(plugin_editor_api::PluginError::Other {
                message: "No file currently open".into(),
            })
        }
    }
}

impl Focusable for ScriptEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for ScriptEditor {}
impl EventEmitter<text_editor::TextEditorEvent> for ScriptEditor {}

impl ScriptEditor {
    /// Synchronize scroll positions between left and right editors in diff mode
    fn sync_diff_editor_scrolls(&mut self, cx: &mut Context<Self>) {
        if self.mode != ScriptEditorMode::Diff {
            return;
        }

        // Get current scroll positions
        let left_scroll = self
            .text_editor
            .update(cx, |editor, cx| editor.get_current_scroll_offset(cx));

        let right_scroll = self
            .diff_editor
            .update(cx, |editor, cx| editor.get_current_scroll_offset(cx));

        // Check if left editor scrolled
        if self.last_left_scroll != left_scroll {
            // Left editor changed, sync right to match
            if let Some(offset) = left_scroll {
                self.diff_editor.update(cx, |editor, cx| {
                    editor.set_scroll_offset(offset, cx);
                });
                self.last_right_scroll = Some(offset);
            }
            self.last_left_scroll = left_scroll;
        }
        // Check if right editor scrolled (and left didn't)
        else if self.last_right_scroll != right_scroll {
            // Right editor changed, sync left to match
            if let Some(offset) = right_scroll {
                self.text_editor.update(cx, |editor, cx| {
                    editor.set_scroll_offset(offset, cx);
                });
                self.last_left_scroll = Some(offset);
            }
            self.last_right_scroll = right_scroll;
        }
    }
}

impl Render for ScriptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Synchronize scroll positions in diff mode
        self.sync_diff_editor_scrolls(cx);

        // Handle file opening based on mode
        if let Some(path) = self
            .file_explorer
            .update(cx, |explorer, _| explorer.get_last_opened_file())
        {
            tracing::debug!("Render: File clicked: {:?}, mode: {:?}", path, self.mode);
            if self.mode == ScriptEditorMode::Diff {
                // In diff mode, select the diff file
                tracing::debug!("Render: Calling select_diff_file_by_path for {:?}", path);
                self.select_diff_file_by_path(&path, window, cx);
            } else {
                // In normal mode, open the file normally
                self.open_file(path, window, cx);
            }
        }

        let editor_panel = match self.mode {
            ScriptEditorMode::Normal => {
                // Single editor
                div()
                    .size_full()
                    .child(self.text_editor.clone())
                    .into_any_element()
            }
            ScriptEditorMode::Diff => {
                // Side-by-side editors with headers
                v_flex()
                    .size_full()
                    .child(
                        // Headers: BEFORE | AFTER
                        h_flex()
                            .w_full()
                            .bg(cx.theme().secondary)
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .flex_1()
                                    .px_3()
                                    .py_1p5()
                                    .border_r_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_medium()
                                            .tracking_wider()
                                            .text_color(cx.theme().danger)
                                            .child(
                                                t!("CodeEditor.BeforeLocal")
                                                    .to_string()
                                                    .to_uppercase(),
                                            ),
                                    ),
                            )
                            .child(
                                div().flex_1().px_3().py_1p5().child(
                                    div()
                                        .text_xs()
                                        .font_medium()
                                        .tracking_wider()
                                        .text_color(cx.theme().success)
                                        .child(
                                            t!("CodeEditor.AfterRemote").to_string().to_uppercase(),
                                        ),
                                ),
                            ),
                    )
                    .child(
                        // Side-by-side editors
                        div().flex_1().child(
                            h_resizable("diff-editors-split")
                                .state(self.diff_resizable_state.clone())
                                .child(
                                    resizable_panel().child(
                                        div()
                                            .size_full()
                                            .bg(cx.theme().background)
                                            .border_r_1()
                                            .border_color(cx.theme().border)
                                            .child(self.text_editor.clone()),
                                    ),
                                )
                                .child(
                                    resizable_panel().child(
                                        div()
                                            .size_full()
                                            .bg(cx.theme().background)
                                            .child(self.diff_editor.clone()),
                                    ),
                                ),
                        ),
                    )
                    .into_any_element()
            }
        };

        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .key_context("ScriptEditor")
            .on_action(cx.listener(Self::save_current_file))
            .on_action(cx.listener(Self::close_current_file))
            .child(
                h_resizable("script-editor-horizontal")
                    .state(self.horizontal_resizable_state.clone())
                    .child(
                        resizable_panel()
                            .size(px(300.))
                            .size_range(px(200.)..px(500.))
                            .child(
                                div()
                                    .size_full()
                                    .bg(cx.theme().sidebar)
                                    .border_r_1()
                                    .border_color(cx.theme().border)
                                    .child(self.file_explorer.clone()),
                            ),
                    )
                    .child(resizable_panel().child(div().size_full().child(editor_panel))),
            )
    }
}
