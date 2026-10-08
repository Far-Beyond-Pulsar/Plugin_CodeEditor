//! GPUI host view for Mockaco's framework-independent editor surface.

use gpui::*;
use mockaco_core::{Grouping, Selection, SelectionSet, Transaction};
use mockaco_diff::DiffRowKind;
use mockaco_gpui::{
    DiffSplitSurface, EditorSurface, InputEvent as MockacoInputEvent, InputRouter, Key, KeyEvent,
    KeyModifiers, SurfaceColor, SurfaceGeometry,
};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use ui::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, PixelsExt as _, Sizable as _, StyledExt,
};
use wgpui_base::ElementExt as _;

#[derive(Clone)]
pub enum TextEditorEvent {
    OpenFolderRequested(PathBuf),
    RunScriptRequested(PathBuf, String),
    DebugScriptRequested(PathBuf),
    FileOpened {
        path: PathBuf,
        content: String,
    },
    FileChanged {
        path: PathBuf,
        content: String,
        version: i32,
    },
    FileSaved {
        path: PathBuf,
        content: String,
    },
    FileClosed {
        path: PathBuf,
    },
    NavigateToLocation {
        path: PathBuf,
        line: u32,
        character: u32,
    },
}

pub struct OpenFile {
    pub path: PathBuf,
    pub surface: EditorSurface,
    pub is_modified: bool,
    pub version: i32,
    diff_lines: HashSet<usize>,
    diff_removed: bool,
    undo_history: Vec<String>,
    redo_history: Vec<String>,
}

impl OpenFile {
    fn new(path: PathBuf, content: String) -> Self {
        let mut surface =
            EditorSurface::new(content, Default::default(), SurfaceGeometry::default());
        Self {
            path,
            surface,
            is_modified: false,
            version: 1,
            diff_lines: HashSet::new(),
            diff_removed: false,
            undo_history: Vec::new(),
            redo_history: Vec::new(),
        }
    }
}

