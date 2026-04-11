use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KeybindingPreset {
    #[default]
    IntelliJ,
    VSCode,
    Vim,
    Emacs,
    Custom,
}

impl KeybindingPreset {
    pub fn all() -> Vec<Self> {
        vec![
            Self::IntelliJ,
            Self::VSCode,
            Self::Vim,
            Self::Emacs,
            Self::Custom,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keybinding {
    pub action: String,
    pub keys: String,
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeybindingConfig {
    pub preset: KeybindingPreset,
    pub overrides: Vec<Keybinding>,
}

impl KeybindingConfig {
    pub fn preset_bindings(preset: KeybindingPreset) -> Vec<Keybinding> {
        match preset {
            KeybindingPreset::IntelliJ => vec![
                keybinding("command_palette", "Ctrl+Shift+A / Cmd+Shift+A", "global"),
                keybinding("new_terminal_tab", "Alt+F12", "terminal"),
                keybinding("close_tab", "Ctrl+F4", "global"),
                keybinding("split_horizontal", "Ctrl+Shift+H", "terminal"),
                keybinding("split_vertical", "Ctrl+Shift+V", "terminal"),
                keybinding("toggle_chat", "Alt+C", "global"),
                keybinding("toggle_git_panel", "Alt+9", "global"),
                keybinding("toggle_marketplace", "Alt+M", "global"),
                keybinding("toggle_kanban", "Alt+K", "global"),
                keybinding("send_message", "Ctrl+Enter", "chat"),
                keybinding("cancel_agent", "Ctrl+C", "chat"),
                keybinding("focus_terminal", "Alt+F12", "global"),
                keybinding("focus_chat", "Alt+C", "global"),
                keybinding("next_tab", "Alt+Right", "global"),
                keybinding("prev_tab", "Alt+Left", "global"),
                keybinding("search_files", "Ctrl+Shift+F", "global"),
                keybinding("quick_open", "Ctrl+Shift+N", "global"),
                keybinding("settings", "Ctrl+Alt+S", "global"),
                keybinding("switch_mode", "Ctrl+Shift+M", "global"),
                keybinding("checkpoint", "Ctrl+S", "global"),
            ],
            KeybindingPreset::VSCode => vec![
                keybinding("command_palette", "Ctrl+Shift+P / Cmd+Shift+P", "global"),
                keybinding("new_terminal_tab", "Ctrl+`", "terminal"),
                keybinding("close_tab", "Ctrl+W", "global"),
                keybinding("split_horizontal", "Ctrl+\\", "terminal"),
                keybinding("split_vertical", "Ctrl+Shift+\\", "terminal"),
                keybinding("toggle_chat", "Ctrl+Shift+I", "global"),
                keybinding("toggle_git_panel", "Ctrl+Shift+G", "global"),
                keybinding("toggle_marketplace", "Ctrl+Shift+M", "global"),
                keybinding("toggle_kanban", "Ctrl+Shift+K", "global"),
                keybinding("send_message", "Ctrl+Enter", "chat"),
                keybinding("cancel_agent", "Ctrl+C", "chat"),
                keybinding("focus_terminal", "Ctrl+`", "global"),
                keybinding("focus_chat", "Ctrl+L", "global"),
                keybinding("next_tab", "Ctrl+Tab", "global"),
                keybinding("prev_tab", "Ctrl+Shift+Tab", "global"),
                keybinding("search_files", "Ctrl+Shift+F", "global"),
                keybinding("quick_open", "Ctrl+P", "global"),
                keybinding("settings", "Ctrl+,", "global"),
                keybinding("switch_mode", "Ctrl+Shift+M", "global"),
                keybinding("checkpoint", "Ctrl+S", "global"),
            ],
            KeybindingPreset::Vim => vec![
                keybinding("command_palette", ":", "global"),
                keybinding("new_terminal_tab", ":term", "terminal"),
                keybinding("close_tab", ":q", "global"),
                keybinding("split_horizontal", ":sp", "terminal"),
                keybinding("split_vertical", ":vsp", "terminal"),
                keybinding("toggle_chat", "<leader>c", "global"),
                keybinding("toggle_git_panel", "<leader>g", "global"),
                keybinding("toggle_marketplace", "<leader>m", "global"),
                keybinding("toggle_kanban", "<leader>k", "global"),
                keybinding("send_message", "<CR>", "chat"),
                keybinding("cancel_agent", "<Esc>", "chat"),
                keybinding("focus_terminal", "<leader>t", "global"),
                keybinding("focus_chat", "<leader>c", "global"),
                keybinding("next_tab", "gt", "global"),
                keybinding("prev_tab", "gT", "global"),
                keybinding("search_files", "/", "global"),
                keybinding("quick_open", ":e", "global"),
                keybinding("settings", ":set", "global"),
                keybinding("switch_mode", ":mode", "global"),
                keybinding("checkpoint", ":w", "global"),
            ],
            KeybindingPreset::Emacs => vec![
                keybinding("command_palette", "Alt+X", "global"),
                keybinding("new_terminal_tab", "Ctrl+Alt+T", "terminal"),
                keybinding("close_tab", "Ctrl+X K", "global"),
                keybinding("split_horizontal", "Ctrl+X 2", "terminal"),
                keybinding("split_vertical", "Ctrl+X 3", "terminal"),
                keybinding("toggle_chat", "Ctrl+C C", "global"),
                keybinding("toggle_git_panel", "Ctrl+C G", "global"),
                keybinding("toggle_marketplace", "Ctrl+C M", "global"),
                keybinding("toggle_kanban", "Ctrl+C K", "global"),
                keybinding("send_message", "Ctrl+Enter", "chat"),
                keybinding("cancel_agent", "Ctrl+G", "chat"),
                keybinding("focus_terminal", "Ctrl+Alt+T", "global"),
                keybinding("focus_chat", "Ctrl+X O", "global"),
                keybinding("next_tab", "Ctrl+PageDown", "global"),
                keybinding("prev_tab", "Ctrl+PageUp", "global"),
                keybinding("search_files", "Ctrl+S", "global"),
                keybinding("quick_open", "Ctrl+X Ctrl+F", "global"),
                keybinding("settings", "Ctrl+H V", "global"),
                keybinding("switch_mode", "Ctrl+C Ctrl+M", "global"),
                keybinding("checkpoint", "Ctrl+X Ctrl+S", "global"),
            ],
            KeybindingPreset::Custom => Vec::new(),
        }
    }

    pub fn effective_bindings(&self) -> Vec<Keybinding> {
        let mut bindings = Self::preset_bindings(self.preset);
        for override_binding in &self.overrides {
            if let Some(existing) = bindings.iter_mut().find(|binding| {
                binding.action == override_binding.action
                    && binding.context == override_binding.context
            }) {
                *existing = override_binding.clone();
            } else {
                bindings.push(override_binding.clone());
            }
        }
        bindings
    }
}

pub fn resolve_platform_shortcut(keys: &str, is_macos: bool) -> String {
    if !keys.contains('/') {
        return keys.trim().to_string();
    }

    let mut selected = keys.trim().to_string();
    for part in keys.split('/') {
        let normalized = part.trim();
        if is_macos && normalized.to_lowercase().contains("cmd+") {
            selected = normalized.to_string();
            break;
        }
        if !is_macos && normalized.to_lowercase().contains("ctrl+") {
            selected = normalized.to_string();
            break;
        }
    }
    selected
}

fn keybinding(action: &str, keys: &str, context: &str) -> Keybinding {
    Keybinding {
        action: action.to_string(),
        keys: keys.to_string(),
        context: Some(context.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preset_is_intellij() {
        assert_eq!(
            KeybindingConfig::default().preset,
            KeybindingPreset::IntelliJ
        );
    }

    #[test]
    fn intellij_preset_contains_expected_command_palette_binding() {
        let bindings = KeybindingConfig::preset_bindings(KeybindingPreset::IntelliJ);
        let palette = bindings
            .iter()
            .find(|binding| binding.action == "command_palette")
            .expect("command_palette binding");
        assert_eq!(palette.keys, "Ctrl+Shift+A / Cmd+Shift+A");
    }

    #[test]
    fn vscode_preset_contains_expected_quick_open_binding() {
        let bindings = KeybindingConfig::preset_bindings(KeybindingPreset::VSCode);
        let quick_open = bindings
            .iter()
            .find(|binding| binding.action == "quick_open")
            .expect("quick_open binding");
        assert_eq!(quick_open.keys, "Ctrl+P");
    }

    #[test]
    fn vim_preset_contains_expected_mode_binding() {
        let bindings = KeybindingConfig::preset_bindings(KeybindingPreset::Vim);
        let switch_mode = bindings
            .iter()
            .find(|binding| binding.action == "switch_mode")
            .expect("switch_mode binding");
        assert_eq!(switch_mode.keys, ":mode");
    }

    #[test]
    fn override_replaces_existing_binding() {
        let config = KeybindingConfig {
            preset: KeybindingPreset::IntelliJ,
            overrides: vec![Keybinding {
                action: "command_palette".to_string(),
                keys: "Ctrl+K".to_string(),
                context: Some("global".to_string()),
            }],
        };

        let palette = config
            .effective_bindings()
            .into_iter()
            .find(|binding| binding.action == "command_palette")
            .expect("command_palette binding");

        assert_eq!(palette.keys, "Ctrl+K");
    }

    #[test]
    fn override_adds_new_action_binding() {
        let config = KeybindingConfig {
            preset: KeybindingPreset::VSCode,
            overrides: vec![Keybinding {
                action: "open_notifications".to_string(),
                keys: "Ctrl+Shift+N".to_string(),
                context: Some("global".to_string()),
            }],
        };

        let bindings = config.effective_bindings();
        assert!(bindings.iter().any(
            |binding| binding.action == "open_notifications" && binding.keys == "Ctrl+Shift+N"
        ));
    }

    #[test]
    fn platform_shortcut_resolves_to_cmd_on_macos() {
        let resolved = resolve_platform_shortcut("Ctrl+Shift+A / Cmd+Shift+A", true);
        assert_eq!(resolved, "Cmd+Shift+A");
    }

    #[test]
    fn platform_shortcut_resolves_to_ctrl_on_non_macos() {
        let resolved = resolve_platform_shortcut("Ctrl+Shift+A / Cmd+Shift+A", false);
        assert_eq!(resolved, "Ctrl+Shift+A");
    }
}
