//! Pulsar host for Mockaco.
//!
//! Mockaco owns everything about the editing surface: text layout, syntax
//! highlighting, folding, caret/selection, scrolling, scrollbars, minimap and
//! input. This module only does what is specific to Pulsar: the set of open
//! documents, file I/O, bridging the UI theme into Mockaco's theme, and
//! forwarding document events to the rest of the plugin.
//!
//! It is headless: it renders nothing. Each open document is shown by a
//! [`super::FilePanel`] living in the editor's dock, which is what gives the
//! editor real tabs, splits and drag-and-drop.

mod lsp;

use gpui::*;
use mockaco_diff::DiffRowKind;
use mockaco_gpui::native::{EditorEvent, WgpuiEditorView};
use mockaco_gpui::{DiffSplitSurface, EditorSurface, Language, SurfaceColor, SurfaceGeometry, SurfaceTheme};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use ui::ActiveTheme as _;

actions!(
    mockaco_editor,
    [
        EditorUndo,
        EditorRedo,
        EditorCut,
        EditorCopy,
        EditorPaste,
        EditorSelectAll,
        EditorUnfoldAll,
        EditorSave,
        EditorCopyPath,
    ]
);

const FONT_FAMILY: &str = "JetBrains Mono";
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;
#[derive(Clone)]
pub enum TextEditorEvent {
    OpenFolderRequested(PathBuf),
    RunScriptRequested(PathBuf, String),
    DebugScriptRequested(PathBuf),
    FileOpened {
        path: PathBuf,
        content: String,
    },
    FileSaved {
        path: PathBuf,
        content: String,
    },
    FileClosed {
        path: PathBuf,
    },
    /// Go to definition landed in another file: zero-based line and UTF-16
    /// column. The panel that owns this editor opens it.
    NavigateToLocation {
        path: PathBuf,
        line: u32,
        character: u32,
    },
}

pub struct OpenFile {
    pub path: PathBuf,
    pub view: Entity<WgpuiEditorView>,
    pub is_modified: bool,
    /// Document version announced to the language server; bumped per edit.
    pub version: i32,
    /// The language server has this document open.
    lsp_open: bool,
    _subscription: Subscription,
}

pub struct TextEditor {
    focus_handle: FocusHandle,
    open_files: Vec<OpenFile>,
    current_file_index: Option<usize>,
    applied_theme: Option<SurfaceTheme>,
    generating: bool,
    /// The engine's shared rust-analyzer, when this editor has one.
    lsp: Option<lsp::LspClient>,
}

