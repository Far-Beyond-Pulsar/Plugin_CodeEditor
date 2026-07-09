//! # Script Editor Plugin
//!
//! This plugin provides a professional code editor with LSP support for various programming languages.
//! It supports standalone script files in multiple languages including Rust, JavaScript, TypeScript, Python, Lua, TOML, and Markdown.
//!
//! ## File Types
//!
//! - **Rust Script** (.rs)
//! - **JavaScript** (.js)
//! - **TypeScript** (.ts)
//! - **Python Script** (.py)
//! - **Lua Script** (.lua)
//! - **TOML Configuration** (.toml)
//! - **Markdown Document** (.md)
//!
//! ## Editors
//!
//! - **Script Editor**: Code editor with file explorer, syntax highlighting, and LSP support

use plugin_editor_api::*;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use gpui::*;
use ui::dock::PanelView;

// Script Editor modules
mod script_editor;

// Re-export main types
pub use script_editor::{
    ScriptEditor as ScriptEditorPanel,
    TextEditorEvent,
    FileExplorer,
    TextEditor,
    ScriptEditorMode,
    DiffFileEntry,
};

/// The Script Editor Plugin
pub struct ScriptEditorPlugin;

impl Default for ScriptEditorPlugin {
    fn default() -> Self {
        Self
    }
}

impl EditorPlugin for ScriptEditorPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: PluginId::new("com.pulsar.script-editor"),
            name: "Script Editor".into(),
            version: "0.1.0".into(),
            author: "Pulsar Team".into(),
            description: "Professional code editor with LSP support for multiple programming languages".into(),
        }
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        vec![
            FileTypeDefinition {
                id: FileTypeId::new("rust_script"),
                extension: "rs".to_string(),
                display_name: "Rust".to_string(),
                icon: ui::IconName::RustLang,
                color: gpui::rgb(0xFF5722).into(),
                structure: FileStructure::Standalone,
                default_content: json!("// New Rust script\n"),
                categories: vec!["Scripts".to_string()],
            },
            FileTypeDefinition {
                id: FileTypeId::new("javascript"),
                extension: "js".to_string(),
                display_name: "JavaScript".to_string(),
                icon: ui::IconName::Code,
                color: gpui::rgb(0xF7DF1E).into(),
                structure: FileStructure::Standalone,
                default_content: json!("// New JavaScript file\n"),
                categories: vec!["Scripts".to_string()],
            },
            FileTypeDefinition {
                id: FileTypeId::new("typescript"),
                extension: "ts".to_string(),
                display_name: "TypeScript".to_string(),
                icon: ui::IconName::Code,
                color: gpui::rgb(0x3178C6).into(),
                structure: FileStructure::Standalone,
                default_content: json!("// New TypeScript file\n"),
                categories: vec!["Scripts".to_string()],
            },
            FileTypeDefinition {
                id: FileTypeId::new("python"),
                extension: "py".to_string(),
                display_name: "Python Script".to_string(),
                icon: ui::IconName::Code,
                color: gpui::rgb(0x3776AB).into(),
                structure: FileStructure::Standalone,
                default_content: json!("# New Python script\n"),
                categories: vec!["Scripts".to_string()],
            },
            FileTypeDefinition {
                id: FileTypeId::new("lua"),
                extension: "lua".to_string(),
                display_name: "Lua Script".to_string(),
                icon: ui::IconName::Code,
                color: gpui::rgb(0x2196F3).into(),
                structure: FileStructure::Standalone,
                default_content: json!("-- New Lua script\n"),
                categories: vec!["Scripts".to_string()],
            },
            FileTypeDefinition {
                id: FileTypeId::new("toml"),
                extension: "toml".to_string(),
                display_name: "TOML Configuration".to_string(),
                icon: ui::IconName::Page,
                color: gpui::rgb(0x9E9E9E).into(),
                structure: FileStructure::Standalone,
                default_content: json!("# TOML configuration file\n"),
                categories: vec!["Data".to_string()],
            },
            FileTypeDefinition {
                id: FileTypeId::new("markdown"),
                extension: "md".to_string(),
                display_name: "Markdown Document".to_string(),
                icon: ui::IconName::Page,
                color: gpui::rgb(0xFF5722).into(),
                structure: FileStructure::Standalone,
                default_content: json!("# New Document\n"),
                categories: vec!["Documents".to_string()],
            },
        ]
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        vec![EditorMetadata {
            id: EditorId::new("script-editor"),
            display_name: "Script Editor".into(),
            supported_file_types: vec![
                FileTypeId::new("rust_script"),
                FileTypeId::new("javascript"),
                FileTypeId::new("typescript"),
                FileTypeId::new("python"),
                FileTypeId::new("lua"),
                FileTypeId::new("toml"),
                FileTypeId::new("markdown"),
            ],
        }]
    }

    fn on_load(&mut self) {
        log::info!("Script Editor Plugin loaded");
    }
}

impl EditorPluginEditor for ScriptEditorPlugin {
    fn register_editors(&'static self, registry: &mut EditorFactoryRegistry) {
        registry.register_fn(EditorId::new("script-editor"), |file_path, window, cx| {
            let panel = cx.new(|cx| ScriptEditorPanel::new(window, cx));

            panel.update(cx, |editor, ecx| {
                editor.open_file(file_path.clone(), window, ecx);
            });

            let panel_arc: Arc<dyn ui::dock::PanelView> = Arc::new(panel.clone());
            log::info!("Created script editor instance for {:?}", file_path);
            Ok(panel_arc)
        });
    }
}

impl EditorPluginStatusbar for ScriptEditorPlugin {}
impl EditorPluginAi for ScriptEditorPlugin {}
impl EditorPluginComponents for ScriptEditorPlugin {
    fn component_definitions(&self) -> Vec<ComponentDefinition> {
        Vec::new()
    }
}
impl EditorPluginSubsystems for ScriptEditorPlugin {
    fn subsystems(&self) -> Vec<Box<dyn Subsystem>> {
        Vec::new()
    }
}

export_plugin!(ScriptEditorPlugin);
