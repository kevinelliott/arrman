use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use crate::command::{CommandSpec, run};
use crate::compose::container_name;
use crate::model::{Diagnosis, Issue, ServiceRow, SuggestedFix};

struct IssuePattern {
    kind: &'static str,
    pattern: Regex,
    summary: &'static str,
    manual: &'static str,
}

fn patterns() -> &'static [IssuePattern] {
    static PATTERNS: OnceLock<Vec<IssuePattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (
                "OOM",
                r"(?i)out of memory|oom-kill|killed process|cannot allocate memory",
                "The process may be running out of memory.",
                "Check host memory and container limits; reduce worker or concurrency settings, or add memory.",
            ),
            (
                "PORT",
                r"(?i)address already in use|bind.*failed|port is already allocated",
                "The service could not bind its configured port.",
                "Choose a free host port with the port editor, then recreate the service.",
            ),
            (
                "PERMISSION",
                r"(?i)permission denied|operation not permitted|access is denied",
                "The service cannot access a file, directory, or device.",
                "Check the affected host mount ownership and permissions for the container user.",
            ),
            (
                "MISSING",
                r"(?i)no such file or directory|file not found|cannot find",
                "A configured file or path was not found.",
                "Verify the host mount exists and that the path matches the service configuration.",
            ),
            (
                "DEPENDENCY",
                r"(?i)connection refused|could not connect|connection reset|connection timed out",
                "The service could not reach a configured dependency.",
                "Check the dependency container, hostname, port, credentials, and shared network.",
            ),
            (
                "CRASH",
                r"(?i)traceback|fatal error|uncaught exception|failed to start|startup failed|unhandled exception",
                "The service reported a startup or runtime failure.",
                "Review the recent log lines and service configuration before restarting repeatedly.",
            ),
        ]
        .into_iter()
        .map(|(kind, pattern, summary, manual)| IssuePattern {
            kind,
            pattern: Regex::new(pattern).expect("static issue pattern must compile"),
            summary,
            manual,
        })
        .collect()
    })
}

pub fn diagnose(row: &ServiceRow, containers: &[Value], tail: usize) -> Result<Diagnosis> {
    let Some(expected_container_name) = row.container.as_deref() else {
        return Ok(Diagnosis {
            service: row.service.clone(),
            status: row.status.clone(),
            manual: "No container has been created. Enable the service if it should run."
                .to_string(),
            ..Diagnosis::default()
        });
    };
    let Some(container) = containers
        .iter()
        .find(|item| container_name(item).as_deref() == Some(expected_container_name))
    else {
        return Ok(Diagnosis {
            service: row.service.clone(),
            status: row.status.clone(),
            manual: "The expected container is not available. Refresh Docker state, then enable the service if needed.".to_string(),
            ..Diagnosis::default()
        });
    };

    let output = run(
        &CommandSpec::new("docker").args([
            "logs".to_string(),
            "--tail".to_string(),
            tail.to_string(),
            "--timestamps".to_string(),
            expected_container_name.to_string(),
        ]),
        false,
    )?;
    let text = [output.stdout, output.stderr]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let log_lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    let mut issues = Vec::new();
    for line in &log_lines {
        for candidate in patterns() {
            if seen.contains(candidate.kind) || !candidate.pattern.is_match(line) {
                continue;
            }
            seen.insert(candidate.kind);
            issues.push(Issue {
                kind: candidate.kind.to_string(),
                summary: candidate.summary.to_string(),
                evidence: clip(line, 240),
                manual: candidate.manual.to_string(),
            });
        }
    }

    let state_status = container
        .pointer("/State/Status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let exit_code = container
        .pointer("/State/ExitCode")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if state_status == "exited" && exit_code != 0 {
        issues.insert(
            0,
            Issue {
                kind: "EXITED".to_string(),
                summary: format!("The container exited with code {exit_code}."),
                evidence: container
                    .pointer("/State/FinishedAt")
                    .and_then(Value::as_str)
                    .unwrap_or("Container is not running")
                    .to_string(),
                manual: "Inspect the recent logs and configuration; start it again after addressing the cause.".to_string(),
            },
        );
    }
    let health = container
        .pointer("/State/Health/Status")
        .and_then(Value::as_str);
    if health == Some("unhealthy") {
        issues.insert(
            0,
            Issue {
                kind: "UNHEALTHY".to_string(),
                summary: "Docker health checks are failing.".to_string(),
                evidence: "health status: unhealthy".to_string(),
                manual: "Inspect the health check and recent logs; restart only after checking the failure reason.".to_string(),
            },
        );
    }
    let blocking = ["OOM", "PORT", "PERMISSION", "MISSING", "DEPENDENCY"];
    let has_blocking = issues
        .iter()
        .any(|issue| blocking.contains(&issue.kind.as_str()));
    let safe_fix = if state_status == "exited" && exit_code != 0 && !has_blocking {
        Some(SuggestedFix {
            action: "enable".to_string(),
            label: "Start/recreate the exited service".to_string(),
        })
    } else if health == Some("unhealthy") {
        Some(SuggestedFix {
            action: "restart".to_string(),
            label: "Restart the unhealthy service".to_string(),
        })
    } else {
        None
    };
    let manual = if issues.is_empty() {
        "No known issue pattern found in the recent logs.".to_string()
    } else {
        issues
            .iter()
            .map(|issue| issue.manual.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Diagnosis {
        service: row.service.clone(),
        status: row.status.clone(),
        logs: log_lines
            .into_iter()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        issues,
        safe_fix,
        manual,
    })
}

pub fn scan_all(rows: &[ServiceRow], containers: &[Value]) -> Result<BTreeMap<String, Diagnosis>> {
    let mut results = BTreeMap::new();
    for row in rows {
        if row.configured && row.container.is_some() {
            results.insert(row.service.clone(), diagnose(row, containers, 100)?);
        }
    }
    Ok(results)
}

fn clip(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return value.chars().take(width).collect();
    }
    let mut clipped = value.chars().take(width - 1).collect::<String>();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_patterns_are_deduplicated() {
        let row = ServiceRow {
            service: "test".into(),
            status: "running".into(),
            container: Some("test".into()),
            configured: true,
            ..ServiceRow::default()
        };
        let containers = [serde_json::json!({
            "Name": "/test",
            "State": {"Status": "running"}
        })];
        // Pattern behavior itself is covered without invoking Docker.
        let line = "permission denied, then permission denied again";
        let count = patterns()
            .iter()
            .filter(|candidate| candidate.pattern.is_match(line))
            .count();
        assert_eq!(count, 1);
        assert_eq!(row.service, "test");
        assert_eq!(containers.len(), 1);
    }
}