impl TextEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_settings(
            window,
            cx,
            plugin_editor_api::EditorSettingsSnapshot::default(),
        )
    }

    pub fn new_with_settings(
        _window: &mut Window,
        cx: &mut Context<Self>,
        settings: plugin_editor_api::EditorSettingsSnapshot,
    ) -> Self {
        let mut editor = Self {
            focus_handle: cx.focus_handle(),
            open_files: Vec::new(),
            current_file_index: None,
            applied_theme: None,
            generating: false,
            lsp: None,
        };
        // The language server arrives through the plugin API's context.
        if let Some(analyzer) = settings.services().language_server.clone() {
            editor.set_rust_analyzer(analyzer, cx);
        }
        editor
    }

    fn create_view(
        &mut self,
        path: &PathBuf,
        content: String,
        cx: &mut Context<Self>,
    ) -> Entity<WgpuiEditorView> {
        let theme = surface_theme(cx.theme());
        let surface = EditorSurface::with_language(
            content,
            Default::default(),
            SurfaceGeometry::default(),
            Language::from_path(path),
        );
        let view = cx.new(|cx| {
            let mut view = WgpuiEditorView::new(surface, cx);
            view.set_font(FONT_FAMILY, FONT_SIZE, LINE_HEIGHT);
            view
        });
        view.update(cx, |view, cx| view.set_theme(theme, cx));
        if self.lsp.is_some() && lsp::is_rust(path) {
            view.update(cx, |view, cx| view.set_language_intelligence(true, cx));
        }
        view
    }

    fn push_file(&mut self, path: PathBuf, view: Entity<WgpuiEditorView>, cx: &mut Context<Self>) {
        let event_path = path.clone();
        let subscription = cx.subscribe(
            &view,
            move |this: &mut Self, view: Entity<WgpuiEditorView>, event: &EditorEvent, cx| {
                this.on_editor_event(&event_path, &view, event, cx);
            },
        );
        self.open_files.push(OpenFile {
            path,
            view,
            is_modified: false,
            version: 1,
            lsp_open: false,
            _subscription: subscription,
        });
        self.current_file_index = Some(self.open_files.len() - 1);
    }

    fn on_editor_event(
        &mut self,
        path: &PathBuf,
        _view: &Entity<WgpuiEditorView>,
        event: &EditorEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.open_files.iter().position(|file| &file.path == path) else {
            return;
        };
        match event {
            EditorEvent::Changed => {
                let file = &mut self.open_files[index];
                let was_modified = file.is_modified;
                file.is_modified = true;
                file.version = file.version.saturating_add(1);
                // The server is told what changed, not the whole file: the
                // editor logged the edits and this drains them.
                self.lsp_sync(index, cx);
                // Only the tab's dirty marker depends on this.
                if !was_modified {
                    cx.notify();
                }
            }
            EditorEvent::SaveRequested => {
                self.save_file_at(index, cx);
            }
            lsp_event => self.on_lsp_event(index, lsp_event, cx),
        }
    }

    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.open_files.iter().position(|file| file.path == path) {
            self.current_file_index = Some(index);
            self.focus_current(window, cx);
            cx.notify();
            return;
        }

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!("Could not read {:?}: {}", path, error);
                String::new()
            }
        };
        let view = self.create_view(&path, content.clone(), cx);
        self.push_file(path.clone(), view, cx);
        self.lsp_open_pending(cx);
        self.focus_current(window, cx);
        cx.emit(TextEditorEvent::FileOpened { path, content });
        cx.notify();
    }

    pub fn load_content_with_diff_highlight(
        &mut self,
        path: PathBuf,
        content: String,
        other_content: Option<(String, bool)>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (diff_lines, diff_removed) = match other_content {
            Some((other, current_is_original)) => {
                let (original, modified) = if current_is_original {
                    (content.clone(), other)
                } else {
                    (other, content.clone())
                };
                let original = mockaco_core::Document::new(original);
                let mut diff = DiffSplitSurface::new(&original.snapshot(), modified);
                let diff_row_count = diff.diff().rows().len().max(1);
                diff.set_viewport(diff_row_count);
                let lines: HashSet<usize> = diff
                    .diff()
                    .rows()
                    .iter()
                    .filter(|row| row.kind != DiffRowKind::Equal)
                    .filter_map(|row| {
                        if current_is_original {
                            row.original.as_ref().map(|line| line.line)
                        } else {
                            row.modified.as_ref().map(|line| line.line)
                        }
                    })
                    .collect();
                (lines, current_is_original)
            }
            None => (HashSet::new(), false),
        };

        let tint = {
            let theme = cx.theme();
            let base = if diff_removed { theme.danger } else { theme.success };
            with_alpha(surface_color_from_hsla(base), 115)
        };
        let backgrounds: HashMap<usize, SurfaceColor> =
            diff_lines.into_iter().map(|line| (line, tint)).collect();

        if let Some(index) = self.open_files.iter().position(|file| file.path == path) {
            let file = &mut self.open_files[index];
            file.is_modified = false;
            file.view.update(cx, |view, cx| {
                view.replace_text(content, cx);
                view.set_line_backgrounds(backgrounds, cx);
            });
            self.current_file_index = Some(index);
            self.lsp_sync(index, cx);
        } else {
            let view = self.create_view(&path, content, cx);
            view.update(cx, |view, cx| view.set_line_backgrounds(backgrounds, cx));
            self.push_file(path, view, cx);
        }
        cx.notify();
    }

    fn save_file_at(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(file) = self.open_files.get(index) else {
            return false;
        };
        let path = file.path.clone();
        let content = file.view.read(cx).text();
        match fs::write(&path, &content) {
            Ok(()) => {
                self.open_files[index].is_modified = false;
                self.lsp_saved(index, &content, cx);
                cx.emit(TextEditorEvent::FileSaved { path, content });
                cx.notify();
                true
            }
            Err(error) => {
                tracing::error!("Could not save {:?}: {}", path, error);
                false
            }
        }
    }

    pub fn save_current_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.current_file_index {
            Some(index) => self.save_file_at(index, cx),
            None => false,
        }
    }

    pub fn close_current_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.current_file_index else {
            return;
        };
        if self.open_files[index].is_modified {
            return;
        }
        let file = self.open_files.remove(index);
        self.lsp_close(&file, cx);
        cx.emit(TextEditorEvent::FileClosed { path: file.path });
        self.current_file_index = if self.open_files.is_empty() {
            None
        } else {
            Some(index.min(self.open_files.len() - 1))
        };
        self.focus_current(window, cx);
        cx.notify();
    }

    /// Moves the caret to a 1-based line/column and centers it.
    pub fn go_to_line(
        &mut self,
        line: usize,
        column: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = self.current_file() {
            file.view.update(cx, |view, cx| {
                view.go_to(line.saturating_sub(1), column.saturating_sub(1), cx)
            });
        }
    }

    pub fn current_file_path(&self) -> Option<PathBuf> {
        self.current_file().map(|file| file.path.clone())
    }

    pub fn get_current_scroll_offset(&self, cx: &mut Context<Self>) -> Option<Point<Pixels>> {
        let file = self.current_file()?;
        let (x, y) = file.view.read(cx).scroll_offset();
        Some(point(px(x), px(y)))
    }

    pub fn set_scroll_offset(&mut self, offset: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(file) = self.current_file() {
            file.view.update(cx, |view, cx| {
                view.set_scroll_offset(offset.x.to_f32(), offset.y.to_f32(), cx)
            });
        }
    }

    /// The editor view of the document at `path`, if it is open.
    pub fn view_for(&self, path: &std::path::Path) -> Option<Entity<WgpuiEditorView>> {
        self.open_files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.view.clone())
    }

    /// The editor view of the current document, if any.
    pub fn current_view(&self) -> Option<Entity<WgpuiEditorView>> {
        self.current_file().map(|file| file.view.clone())
    }

    pub fn has_open_files(&self) -> bool {
        !self.open_files.is_empty()
    }

    pub fn is_generating(&self) -> bool {
        self.generating
    }

    pub fn is_modified(&self, path: &std::path::Path) -> bool {
        self.open_files
            .iter()
            .any(|file| file.path == path && file.is_modified)
    }

    pub fn current_is_modified(&self) -> bool {
        self.current_file().is_some_and(|file| file.is_modified)
    }

    /// Makes the document at `path` the current one (the dock activated or
    /// focused its tab).
    pub fn activate_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let Some(index) = self.open_files.iter().position(|file| file.path == path) else {
            return;
        };
        if self.current_file_index != Some(index) {
            self.current_file_index = Some(index);
            cx.notify();
        }
    }

    pub fn save_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) -> bool {
        match self.open_files.iter().position(|file| file.path == path) {
            Some(index) => self.save_file_at(index, cx),
            None => false,
        }
    }

    /// Drops the document at `path` (its dock tab was closed) and tells the
    /// language server. Idempotent.
    pub fn close_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let Some(index) = self.open_files.iter().position(|file| file.path == path) else {
            return;
        };
        let file = self.open_files.remove(index);
        self.lsp_close(&file, cx);
        cx.emit(TextEditorEvent::FileClosed { path: file.path });
        self.current_file_index = match self.current_file_index {
            _ if self.open_files.is_empty() => None,
            Some(current) if current > index => Some(current - 1),
            Some(current) => Some(current.min(self.open_files.len() - 1)),
            None => None,
        };
        cx.notify();
    }

    fn current_file(&self) -> Option<&OpenFile> {
        self.current_file_index
            .and_then(|index| self.open_files.get(index))
    }

    fn focus_current(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.current_file() {
            let handle = file.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Writes a ~1,000,000-line Rust file to the temp directory on a
    /// background thread and opens it, to stress-test the editor.
    pub fn generate_stress_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.generating {
            return;
        }
        self.generating = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            let path = std::env::temp_dir().join("pulsar_mockaco_1m_lines.rs");
            let result = write_stress_file(&path, 1_000_000).map(|_| path);
            result
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.generating = false;
                match result {
                    Ok(path) => this.open_file(path, window, cx),
                    Err(error) => {
                        tracing::error!("Could not generate stress file: {}", error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Pushes the active UI theme into every open editor when it changes.
    pub(crate) fn apply_theme(&mut self, cx: &mut Context<Self>) {
        let theme = surface_theme(cx.theme());
        if self.applied_theme.as_ref() == Some(&theme) {
            return;
        }
        for file in &self.open_files {
            file.view
                .update(cx, |view, cx| view.set_theme(theme.clone(), cx));
        }
        self.applied_theme = Some(theme);
    }
}

/// Generates varied, realistic Rust (structs, impls, enums, traits, tests,
/// comments, strings, numbers) so highlighting and folding have real work.
fn write_stress_file(path: &std::path::Path, target_lines: usize) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, fs::File::create(path)?);
    let mut lines = 0usize;
    let mut module = 0usize;
    writeln!(out, "//! Generated stress file: {target_lines} lines.\n")?;
    lines += 2;
    while lines < target_lines {
        let m = module;
        module += 1;
        let chunk = format!(
            r#"/// Module {m} documentation.
pub mod module_{m} {{
    use std::collections::HashMap;

    #[derive(Debug, Clone, PartialEq)]
    pub struct Item{m} {{
        pub id: u64,
        pub name: String,
        pub weight: f32,
        pub tags: Vec<&'static str>,
    }}

    #[derive(Debug, Clone, Copy)]
    pub enum Kind{m} {{
        Small,
        Large {{ factor: u32 }},
        Custom(i64),
    }}

    pub trait Describe{m} {{
        fn describe(&self) -> String;
    }}

    impl Describe{m} for Item{m} {{
        fn describe(&self) -> String {{
            // Build a human readable description.
            format!("item {{}} ({{}}) weighs {{:.2}}", self.id, self.name, self.weight)
        }}
    }}

    impl Item{m} {{
        pub fn new(id: u64, name: &str) -> Self {{
            Self {{
                id,
                name: name.to_string(),
                weight: {m} as f32 * 0.5 + 1.25,
                tags: vec!["generated", "stress"],
            }}
        }}

        pub fn classify(&self) -> Kind{m} {{
            match self.id % 3 {{
                0 => Kind{m}::Small,
                1 => Kind{m}::Large {{ factor: {m} }},
                _ => Kind{m}::Custom(-(self.id as i64)),
            }}
        }}

        /* Sum weights across a lookup table. */
        pub fn total(table: &HashMap<u64, Item{m}>) -> f32 {{
            let mut sum = 0.0;
            for (key, item) in table {{
                if *key % 2 == 0 {{
                    sum += item.weight;
                }} else {{
                    sum -= 0.5;
                }}
            }}
            sum
        }}
    }}

    #[cfg(test)]
    mod tests {{
        use super::*;

        #[test]
        fn builds_item() {{
            let item = Item{m}::new({m}, "sample");
            assert_eq!(item.id, {m});
            assert!(item.describe().contains("sample"));
        }}
    }}
}}

"#
        );
        out.write_all(chunk.as_bytes())?;
        lines += chunk.bytes().filter(|b| *b == b'\n').count();
    }
    out.flush()
}

fn surface_color_from_hsla(color: Hsla) -> SurfaceColor {
    let rgba: Rgba = color.into();
    SurfaceColor::rgba(
        (rgba.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.a.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn with_alpha(color: SurfaceColor, alpha: u8) -> SurfaceColor {
    SurfaceColor { alpha, ..color }
}

/// Mockaco token kinds mapped to the UI theme's syntax names, first match
/// wins. Kinds the theme does not style keep Mockaco's built-in color.
const SYNTAX_MAP: &[(&str, &[&str])] = &[
    ("keyword", &["keyword"]),
    ("keyword.operator", &["keyword", "operator"]),
    ("function", &["function"]),
    ("function.method", &["function"]),
    ("function.macro", &["function", "preproc"]),
    ("type", &["type", "enum"]),
    ("type.builtin", &["type"]),
    ("constructor", &["constructor", "type"]),
    ("string", &["string"]),
    ("character", &["string"]),
    ("escape", &["string.escape", "string"]),
    ("comment", &["comment"]),
    ("comment.documentation", &["comment.doc", "comment"]),
    ("constant", &["constant", "boolean"]),
    ("constant.builtin", &["constant", "boolean"]),
    ("number", &["number"]),
    ("attribute", &["attribute"]),
    ("property", &["property"]),
    ("variable", &["variable"]),
    ("variable.parameter", &["variable"]),
    ("variable.builtin", &["variable.special", "variable"]),
    ("operator", &["operator"]),
    ("punctuation.bracket", &["punctuation.bracket", "punctuation"]),
    ("punctuation.delimiter", &["punctuation.delimiter", "punctuation"]),
    ("punctuation.list_marker", &["punctuation.list_marker", "punctuation"]),
    ("label", &["label"]),
    ("tag", &["tag"]),
    ("title", &["title"]),
    ("link_text", &["link_text"]),
    ("link_uri", &["link_uri"]),
    ("emphasis", &["emphasis"]),
    ("emphasis.strong", &["emphasis.strong"]),
];

fn syntax_theme(theme: &ui::Theme) -> mockaco_language::Theme {
    let mut syntax = SurfaceTheme::default().syntax;
    for (kind, names) in SYNTAX_MAP {
        for name in *names {
            let Some(style) = theme.highlight_theme.style(name) else {
                continue;
            };
            let Some(color) = style.color else {
                continue;
            };
            let color = surface_color_from_hsla(color.solid);
            syntax.set_token_style(
                *kind,
                mockaco_language::TokenStyle {
                    foreground: mockaco_language::Rgba {
                        red: color.red,
                        green: color.green,
                        blue: color.blue,
                        alpha: color.alpha,
                    },
                    background: None,
                    font: mockaco_language::FontStyle {
                        bold: style.font_weight.is_some_and(|weight| weight >= FontWeight::SEMIBOLD),
                        italic: matches!(style.font_style, Some(FontStyle::Italic)),
                        underline: false,
                    },
                },
            );
            break;
        }
    }
    syntax
}

fn surface_theme(theme: &ui::Theme) -> SurfaceTheme {
    let editor = &theme.highlight_theme.style;
    let active_line = editor
        .editor_active_line
        .map(surface_color_from_hsla)
        .unwrap_or_else(|| with_alpha(surface_color_from_hsla(theme.list_active), 90));
    let gutter_foreground = editor
        .editor_line_number
        .map(surface_color_from_hsla)
        .unwrap_or_else(|| surface_color_from_hsla(theme.muted_foreground));
    let background = surface_color_from_hsla(theme.background);
    SurfaceTheme {
        background,
        gutter_background: background,
        foreground: surface_color_from_hsla(theme.foreground),
        gutter_foreground,
        selection: surface_color_from_hsla(theme.selection),
        primary_selection: surface_color_from_hsla(theme.selection),
        caret: surface_color_from_hsla(theme.caret),
        decoration: surface_color_from_hsla(theme.warning),
        active_line,
        syntax: syntax_theme(theme),
    }
}

impl EventEmitter<TextEditorEvent> for TextEditor {}

impl Focusable for TextEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.current_file() {
            Some(file) => file.view.focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}
