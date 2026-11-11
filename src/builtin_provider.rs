//! The plugin's editor provider: its file types and editors, offered to
//! the editor while the plugin is enabled (`plugin_manager::PluginEditorProvider`,
//! registered by the root's `build`).

use gpui::AppContext;
use gpui::{App, Window};
use plugin_editor_api::*;
use plugin_manager::{BuiltinEditorProvider, EditorContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ui::dock::PanelView;

/// Wraps the script_editor_plugin crate as a built-in editor provider.
pub struct ScriptEditorBuiltinProvider;

impl BuiltinEditorProvider for ScriptEditorBuiltinProvider {
    fn provider_id(&self) -> &str {
        "com.pulsar.script-editor"
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        use serde_json::json;

        vec![
            FileTypeDefinition {
                id: FileTypeId::new("rust_script"),
                extension: "rs".to_string(),
                display_name: "Rust".to_string(),
                icon: ui::IconName::Code,
                color: gpui::rgb(0xFF5722).into(),
                structure: FileStructure::Standalone,
                default_content: json!("// New Rust script\n"),
                creation_directory: None,

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
                creation_directory: None,

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
                creation_directory: None,

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
                creation_directory: None,

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
                creation_directory: None,

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
                creation_directory: None,

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
                creation_directory: None,

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

    fn can_handle(&self, editor_id: &EditorId) -> bool {
        editor_id.as_str() == "script-editor"
    }

    fn ai_tools(&self) -> Vec<AiToolDefinition> {
        crate::ScriptEditorPlugin::default().ai_tools()
    }

    fn capabilities_for_file(&self, file_path: &Path) -> Vec<String> {
        crate::ScriptEditorPlugin::default().capabilities_for_file(file_path)
    }

    fn execute_ai_tool(
        &self,
        file_path: &Path,
        tool_name: &str,
        tool_args: JsonValue,
    ) -> Result<JsonValue, PluginError> {
        crate::ScriptEditorPlugin::default().execute_ai_tool(file_path, tool_name, tool_args)
    }

    fn create_editor(
        &self,
        file_path: PathBuf,
        editor_context: &EditorContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        // The language server comes from the editor's context, exactly as it does
        // for a plugin library; there is no process-wide handle to fall back on.
        let services = editor_context.services.clone();
        let project_root = editor_context.project_root.clone();
        if services.language_server.is_none() {
            tracing::warn!("No language server in the editor context; Script Editor LSP is off");
        }
        let settings = plugin_editor_api::EditorSettingsSnapshot::default().with_services(services);
        let panel = cx.new(|cx| crate::ScriptEditorPanel::new_with_settings(window, cx, settings));
        panel.update(cx, |editor, ecx| {
            if let Some(project_root) = project_root {
                editor.set_project_path(project_root, window, ecx);
            }
            editor.open_file(file_path.clone(), window, ecx);
        });

        Ok(Arc::new(panel))
    }
}

/// Registers the editor provider.
pub(crate) fn build(p: &mut pulsar_plugins::PluginBuilder) {
    p.add(plugin_manager::PluginEditorProvider {
        origin: p.origin(),
        id: "com.pulsar.script-editor",
        create: || Arc::new(ScriptEditorBuiltinProvider),
    });
}
