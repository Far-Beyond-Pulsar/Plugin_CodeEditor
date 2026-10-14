//! Dock panels for the Script Editor.

use super::file_explorer::FileExplorer;
use super::text_editor::{
    EditorAddCursorAbove, EditorAddCursorBelow, EditorAddNextOccurrence, EditorCloseBar,
    EditorCopy, EditorCopyPath, EditorCut, EditorExpandMacro, EditorFind, EditorFindNext,
    EditorFindPrevious, EditorFormatDocument, EditorFormatSelection, EditorGenerateStressFile,
    EditorGoToDefinition, EditorGoToLine, EditorGoToParentModule, EditorGoToReferences,
    EditorGoToSymbol, EditorGoToWorkspaceSymbol, EditorOpenCargoToml, EditorPaste, EditorRedo,
    EditorReloadWorkspace, EditorReplace, EditorSave, EditorSelectAll, EditorToggleFindCase,
    EditorToggleFindRegex, EditorToggleFindWholeWord, EditorUndo, EditorUnfoldAll, LocationItem,
    TextEditor,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mockaco_core::{CaseSensitivity, SearchQuery};
use mockaco_gpui::native::WgpuiEditorView;
use rust_i18n::t;
use std::path::PathBuf;
use ui::{
    button::{Button, ButtonVariants as _},
    context_menu::ContextMenuExt as _,
    dock::{Panel, PanelEvent},
    h_flex,
    input::{InputEvent, InputState, TextInput},
    v_flex, ActiveTheme, IconName, Selectable as _, Sizable as _,
};

/// The key context of the bars above the text, for Escape.
const BAR_CONTEXT: &str = "ScriptEditorBar";

/// The editor's key bindings for the commands its menus list. Bindings in
/// the panel's context run before the text view sees the key.
pub(crate) fn bind_editor_keys(cx: &mut App) {
    let context = Some("MockacoHost");
    cx.bind_keys([
        KeyBinding::new("ctrl-f", EditorFind, context),
        KeyBinding::new("ctrl-h", EditorReplace, context),
        KeyBinding::new("f3", EditorFindNext, context),
        KeyBinding::new("shift-f3", EditorFindPrevious, context),
        KeyBinding::new("ctrl-g", EditorGoToLine, context),
        KeyBinding::new("f12", EditorGoToDefinition, context),
        KeyBinding::new("shift-f12", EditorGoToReferences, context),
        KeyBinding::new("ctrl-shift-o", EditorGoToSymbol, context),
        KeyBinding::new("shift-alt-f", EditorFormatDocument, context),
        KeyBinding::new("ctrl-k ctrl-f", EditorFormatSelection, context),
        KeyBinding::new("ctrl-d", EditorAddNextOccurrence, context),
        KeyBinding::new("ctrl-t", EditorGoToWorkspaceSymbol, context),
        KeyBinding::new("escape", EditorCloseBar, Some(BAR_CONTEXT)),
        KeyBinding::new("alt-c", EditorToggleFindCase, Some(BAR_CONTEXT)),
        KeyBinding::new("alt-w", EditorToggleFindWholeWord, Some(BAR_CONTEXT)),
        KeyBinding::new("alt-r", EditorToggleFindRegex, Some(BAR_CONTEXT)),
    ]);
}

/// The bar shown above the text, if any.
enum Bar {
    Find(FindBar),
    GoToLine {
        input: Entity<InputState>,
        _subscription: Subscription,
    },
    /// Go to References / Symbol: the list lives in [`TextEditor`], this
    /// filters it. For workspace symbols the server does the matching, so
    /// the field's text is sent as the query instead.
    Locations {
        filter: Entity<InputState>,
        workspace: bool,
        _subscription: Subscription,
    },
}

struct FindBar {
    query: Entity<InputState>,
    /// Shown for Replace.
    replacement: Option<Entity<InputState>>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    _subscriptions: Vec<Subscription>,
}

impl FindBar {
    fn search_query(&self, cx: &App) -> SearchQuery {
        SearchQuery::new(self.query.read(cx).value().to_string())
            .case_sensitivity(if self.case_sensitive {
                CaseSensitivity::Sensitive
            } else {
                CaseSensitivity::Insensitive
            })
            .whole_word(self.whole_word)
            .regex(self.regex)
    }
}

/// One open document as a dock tab. The document itself (text, language
/// server sync, dirty state) stays in [`TextEditor`]; this is the part the dock
/// can place, split and drag.
pub struct FilePanel {
    text_editor: WeakEntity<TextEditor>,
    path: PathBuf,
    view: Entity<WgpuiEditorView>,
    bar: Option<Bar>,
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
            bar: None,
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

    /// Runs `f` on the text editor with this panel's path.
    fn with_editor(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TextEditor, &std::path::Path, &mut Context<TextEditor>),
    ) {
        if let Some(editor) = self.text_editor.upgrade() {
            let path = self.path.clone();
            editor.update(cx, |editor, cx| f(editor, &path, cx));
        }
    }

    // ---- find / replace ------------------------------------------------------

    /// Opens the find bar (with a replace field when `replace`), seeded with
    /// the selected text when it is on one line, and searches.
    fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_text(cx);
        if let Some(Bar::Find(find)) = &mut self.bar {
            if let Some(text) = selected {
                find.query
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            }
            if replace && find.replacement.is_none() {
                let (input, subscription) = self.replacement_input(window, cx);
                if let Some(Bar::Find(find)) = &mut self.bar {
                    find.replacement = Some(input);
                    find._subscriptions.push(subscription);
                }
            }
            if let Some(Bar::Find(find)) = &self.bar {
                let query = find.query.clone();
                query.update(cx, |input, cx| input.focus(window, cx));
            }
            self.search(cx);
            cx.notify();
            return;
        }

        let query = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Find");
            if let Some(text) = &selected {
                input.set_value(text.clone(), window, cx);
            }
            input.focus(window, cx);
            input
        });
        let mut subscriptions = vec![cx.subscribe_in(
            &query,
            window,
            |this: &mut Self, _, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => this.search(cx),
                InputEvent::PressEnter { secondary } => this.step(!secondary, cx),
                _ => {}
            },
        )];
        let replacement = replace.then(|| {
            let (input, subscription) = self.replacement_input(window, cx);
            subscriptions.push(subscription);
            input
        });
        self.bar = Some(Bar::Find(FindBar {
            query,
            replacement,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            _subscriptions: subscriptions,
        }));
        self.search(cx);
        cx.notify();
    }

    fn replacement_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Subscription) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, _window, cx| {
                if let InputEvent::PressEnter { secondary } = event {
                    if *secondary {
                        this.replace_all(cx);
                    } else {
                        this.replace_current(cx);
                    }
                }
            },
        );
        (input, subscription)
    }

    /// The selection's text, when it is non-empty and on one line.
    fn selected_text(&self, cx: &App) -> Option<String> {
        let view = self.view.read(cx);
        let range = view
            .surface
            .editor()
            .selections()
            .primary()?
            .ordered_range();
        if range.is_empty() || range.len() > 512 {
            return None;
        }
        let text = view.surface.document().text().slice(range).into_owned();
        (!text.contains('\n')).then_some(text)
    }

    fn find_query(&self, cx: &App) -> Option<SearchQuery> {
        match &self.bar {
            Some(Bar::Find(find)) => Some(find.search_query(cx)),
            _ => None,
        }
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        if let Some(query) = self.find_query(cx) {
            self.view.update(cx, |view, cx| view.find_query(query, cx));
            cx.notify();
        }
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.view.update(cx, |view, cx| {
            if forward {
                view.find_next(cx)
            } else {
                view.find_previous(cx)
            }
        });
        cx.notify();
    }

    fn replacement(&self, cx: &App) -> Option<String> {
        match &self.bar {
            Some(Bar::Find(FindBar {
                replacement: Some(input),
                ..
            })) => Some(input.read(cx).value().to_string()),
            _ => None,
        }
    }

    fn replace_current(&mut self, cx: &mut Context<Self>) {
        if let Some(replacement) = self.replacement(cx) {
            self.view
                .update(cx, |view, cx| view.replace_current(&replacement, cx));
            cx.notify();
        }
    }

    fn replace_all(&mut self, cx: &mut Context<Self>) {
        if let Some(replacement) = self.replacement(cx) {
            self.view
                .update(cx, |view, cx| view.replace_all(&replacement, cx));
            cx.notify();
        }
    }

    /// Flips one of the find bar's switches and searches again.
    fn toggle_find(&mut self, switch: fn(&mut FindBar) -> &mut bool, cx: &mut Context<Self>) {
        if let Some(Bar::Find(find)) = &mut self.bar {
            let value = switch(find);
            *value = !*value;
            self.search(cx);
        }
    }

    /// Closes whichever bar is open and returns to the text.
    fn close_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.bar.take() {
            Some(Bar::Find(_)) => self.view.update(cx, |view, cx| view.clear_find(cx)),
            Some(Bar::Locations { .. }) => {
                self.with_editor(cx, |editor, _, cx| editor.clear_locations(cx))
            }
            _ => {}
        }
        let focus = self.view.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    // ---- go to ---------------------------------------------------------------

    fn open_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder("Line, or line:column");
            input.focus(window, cx);
            input
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let value = input.read(cx).value();
                    if let Some((line, column)) = parse_line_column(&value) {
                        this.view.update(cx, |view, cx| {
                            view.go_to(line.saturating_sub(1), column.saturating_sub(1), cx)
                        });
                        this.close_bar(window, cx);
                    }
                }
            },
        );
        self.close_other_bar(cx);
        self.bar = Some(Bar::GoToLine {
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Asks for references or symbols and shows the filterable list.
    fn open_locations(
        &mut self,
        request: fn(&mut TextEditor, &std::path::Path, &mut Context<TextEditor>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_location_bar(false, None, window, cx);
        self.with_editor(cx, |editor, path, cx| request(editor, path, cx));
        cx.notify();
    }

    /// Searches the workspace's symbols, starting from the selected text.
    fn open_workspace_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seed = self.selected_text(cx).unwrap_or_default();
        self.open_location_bar(true, Some(seed.clone()), window, cx);
        self.with_editor(cx, |editor, path, cx| editor.workspace_symbols(path, &seed, cx));
        cx.notify();
    }

    fn open_location_bar(
        &mut self,
        workspace: bool,
        seed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_other_bar(cx);
        let filter = cx.new(|cx| {
            let placeholder = if workspace { "Symbol name" } else { "Filter" };
            let mut input = InputState::new(window, cx).placeholder(placeholder);
            if let Some(seed) = seed {
                input.set_value(seed, window, cx);
            }
            input.focus(window, cx);
            input
        });
        let subscription = cx.subscribe_in(
            &filter,
            window,
            move |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change if workspace => {
                    let query = input.read(cx).value().to_string();
                    this.with_editor(cx, |editor, path, cx| {
                        editor.workspace_symbols(path, &query, cx)
                    });
                    cx.notify();
                }
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    if let Some(item) = this.filtered_locations(cx).into_iter().next() {
                        this.open_location(item, window, cx);
                    }
                }
                _ => {}
            },
        );
        self.bar = Some(Bar::Locations {
            filter,
            workspace,
            _subscription: subscription,
        });
    }

    /// Clears what the bar being replaced left behind.
    fn close_other_bar(&mut self, cx: &mut Context<Self>) {
        match self.bar.take() {
            Some(Bar::Find(_)) => self.view.update(cx, |view, cx| view.clear_find(cx)),
            Some(Bar::Locations { .. }) => {
                self.with_editor(cx, |editor, _, cx| editor.clear_locations(cx))
            }
            _ => {}
        }
    }

    /// The listed places matching the filter, when this panel asked for them.
    fn filtered_locations(&self, cx: &App) -> Vec<LocationItem> {
        let Some(Bar::Locations {
            filter, workspace, ..
        }) = &self.bar
        else {
            return Vec::new();
        };
        // The server already matched workspace symbols, fuzzily.
        let filter = if *workspace {
            String::new()
        } else {
            filter.read(cx).value().to_lowercase()
        };
        let Some(editor) = self.text_editor.upgrade() else {
            return Vec::new();
        };
        let editor = editor.read(cx);
        let Some(list) = editor.locations().filter(|list| list.origin == self.path) else {
            return Vec::new();
        };
        list.items
            .iter()
            .flatten()
            .filter(|item| {
                filter.is_empty()
                    || item.label.to_lowercase().contains(&filter)
                    || item.detail.to_lowercase().contains(&filter)
            })
            .cloned()
            .collect()
    }

    fn open_location(&mut self, item: LocationItem, window: &mut Window, cx: &mut Context<Self>) {
        self.close_bar(window, cx);
        self.with_editor(cx, |editor, _, cx| editor.open_location(&item, cx));
    }

    // ---- rendering -------------------------------------------------------------

    fn render_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bar = self.bar.as_ref()?;
        let theme = cx.theme();
        let frame = v_flex()
            .key_context(BAR_CONTEXT)
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .bg(theme.sidebar)
            .border_b_1()
            .border_color(theme.border);
        let muted = theme.muted_foreground;
        let element = match bar {
            Bar::Find(find) => {
                let invalid = find.search_query(cx).error().is_some();
                let status = match self.view.read(cx).find_status() {
                    _ if invalid => "Invalid expression".to_owned(),
                    Some((_, 0)) => "No results".to_owned(),
                    Some((Some(current), total)) => format!("{} of {total}", current + 1),
                    Some((None, total)) => format!("{total} found"),
                    None => String::new(),
                };
                let find_row = h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().child(TextInput::new(&find.query).small()))
                    .child(
                        Button::new("find-case")
                            .label("Aa")
                            .ghost()
                            .xsmall()
                            .selected(find.case_sensitive)
                            .tooltip("Match Case (Alt+C)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_find(|find| &mut find.case_sensitive, cx)
                            })),
                    )
                    .child(
                        Button::new("find-whole-word")
                            .label("ab")
                            .ghost()
                            .xsmall()
                            .selected(find.whole_word)
                            .tooltip("Match Whole Word (Alt+W)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_find(|find| &mut find.whole_word, cx)
                            })),
                    )
                    .child(
                        Button::new("find-regex")
                            .label(".*")
                            .ghost()
                            .xsmall()
                            .selected(find.regex)
                            .tooltip("Use Regular Expression (Alt+R)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_find(|find| &mut find.regex, cx)
                            })),
                    )
                    .child(
                        div()
                            .min_w(px(64.))
                            .text_xs()
                            .text_color(muted)
                            .child(status),
                    )
                    .child(
                        Button::new("find-previous")
                            .icon(IconName::ArrowUp)
                            .ghost()
                            .xsmall()
                            .tooltip("Previous Match (Shift+F3)")
                            .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
                    )
                    .child(
                        Button::new("find-next")
                            .icon(IconName::ArrowDown)
                            .ghost()
                            .xsmall()
                            .tooltip("Next Match (F3)")
                            .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
                    )
                    .child(
                        Button::new("find-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("Close (Escape)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_bar(window, cx)),
                            ),
                    );
                let replace_row = find.replacement.as_ref().map(|input| {
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().flex_1().child(TextInput::new(input).small()))
                        .child(
                            Button::new("replace-one")
                                .label("Replace")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.replace_current(cx))),
                        )
                        .child(
                            Button::new("replace-all")
                                .label("Replace All")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx))),
                        )
                });
                frame.child(find_row).children(replace_row)
            }
            Bar::GoToLine { input, .. } => frame.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("Go to Line"))
                    .child(div().flex_1().child(TextInput::new(input).small())),
            ),
            Bar::Locations { filter, .. } => {
                let list = self
                    .text_editor
                    .upgrade()
                    .and_then(|editor| editor.read(cx).locations().cloned())
                    .filter(|list| list.origin == self.path);
                let title = list
                    .as_ref()
                    .map(|list| list.title.clone())
                    .unwrap_or_default();
                let note = match &list {
                    Some(list) if list.message.is_some() => list.message.clone(),
                    Some(list) if list.items.is_none() => {
                        Some("Asking the language server…".into())
                    }
                    Some(list) if list.items.as_ref().is_some_and(Vec::is_empty) => {
                        Some("Nothing found".into())
                    }
                    _ => None,
                };
                let items = self.filtered_locations(cx);
                let foreground = cx.theme().foreground;
                let hover = cx.theme().list_hover;
                let rows = items.into_iter().enumerate().map(|(index, item)| {
                    let label = item.label.clone();
                    let detail = item.detail.clone();
                    h_flex()
                        .id(("location", index))
                        .w_full()
                        .gap_2()
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(
                            div()
                                .text_xs()
                                .text_color(foreground)
                                .whitespace_nowrap()
                                .child(label),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(detail),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_location(item.clone(), window, cx)
                        }))
                });
                frame
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_xs().text_color(muted).child(title))
                            .child(div().flex_1().child(TextInput::new(filter).small()))
                            .child(
                                Button::new("locations-close")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_bar(window, cx)
                                    })),
                            ),
                    )
                    .when_some(note, |frame, note| {
                        frame.child(div().text_xs().text_color(muted).child(note))
                    })
                    .child(
                        v_flex()
                            .id("locations")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .children(rows),
                    )
            }
        };
        Some(element.into_any_element())
    }
}

