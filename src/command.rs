use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl CommandSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, value: impl Into<OsString>) -> Self {
        self.args.push(value.into());
        self
    }

    pub fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(values.into_iter().map(Into::into));
        self
    }

    pub fn display(&self) -> String {
        std::iter::once(self.program.as_os_str())
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(shell_neutral_display)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

fn shell_neutral_display(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._/:=@".contains(character))
    {
        text.into_owned()
    } else {
        format!("{:?}", text.as_ref())
    }
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn combined(&self) -> String {
        [self.stdout.trim(), self.stderr.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn run(spec: &CommandSpec, check: bool) -> Result<CommandOutput> {
    let output = spec
        .command()
        .output()
        .with_context(|| format!("required command could not start: {}", spec.display()))?;
    let result = CommandOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    if check && !result.status.success() {
        let detail = if result.stderr.trim().is_empty() {
            result.stdout.trim()
        } else {
            result.stderr.trim()
        };
        bail!("command failed: {}\n{}", spec.display(), detail);
    }
    Ok(result)
}

pub fn run_json(spec: &CommandSpec) -> Result<serde_json::Value> {
    let output = run(spec, true)?;
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("command returned invalid JSON: {}", spec.display()))
}

pub fn run_streaming<F>(spec: &CommandSpec, mut progress: F) -> Result<CommandOutput>
where
    F: FnMut(String),
{
    let mut child = spec
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("required command could not start: {}", spec.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("stderr pipe unavailable"))?;
    let (sender, receiver) = mpsc::channel::<(bool, String)>();

    let stdout_reader = spawn_reader(stdout, false, sender.clone());
    let stderr_reader = spawn_reader(stderr, true, sender.clone());
    drop(sender);

    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    for (is_stderr, line) in receiver {
        progress(line.clone());
        if is_stderr {
            stderr_lines.push(line);
        } else {
            stdout_lines.push(line);
        }
    }

    stdout_reader
        .join()
        .map_err(|_| anyhow!("stdout reader thread panicked"))??;
    stderr_reader
        .join()
        .map_err(|_| anyhow!("stderr reader thread panicked"))??;
    let status = child
        .wait()
        .with_context(|| format!("could not wait for command: {}", spec.display()))?;
    let result = CommandOutput {
        status,
        stdout: stdout_lines.join("\n"),
        stderr: stderr_lines.join("\n"),
    };
    if !result.status.success() {
        let detail = if result.stderr.trim().is_empty() {
            result.stdout.trim()
        } else {
            result.stderr.trim()
        };
        bail!("command failed: {}\n{}", spec.display(), detail);
    }
    Ok(result)
}

fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    is_stderr: bool,
    sender: mpsc::Sender<(bool, String)>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            sender
                .send((is_stderr, line?))
                .map_err(|_| anyhow!("activity receiver closed"))?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_failure_is_normalized() {
        let error = run(&CommandSpec::new("false"), true).unwrap_err();
        assert!(error.to_string().contains("command failed: false"));
    }

    #[test]
    fn display_does_not_invoke_a_shell() {
        let spec = CommandSpec::new("docker").args(["compose", "service with space"]);
        assert_eq!(spec.display(), "docker compose \"service with space\"");
    }
}
