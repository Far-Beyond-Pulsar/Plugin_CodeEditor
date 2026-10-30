//! The inline prompt: a box that opens in the text above the code it is
//! about, takes an instruction, and either streams an answer into itself or
//! hands a rewrite to Mockaco's inline review.
//!
//! It lives in a Mockaco block, so the text below it moves down rather than
//! being covered, and it stays attached to its line while the file is edited.

use super::prompt::{
    build_request, extract_code, region_lines, splice, AssistContext, AssistKind, DiagnosticNote,
    LineRegion,
};
use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mockaco_core::DiagnosticSeverity;
use mockaco_gpui::native::WgpuiEditorView;
use mockaco_gpui::BlockPlacement;
use plugin_editor_api::llm::{self, EditorLlm as _};
use std::path::PathBuf;
use ui::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState, TextInput},
    v_flex, ActiveTheme, Selectable as _, Sizable as _,
};

/// Block ids below the review's range (`1 << 48`) belong to the host.
const BLOCK_ID: u64 = 1;
const LINE_HEIGHT: f32 = 20.0;
const INPUT_ROWS: u32 = 4;
const WORKING_ROWS: u32 = 2;
const FAILED_ROWS: u32 = 3;
const MAX_ANSWER_ROWS: u32 = 16;
/// Characters of an answer that fit on one row of the box.
const ANSWER_COLUMNS: usize = 96;

pub enum InlineAssistEvent {
    /// The box closed itself (Escape, Close, or a finished edit).
    Closed,
}

impl EventEmitter<InlineAssistEvent> for InlineAssist {}

enum Stage {
    Input,
    Working { received: usize },
    Answer { text: String, streaming: bool },
    Failed(String),
}

/// Something the request thread reports back.
enum Progress {
    Chunk(String),
    Done(Result<String, String>),
}

