use std::collections::VecDeque;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};

use crate::backup::{BackupRecord, BackupStore, RestoreReport};
use crate::compose::Manager;
use crate::integrations::apply_integration_plan;
use crate::model::{IntegrationPlan, ServiceAction};

const EVENT_CAPACITY: usize = 1_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActivityLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityEvent {
    pub id: u64,
    pub at: String,
    pub level: ActivityLevel,
    pub source: String,
    pub operation_id: Option<u64>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveOperation {
    pub id: u64,
    pub label: String,
    pub source: String,
    pub started_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlStatus {
    pub active_operation: Option<ActiveOperation>,
    pub last_event_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupHealth {
    pub directory: String,
    pub interval_seconds: u64,
    pub retention: usize,
    pub last_success: Option<BackupRecord>,
    pub last_attempt_at: Option<String>,
    pub next_due_at: Option<String>,
    pub overdue: bool,
    pub last_error: Option<String>,
    pub recovery: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ControlPlane {
    inner: Arc<ControlInner>,
}

#[derive(Debug)]
struct ControlInner {
    manager: Manager,
    backups: BackupStore,
    lock_path: PathBuf,
    operation_reserved: AtomicBool,
    next_operation_id: AtomicU64,
    next_event_id: AtomicU64,
    state: Mutex<ControlState>,
}

#[derive(Debug, Default)]
struct ControlState {
    active_operation: Option<ActiveOperation>,
    events: VecDeque<ActivityEvent>,
    backup_last_attempt_at: Option<String>,
    backup_last_error: Option<String>,
}

#[derive(Debug)]
pub struct OperationBusy {
    pub label: Option<String>,
}

impl Display for OperationBusy {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match &self.label {
            Some(label) => write!(formatter, "another operation is active: {label}"),
            None => formatter.write_str("another docker-services process is changing this stack"),
        }
    }
}

impl Error for OperationBusy {}

pub struct OperationPermit {
    control: ControlPlane,
    operation: ActiveOperation,
    lock_file: Option<File>,
    finished: bool,
}

pub struct BackupScheduler {
    stop: Arc<(Mutex<bool>, Condvar)>,
    join: Option<JoinHandle<()>>,
}

impl ControlPlane {
    pub fn new(manager: Manager, backups: BackupStore) -> Self {
        let lock_path = manager
            .compose_file()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".docker-services-operation.lock");
        Self {
            inner: Arc::new(ControlInner {
                manager,
                backups,
                lock_path,
                operation_reserved: AtomicBool::new(false),
                next_operation_id: AtomicU64::new(1),
                next_event_id: AtomicU64::new(1),
                state: Mutex::new(ControlState::default()),
            }),
        }
    }

    pub fn manager(&self) -> &Manager {
        &self.inner.manager
    }

    pub fn backup_store(&self) -> &BackupStore {
        &self.inner.backups
    }

    pub fn status(&self) -> ControlStatus {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|item| item.into_inner());
        ControlStatus {
            active_operation: state.active_operation.clone(),
            last_event_id: state.events.back().map(|event| event.id).unwrap_or(0),
        }
    }

    pub fn events_since(&self, after: u64) -> Vec<ActivityEvent> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|item| item.into_inner())
            .events
            .iter()
            .filter(|event| event.id > after)
            .cloned()
            .collect()
    }

    pub fn backup_health(&self) -> Result<BackupHealth> {
        let records = self.inner.backups.list()?;
        let last_success = records.into_iter().next();
        let interval = self.inner.backups.policy().interval;
        let next_due = last_success
            .as_ref()
            .and_then(|record| DateTime::parse_from_rfc3339(&record.created_at).ok())
            .and_then(|created| {
                chrono::Duration::from_std(interval)
                    .ok()
                    .map(|step| created + step)
            })
            .map(|value| value.with_timezone(&Utc));
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|item| item.into_inner());
        let last_error = state.backup_last_error.clone();
        Ok(BackupHealth {
            directory: self.inner.backups.policy().directory.display().to_string(),
            interval_seconds: interval.as_secs(),
            retention: self.inner.backups.policy().retention,
            last_success,
            last_attempt_at: state.backup_last_attempt_at.clone(),
            next_due_at: next_due.map(|value| value.to_rfc3339()),
            overdue: next_due.is_none_or(|value| Utc::now() >= value),
            recovery: last_error.as_ref().map(|_| {
                "Check backup storage and the Activity log, then create a manual bundle to verify recovery."
                    .to_string()
            }),
            last_error,
        })
    }

    pub fn record(
        &self,
        source: impl Into<String>,
        level: ActivityLevel,
        message: impl Into<String>,
    ) {
        self.publish(source.into(), level, None, message.into());
    }

    pub fn begin_operation(
        &self,
        label: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<OperationPermit> {
        if self
            .inner
            .operation_reserved
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(anyhow::Error::new(OperationBusy {
                label: self.status().active_operation.map(|active| active.label),
            }));
        }

        let lock_file = (|| -> Result<File> {
            if let Some(parent) = self.inner.lock_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&self.inner.lock_path)
                .with_context(|| {
                    format!(
                        "could not open operation lock {}",
                        self.inner.lock_path.display()
                    )
                })?;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            FileExt::try_lock_exclusive(&file)
                .map_err(|_| anyhow::Error::new(OperationBusy { label: None }))?;
            Ok(file)
        })();
        let lock_file = match lock_file {
            Ok(file) => file,
            Err(error) => {
                self.inner
                    .operation_reserved
                    .store(false, Ordering::Release);
                return Err(error);
            }
        };

        let operation = ActiveOperation {
            id: self.inner.next_operation_id.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            source: source.into(),
            started_at: Utc::now().to_rfc3339(),
        };
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|item| item.into_inner());
            state.active_operation = Some(operation.clone());
        }
        self.publish(
            operation.source.clone(),
            ActivityLevel::Info,
            Some(operation.id),
            format!("START · {}", operation.label),
        );
        Ok(OperationPermit {
            control: self.clone(),
            operation,
            lock_file: Some(lock_file),
            finished: false,
        })
    }

    pub fn run_service_action(
        &self,
        service: &str,
        action: ServiceAction,
        source: &str,
    ) -> Result<String> {
        let mut operation =
            self.begin_operation(format!("{} {service}", action.label()), source)?;
        let result = self
            .inner
            .manager
            .run_service_action(service, action, |line| {
                operation.progress(line);
            });
        operation.finish(result)
    }

    pub fn change_port(
        &self,
        service: &str,
        port_index: usize,
        port: u16,
        source: &str,
    ) -> Result<String> {
        let mut operation =
            self.begin_operation(format!("Change {service} port to {port}"), source)?;
        let result = self
            .inner
            .manager
            .change_port(service, port_index, port, |line| operation.progress(line));
        operation.finish(result)
    }

    pub fn apply_integrations(&self, plan: &IntegrationPlan, source: &str) -> Result<String> {
        let mut operation = self.begin_operation(
            format!("Apply {} integration change(s)", plan.changes.len()),
            source,
        )?;
        let result =
            apply_integration_plan(&self.inner.manager, plan, |line| operation.progress(line))
                .map(|lines| lines.join("\n"));
        operation.finish(result)
    }

    pub fn create_backup(&self, kind: &str, source: &str) -> Result<BackupRecord> {
        let mut operation = self.begin_operation(format!("Create {kind} setup bundle"), source)?;
        let result = self
            .inner
            .backups
            .create_bundle(&self.inner.manager, kind, |line| operation.progress(line));
        self.record_backup_result(&result);
        operation.finish(result)
    }

    pub fn delete_backup(&self, id: &str, source: &str) -> Result<()> {
        let mut operation = self.begin_operation(format!("Delete setup bundle {id}"), source)?;
        operation.progress("Removing the private archive and its catalog record");
        let result = self.inner.backups.delete(id);
        operation.finish(result)
    }

    pub fn prune_backups(&self, source: &str) -> Result<usize> {
        let mut operation = self.begin_operation("Prune setup bundle retention", source)?;
        operation.progress(format!(
            "Keeping the newest {} setup bundles",
            self.inner.backups.policy().retention
        ));
        let result = self.inner.backups.prune_retention();
        operation.finish(result)
    }

    pub fn restore_bundle(&self, path: &Path, source: &str) -> Result<RestoreReport> {
        let mut operation = self.begin_operation("Restore setup bundle", source)?;
        let result = self
            .inner
            .backups
            .restore(&self.inner.manager, path, |line| operation.progress(line));
        operation.finish(result)
    }

    pub fn start_backup_scheduler(&self) -> BackupScheduler {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_stop = stop.clone();
        let control = self.clone();
        let interval = self.inner.backups.policy().interval;
        let cadence = interval
            .checked_div(4)
            .unwrap_or(Duration::from_secs(300))
            .clamp(Duration::from_secs(30), Duration::from_secs(300));
        let join = thread::spawn(move || {
            control.record(
                "backup",
                ActivityLevel::Info,
                format!(
                    "Scheduled configuration backups active · every {} · retention {}",
                    human_duration(interval),
                    control.backup_store().policy().retention
                ),
            );
            let mut initial = true;
            loop {
                let wait = if initial {
                    initial = false;
                    Duration::from_secs(30)
                } else {
                    cadence
                };
                let (guard, timeout) = thread_stop
                    .1
                    .wait_timeout(
                        thread_stop
                            .0
                            .lock()
                            .unwrap_or_else(|item| item.into_inner()),
                        wait,
                    )
                    .unwrap_or_else(|item| item.into_inner());
                if *guard {
                    break;
                }
                drop(guard);
                if !timeout.timed_out() {
                    continue;
                }
                match control.backup_store().due(Utc::now()) {
                    Ok(true) => match control.create_backup("scheduled", "backup") {
                        Ok(record) => control.record(
                            "backup",
                            ActivityLevel::Success,
                            format!("Scheduled backup retained as {}", record.file_name),
                        ),
                        Err(error) if is_busy_error(&error) => control.record(
                            "backup",
                            ActivityLevel::Warning,
                            "Scheduled backup deferred while another operation is active",
                        ),
                        Err(error) => control.record(
                            "backup",
                            ActivityLevel::Error,
                            format!("Scheduled backup failed: {error:#}"),
                        ),
                    },
                    Ok(false) => {}
                    Err(error) => control.record(
                        "backup",
                        ActivityLevel::Error,
                        format!("Could not evaluate backup schedule: {error:#}"),
                    ),
                }
            }
        });
        BackupScheduler {
            stop,
            join: Some(join),
        }
    }

    pub(crate) fn record_backup_result<T>(&self, result: &Result<T>) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|item| item.into_inner());
        state.backup_last_attempt_at = Some(Utc::now().to_rfc3339());
        state.backup_last_error = result
            .as_ref()
            .err()
            .map(|error| redact(&format!("{error:#}")));
    }

    fn publish(
        &self,
        source: String,
        level: ActivityLevel,
        operation_id: Option<u64>,
        message: String,
    ) {
        let lines = message
            .replace('\r', "")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(redact)
            .collect::<Vec<_>>();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|item| item.into_inner());
        for message in lines {
            let event = ActivityEvent {
                id: self.inner.next_event_id.fetch_add(1, Ordering::Relaxed),
                at: Utc::now().to_rfc3339(),
                level,
                source: source.clone(),
                operation_id,
                message,
            };
            state.events.push_back(event);
            while state.events.len() > EVENT_CAPACITY {
                state.events.pop_front();
            }
        }
    }

    fn complete_operation(&self, operation: &ActiveOperation, success: bool, message: String) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|item| item.into_inner());
            if state
                .active_operation
                .as_ref()
                .is_some_and(|active| active.id == operation.id)
            {
                state.active_operation = None;
            }
        }
        self.publish(
            operation.source.clone(),
            if success {
                ActivityLevel::Success
            } else {
                ActivityLevel::Error
            },
            Some(operation.id),
            message,
        );
    }
}

