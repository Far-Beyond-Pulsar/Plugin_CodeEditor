//! Standalone harness for the Script Editor plugin (runs without the Pulsar engine).
//!
//! Usage:
//!   cargo run --example standalone -- [--fullscreen] [project_dir] [files...]
//!
//! Environment:
//!   RUST_LOG overrides the log filter (defaults to "info").

use std::path::PathBuf;

use engine_backend::services::rust_analyzer_manager::RustAnalyzerManager;
use gpui::*;
use script_editor_plugin::{ScriptEditorPanel, TextEditorEvent};
use tracing_subscriber::EnvFilter;
use ui::{ActiveTheme as _, Root};

const USAGE: &str = "\
Script Editor standalone harness

USAGE:
    cargo run --example standalone -- [--fullscreen] [project_dir] [files...]

ARGS:
    project_dir    Project directory to load (default: current directory)
    files...       Files to open on startup

OPTIONS:
    --fullscreen   Open the window in fullscreen mode
    -h, --help     Print this help message

ENVIRONMENT:
    RUST_LOG       Log filter, defaults to \"info\"";

struct StandaloneApp {
    editor: Entity<ScriptEditorPanel>,
    _sub: Subscription,
}

impl StandaloneApp {
    fn new(
        project_root: PathBuf,
        files_to_open: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| ScriptEditorPanel::new(window, cx));

        let analyzer = cx.new(|cx| RustAnalyzerManager::new(window, cx));
        analyzer.update(cx, |analyzer, cx| {
            analyzer.start(project_root.clone(), window, cx);
        });
        editor.update(cx, |editor, cx| {
            editor.set_rust_analyzer(analyzer, cx);
        });

        tracing::info!("Loading project root: {:?}", project_root);
        editor.update(cx, |editor, cx| {
            editor.set_project_path(project_root, window, cx);
        });

        for file in files_to_open {
            tracing::info!("Opening startup file: {:?}", file);
            editor.update(cx, |editor, cx| {
                editor.open_file(file, window, cx);
            });
        }

        let _sub = cx.subscribe(
            &editor,
            |_this: &mut Self, _editor, event: &TextEditorEvent, _cx| {
                #[allow(unreachable_patterns)]
                match event {
                    TextEditorEvent::OpenFolderRequested(path) => {
                        tracing::info!("Folder open requested: {:?}", path);
                    }
                    TextEditorEvent::RunScriptRequested(path, command) => {
                        tracing::info!("Run requested for {:?}: {}", path, command);
                    }
                    TextEditorEvent::DebugScriptRequested(path) => {
                        tracing::info!("Debug requested: {:?}", path);
                    }
                    TextEditorEvent::FileOpened { path, content } => {
                        tracing::info!("File opened {:?} ({} bytes)", path, content.len());
                    }
                    TextEditorEvent::FileSaved { path, content } => {
                        tracing::info!("File saved {:?} ({} bytes)", path, content.len());
                    }
                    TextEditorEvent::FileClosed { path } => {
                        tracing::info!("File closed: {:?}", path);
                    }
                    TextEditorEvent::NavigateToLocation {
                        path,
                        line,
                        character,
                    } => {
                        tracing::info!(
                            "Navigation requested: {}:{}:{}",
                            path.display(),
                            line,
                            character
                        );
                    }
                    other => {
                        tracing::info!(
                            "Unhandled editor event: {:?}",
                            std::mem::discriminant(other)
                        );
                    }
                }
            },
        );

        let explorer_focus = editor
            .read(cx)
            .get_file_explorer()
            .read(cx)
            .focus_handle(cx);
        window.focus(&explorer_focus, cx);

        Self { editor, _sub }
    }
}

impl Focusable for StandaloneApp {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for StandaloneApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(self.editor.clone())
            .into_any_element()
    }
}

fn main() {
    let mut fullscreen = false;
    let mut wants_help = false;
    let mut positionals: Vec<PathBuf> = Vec::new();

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--help" | "-h" => wants_help = true,
            "--fullscreen" => fullscreen = true,
            _ => positionals.push(PathBuf::from(arg)),
        }
    }

    if wants_help {
        println!("{}", USAGE);
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut positionals = positionals.into_iter();
    let project_root = positionals
        .next()
        .or_else(|| std::env::current_dir().ok())
        .map(|path| path.canonicalize().unwrap_or(path))
        .unwrap_or_else(|| PathBuf::from("."));
    let files_to_open: Vec<PathBuf> = positionals.map(|p| p.canonicalize().unwrap_or(p)).collect();

    Application::new()
        .with_assets(ui::assets::Assets)
        .run(move |cx| {
            ui::init(cx);
            ui::themes::init(cx);

            let restore_bounds = Bounds {
                origin: Point::default(),
                size: size(px(1280.), px(800.)),
            };

            cx.open_window(
                WindowOptions {
                    window_bounds: Some(if fullscreen {
                        WindowBounds::Fullscreen(restore_bounds)
                    } else {
                        WindowBounds::Maximized(restore_bounds)
                    }),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Script Editor".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let app =
                        cx.new(|cx| StandaloneApp::new(project_root, files_to_open, window, cx));
                    cx.new(|cx| Root::new(app.into(), window, cx))
                },
            )
            .unwrap();

            cx.activate(true);
        });
}
