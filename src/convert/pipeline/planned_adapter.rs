//! Adapter from `tonepoet-pipeline` commands into `ToolRunner` commands.

use std::time::Duration;

use tonepoet_pipeline::{PlannedCommand, ToolIdentifier};

use super::errors::ConvertError;
use super::tool::{EnvVar, ToolBinary, ToolCommand};
use super::types::SecretString;

pub const DEFAULT_PLANNED_COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// A planner command's `expected_duration` is media/progress time. It must not
/// double as the wall-clock deadline: legitimate work can exceed realtime when
/// the machine is contended. Unless the planner supplied an explicit deadline,
/// give duration-bearing work the normal process budget *in addition to* its
/// media duration. Commands without a duration retain the normal process budget.
fn planned_command_timeout(planned: &PlannedCommand, default_timeout: Duration) -> Duration {
    if let Some(timeout_budget) = planned.timeout_budget {
        return timeout_budget;
    }

    planned
        .expected_duration
        .map(|expected| expected.saturating_add(default_timeout))
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
        assert_eq!(cmd.timeout, Duration::from_secs(69));
        assert_eq!(
            cmd.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(cmd.env_keys(), vec!["LC_ALL".to_string()]);
        assert_eq!(cmd.env[0].value.expose(), "C");
    }

    #[test]
    fn planned_duration_timeout_adds_wall_clock_headroom_and_honors_explicit_budget() {
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
            Duration::from_millis(60_008),
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
            Duration::from_secs(150),
        );

        let mut explicit = PlannedCommand::new(
            ToolIdentifier::Sox,
            vec!["in.wav".into(), "out.wav".into()],
            InputSource::Path(PathBuf::from("in.wav")),
            OutputSink::Path(PathBuf::from("out.wav")),
            Some(Duration::from_secs(90)),
            "reference analyzer",
        );
        explicit.timeout_budget = Some(Duration::from_secs(37));
        assert_eq!(
            planned_command_to_tool_command(&explicit, Duration::from_secs(60))
                .unwrap()
                .timeout,
            Duration::from_secs(37),
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

    #[tokio::test(start_paused = true)]
    async fn legitimate_work_longer_than_media_duration_survives() {
        let media_duration = Duration::from_secs(31);
        let planned = PlannedCommand::new(
            ToolIdentifier::Sox,
            vec!["in.dsf".into(), "out.f64le".into()],
            InputSource::Path(PathBuf::from("in.dsf")),
            OutputSink::Path(PathBuf::from("out.f64le")),
            Some(media_duration),
            "album gain analysis",
        );
        let command = planned_command_to_tool_command(&planned, Duration::from_secs(60)).unwrap();

        // Model legitimate contended work at >1x realtime. Tokio's paused clock
        // makes this deterministic and instant while exercising the actual
        // process budget produced by the adapter.
        let legitimate_work = Duration::from_secs(45);
        tokio::time::timeout(command.timeout, tokio::time::sleep(legitimate_work))
            .await
            .expect("legitimate work exceeding media duration must not time out");
    }

    #[test]
    fn rejects_custom_tool() {
        let err = tool_identifier_to_binary(&ToolIdentifier::Custom("x".into())).unwrap_err();
        assert!(err.to_string().contains("custom planner tool"));
    }
}
