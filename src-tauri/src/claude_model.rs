use crate::runtime::Core;
use serde_json::Value;
use tokio::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaudeModel {
    Auto,
    Haiku,
    Sonnet,
    Opus,
}

impl ClaudeModel {
    pub(crate) fn parse(value: &Value) -> Result<Self, String> {
        match value.as_str() {
            Some("auto") => Ok(Self::Auto),
            Some("haiku") => Ok(Self::Haiku),
            Some("sonnet") => Ok(Self::Sonnet),
            Some("opus") => Ok(Self::Opus),
            _ => Err("CLAUDE_MODEL_INVALID: elige Automático, Haiku, Sonnet u Opus".into()),
        }
    }

    pub(crate) fn load(core: &Core) -> Result<Self, String> {
        let value = core.store.setting("claude_model");
        if value.is_null() {
            Ok(Self::Auto)
        } else {
            Self::parse(&value)
        }
    }

    pub(crate) fn apply(self, command: &mut Command) {
        let alias = match self {
            Self::Auto => return,
            Self::Haiku => "haiku",
            Self::Sonnet => "sonnet",
            Self::Opus => "opus",
        };
        command.args(["--model", alias]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selection_persists_and_an_in_flight_snapshot_does_not_change() {
        let root = std::env::temp_dir().join(format!("ap-model-{}", crate::storage::id()));
        let resources = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        {
            let core = Core::new(root.clone(), resources.clone()).unwrap();
            assert_eq!(ClaudeModel::load(&core).unwrap(), ClaudeModel::Auto);
            core.store
                .set_setting("claude_model", &json!("haiku"))
                .unwrap();
            let snapshot = ClaudeModel::load(&core).unwrap();
            core.store
                .set_setting("claude_model", &json!("sonnet"))
                .unwrap();
            assert_eq!(snapshot, ClaudeModel::Haiku);
            assert_eq!(ClaudeModel::load(&core).unwrap(), ClaudeModel::Sonnet);
        }
        {
            let core = Core::new(root.clone(), resources).unwrap();
            assert_eq!(ClaudeModel::load(&core).unwrap(), ClaudeModel::Sonnet);
            core.store
                .set_setting("claude_model", &json!("unknown"))
                .unwrap();
            assert!(ClaudeModel::load(&core).is_err());
        }
        assert_eq!(root.parent(), Some(std::env::temp_dir().as_path()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_model_reaches_new_and_resumed_cli_commands() {
        for alias in ["haiku", "sonnet", "opus"] {
            for resumed in [false, true] {
                let mut command = Command::new("claude");
                command.arg("-p");
                ClaudeModel::parse(&json!(alias))
                    .unwrap()
                    .apply(&mut command);
                if resumed {
                    command.args(["--resume", "existing-session"]);
                }
                let args: Vec<_> = command
                    .as_std()
                    .get_args()
                    .map(|s| s.to_str().unwrap())
                    .collect();
                assert_eq!(&args[..3], &["-p", "--model", alias]);
                if resumed {
                    assert_eq!(&args[3..], &["--resume", "existing-session"]);
                }
            }
        }
    }

    #[test]
    fn automatic_preserves_cli_selection_and_rejects_untrusted_values() {
        let mut command = Command::new("claude");
        command.args(["-p", "--resume", "existing-session"]);
        ClaudeModel::Auto.apply(&mut command);
        assert_eq!(command.as_std().get_args().count(), 3);
        for value in [
            Value::Null,
            json!(false),
            json!({"model":"opus"}),
            json!(""),
            json!("--model opus"),
            json!("sonnet; command"),
            json!("unknown-model"),
        ] {
            assert!(ClaudeModel::parse(&value).is_err());
        }
    }
}