pub struct InlineAssist {
    view: Entity<WgpuiEditorView>,
    path: PathBuf,
    kind: AssistKind,
    input: Entity<InputState>,
    stage: Stage,
    /// The lines the request is about; the box hangs above the first.
    region: LineRegion,
    /// Diagnostics the user can see in the region, sent along with a Fix.
    notes: Vec<DiagnosticNote>,
    model_label: Option<String>,
    /// Bumped by every submit and close, so a stale reply is ignored.
    generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl InlineAssist {
    /// Opens the box over the selection (or caret line) of `view`.
    pub fn open(
        view: Entity<WgpuiEditorView>,
        path: PathBuf,
        kind: AssistKind,
        instruction: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let assist = cx.new(|cx| {
            let (region, notes) = {
                let view = view.read(cx);
                let text = view.text();
                let region = region_lines(&text, view.selection_range());
                let notes = diagnostics_in(view, &text, &region);
                (region, notes)
            };
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(kind.placeholder());
                if let Some(text) = &instruction {
                    input.set_value(text.clone(), window, cx);
                }
                input
            });
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(window, cx);
                    }
                },
            );
            Self {
                view,
                path,
                kind,
                input,
                stage: Stage::Input,
                region,
                notes,
                model_label: llm::editor_llm().map(|llm| llm.label()),
                generation: 0,
                _subscriptions: vec![subscription],
            }
        });
        assist.update(cx, |this, cx| {
            let handle = cx.entity();
            this.place(&handle, cx);
            this.input.update(cx, |input, cx| input.focus(window, cx));
        });
        assist
    }

    fn rows(&self) -> u32 {
        match &self.stage {
            Stage::Input => INPUT_ROWS,
            Stage::Working { .. } => WORKING_ROWS,
            Stage::Failed(_) => FAILED_ROWS,
            Stage::Answer { text, .. } => {
                let wrapped: usize = text
                    .lines()
                    .map(|line| line.chars().count().max(1).div_ceil(ANSWER_COLUMNS))
                    .sum();
                (wrapped as u32 + 2).clamp(4, MAX_ANSWER_ROWS)
            }
        }
    }

    /// (Re)shows the box in the text at its current size.
    fn place(&self, this: &Entity<Self>, cx: &mut Context<Self>) {
        let rows = self.rows();
        let line = self.region.start;
        let handle = AnyView::from(this.clone());
        self.view.update(cx, |view, cx| {
            view.set_block(BLOCK_ID, line, BlockPlacement::Above, rows, handle, cx)
        });
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity();
        self.place(&this, cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.view
            .update(cx, |view, cx| view.remove_block(BLOCK_ID, cx));
        cx.emit(InlineAssistEvent::Closed);
    }

    fn set_kind(&mut self, kind: AssistKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.input.update(cx, |input, cx| {
            input.set_placeholder(kind.placeholder(), window, cx)
        });
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let instruction = self.input.read(cx).value().to_string();
        if instruction.trim().is_empty() && !self.kind.instruction_optional() {
            return;
        }
        let Some(model) = llm::editor_llm() else {
            self.stage = Stage::Failed(
                "No AI provider is ready. Choose and set one up in the Agent Chat panel.".into(),
            );
            self.refresh(cx);
            return;
        };
        let view = self.view.read(cx);
        let text = view.text();
        let context = AssistContext {
            path: self.path.display().to_string(),
            language: view.surface.language().id(),
            text: text.clone(),
            region: self.region.clone(),
            diagnostics: self.notes.clone(),
        };
        let base_version = view.surface.document().version();
        let request = build_request(self.kind, &instruction, &context);
        let kind = self.kind;
        self.generation += 1;
        let generation = self.generation;
        self.model_label = Some(model.label());
        self.stage = if kind.edits() {
            Stage::Working { received: 0 }
        } else {
            Stage::Answer {
                text: String::new(),
                streaming: true,
            }
        };
        self.refresh(cx);
        let _ = window;

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Progress>();
        let sender = tx.clone();
        cx.background_spawn(async move {
            let result = model.complete(request, &mut |chunk| {
                sender.unbounded_send(Progress::Chunk(chunk.to_owned())).ok();
            });
            tx.unbounded_send(Progress::Done(result)).ok();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Some(progress) = rx.next().await {
                let finished = matches!(progress, Progress::Done(_));
                let alive = this
                    .update(cx, |this, cx| {
                        this.on_progress(generation, base_version, &text, progress, cx)
                    })
                    .unwrap_or(false);
                if finished || !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Applies one report from the request; false once it no longer matters.
    fn on_progress(
        &mut self,
        generation: u64,
        base_version: u64,
        base_text: &str,
        progress: Progress,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        match progress {
            Progress::Chunk(chunk) => {
                match &mut self.stage {
                    Stage::Working { received } => *received += chunk.len(),
                    Stage::Answer { text, .. } => text.push_str(&chunk),
                    _ => {}
                }
                self.refresh(cx);
            }
            Progress::Done(Err(error)) => {
                self.stage = Stage::Failed(error);
                self.refresh(cx);
            }
            Progress::Done(Ok(reply)) => self.finish(base_version, base_text, &reply, cx),
        }
        true
    }

    fn finish(&mut self, base_version: u64, base_text: &str, reply: &str, cx: &mut Context<Self>) {
        if !self.kind.edits() {
            self.stage = Stage::Answer {
                text: reply.trim().to_owned(),
                streaming: false,
            };
            self.refresh(cx);
            return;
        }
        if self.view.read(cx).surface.document().version() != base_version {
            self.stage = Stage::Failed(
                "The file changed while the answer was being written. Run it again.".into(),
            );
            self.refresh(cx);
            return;
        }
        let Some(code) = extract_code(reply) else {
            self.stage = Stage::Failed("The model returned no code.".into());
            self.refresh(cx);
            return;
        };
        let proposed = splice(base_text, &self.region, &code);
        // The box makes way first: the review claims the same rows.
        self.close(cx);
        let opened = self
            .view
            .update(cx, |view, cx| view.begin_review(&proposed, cx));
        if opened == 0 {
            // Nothing differs. Say so where the user is looking.
            tracing::info!("AI edit for {:?} proposed no changes", self.path);
        }
    }
}

/// The diagnostics inside `region`, nearest the top first.
fn diagnostics_in(view: &WgpuiEditorView, text: &str, region: &LineRegion) -> Vec<DiagnosticNote> {
    let mut notes: Vec<DiagnosticNote> = view
        .surface
        .lsp_diagnostics()
        .iter()
        .filter_map(|diagnostic| {
            let start = diagnostic.range.start.min(text.len());
            let line = text[..start].matches('\n').count();
            region.contains(&line).then(|| DiagnosticNote {
                line: line + 1,
                severity: match diagnostic.severity {
                    DiagnosticSeverity::Error => "error",
                    DiagnosticSeverity::Warning => "warning",
                    DiagnosticSeverity::Info => "info",
                    DiagnosticSeverity::Hint => "hint",
                },
                message: diagnostic.message.clone(),
            })
        })
        .collect();
    notes.sort_by_key(|note| note.line);
    notes
}


impl Render for InlineAssist {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let frame = v_flex()
            .size_full()
            .px_2()
            .py_1()
            .gap_1()
            .bg(theme.popover)
            .border_y_1()
            .border_color(theme.border)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }));
        let muted = theme.muted_foreground;
        match &self.stage {
            Stage::Input => {
                let chips = h_flex().gap_1().items_center().children(
                    AssistKind::ALL.iter().map(|&kind| {
                        Button::new(SharedString::from(format!("assist-{}", kind.label())))
                            .label(kind.label())
                            .ghost()
                            .xsmall()
                            .selected(self.kind == kind)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_kind(kind, window, cx)
                            }))
                    }),
                );
                let hint = self
                    .model_label
                    .clone()
                    .map(|label| format!("{label}  ·  Enter to run  ·  Esc to close"))
                    .unwrap_or_else(|| {
                        "No AI provider ready - set one up in Agent Chat  ·  Esc to close".into()
                    });
                frame
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().flex_1().child(TextInput::new(&self.input).small()))
                            .child(
                                Button::new("assist-run")
                                    .label("Run")
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit(window, cx)
                                    })),
                            ),
                    )
                    .child(chips)
                    .child(div().text_xs().text_color(muted).child(hint))
                    .into_any_element()
            }
            Stage::Working { received } => frame
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(div().text_sm().child(format!(
                            "{} - working on it ({} characters so far)",
                            self.kind.label(),
                            received
                        )))
                        .child(
                            Button::new("assist-stop")
                                .label("Cancel")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        ),
                )
                .into_any_element(),
            Stage::Failed(error) => frame
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(error.clone()),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("assist-retry")
                                .label("Edit prompt")
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.stage = Stage::Input;
                                    this.refresh(cx);
                                })),
                        )
                        .child(
                            Button::new("assist-close")
                                .label("Close")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        ),
                )
                .into_any_element(),
            Stage::Answer { text, streaming } => {
                let mut in_code = false;
                let body = text.lines().map(|line| {
                    if line.trim_start().starts_with("```") {
                        in_code = !in_code;
                        return div().into_any_element();
                    }
                    div()
                        .text_sm()
                        .min_h(px(LINE_HEIGHT * 0.8))
                        .when(in_code, |line_div| {
                            line_div
                                .font_family("JetBrains Mono")
                                .bg(theme.muted.opacity(0.4))
                                .px_1()
                        })
                        .child(line.to_owned())
                        .into_any_element()
                });
                frame
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_xs().text_color(muted).child(if *streaming {
                                format!("{} - writing...", self.kind.label())
                            } else {
                                self.kind.label().to_owned()
                            }))
                            .child(
                                Button::new("assist-answer-close")
                                    .label("Close")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            ),
                    )
                    .child(
                        div()
                            .id("assist-answer")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(body),
                    )
                    .into_any_element()
            }
        }
    }
}

