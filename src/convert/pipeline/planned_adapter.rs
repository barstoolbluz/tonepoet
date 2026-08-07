//! Adapter from `tonepoet-pipeline` commands into `ToolRunner` commands.

use std::time::Duration;

use tonepoet_pipeline::{PlannedCommand, ToolIdentifier};

use super::errors::ConvertError;
use super::tool::{EnvVar, ToolBinary, ToolCommand};
use super::types::SecretString;

pub const DEFAULT_PLANNED_COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// A planner command's `expected_duration` describes media/progress duration,
/// not process-startup latency. Very short synthetic (and legitimate) inputs
/// can therefore report millisecond-scale durations even though spawning and
/// initializing ffmpeg/SoX takes materially longer. Preserve the historical
/// duration-derived timeout for ordinary inputs, but never let it collapse
/// below a process-startup-safe floor.
const MIN_PLANNED_EXPECTED_DURATION_TIMEOUT: Duration = Duration::from_secs(30);

fn planned_command_timeout(planned: &PlannedCommand, default_timeout: Duration) -> Duration {
    planned
        .expected_duration
        .map(|expected| expected.max(MIN_PLANNED_EXPECTED_DURATION_TIMEOUT))
        .unwrap_or(default_timeout)
}

pub fn planned_command_to_tool_command(
    planned: &PlannedCommand,
    default_timeout: Duration,
) -> Result<ToolCommand, ConvertError> {
    Ok(ToolCommand {
        binary: tool_identifier_to_binary(&planned.tool)?,
        args: planned.args.clone(),
        secret_args: Vec::new(),
        cwd: None,
        environment_policy: planned.environment_policy,
        env: planned
            .environment
            .iter()
            .map(|(key, value)| EnvVar {
                key: key.clone(),
                value: SecretString::new(value.clone()),
                secret: false,
            })
            .collect(),
        timeout: planned_command_timeout(planned, default_timeout),
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
        planned.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
        planned.environment = env;

        let cmd = planned_command_to_tool_command(&planned, Duration::from_secs(60)).unwrap();
        assert_eq!(cmd.binary, ToolBinary::Ssrc);
        assert_eq!(cmd.timeout, MIN_PLANNED_EXPECTED_DURATION_TIMEOUT);
        assert_eq!(
            cmd.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(cmd.env_keys(), vec!["LC_ALL".to_string()]);
        assert_eq!(cmd.env[0].value.expose(), "C");
    }

    #[test]
    fn planned_duration_timeout_keeps_longer_estimates_and_default_fallback() {
        let near_zero = PlannedCommand::new(
            ToolIdentifier::Sox,
            vec!["in.dff".into(), "out.wav".into()],
            InputSource::Path(PathBuf::from("in.dff")),
            OutputSink::Path(PathBuf::from("out.wav")),
            Some(Duration::from_millis(8)),
            "DSD to PCM",
        );
        assert_eq!(
            planned_command_to_tool_command(&near_zero, Duration::from_secs(60))
                .unwrap()
                .timeout,
            MIN_PLANNED_EXPECTED_DURATION_TIMEOUT,
        );

        let long = PlannedCommand::new(
            ToolIdentifier::Sox,
            vec!["in.wav".into(), "out.wav".into()],
            InputSource::Path(PathBuf::from("in.wav")),
            OutputSink::Path(PathBuf::from("out.wav")),
            Some(Duration::from_secs(90)),
            "convert",
        );
        assert_eq!(
            planned_command_to_tool_command(&long, Duration::from_secs(60))
                .unwrap()
                .timeout,
            Duration::from_secs(90),
        );

        let no_estimate = PlannedCommand::new(
            ToolIdentifier::Sox,
            vec!["in.wav".into(), "out.wav".into()],
            InputSource::Path(PathBuf::from("in.wav")),
            OutputSink::Path(PathBuf::from("out.wav")),
            None,
            "convert",
        );
        assert_eq!(
            planned_command_to_tool_command(&no_estimate, Duration::from_secs(47))
                .unwrap()
                .timeout,
            Duration::from_secs(47),
        );
    }

    #[test]
    fn rejects_custom_tool() {
        let err = tool_identifier_to_binary(&ToolIdentifier::Custom("x".into())).unwrap_err();
        assert!(err.to_string().contains("custom planner tool"));
    }
}
