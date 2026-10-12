//! Dock panels for the Script Editor.

use super::file_explorer::FileExplorer;
use super::text_editor::{
    EditorCopy, EditorCopyPath, EditorCut, EditorPaste, EditorRedo, EditorSave, EditorSelectAll,
    EditorUndo, EditorUnfoldAll, TextEditor,
};
use gpui::*;
use mockaco_gpui::native::WgpuiEditorView;
use rust_i18n::t;
use std::path::PathBuf;
use ui::{
    context_menu::ContextMenuExt as _,
    dock::{Panel, PanelEvent},
    ActiveTheme, IconName, v_flex,
};

/// One open document as a dock tab. The document itself (text, language
/// server sync, dirty state) stays in [`TextEditor`]; this is the part the dock
/// can place, split and drag.
pub struct FilePanel {
    text_editor: WeakEntity<TextEditor>,
    path: PathBuf,
    view: Entity<WgpuiEditorView>,
    _subscriptions: Vec<Subscription>,
}

impl FilePanel {
    pub fn new(
        text_editor: Entity<TextEditor>,
        path: PathBuf,
        view: Entity<WgpuiEditorView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();

        // The dirty marker on the tab follows the document.
        subscriptions.push(cx.observe(&text_editor, |_, _, cx| cx.notify()));

        // Clicking into a split group does not change that group's active tab,
        // so the focused editor is what decides the current document.
        let focus_handle = view.focus_handle(cx);
        let weak_editor = text_editor.downgrade();
        let focus_path = path.clone();
        subscriptions.push(window.on_focus_in(&focus_handle, cx, move |_window, cx| {
            weak_editor
                .update(cx, |editor, cx| editor.activate_path(&focus_path, cx))
                .ok();
        }));

        // The dock drops the panel when its tab is closed, in any group.
        let weak_editor = text_editor.downgrade();
        let release_path = path.clone();
        subscriptions.push(cx.on_release(move |_this, cx| {
            weak_editor
                .update(cx, |editor, cx| editor.close_path(&release_path, cx))
                .ok();
        }));

        Self {
            text_editor: text_editor.downgrade(),
            path,
            view,
            _subscriptions: subscriptions,
        }
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled")
            .to_owned()
    }

    fn is_modified(&self, cx: &App) -> bool {
        self.text_editor
            .upgrade()
            .is_some_and(|editor| editor.read(cx).is_modified(&self.path))
    }
}

impl EventEmitter<PanelEvent> for FilePanel {}

impl Render for FilePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("editor-context-area")
            .size_full()
            .bg(cx.theme().background)
            .key_context("MockacoHost")
            .on_action(cx.listener(|this, _: &EditorUndo, _, cx| {
                this.view.update(cx, |view, cx| view.undo(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorRedo, _, cx| {
                this.view.update(cx, |view, cx| view.redo(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorCut, _, cx| {
                this.view.update(cx, |view, cx| view.cut(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorCopy, _, cx| {
                this.view.update(cx, |view, cx| view.copy(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorPaste, _, cx| {
                this.view.update(cx, |view, cx| view.paste(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorSelectAll, _, cx| {
                this.view.update(cx, |view, cx| view.select_all(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorUnfoldAll, _, cx| {
                this.view.update(cx, |view, cx| view.unfold_all(cx))
            }))
            .on_action(cx.listener(|this, _: &EditorSave, _, cx| {
                if let Some(editor) = this.text_editor.upgrade() {
                    let path = this.path.clone();
                    editor.update(cx, |editor, cx| editor.save_path(&path, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &EditorCopyPath, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    this.path.to_string_lossy().into_owned(),
                ));
            }))
            .child(self.view.clone())
            .context_menu(move |menu, _window, _cx| {
                menu.menu("Undo", Box::new(EditorUndo))
                    .menu("Redo", Box::new(EditorRedo))
                    .separator()
                    .menu("Cut", Box::new(EditorCut))
                    .menu("Copy", Box::new(EditorCopy))
                    .menu("Paste", Box::new(EditorPaste))
                    .separator()
                    .menu("Select All", Box::new(EditorSelectAll))
                    .menu("Unfold All", Box::new(EditorUnfoldAll))
                    .separator()
                    .menu("Save", Box::new(EditorSave))
                    .menu("Copy File Path", Box::new(EditorCopyPath))
            })
    }
}

impl Focusable for FilePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }
}

impl Panel for FilePanel {
    fn panel_name(&self) -> &'static str {
        "Script File"
    }

    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some(self.file_name().into())
    }

    fn tab_icon(&self, _cx: &App) -> Option<IconName> {
        Some(IconName::Code)
    }

    fn tab_unsaved(&self, cx: &App) -> bool {
        self.is_modified(cx)
    }

    fn panel_file_path(&self, _cx: &App) -> Option<PathBuf> {
        Some(self.path.clone())
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        self.file_name().into_any_element()
    }

    // Closing a dirty tab would drop the edits with no way back, so the tab
    // refuses until the file is saved.
    fn closable(&self, cx: &App) -> bool {
        !self.is_modified(cx)
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut App) {
        if !active {
            return;
        }
        let path = self.path.clone();
        self.text_editor
            .update(cx, |editor, cx| editor.activate_path(&path, cx))
            .ok();
    }
}

/// Welcome Panel - shown when no files are open
pub struct WelcomePanelWrapper {
    focus_handle: FocusHandle,
}

impl WelcomePanelWrapper {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl EventEmitter<PanelEvent> for WelcomePanelWrapper {}

impl Render for WelcomePanelWrapper {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(cx.theme().foreground)
                    .child(t!("CodeEditor.Welcome").to_string()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("CodeEditor.OpenFileHint").to_string()),
            )
    }
}

impl Focusable for WelcomePanelWrapper {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for WelcomePanelWrapper {
    fn panel_name(&self) -> &'static str {
        "welcome"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Welcome".into_any_element()
    }
}

/// File Explorer Panel
pub struct FileExplorerPanel {
    file_explorer: Entity<FileExplorer>,
    focus_handle: FocusHandle,
}

impl FileExplorerPanel {
    pub fn new(file_explorer: Entity<FileExplorer>, cx: &mut Context<Self>) -> Self {
        Self {
            file_explorer,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl EventEmitter<PanelEvent> for FileExplorerPanel {}

impl Render for FileExplorerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .child(self.file_explorer.clone())
    }
}

impl Focusable for FileExplorerPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for FileExplorerPanel {
    fn panel_name(&self) -> &'static str {
        "file_explorer"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Files".into_any_element()
    }
}