impl OperationPermit {
    pub fn operation(&self) -> &ActiveOperation {
        &self.operation
    }

    pub fn progress(&mut self, message: impl Into<String>) {
        self.control.publish(
            self.operation.source.clone(),
            ActivityLevel::Info,
            Some(self.operation.id),
            message.into(),
        );
    }

    pub fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        match &result {
            Ok(_) => self.control.complete_operation(
                &self.operation,
                true,
                format!("DONE · {}", self.operation.label),
            ),
            Err(error) => self.control.complete_operation(
                &self.operation,
                false,
                format!("FAILED · {} · {error:#}", self.operation.label),
            ),
        }
        self.finished = true;
        self.release();
        result
    }

    fn release(&mut self) {
        if let Some(file) = self.lock_file.take() {
            let _ = FileExt::unlock(&file);
        }
        self.control
            .inner
            .operation_reserved
            .store(false, Ordering::Release);
    }
}

impl Drop for OperationPermit {
    fn drop(&mut self) {
        if !self.finished {
            self.control.complete_operation(
                &self.operation,
                false,
                format!("ABORTED · {}", self.operation.label),
            );
            self.release();
        }
    }
}

impl Drop for BackupScheduler {
    fn drop(&mut self) {
        {
            let mut stop = self.stop.0.lock().unwrap_or_else(|item| item.into_inner());
            *stop = true;
            self.stop.1.notify_all();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn is_busy_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OperationBusy>().is_some()
}

fn redact(value: &str) -> String {
    static SENSITIVE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static BEARER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let sensitive = SENSITIVE.get_or_init(|| {
        Regex::new(r"(?i)(api[_-]?key|token|secret|password)(\s*[=:]\s*|%3[dD])([^&\s,;]+)")
            .expect("redaction pattern must compile")
    });
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)(authorization\s*:\s*bearer\s+)([^\s]+)")
            .expect("bearer pattern must compile")
    });
    let value = sensitive.replace_all(value, |captures: &Captures<'_>| {
        format!("{}{}[REDACTED]", &captures[1], &captures[2])
    });
    bearer
        .replace_all(&value, |captures: &Captures<'_>| {
            format!("{}[REDACTED]", &captures[1])
        })
        .into_owned()
}

fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds % 86_400 == 0 {
        format!("{}d", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}h", seconds / 3_600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::BackupPolicy;
    use tempfile::tempdir;

    #[test]
    fn activity_redaction_hides_common_secret_forms() {
        assert_eq!(redact("API_KEY=abc123"), "API_KEY=[REDACTED]");
        assert_eq!(
            redact("Authorization: Bearer abc123"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(redact("normal progress"), "normal progress");
    }

    #[test]
    fn empty_backup_catalog_is_explicitly_overdue() {
        let directory = tempdir().unwrap();
        let manager = Manager::new(directory.path().join("docker-compose.yml"), "host".into());
        let control = ControlPlane::new(
            manager,
            BackupStore::new(BackupPolicy::new(
                directory.path().join("backups"),
                Duration::from_secs(86_400),
                14,
            )),
        );
        let health = control.backup_health().unwrap();
        assert!(health.overdue);
        assert!(health.last_success.is_none());
        assert_eq!(health.retention, 14);
    }

    #[test]
    fn backup_attempt_failure_is_visible_and_recoverable() {
        let directory = tempdir().unwrap();
        let manager = Manager::new(directory.path().join("docker-compose.yml"), "host".into());
        let control = ControlPlane::new(
            manager,
            BackupStore::new(BackupPolicy::new(
                directory.path().join("backups"),
                Duration::from_secs(86_400),
                14,
            )),
        );
        control.record_backup_result(&Result::<(), _>::Err(anyhow::anyhow!("disk full")));
        let health = control.backup_health().unwrap();
        assert!(health.last_attempt_at.is_some());
        assert_eq!(health.last_error.as_deref(), Some("disk full"));
        assert!(health.recovery.is_some());
    }
}