/// `line` or `line:column` (1-based; a missing column is 1).
fn parse_line_column(text: &str) -> Option<(usize, usize)> {
    let mut parts = text.trim().splitn(2, [':', ',']);
    let line = parts.next()?.trim().parse::<usize>().ok()?;
    let column = match parts.next() {
        Some(column) => column.trim().parse::<usize>().ok()?,
        None => 1,
    };
    Some((line.max(1), column.max(1)))
}

impl EventEmitter<PanelEvent> for FilePanel {}

impl Render for FilePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = self.render_bar(cx);
        v_flex()
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
                this.with_editor(cx, |editor, path, cx| {
                    editor.save_path(path, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &EditorCopyPath, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    this.path.to_string_lossy().into_owned(),
                ));
            }))
            .on_action(
                cx.listener(|this, _: &EditorFind, window, cx| this.open_find(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &EditorReplace, window, cx| this.open_find(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &EditorFindNext, window, cx| match this.bar {
                    Some(Bar::Find(_)) => this.step(true, cx),
                    _ => this.open_find(false, window, cx),
                }),
            )
            .on_action(
                cx.listener(|this, _: &EditorFindPrevious, window, cx| match this.bar {
                    Some(Bar::Find(_)) => this.step(false, cx),
                    _ => this.open_find(false, window, cx),
                }),
            )
            .on_action(
                cx.listener(|this, _: &EditorCloseBar, window, cx| this.close_bar(window, cx)),
            )
            .on_action(cx.listener(|this, _: &EditorFormatDocument, _, cx| {
                this.with_editor(cx, |editor, path, cx| editor.format_document(path, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorFormatSelection, _, cx| {
                this.with_editor(cx, |editor, path, cx| editor.format_selection(path, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorAddNextOccurrence, _, cx| {
                this.view.update(cx, |view, cx| view.add_next_occurrence(cx));
            }))
            .on_action(cx.listener(|this, _: &EditorToggleFindCase, _, cx| {
                this.toggle_find(|find| &mut find.case_sensitive, cx)
            }))
            .on_action(cx.listener(|this, _: &EditorToggleFindWholeWord, _, cx| {
                this.toggle_find(|find| &mut find.whole_word, cx)
            }))
            .on_action(cx.listener(|this, _: &EditorToggleFindRegex, _, cx| {
                this.toggle_find(|find| &mut find.regex, cx)
            }))
            .on_action(cx.listener(|this, _: &EditorAddCursorAbove, _, cx| {
                this.view.update(cx, |view, cx| view.add_cursor(true, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorAddCursorBelow, _, cx| {
                this.view.update(cx, |view, cx| view.add_cursor(false, cx))
            }))
            .on_action(
                cx.listener(|this, _: &EditorGoToLine, window, cx| {
                    this.open_go_to_line(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &EditorGoToDefinition, _, cx| {
                this.with_editor(cx, |editor, path, cx| editor.go_to_definition(path, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorGoToReferences, window, cx| {
                this.open_locations(TextEditor::find_references, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EditorGoToSymbol, window, cx| {
                this.open_locations(TextEditor::document_symbols, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EditorGoToWorkspaceSymbol, window, cx| {
                this.open_workspace_symbols(window, cx)
            }))
            .on_action(cx.listener(|this, _: &EditorGoToParentModule, _, cx| {
                this.with_editor(cx, |editor, path, cx| editor.go_to_parent_module(path, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorExpandMacro, _, cx| {
                this.with_editor(cx, |editor, path, cx| editor.expand_macro(path, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorOpenCargoToml, _, cx| {
                this.with_editor(cx, |editor, path, cx| editor.open_cargo_toml(path, cx))
            }))
            .on_action(cx.listener(|this, _: &EditorReloadWorkspace, _, cx| {
                this.with_editor(cx, |editor, _, cx| editor.reload_workspace(cx))
            }))
            .on_action(
                cx.listener(|this, _: &EditorGenerateStressFile, window, cx| {
                    if let Some(editor) = this.text_editor.upgrade() {
                        editor.update(cx, |editor, cx| editor.generate_stress_file(window, cx));
                    }
                }),
            )
            .children(bar)
            .child(div().flex_1().min_h_0().child(self.view.clone()))
            .context_menu(move |menu, _window, _cx| {
                menu.menu("Undo", Box::new(EditorUndo))
                    .menu("Redo", Box::new(EditorRedo))
                    .separator()
                    .menu("Cut", Box::new(EditorCut))
                    .menu("Copy", Box::new(EditorCopy))
                    .menu("Paste", Box::new(EditorPaste))
                    .separator()
                    .menu("Go to Definition", Box::new(EditorGoToDefinition))
                    .menu("Go to References", Box::new(EditorGoToReferences))
                    .menu("Format Document", Box::new(EditorFormatDocument))
                    .menu("Format Selection", Box::new(EditorFormatSelection))
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

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui::*` brings a `test` attribute that would shadow the built-in one.
    use core::prelude::v1::test;

    #[test]
    fn go_to_line_reads_line_and_column() {
        assert_eq!(parse_line_column("12"), Some((12, 1)));
        assert_eq!(parse_line_column(" 3:7 "), Some((3, 7)));
        assert_eq!(parse_line_column("3,7"), Some((3, 7)));
        assert_eq!(parse_line_column("0"), Some((1, 1)));
        assert_eq!(parse_line_column("x"), None);
        assert_eq!(parse_line_column("4:"), None);
    }

    /// The menus' commands as the keys reach them: Ctrl+F finds and Enter
    /// steps, Alt+W and Alt+R switch to whole words and regular
    /// expressions, Escape closes the bar, Ctrl+D selects occurrences,
    /// Ctrl+G moves the caret, Add Cursor adds one, and the symbol lists
    /// open.
    #[gpui::test]
    fn find_go_to_line_and_add_cursor_through_the_panel(cx: &mut TestAppContext) {
        cx.update(|cx| {
            ui::init(cx);
            bind_editor_keys(cx);
        });
        let path =
            std::env::temp_dir().join(format!("pulsar_script_panel_{}.rs", std::process::id()));
        std::fs::write(&path, "fn a() {}\nfn b() { a(); a(); }\n").unwrap();
        let panel_path = path.clone();
        // As in the editor: the window's root is `ui::Root` (inputs paint
        // through it), and the script editor holds the text editor, which
        // the panel only refers to.
        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let created = slot.clone();
        let (_root, cx) = cx.add_window_view(move |window, cx| {
            let editor = cx.new(|cx| TextEditor::new(window, cx));
            editor.update(cx, |editor, cx| {
                editor.open_file(panel_path.clone(), window, cx)
            });
            let view = editor.read(cx).view_for(&panel_path).unwrap();
            let panel = cx.new(|cx| FilePanel::new(editor.clone(), panel_path, view, window, cx));
            *created.borrow_mut() = Some((editor, panel.clone()));
            ui::Root::new(panel.into(), window, cx)
        });
        let (_editor, panel) = slot.borrow_mut().take().expect("the panel was created");
        let view = panel.read_with(cx, |panel, _| panel.view.clone());
        cx.update(|window, app| {
            let handle = view.focus_handle(app);
            window.focus(&handle, app);
        });
        cx.run_until_parked();
        let status = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.find_status());
        let caret = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| {
                view.surface
                    .editor()
                    .selections()
                    .selections()
                    .iter()
                    .map(|selection| selection.cursor())
                    .collect::<Vec<_>>()
            })
        };

        // The test platform delivers a typed character to a text input twice
        // (key down and simulated IME) and turns Enter into a newline, which
        // the desktop platforms do not. So the bars' fields are filled, and
        // their Enter raised, as the input does; the shortcuts go through
        // the keys.
        let bar_input = |cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| match &panel.bar {
                Some(Bar::Find(find)) => find.query.clone(),
                Some(Bar::GoToLine { input, .. }) => input.clone(),
                Some(Bar::Locations { filter, .. }) => filter.clone(),
                None => panic!("no bar is open"),
            })
        };
        let type_into_bar = |cx: &mut VisualTestContext, text: &str| {
            let input = bar_input(cx);
            let text = text.to_owned();
            cx.update(|window, app| {
                input.update(app, |input, cx| input.set_value(text, window, cx));
            });
            cx.run_until_parked();
        };
        let press_enter = |cx: &mut VisualTestContext| {
            let input = bar_input(cx);
            cx.update(|_, app| {
                input.update(app, |_, cx| {
                    cx.emit(InputEvent::PressEnter { secondary: false })
                });
            });
            cx.run_until_parked();
        };

        cx.simulate_keystrokes("ctrl-f");
        type_into_bar(cx, "a(");
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| matches!(panel.bar, Some(Bar::Find(_)))));
        assert_eq!(status(cx), Some((Some(0), 3)));
        press_enter(cx);
        assert_eq!(status(cx), Some((Some(1), 3)));
        cx.simulate_keystrokes("f3");
        assert_eq!(status(cx), Some((Some(2), 3)));
        cx.simulate_keystrokes("shift-f3");
        assert_eq!(status(cx), Some((Some(1), 3)));
        cx.simulate_keystrokes("shift-f3");
        assert_eq!(status(cx), Some((Some(0), 3)));

        // The switches, by their keys while the field has focus: whole
        // word, then a regular expression, and an invalid one.
        let switches = |cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| match &panel.bar {
                Some(Bar::Find(find)) => (find.case_sensitive, find.whole_word, find.regex),
                _ => panic!("the find bar is open"),
            })
        };
        type_into_bar(cx, "a");
        assert_eq!(status(cx).map(|(_, total)| total), Some(3));
        cx.simulate_keystrokes("alt-w");
        assert_eq!(switches(cx), (false, true, false));
        assert_eq!(status(cx).map(|(_, total)| total), Some(3), "a is a word each time");
        cx.simulate_keystrokes("alt-w alt-r");
        assert_eq!(switches(cx), (false, false, true));
        type_into_bar(cx, r"\w\(\)");
        assert_eq!(status(cx).map(|(_, total)| total), Some(4), "a(), b(), a(), a()");
        type_into_bar(cx, "(");
        assert_eq!(status(cx).map(|(_, total)| total), Some(0));
        let invalid = panel.read_with(cx, |panel, cx| match &panel.bar {
            Some(Bar::Find(find)) => find.search_query(cx).error().is_some(),
            _ => false,
        });
        assert!(invalid, "the bar says the expression is invalid");

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.bar.is_none()));
        assert_eq!(status(cx), None, "closing the bar clears the highlights");

        // Ctrl+D: the word under the caret, then its next occurrence.
        view.update(cx, |view, cx| view.go_to(1, 9, cx));
        cx.simulate_keystrokes("ctrl-d ctrl-d");
        cx.run_until_parked();
        let selected = view.read_with(cx, |view, _| {
            view.surface
                .editor()
                .selections()
                .selections()
                .iter()
                .map(|selection| selection.ordered_range())
                .collect::<Vec<_>>()
        });
        assert_eq!(selected, vec![19..20, 24..25], "both calls to a");

        cx.simulate_keystrokes("ctrl-g");
        type_into_bar(cx, "2:4");
        press_enter(cx);
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.bar.is_none()));
        assert_eq!(caret(cx), vec![10 + 3], "line 2, column 4");

        cx.dispatch_action(EditorAddCursorAbove);
        cx.run_until_parked();
        assert_eq!(
            caret(cx).len(),
            2,
            "Add Cursor Above reached the focused view"
        );

        // Without a language server the Go menu's lists say so, and do not hang.
        cx.simulate_keystrokes("ctrl-shift-o");
        cx.run_until_parked();

        let note = panel.read_with(cx, |panel, cx| {
            panel
                .text_editor
                .upgrade()
                .and_then(|editor| editor.read(cx).locations().cloned())
                .and_then(|list| list.message)
        });
        assert!(note.is_some());

        // Go to Symbol in Workspace opens a list the server fills as you
        // type; without a server it says so.
        cx.simulate_keystrokes("escape ctrl-t");
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| matches!(
            panel.bar,
            Some(Bar::Locations {
                workspace: true,
                ..
            })
        )));
        type_into_bar(cx, "fn");
        let list = panel.read_with(cx, |panel, cx| {
            panel
                .text_editor
                .upgrade()
                .and_then(|editor| editor.read(cx).locations().cloned())
        });
        let list = list.expect("a list for the panel");
        assert_eq!(list.title.as_ref(), "Workspace Symbols");
        assert!(list.message.is_some());
        let _ = std::fs::remove_file(&path);
    }
}