pub struct TextEditor {
    focus_handle: FocusHandle,
    open_files: Vec<OpenFile>,
    current_file_index: Option<usize>,
    input_router: InputRouter,
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
        _settings: plugin_editor_api::EditorSettingsSnapshot,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            open_files: Vec::new(),
            current_file_index: None,
            input_router: InputRouter::default(),
        }
    }

    pub fn set_rust_analyzer(
        &mut self,
        _analyzer: Entity<engine_backend::services::rust_analyzer_manager::RustAnalyzerManager>,
        _cx: &mut Context<Self>,
    ) {
        // The ScriptEditor event bridge forwards Mockaco document changes.
    }

    pub fn open_file(&mut self, path: PathBuf, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.open_files.iter().position(|file| file.path == path) {
            self.current_file_index = Some(index);
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
        self.open_files
            .push(OpenFile::new(path.clone(), content.clone()));
        self.current_file_index = Some(self.open_files.len() - 1);
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
                let lines = diff
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
        if let Some(index) = self.open_files.iter().position(|file| file.path == path) {
            let mut file = OpenFile::new(path, content);
            file.diff_lines = diff_lines;
            file.diff_removed = diff_removed;
            self.open_files[index] = file;
            self.current_file_index = Some(index);
        } else {
            let mut file = OpenFile::new(path, content);
            file.diff_lines = diff_lines;
            file.diff_removed = diff_removed;
            self.open_files.push(file);
            self.current_file_index = Some(self.open_files.len() - 1);
        }
        cx.notify();
    }

    pub fn save_current_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(file) = self.current_file_mut() else {
            return false;
        };
        let path = file.path.clone();
        let content = file.surface.document().text().to_owned();
        match fs::write(&path, &content) {
            Ok(()) => {
                file.is_modified = false;
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

    pub fn close_current_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.current_file_index else {
            return;
        };
        if self.open_files[index].is_modified {
            return;
        }
        let file = self.open_files.remove(index);
        cx.emit(TextEditorEvent::FileClosed { path: file.path });
        self.current_file_index = if self.open_files.is_empty() {
            None
        } else {
            Some(index.min(self.open_files.len() - 1))
        };
        cx.notify();
    }

    pub fn go_to_line(
        &mut self,
        line: usize,
        column: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = self.current_file_mut() {
            let map = file.surface.document().position_map();
            let line_index = line.saturating_sub(1);
            if let (Ok(start), Ok(end)) = (map.line_start(line_index), map.line_end(line_index)) {
                let caret = start.saturating_add(column.saturating_sub(1)).min(end);
                file.surface
                    .editor_mut()
                    .set_selections(SelectionSet::new([Selection::caret(caret)]));
                file.surface.scroll_to(line_index.saturating_sub(1), 0);
            }
        }
        cx.notify();
    }

    pub fn current_file_path(&self) -> Option<PathBuf> {
        self.current_file().map(|file| file.path.clone())
    }

    pub fn get_current_scroll_offset(&self, _cx: &mut Context<Self>) -> Option<Point<Pixels>> {
        let file = self.current_file()?;
        let scroll = file.surface.scroll();
        Some(point(
            px(scroll.horizontal_columns as f32 * 8.0),
            px(scroll.top_row as f32 * 20.0),
        ))
    }

    pub fn set_scroll_offset(&mut self, offset: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(file) = self.current_file_mut() {
            let row = (offset.y.as_f32() / 20.0).max(0.0) as usize;
            let column = (offset.x.as_f32() / 8.0).max(0.0) as usize;
            file.surface.scroll_to(row, column);
        }
        cx.notify();
    }

    fn current_file(&self) -> Option<&OpenFile> {
        self.current_file_index
            .and_then(|index| self.open_files.get(index))
    }

    fn current_file_mut(&mut self) -> Option<&mut OpenFile> {
        self.current_file_index
            .and_then(|index| self.open_files.get_mut(index))
    }

    fn route_input(&mut self, event: MockacoInputEvent, cx: &mut Context<Self>) {
        let mut changed = None;
        if let Some(index) = self.current_file_index {
            let (input_router, open_files) = (&mut self.input_router, &mut self.open_files);
            if let Some(file) = open_files.get_mut(index) {
                let before = file.surface.document().text().to_owned();
                if let Ok(outcome) = input_router.route(&mut file.surface, event) {
                    if outcome.document_changed {
                        file.undo_history.push(before);
                        file.redo_history.clear();
                        file.is_modified = true;
                        file.version = file.version.saturating_add(1);
                        changed = Some((
                            file.path.clone(),
                            file.surface.document().text().to_owned(),
                            file.version,
                        ));
                    }
                }
            }
        }
        if let Some((path, content, version)) = changed {
            cx.emit(TextEditorEvent::FileChanged {
                path,
                content,
                version,
            });
        }
        cx.notify();
    }

    fn restore_history(&mut self, redo: bool, cx: &mut Context<Self>) {
        let mut changed = None;
        if let Some(file) = self.current_file_mut() {
            let target = if redo {
                file.redo_history.pop()
            } else {
                file.undo_history.pop()
            };
            if let Some(target) = target {
                let current = file.surface.document().text().to_owned();
                if redo {
                    file.undo_history.push(current.clone());
                } else {
                    file.redo_history.push(current.clone());
                }
                let transaction = Transaction::new().replace(0..current.len(), target.clone());
                if file
                    .surface
                    .apply_transaction(&transaction, Grouping::Separate)
                    .is_ok()
                {
                    let end = target.len();
                    file.surface
                        .editor_mut()
                        .set_selections(SelectionSet::caret(end));
                    file.is_modified = true;
                    file.version = file.version.saturating_add(1);
                    changed = Some((file.path.clone(), target, file.version));
                }
            }
        }
        if let Some((path, content, version)) = changed {
            cx.emit(TextEditorEvent::FileChanged {
                path,
                content,
                version,
            });
        }
        cx.notify();
    }

    fn move_vertical(&mut self, direction: isize, extend: bool, cx: &mut Context<Self>) {
        if let Some(file) = self.current_file_mut() {
            let snapshot = file.surface.document();
            let text = snapshot.text();
            let map = snapshot.position_map();
            let selections = file.surface.editor().selections().selections();
            let moved = selections
                .iter()
                .map(|selection| {
                    let head = selection.head.min(text.len());
                    let position = map.byte_to_line_column(head).ok()?;
                    let line = (position.line as isize + direction)
                        .clamp(0, map.line_count().saturating_sub(1) as isize)
                        as usize;
                    let start = map.line_start(line).ok()?;
                    let end = map.line_end(line).ok()?;
                    let mut caret = (start + position.column).min(end);
                    while caret > start && !text.is_char_boundary(caret) {
                        caret -= 1;
                    }
                    Some(if extend {
                        Selection::range(selection.anchor, caret)
                    } else {
                        Selection::caret(caret)
                    })
                })
                .collect::<Option<Vec<_>>>();
            if let Some(moved) = moved {
                file.surface
                    .editor_mut()
                    .set_selections(SelectionSet::new(moved));
            }
        }
        cx.notify();
    }

    fn selected_text(&self) -> Option<String> {
        let file = self.current_file()?;
        let text = file.surface.document().text();
        let selections = file.surface.editor().selections().selections();
        Some(
            selections
                .iter()
                .filter_map(|selection| {
                    let range = selection.ordered_range();
                    (!range.is_empty()).then(|| text.get(range).unwrap_or_default())
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            command: keystroke.modifiers.platform,
        };
        if modifiers.control || modifiers.command {
            if let Some(key) = keystroke.key_char.as_deref() {
                if key.eq_ignore_ascii_case("s") {
                    self.save_current_file(_window, cx);
                    return;
                }
                if key.eq_ignore_ascii_case("z") || key.eq_ignore_ascii_case("y") {
                    let redo = key.eq_ignore_ascii_case("y")
                        || (key.eq_ignore_ascii_case("z") && modifiers.shift);
                    self.restore_history(redo, cx);
                    return;
                }
                if key.eq_ignore_ascii_case("c") {
                    if let Some(text) = self.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return;
                }
                if key.eq_ignore_ascii_case("x") {
                    if let Some(text) = self.selected_text().filter(|text| !text.is_empty()) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        self.route_input(
                            MockacoInputEvent::Key(KeyEvent {
                                key: Key::Delete,
                                modifiers: KeyModifiers::default(),
                            }),
                            cx,
                        );
                    }
                    return;
                }
                if key.eq_ignore_ascii_case("v") {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.route_input(MockacoInputEvent::Text(text), cx);
                    }
                    return;
                }
            }
        }

        let key = match keystroke.key.as_str() {
            "backspace" => Key::Backspace,
            "delete" => Key::Delete,
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "escape" => Key::Escape,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "up" => {
                self.move_vertical(-1, modifiers.shift, cx);
                return;
            }
            "down" => {
                self.move_vertical(1, modifiers.shift, cx);
                return;
            }
            "pageup" => {
                self.move_vertical(-32, modifiers.shift, cx);
                return;
            }
            "pagedown" => {
                self.move_vertical(32, modifiers.shift, cx);
                return;
            }
            _ => {
                if !modifiers.control && !modifiers.command && !modifiers.alt {
                    if let Some(text) = keystroke.key_char.clone() {
                        self.route_input(MockacoInputEvent::Text(text), cx);
                        return;
                    }
                }
                Key::Unsupported(keystroke.key.clone())
            }
        };
        self.route_input(MockacoInputEvent::Key(KeyEvent { key, modifiers }), cx);
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (vertical, horizontal) = match event.delta {
            ScrollDelta::Lines(delta) => (-(delta.y.round() as isize), delta.x.round() as isize),
            ScrollDelta::Pixels(delta) => (
                (delta.y.as_f32() / 20.0).round() as isize,
                (delta.x.as_f32() / 8.0).round() as isize,
            ),
        };
        self.route_input(
            MockacoInputEvent::Scroll {
                vertical,
                horizontal,
            },
            cx,
        );
    }

    fn render_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = self.current_file_mut() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Open a source file to begin editing")
                .into_any_element();
        };

        let theme = surface_theme(cx.theme());
        if file.surface.theme() != theme {
            file.surface.set_theme(theme);
        }
        let frame = file.surface.render_frame();
        let surface_theme = frame.theme;
        let mut rows = v_flex().size_full().overflow_hidden();
        for row in frame.rows {
            let gutter = frame
                .gutter
                .rows
                .iter()
                .find(|gutter| gutter.display_row == row.display_row);
            let line_number = gutter.map(|gutter| gutter.line_number).unwrap_or(0);
            let is_active = frame
                .carets
                .iter()
                .any(|caret| caret.display_row == row.display_row && caret.primary);
            let row_background = if file.diff_lines.contains(&row.buffer_line) {
                if file.diff_removed {
                    with_alpha(surface_color_from_hsla(cx.theme().danger), 115)
                } else {
                    with_alpha(surface_color_from_hsla(cx.theme().success), 115)
                }
            } else if is_active {
                with_alpha(surface_color_from_hsla(cx.theme().list_active), 150)
            } else {
                surface_theme.background
            };
            let mut code = div()
                .relative()
                .flex_1()
                .h(px(row.height))
                .pl(px(12.0))
                .font_family("JetBrains Mono")
                .text_color(surface_color(surface_theme.foreground))
                .bg(surface_color(row_background))
                .whitespace_nowrap();
            for selection in frame
                .selections
                .iter()
                .filter(|selection| selection.display_row == row.display_row)
            {
                code = code.child(
                    div()
                        .absolute()
                        .left(px(selection.x + 12.0))
                        .top(px(selection.y - row.y))
                        .w(px(selection.width.max(1.0)))
                        .h(px(selection.height))
                        .bg(surface_color(if selection.primary {
                            surface_theme.primary_selection
                        } else {
                            surface_theme.selection
                        })),
                );
            }
            code = code.child(row.text.clone());
            for caret in frame
                .carets
                .iter()
                .filter(|caret| caret.display_row == row.display_row)
            {
                code = code.child(
                    div()
                        .absolute()
                        .left(px(caret.x + 12.0))
                        .top(px(caret.y - row.y))
                        .w(px(if caret.primary { 2.0 } else { 1.0 }))
                        .h(px(caret.height))
                        .bg(surface_color(surface_theme.caret)),
                );
            }
            rows = rows.child(
                h_flex()
                    .h(px(row.height))
                    .w_full()
                    .child(
                        div()
                            .w(px(frame.gutter.line_number_width as f32 * 8.0 + 24.0))
                            .pr_2()
                            .text_right()
                            .font_family("JetBrains Mono")
                            .text_color(surface_color(surface_theme.gutter_foreground))
                            .bg(surface_color(surface_theme.gutter_background))
                            .child(line_number.to_string()),
                    )
                    .child(code),
            );
        }

        let editor_entity = cx.entity();
        div()
            .size_full()
            .overflow_hidden()
            .font_family("JetBrains Mono")
            .text_size(px(14.0))
            .line_height(px(20.0))
            .bg(surface_color(theme.background))
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle, cx);
                }),
            )
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(rows)
            .on_prepaint(move |bounds, _, cx| {
                let width = bounds.size.width.as_f32().max(1.0);
                let height = bounds.size.height.as_f32().max(1.0);
                editor_entity.update(cx, |editor, cx| {
                    let Some(file) = editor.current_file_mut() else {
                        return;
                    };

                    let mut geometry = file.surface.geometry();
                    geometry.width = width;
                    geometry.height = height;
                    if file.surface.geometry() != geometry {
                        file.surface.set_geometry(geometry);
                        cx.notify();
                    }
                });
            })
            .into_any_element()
    }
}

