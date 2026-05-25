//! Adapter from `tonepoet-pipeline` commands into `ToolRunner` commands.

use std::time::Duration;

use tonepoet_pipeline::{PlannedCommand, ToolIdentifier};

use super::errors::ConvertError;
use super::tool::{EnvVar, ToolBinary, ToolCommand};
use super::types::SecretString;

pub const DEFAULT_PLANNED_COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);

pub fn planned_command_to_tool_command(
    planned: &PlannedCommand,
    default_timeout: Duration,
) -> Result<ToolCommand, ConvertError> {
    Ok(ToolCommand {
        binary: tool_identifier_to_binary(&planned.tool)?,
        args: planned.args.clone(),
        secret_args: Vec::new(),
        cwd: None,
        env: planned
            .environment
            .iter()
            .map(|(key, value)| EnvVar {
                key: key.clone(),
                value: SecretString::new(value.clone()),
                secret: false,
            })
            .collect(),
        timeout: planned.expected_duration.unwrap_or(default_timeout),
    })
}

pub fn tool_identifier_to_binary(identifier: &ToolIdentifier) -> Result<ToolBinary, ConvertError> {
    match identifier {
        ToolIdentifier::Ffmpeg => Ok(ToolBinary::Ffmpeg),
        ToolIdentifier::Sox => Ok(ToolBinary::Sox),
        ToolIdentifier::Ssrc => Ok(ToolBinary::Ssrc),
        ToolIdentifier::Loudgain => Ok(ToolBinary::Loudgain),
        ToolIdentifier::Metaflac => Ok(ToolBinary::Metaflac),
        ToolIdentifier::Flac => Ok(ToolBinary::Flac),
        ToolIdentifier::Custom(name) => Err(ConvertError::Backend(format!(
            "custom planner tool `{name}` is not permitted by this orchestrator build"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tonepoet_pipeline::{InputSource, OutputSink};

    use super::*;

    #[test]
    fn maps_builtin_tool_and_environment() {
        let mut planned = PlannedCommand::new(
            ToolIdentifier::Ssrc,
            vec!["--rate".into(), "44100".into()],
            InputSource::Path(PathBuf::from("in.wav")),
            OutputSink::Path(PathBuf::from("out.wav")),
            Some(Duration::from_secs(9)),
            "resample",
        );
        let mut env = BTreeMap::new();
        env.insert("LC_ALL".to_string(), "C".to_string());
        planned.environment = env;

        let cmd = planned_command_to_tool_command(&planned, Duration::from_secs(60)).unwrap();
        assert_eq!(cmd.binary, ToolBinary::Ssrc);
        assert_eq!(cmd.timeout, Duration::from_secs(9));
        assert_eq!(cmd.env_keys(), vec!["LC_ALL".to_string()]);
        assert_eq!(cmd.env[0].value.expose(), "C");
    }

    #[test]
    fn rejects_custom_tool() {
        let err = tool_identifier_to_binary(&ToolIdentifier::Custom("x".into())).unwrap_err();
        assert!(err.to_string().contains("custom planner tool"));
    }
}