fn surface_color(color: SurfaceColor) -> Hsla {
    Rgba {
        r: f32::from(color.red) / 255.0,
        g: f32::from(color.green) / 255.0,
        b: f32::from(color.blue) / 255.0,
        a: f32::from(color.alpha) / 255.0,
    }
    .into()
}

fn surface_theme(theme: &ui::Theme) -> mockaco_gpui::SurfaceTheme {
    mockaco_gpui::SurfaceTheme {
        background: surface_color_from_hsla(theme.background),
        gutter_background: surface_color_from_hsla(theme.muted),
        foreground: surface_color_from_hsla(theme.foreground),
        gutter_foreground: surface_color_from_hsla(theme.muted_foreground),
        selection: surface_color_from_hsla(theme.selection),
        primary_selection: surface_color_from_hsla(theme.accent),
        caret: surface_color_from_hsla(theme.caret),
        decoration: surface_color_from_hsla(theme.warning),
    }
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

impl EventEmitter<TextEditorEvent> for TextEditor {}

impl Focusable for TextEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.open_files.iter().enumerate().fold(
            h_flex()
                .w_full()
                .h(px(34.0))
                .gap_1()
                .border_b_1()
                .border_color(cx.theme().border),
            |tabs, (index, file)| {
                let name = file
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Untitled");
                let label = if file.is_modified {
                    format!("{name} •")
                } else {
                    name.to_owned()
                };
                tabs.child(
                    Button::new(format!("file-tab-{index}"))
                        .label(label)
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.current_file_index = Some(index);
                            cx.notify();
                        })),
                )
            },
        );
        let dirty = self.current_file().is_some_and(|file| file.is_modified);
        let toolbar = h_flex()
            .w_full()
            .h(px(36.0))
            .px_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Mockaco editor"),
            )
            .child(
                Button::new("save-file")
                    .label(if dirty { "Save •" } else { "Save" })
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_current_file(window, cx);
                    })),
            );
        let body = self.render_editor(cx);
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(toolbar)
            .child(tabs)
            .child(div().flex_1().min_h_0().child(body))
    }
}
