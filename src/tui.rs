use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};

use crate::backup::{BackupRecord, BundleInspection};
use crate::control::{ActivityLevel, BackupHealth, ControlPlane};
use crate::diagnostics::{diagnose, scan_all};
use crate::integrations::integration_plan;
use crate::model::{
    CpuTimes, Diagnosis, HostStats, IntegrationPlan, RuntimeData, ServiceAction, ServiceRow,
    Snapshot,
};

const INK: Color = Color::Rgb(7, 14, 20);
const PANEL: Color = Color::Rgb(13, 25, 34);
const PANEL_ACTIVE: Color = Color::Rgb(24, 45, 56);
const STEEL: Color = Color::Rgb(77, 100, 112);
const TEXT: Color = Color::Rgb(218, 229, 231);
const MUTED: Color = Color::Rgb(137, 158, 166);
const SEA_GLASS: Color = Color::Rgb(104, 211, 190);
const GOOD: Color = Color::Rgb(112, 207, 137);
const WARN: Color = Color::Rgb(239, 185, 88);
const BAD: Color = Color::Rgb(239, 104, 111);
const INFO: Color = Color::Rgb(111, 175, 219);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Services,
    Integrations,
    Diagnostics,
    Recovery,
    Activity,
}

impl Tab {
    const ALL: [Self; 5] = [
        Self::Services,
        Self::Integrations,
        Self::Diagnostics,
        Self::Recovery,
        Self::Activity,
    ];

    fn index(self) -> usize {
        match self {
            Self::Services => 0,
            Self::Integrations => 1,
            Self::Diagnostics => 2,
            Self::Recovery => 3,
            Self::Activity => 4,
        }
    }
}

#[derive(Clone, Debug)]
enum Modal {
    Help,
    Confirm {
        title: String,
        detail: Vec<String>,
        action: ConfirmedAction,
    },
    PortSelect {
        service: String,
        ports: Vec<crate::model::PortSpec>,
        selected: usize,
    },
    PortInput {
        service: String,
        port_index: usize,
        current: String,
        input: String,
    },
    TextInput {
        title: String,
        detail: String,
        prompt: String,
        input: String,
        action: TextAction,
    },
}

#[derive(Clone, Debug)]
enum TextAction {
    InspectSetupSource,
    RestoreSetup { path: PathBuf, temporary: bool },
    DeleteBackup { id: String },
}

#[derive(Clone, Debug)]
enum ConfirmedAction {
    Service {
        service: String,
        action: ServiceAction,
    },
    ApplyIntegrations,
    CreateBackup,
    PruneBackups,
}

#[derive(Clone, Debug)]
enum InputMode {
    Normal,
    Filter,
    Modal(Modal),
}

#[derive(Clone, Debug)]
struct ActivityLine {
    at: String,
    text: String,
    error: bool,
}

#[derive(Clone, Debug)]
struct BusyState {
    label: String,
    started: Instant,
    operation_id: Option<u64>,
}

#[derive(Debug)]
enum BackendRequest {
    Refresh(Option<CpuTimes>),
    ActionScope {
        service: String,
        action: ServiceAction,
    },
    Action {
        service: String,
        action: ServiceAction,
    },
    ChangePort {
        service: String,
        port_index: usize,
        port: u16,
    },
    Diagnose(String),
    ScanAll,
    PlanIntegrations,
    ApplyIntegrations(IntegrationPlan),
    LoadBackups,
    CreateBackup,
    PruneBackups,
    DeleteBackup(String),
    InspectBackup(PathBuf),
    InspectSource(String),
    RestoreBackup {
        path: PathBuf,
        temporary: bool,
    },
    Shutdown,
}

#[derive(Debug)]
enum BackendEvent {
    Busy(String),
    Runtime(std::result::Result<RuntimeData, String>),
    ActionScope {
        service: String,
        action: ServiceAction,
        result: std::result::Result<Vec<String>, String>,
    },
    OperationFinished {
        label: String,
        result: std::result::Result<String, String>,
    },
    Diagnosis {
        service: String,
        result: std::result::Result<Diagnosis, String>,
    },
    ScanFinished(std::result::Result<BTreeMap<String, Diagnosis>, String>),
    IntegrationPlan(std::result::Result<IntegrationPlan, String>),
    BackupsLoaded(std::result::Result<(BackupHealth, Vec<BackupRecord>), String>),
    BackupInspected {
        path: PathBuf,
        temporary: bool,
        result: std::result::Result<BundleInspection, String>,
    },
}

#[derive(Clone, Debug)]
struct InspectedSetup {
    path: PathBuf,
    temporary: bool,
    inspection: BundleInspection,
}

struct App {
    request_tx: Sender<BackendRequest>,
    services: Vec<ServiceRow>,
    stats: HostStats,
    cpu_times: Option<CpuTimes>,
    selected: usize,
    selected_name: Option<String>,
    filter: String,
    tab: Tab,
    input_mode: InputMode,
    status: String,
    status_error: bool,
    activity: VecDeque<ActivityLine>,
    busy: Option<BusyState>,
    refresh_pending: bool,
    last_refresh: Instant,
    diagnoses: BTreeMap<String, Diagnosis>,
    integration_plan: Option<IntegrationPlan>,
    backups: Vec<BackupRecord>,
    backup_health: Option<BackupHealth>,
    selected_backup: usize,
    inspected_setup: Option<InspectedSetup>,
    detail_scroll: u16,
    activity_scroll: u16,
    compact_detail: bool,
    frame: usize,
    should_quit: bool,
    last_shared_event: u64,
}

impl App {
    fn new(request_tx: Sender<BackendRequest>) -> Self {
        Self {
            request_tx,
            services: Vec::new(),
            stats: HostStats::default(),
            cpu_times: None,
            selected: 0,
            selected_name: None,
            filter: String::new(),
            tab: Tab::Services,
            input_mode: InputMode::Normal,
            status: "Loading Compose and Docker state…".to_string(),
            status_error: false,
            activity: VecDeque::with_capacity(300),
            busy: None,
            refresh_pending: true,
            last_refresh: Instant::now(),
            diagnoses: BTreeMap::new(),
            integration_plan: None,
            backups: Vec::new(),
            backup_health: None,
            selected_backup: 0,
            inspected_setup: None,
            detail_scroll: 0,
            activity_scroll: 0,
            compact_detail: false,
            frame: 0,
            should_quit: false,
            last_shared_event: 0,
        }
    }

    fn demo() -> Self {
        let (request_tx, _) = mpsc::channel();
        let mut app = Self::new(request_tx);
        app.status = "DEMO DATA · synthetic state sampled 2s ago".to_string();
        app.refresh_pending = false;
        app.stats = HostStats {
            ip: "192.168.1.50".to_string(),
            cpu_cores: Some(4),
            cpu_percent: Some(18.7),
            load_1m: Some(0.72),
            memory_total: Some(16 * 1024_u64.pow(3)),
            memory_used: Some(7_891_000_000),
            memory_available: Some(9_288_869_184),
            disk_total: Some(20_294_205_440),
            disk_free: Some(1_610_612_736),
            uptime_seconds: Some(1_124_340),
        };
        app.services = demo_services();
        app.selected = 4;
        app.selected_name = Some("jellyfin".to_string());
        app.activity = VecDeque::from([
            ActivityLine {
                at: "17:44:02".to_string(),
                text: "Runtime refresh complete · 28 managed services".to_string(),
                error: false,
            },
            ActivityLine {
                at: "17:44:08".to_string(),
                text: "Scraparr connectors aligned: sonarr, radarr, prowlarr, bazarr, jellyfin"
                    .to_string(),
                error: false,
            },
            ActivityLine {
                at: "17:44:11".to_string(),
                text: "Tracearr → Jellyfin awaits owner setup".to_string(),
                error: false,
            },
        ]);
        app
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let query = self.filter.to_ascii_lowercase();
        self.services
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || row.service.to_ascii_lowercase().contains(&query)
                    || row.purpose.to_ascii_lowercase().contains(&query)
                    || row.status.to_ascii_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_row(&self) -> Option<&ServiceRow> {
        let indices = self.filtered_indices();
        indices
            .get(self.selected.min(indices.len().saturating_sub(1)))
            .and_then(|index| self.services.get(*index))
    }

    fn selected_backup(&self) -> Option<&BackupRecord> {
        self.backups.get(
            self.selected_backup
                .min(self.backups.len().saturating_sub(1)),
        )
    }

    fn move_backup_selection(&mut self, amount: isize) {
        if self.backups.is_empty() {
            self.selected_backup = 0;
            return;
        }
        self.selected_backup = (self.selected_backup as isize + amount)
            .rem_euclid(self.backups.len() as isize) as usize;
        self.detail_scroll = 0;
    }

    fn replace_inspection(&mut self, inspection: Option<InspectedSetup>) {
        if let Some(previous) = self.inspected_setup.take()
            && previous.temporary
        {
            let _ = fs::remove_file(previous.path);
        }
        self.inspected_setup = inspection;
    }

    fn request_backups(&mut self) {
        self.set_status("Reading setup bundles and schedule health…", false);
        self.request(BackendRequest::LoadBackups);
    }

    fn move_selection(&mut self, amount: isize) {
        let length = self.filtered_indices().len();
        if length == 0 {
            self.selected = 0;
            self.selected_name = None;
            return;
        }
        self.selected = (self.selected as isize + amount).rem_euclid(length as isize) as usize;
        self.selected_name = self.selected_row().map(|row| row.service.clone());
        self.detail_scroll = 0;
    }

    fn select_first(&mut self) {
        self.selected = 0;
        self.selected_name = self.selected_row().map(|row| row.service.clone());
    }

    fn select_last(&mut self) {
        self.selected = self.filtered_indices().len().saturating_sub(1);
        self.selected_name = self.selected_row().map(|row| row.service.clone());
    }

    fn apply_runtime(&mut self, mut runtime: RuntimeData) {
        let prior = self
            .selected_row()
            .map(|row| row.service.clone())
            .or_else(|| self.selected_name.clone());
        for row in &mut runtime.services {
            if let Some(diagnosis) = self.diagnoses.get(&row.service) {
                row.issue_count = diagnosis.issues.len();
                row.issues = diagnosis.issues.clone();
            }
        }
        self.services = runtime.services;
        self.stats = runtime.stats;
        self.cpu_times = runtime.cpu_times;
        self.refresh_pending = false;
        self.last_refresh = Instant::now();
        if let Some(prior) = prior {
            let indices = self.filtered_indices();
            self.selected = indices
                .iter()
                .position(|index| self.services[*index].service == prior)
                .unwrap_or_else(|| self.selected.min(indices.len().saturating_sub(1)));
        }
        self.selected_name = self.selected_row().map(|row| row.service.clone());
        if self.status.starts_with("Loading") || self.status.starts_with("Refreshing") {
            self.set_status("Watch refreshed", false);
        }
    }

    fn set_status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.status_error = error;
    }

    fn push_activity(&mut self, message: impl Into<String>) {
        let message = message.into().replace('\r', "");
        for line in message.lines().filter(|line| !line.trim().is_empty()) {
            let lowered = line.to_ascii_lowercase();
            self.activity.push_back(ActivityLine {
                at: Local::now().format("%H:%M:%S").to_string(),
                text: line.to_string(),
                error: lowered.contains("error") || lowered.contains("failed"),
            });
            while self.activity.len() > 300 {
                self.activity.pop_front();
            }
        }
        self.activity_scroll = 0;
    }

    fn sync_control(&mut self, control: &ControlPlane) {
        for event in control.events_since(self.last_shared_event) {
            self.last_shared_event = self.last_shared_event.max(event.id);
            let at = DateTime::parse_from_rfc3339(&event.at)
                .map(|value| value.with_timezone(&Local).format("%H:%M:%S").to_string())
                .unwrap_or_else(|_| Local::now().format("%H:%M:%S").to_string());
            self.activity.push_back(ActivityLine {
                at,
                text: format!("{} · {}", event.source.to_ascii_uppercase(), event.message),
                error: event.level == ActivityLevel::Error,
            });
            while self.activity.len() > 300 {
                self.activity.pop_front();
            }
            if event.message.to_ascii_lowercase().contains("setup bundle")
                && (event.message.starts_with("DONE")
                    || event.message.starts_with("FAILED")
                    || event.message.starts_with("ABORTED"))
            {
                self.request(BackendRequest::LoadBackups);
            }
        }
        let active = control.status().active_operation;
        match active {
            Some(active) => {
                if let Some(busy) = &mut self.busy {
                    if busy.label == active.label && busy.operation_id.is_none() {
                        busy.operation_id = Some(active.id);
                    }
                } else {
                    self.busy = Some(BusyState {
                        label: active.label.clone(),
                        started: Instant::now(),
                        operation_id: Some(active.id),
                    });
                    self.set_status(
                        format!("{} · started from {}", active.label, active.source),
                        false,
                    );
                }
            }
            None => {
                if self
                    .busy
                    .as_ref()
                    .is_some_and(|busy| busy.operation_id.is_some())
                {
                    self.busy = None;
                    self.set_status("Shared operation complete", false);
                }
            }
        }
    }

    fn request(&mut self, request: BackendRequest) {
        if self.request_tx.send(request).is_err() {
            self.set_status("Backend worker is unavailable", true);
        }
    }

    fn begin_service_action(&mut self, action: ServiceAction) {
        if self.busy.is_some() {
            self.set_status(
                "Another operation is already running; navigation remains available",
                true,
            );
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if !row.configured {
            self.set_status("Unmanaged containers are view-only", true);
            return;
        }
        self.set_status(
            format!(
                "Calculating {} scope for {}…",
                action.label().to_ascii_lowercase(),
                row.service
            ),
            false,
        );
        self.request(BackendRequest::ActionScope {
            service: row.service,
            action,
        });
    }

    fn begin_port_change(&mut self) {
        if self.busy.is_some() {
            self.set_status("Wait for the active operation before changing a port", true);
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if !row.configured {
            self.set_status("Unmanaged containers are view-only", true);
            return;
        }
        let ports = if row.port_specs.is_empty() {
            row.ports
                .iter()
                .map(|published| crate::model::PortSpec {
                    published: published.clone(),
                    target: "unknown".to_string(),
                    protocol: "tcp".to_string(),
                })
                .collect::<Vec<_>>()
        } else {
            row.port_specs.clone()
        };
        if ports.is_empty() {
            self.set_status("This service does not declare a published port", true);
        } else if ports.len() == 1 {
            self.input_mode = InputMode::Modal(Modal::PortInput {
                service: row.service,
                port_index: 0,
                current: ports[0].published.clone(),
                input: String::new(),
            });
        } else {
            self.input_mode = InputMode::Modal(Modal::PortSelect {
                service: row.service,
                ports,
                selected: 0,
            });
        }
    }

    fn request_diagnosis(&mut self) {
        let Some(service) = self.selected_row().map(|row| row.service.clone()) else {
            return;
        };
        self.tab = Tab::Diagnostics;
        self.set_status(format!("Scanning recent logs for {service}…"), false);
        self.request(BackendRequest::Diagnose(service));
    }

    fn request_integration_plan(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.set_status("Recalculating supported integrations…", false);
        self.request(BackendRequest::PlanIntegrations);
    }

    fn handle_backend_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::Busy(label) => {
                self.busy = Some(BusyState {
                    label: label.clone(),
                    started: Instant::now(),
                    operation_id: None,
                });
                self.set_status(label, false);
            }
            BackendEvent::Runtime(Ok(runtime)) => self.apply_runtime(runtime),
            BackendEvent::Runtime(Err(error)) => {
                self.refresh_pending = false;
                self.set_status(format!("Refresh failed: {error}"), true);
                self.push_activity(format!("ERROR · refresh failed · {error}"));
            }
            BackendEvent::ActionScope {
                service,
                action,
                result: Ok(scope),
            } => {
                self.set_status(
                    format!(
                        "{} scope ready · {} service(s)",
                        action.label(),
                        scope.len()
                    ),
                    false,
                );
                self.input_mode = InputMode::Modal(Modal::Confirm {
                    title: format!("{} {service}?", action.label()),
                    detail: vec![
                        match action {
                            ServiceAction::Enable => {
                                "Dependencies are enabled first; integrations are reconciled afterward."
                            }
                            ServiceAction::Disable => {
                                "Dependents stop first; service-owned dependencies are disabled too."
                            }
                            ServiceAction::Restart => {
                                "The selected service will be restarted in place."
                            }
                        }
                        .to_string(),
                        format!("Exact scope: {}", scope.join(" → ")),
                    ],
                    action: ConfirmedAction::Service { service, action },
                });
            }
            BackendEvent::ActionScope {
                service,
                action,
                result: Err(error),
            } => self.set_status(
                format!(
                    "Could not calculate {} scope for {service}: {error}",
                    action.label().to_ascii_lowercase()
                ),
                true,
            ),
            BackendEvent::OperationFinished { label, result } => {
                self.busy = None;
                match result {
                    Ok(_output) => {
                        if label == "Restore setup bundle" {
                            self.replace_inspection(None);
                        }
                        if label.starts_with("Delete setup bundle")
                            && self
                                .inspected_setup
                                .as_ref()
                                .is_some_and(|inspection| !inspection.temporary)
                        {
                            self.replace_inspection(None);
                        }
                        self.set_status(format!("{label} complete"), false);
                    }
                    Err(error) => {
                        self.set_status(format!("{label} failed: {error}"), true);
                        self.push_activity(format!("ERROR · {label} · {error}"));
                    }
                }
            }
            BackendEvent::Diagnosis {
                service,
                result: Ok(diagnosis),
            } => {
                let count = diagnosis.issues.len();
                self.diagnoses.insert(service.clone(), diagnosis);
                if let Some(row) = self.services.iter_mut().find(|row| row.service == service) {
                    row.issue_count = count;
                    row.issues = self
                        .diagnoses
                        .get(&service)
                        .map(|item| item.issues.clone())
                        .unwrap_or_default();
                }
                self.set_status(
                    format!("Log scan complete · {count} issue(s) in {service}"),
                    false,
                );
            }
            BackendEvent::Diagnosis {
                service,
                result: Err(error),
            } => self.set_status(format!("Log scan failed for {service}: {error}"), true),
            BackendEvent::ScanFinished(Ok(results)) => {
                let total = results
                    .values()
                    .map(|item| item.issues.len())
                    .sum::<usize>();
                self.diagnoses = results;
                for row in &mut self.services {
                    if let Some(diagnosis) = self.diagnoses.get(&row.service) {
                        row.issue_count = diagnosis.issues.len();
                        row.issues = diagnosis.issues.clone();
                    }
                }
                self.busy = None;
                self.set_status(
                    format!(
                        "Fleet log scan complete · {total} issue(s) across {} services",
                        self.diagnoses.len()
                    ),
                    false,
                );
            }
            BackendEvent::ScanFinished(Err(error)) => {
                self.busy = None;
                self.set_status(format!("Fleet log scan failed: {error}"), true);
            }
            BackendEvent::IntegrationPlan(Ok(plan)) => {
                let pending = plan.changes.len();
                let blocked = plan.blocked.len();
                self.integration_plan = Some(plan);
                self.busy = None;
                self.set_status(
                    format!("Integration watch · {pending} pending · {blocked} blocked/manual"),
                    false,
                );
            }
            BackendEvent::IntegrationPlan(Err(error)) => {
                self.busy = None;
                self.set_status(format!("Integration plan failed: {error}"), true);
            }
            BackendEvent::BackupsLoaded(Ok((health, records))) => {
                self.backup_health = Some(health);
                self.backups = records;
                self.selected_backup = self
                    .selected_backup
                    .min(self.backups.len().saturating_sub(1));
                self.set_status(
                    format!(
                        "Recovery watch · {} retained setup bundle(s)",
                        self.backups.len()
                    ),
                    false,
                );
            }
            BackendEvent::BackupsLoaded(Err(error)) => {
                self.set_status(format!("Could not read setup bundles: {error}"), true);
            }
            BackendEvent::BackupInspected {
                path,
                temporary,
                result: Ok(inspection),
            } => {
                let services = inspection.manifest.services.len();
                let mounts = inspection
                    .manifest
                    .mounts
                    .iter()
                    .filter(|mount| mount.present)
                    .count();
                self.replace_inspection(Some(InspectedSetup {
                    path,
                    temporary,
                    inspection,
                }));
                self.set_status(
                    format!("Setup inspected · {services} services · {mounts} state mounts"),
                    false,
                );
            }
            BackendEvent::BackupInspected {
                path,
                temporary,
                result: Err(error),
            } => {
                if temporary {
                    let _ = fs::remove_file(path);
                }
                self.set_status(format!("Setup inspection failed: {error}"), true);
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, page: usize) {
        match self.input_mode.clone() {
            InputMode::Filter => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => self.input_mode = InputMode::Normal,
                    KeyCode::Backspace => {
                        self.filter.pop();
                        self.select_first();
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.filter.push(character);
                        self.select_first();
                    }
                    _ => {}
                }
                return;
            }
            InputMode::Modal(modal) => {
                self.handle_modal_key(key, modal);
                return;
            }
            InputMode::Normal => {}
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.busy.is_some() {
                self.set_status(
                    "An operation is active; wait for it to finish before quitting",
                    true,
                );
            } else {
                self.should_quit = true;
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.compact_detail {
                    self.compact_detail = false;
                } else if self.busy.is_some() {
                    self.set_status(
                        "An operation is active; wait for it to finish before quitting",
                        true,
                    );
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.input_mode = InputMode::Modal(Modal::Help),
            KeyCode::Char('1') => self.tab = Tab::Services,
            KeyCode::Char('2') => {
                self.tab = Tab::Integrations;
                if self.integration_plan.is_none() {
                    self.request_integration_plan();
                }
            }
            KeyCode::Char('3') => self.tab = Tab::Diagnostics,
            KeyCode::Char('4') => {
                self.tab = Tab::Recovery;
                if self.backup_health.is_none() {
                    self.request_backups();
                }
            }
            KeyCode::Char('5') => self.tab = Tab::Activity,
            KeyCode::Tab => {
                self.tab = Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()];
                if self.tab == Tab::Integrations && self.integration_plan.is_none() {
                    self.request_integration_plan();
                }
                if self.tab == Tab::Recovery && self.backup_health.is_none() {
                    self.request_backups();
                }
            }
            KeyCode::BackTab => {
                self.tab = Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()];
                if self.tab == Tab::Recovery && self.backup_health.is_none() {
                    self.request_backups();
                }
            }
            _ => match self.tab {
                Tab::Services => self.handle_service_key(key, page),
                Tab::Integrations => self.handle_integration_key(key, page),
                Tab::Diagnostics => self.handle_diagnostics_key(key, page),
                Tab::Recovery => self.handle_recovery_key(key, page),
                Tab::Activity => self.handle_activity_key(key, page),
            },
        }
    }

    fn handle_service_key(&mut self, key: KeyEvent, page: usize) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-(page as isize)),
            KeyCode::PageDown => self.move_selection(page as isize),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_last(),
            KeyCode::Enter => self.compact_detail = !self.compact_detail,
            KeyCode::Char('/') => self.input_mode = InputMode::Filter,
            KeyCode::Char('e') => self.begin_service_action(ServiceAction::Enable),
            KeyCode::Char('d') => self.begin_service_action(ServiceAction::Disable),
            KeyCode::Char('R') => self.begin_service_action(ServiceAction::Restart),
            KeyCode::Char('p') => self.begin_port_change(),
            KeyCode::Char('l') => self.request_diagnosis(),
            KeyCode::Char('L') => {
                if self.busy.is_none() {
                    self.request(BackendRequest::ScanAll);
                }
            }
            KeyCode::Char('r') => {
                if !self.refresh_pending && self.busy.is_none() {
                    self.refresh_pending = true;
                    self.set_status("Refreshing Compose and Docker state…", false);
                    self.request(BackendRequest::Refresh(self.cpu_times));
                }
            }
            _ => {}
        }
    }

    fn handle_integration_key(&mut self, key: KeyEvent, page: usize) {
        match key.code {
            KeyCode::Char('r') => self.request_integration_plan(),
            KeyCode::Char('a') => {
                if self.busy.is_some() {
                    self.set_status(
                        "Wait for the active operation before applying integrations",
                        true,
                    );
                    return;
                }
                let Some(plan) = self.integration_plan.as_ref() else {
                    self.request_integration_plan();
                    return;
                };
                if plan.changes.is_empty() {
                    self.set_status("No supported integration changes are pending", false);
                    return;
                }
                self.input_mode = InputMode::Modal(Modal::Confirm {
                    title: format!("Apply {} integration change(s)?", plan.changes.len()),
                    detail: plan
                        .changes
                        .iter()
                        .take(6)
                        .map(|change| format!("{} · {}", change.target, change.summary))
                        .collect(),
                    action: ConfirmedAction::ApplyIntegrations,
                });
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(page as u16),
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(page as u16)
            }
            _ => {}
        }
    }

    fn handle_diagnostics_key(&mut self, key: KeyEvent, page: usize) {
        match key.code {
            KeyCode::Char('r') => self.request_diagnosis(),
            KeyCode::Char('f') => {
                let Some(row) = self.selected_row() else {
                    return;
                };
                let Some(diagnosis) = self.diagnoses.get(&row.service) else {
                    return;
                };
                let Some(fix) = &diagnosis.safe_fix else {
                    self.set_status("No conservative automatic fix is available", true);
                    return;
                };
                let action = if fix.action == "restart" {
                    ServiceAction::Restart
                } else {
                    ServiceAction::Enable
                };
                self.input_mode = InputMode::Modal(Modal::Confirm {
                    title: format!("{}?", fix.label),
                    detail: vec![format!("Service: {}", row.service)],
                    action: ConfirmedAction::Service {
                        service: row.service.clone(),
                        action,
                    },
                });
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(page as u16),
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(page as u16)
            }
            _ => {}
        }
    }

    fn handle_recovery_key(&mut self, key: KeyEvent, page: usize) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_backup_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_backup_selection(1),
            KeyCode::Home | KeyCode::Char('g') => self.selected_backup = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected_backup = self.backups.len().saturating_sub(1)
            }
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(page as u16),
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(page as u16)
            }
            KeyCode::Char('r') => self.request_backups(),
            KeyCode::Enter | KeyCode::Char('i') => {
                if let Some(path) = self.selected_backup().map(|record| record.path.clone()) {
                    self.set_status("Inspecting retained setup bundle…", false);
                    self.request(BackendRequest::InspectBackup(path));
                } else {
                    self.set_status("No retained setup bundle is selected", true);
                }
            }
            KeyCode::Char('u') => {
                self.input_mode = InputMode::Modal(Modal::TextInput {
                    title: "INSPECT SETUP SOURCE".to_string(),
                    detail:
                        "Enter an absolute local .tar.gz path or an HTTPS URL. The source is transferred and validated without applying it."
                            .to_string(),
                    prompt: "PATH / HTTPS URL".to_string(),
                    input: String::new(),
                    action: TextAction::InspectSetupSource,
                });
            }
            KeyCode::Char('e') => {
                if let Some(record) = self.selected_backup() {
                    self.set_status(format!("Export file · {}", record.path.display()), false);
                }
            }
            KeyCode::Char('c') => {
                if self.busy.is_some() {
                    self.set_status(
                        "Wait for the active operation before creating a bundle",
                        true,
                    );
                } else {
                    self.input_mode = InputMode::Modal(Modal::Confirm {
                        title: "Create an online setup bundle?".to_string(),
                        detail: vec![
                            "Compose, environment, enablement intent, configuration binds, and named volumes are included."
                                .to_string(),
                            "Media, downloads, caches, logs, devices, and generated assets remain excluded."
                                .to_string(),
                        ],
                        action: ConfirmedAction::CreateBackup,
                    });
                }
            }
            KeyCode::Char('p') => {
                if self.busy.is_some() {
                    self.set_status(
                        "Wait for the active operation before pruning retention",
                        true,
                    );
                } else {
                    let retention = self
                        .backup_health
                        .as_ref()
                        .map(|health| health.retention)
                        .unwrap_or(0);
                    self.input_mode = InputMode::Modal(Modal::Confirm {
                        title: "Prune setup bundle retention?".to_string(),
                        detail: vec![format!(
                            "Keep the newest {retention} bundle(s) and delete older retained archives."
                        )],
                        action: ConfirmedAction::PruneBackups,
                    });
                }
            }
            KeyCode::Char('d') => {
                if self.busy.is_some() {
                    self.set_status(
                        "Wait for the active operation before deleting a bundle",
                        true,
                    );
                } else if let Some(id) = self.selected_backup().map(|record| record.id.clone()) {
                    self.input_mode = InputMode::Modal(Modal::TextInput {
                        title: "DELETE RECOVERY BUNDLE".to_string(),
                        detail: "This permanently removes the selected private setup archive and catalog record."
                            .to_string(),
                        prompt: format!("TYPE {id}"),
                        input: String::new(),
                        action: TextAction::DeleteBackup { id },
                    });
                }
            }
            KeyCode::Char('R') => {
                if self.busy.is_some() {
                    self.set_status("Wait for the active operation before restoring setup", true);
                } else if let Some(inspected) = self.inspected_setup.as_ref() {
                    self.input_mode = InputMode::Modal(Modal::TextInput {
                        title: "STOPPED-STACK RESTORE".to_string(),
                        detail: "A pre-restore bundle is created, the stack stops, staged state is restored, Compose is validated, and failure rolls back."
                            .to_string(),
                        prompt: "TYPE RESTORE".to_string(),
                        input: String::new(),
                        action: TextAction::RestoreSetup {
                            path: inspected.path.clone(),
                            temporary: inspected.temporary,
                        },
                    });
                } else {
                    self.set_status(
                        "Inspect a retained, local, or HTTPS setup bundle first",
                        true,
                    );
                }
            }
            _ => {}
        }
    }

    fn handle_activity_key(&mut self, key: KeyEvent, page: usize) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.activity_scroll = self.activity_scroll.saturating_add(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.activity_scroll = self.activity_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => {
                self.activity_scroll = self.activity_scroll.saturating_add(page as u16)
            }
            KeyCode::PageDown | KeyCode::End => self.activity_scroll = 0,
            KeyCode::Char('c') if self.busy.is_none() => self.activity.clear(),
            _ => {}
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent, modal: Modal) {
        match modal {
            Modal::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter
                ) {
                    self.input_mode = InputMode::Normal;
                }
            }
            Modal::Confirm {
                title,
                detail,
                action,
            } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.input_mode = InputMode::Normal;
                    match action {
                        ConfirmedAction::Service { service, action } => {
                            self.request(BackendRequest::Action { service, action });
                        }
                        ConfirmedAction::ApplyIntegrations => {
                            if let Some(plan) = self.integration_plan.clone() {
                                self.request(BackendRequest::ApplyIntegrations(plan));
                            }
                        }
                        ConfirmedAction::CreateBackup => {
                            self.request(BackendRequest::CreateBackup);
                        }
                        ConfirmedAction::PruneBackups => {
                            self.request(BackendRequest::PruneBackups);
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.input_mode = InputMode::Normal;
                    self.set_status("Action cancelled", false);
                }
                _ => {
                    self.input_mode = InputMode::Modal(Modal::Confirm {
                        title,
                        detail,
                        action,
                    });
                }
            },
            Modal::PortSelect {
                service,
                ports,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(ports.len().saturating_sub(1));
                    }
                    KeyCode::Enter => {
                        self.input_mode = InputMode::Modal(Modal::PortInput {
                            service,
                            port_index: selected,
                            current: ports[selected].published.clone(),
                            input: String::new(),
                        });
                        return;
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.input_mode = InputMode::Normal;
                        return;
                    }
                    _ => {}
                }
                self.input_mode = InputMode::Modal(Modal::PortSelect {
                    service,
                    ports,
                    selected,
                });
            }
            Modal::PortInput {
                service,
                port_index,
                current,
                mut input,
            } => {
                match key.code {
                    KeyCode::Char(character) if character.is_ascii_digit() && input.len() < 5 => {
                        input.push(character);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        let port = input.parse::<u16>();
                        match port {
                            Ok(0) | Err(_) => {
                                self.set_status("Port must be between 1 and 65535", true);
                            }
                            Ok(port) => {
                                self.input_mode = InputMode::Normal;
                                self.request(BackendRequest::ChangePort {
                                    service,
                                    port_index,
                                    port,
                                });
                                return;
                            }
                        }
                    }
                    KeyCode::Esc => {
                        self.input_mode = InputMode::Normal;
                        self.set_status("Port change cancelled", false);
                        return;
                    }
                    _ => {}
                }
                self.input_mode = InputMode::Modal(Modal::PortInput {
                    service,
                    port_index,
                    current,
                    input,
                });
            }
            Modal::TextInput {
                title,
                detail,
                prompt,
                mut input,
                action,
            } => {
                match key.code {
                    KeyCode::Char(character)
                        if !key.modifiers.contains(KeyModifiers::CONTROL) && input.len() < 2048 =>
                    {
                        input.push(character);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        let value = input.trim().to_string();
                        match &action {
                            TextAction::InspectSetupSource if value.is_empty() => {
                                self.set_status("Enter a local path or HTTPS URL", true);
                            }
                            TextAction::InspectSetupSource => {
                                self.input_mode = InputMode::Normal;
                                self.set_status("Transferring and inspecting setup source…", false);
                                self.request(BackendRequest::InspectSource(value));
                                return;
                            }
                            TextAction::RestoreSetup { path, temporary } if value == "RESTORE" => {
                                self.input_mode = InputMode::Normal;
                                self.request(BackendRequest::RestoreBackup {
                                    path: path.clone(),
                                    temporary: *temporary,
                                });
                                return;
                            }
                            TextAction::RestoreSetup { .. } => {
                                self.set_status("Type RESTORE exactly to authorize", true);
                            }
                            TextAction::DeleteBackup { id } if value == *id => {
                                self.input_mode = InputMode::Normal;
                                self.request(BackendRequest::DeleteBackup(id.clone()));
                                return;
                            }
                            TextAction::DeleteBackup { id } => {
                                self.set_status(format!("Type {id} exactly to delete"), true);
                            }
                        }
                    }
                    KeyCode::Esc => {
                        self.input_mode = InputMode::Normal;
                        self.set_status("Protected action cancelled", false);
                        return;
                    }
                    _ => {}
                }
                self.input_mode = InputMode::Modal(Modal::TextInput {
                    title,
                    detail,
                    prompt,
                    input,
                    action,
                });
            }
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        let InputMode::Modal(Modal::TextInput {
            title,
            detail,
            prompt,
            mut input,
            action,
        }) = self.input_mode.clone()
        else {
            return;
        };
        let remaining = 2048_usize.saturating_sub(input.len());
        input.extend(
            pasted
                .chars()
                .filter(|character| *character != '\r' && *character != '\n')
                .take(remaining),
        );
        self.input_mode = InputMode::Modal(Modal::TextInput {
            title,
            detail,
            prompt,
            input,
            action,
        });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(inspected) = self.inspected_setup.take()
            && inspected.temporary
        {
            let _ = fs::remove_file(inspected.path);
        }
    }
}

pub fn run(control: ControlPlane, refresh_seconds: f64) -> Result<()> {
    let (request_tx, request_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    spawn_backend(control.clone(), request_rx, event_tx);

    let mut terminal = setup_terminal()?;
    let guard = TerminalGuard;
    let mut app = App::new(request_tx);
    app.request(BackendRequest::Refresh(None));
    let refresh_interval = Duration::from_secs_f64(refresh_seconds);
    let tick = Duration::from_millis(50);

    let result = (|| -> Result<()> {
        while !app.should_quit {
            app.sync_control(&control);
            while let Ok(message) = event_rx.try_recv() {
                app.handle_backend_event(message);
            }
            if app.busy.is_none()
                && !app.refresh_pending
                && app.last_refresh.elapsed() >= refresh_interval
            {
                app.refresh_pending = true;
                app.request(BackendRequest::Refresh(app.cpu_times));
            }
            app.frame = app.frame.wrapping_add(1);
            terminal.draw(|frame| render(frame, &mut app))?;

            if event::poll(tick)? {
                match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                        let page = terminal.size()?.height.saturating_sub(14).max(1) as usize;
                        app.handle_key(key, page);
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 8)
                        }
                        MouseEventKind::ScrollDown => {
                            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 8)
                        }
                        _ => {}
                    },
                    Event::Paste(pasted) => app.handle_paste(pasted),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
        let _ = app.request_tx.send(BackendRequest::Shutdown);
        Ok(())
    })();
    restore_terminal(&mut terminal)?;
    std::mem::forget(guard);
    result
}

pub fn demo_snapshot(width: u16, height: u16) -> Result<String> {
    render_snapshot(App::demo(), width, height)
}

pub fn snapshot_from_data(snapshot: Snapshot, width: u16, height: u16) -> Result<String> {
    let (request_tx, _) = mpsc::channel();
    let mut app = App::new(request_tx);
    app.services = snapshot.services;
    app.stats = snapshot.host_stats;
    app.status = "One-shot runtime snapshot".to_string();
    app.refresh_pending = false;
    app.integration_plan = Some(IntegrationPlan {
        changes: Vec::new(),
        blocked: snapshot.integrations.blocked,
    });
    app.selected_name = app.services.first().map(|row| row.service.clone());
    render_snapshot(app, width, height)
}

fn render_snapshot(mut app: App, width: u16, height: u16) -> Result<String> {
    if width < 40 || height < 16 {
        return Err(anyhow!("snapshot dimensions must be at least 40x16"));
    }
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render(frame, &mut app))?;
    Ok(buffer_text(terminal.backend().buffer()))
}

fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut output = String::new();
    for y in area.y..area.y + area.height {
        let mut line = String::new();
        for x in area.x..area.x + area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        output.push_str(line.trim_end());
        output.push('\n');
    }
    output
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("could not enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        Hide
    )?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("could not initialize terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("could not disable terminal raw mode")?;
    execute!(
        terminal.backend_mut(),
        Show,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            Show,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

fn spawn_backend(
    control: ControlPlane,
    requests: Receiver<BackendRequest>,
    events: Sender<BackendEvent>,
) {
    thread::spawn(move || {
        for request in requests {
            match request {
                BackendRequest::Refresh(previous_cpu) => {
                    let result = control
                        .manager()
                        .runtime(previous_cpu)
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::Runtime(result));
                }
                BackendRequest::ActionScope { service, action } => {
                    let result = control
                        .manager()
                        .action_scope(&service, action)
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::ActionScope {
                        service,
                        action,
                        result,
                    });
                }
                BackendRequest::Action { service, action } => {
                    let label = format!("{} {service}", action.label());
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .run_service_action(&service, action, "tui")
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let _ = events.send(BackendEvent::Runtime(
                        control.manager().runtime(None).map_err(format_error),
                    ));
                }
                BackendRequest::ChangePort {
                    service,
                    port_index,
                    port,
                } => {
                    let label = format!("Change {service} port to {port}");
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .change_port(&service, port_index, port, "tui")
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let _ = events.send(BackendEvent::Runtime(
                        control.manager().runtime(None).map_err(format_error),
                    ));
                }
                BackendRequest::Diagnose(service) => {
                    let result = (|| -> Result<Diagnosis> {
                        let runtime = control.manager().runtime(None)?;
                        let row = runtime
                            .services
                            .iter()
                            .find(|row| row.service == service)
                            .ok_or_else(|| {
                                anyhow!("service {service:?} is no longer configured")
                            })?;
                        diagnose(row, &runtime.containers, 100)
                    })()
                    .map_err(format_error);
                    let _ = events.send(BackendEvent::Diagnosis { service, result });
                }
                BackendRequest::ScanAll => {
                    let _ = events.send(BackendEvent::Busy(
                        "Scanning logs across the service fleet".to_string(),
                    ));
                    let result = (|| -> Result<BTreeMap<String, Diagnosis>> {
                        let runtime = control.manager().runtime(None)?;
                        scan_all(&runtime.services, &runtime.containers)
                    })()
                    .map_err(format_error);
                    let _ = events.send(BackendEvent::ScanFinished(result));
                }
                BackendRequest::PlanIntegrations => {
                    let _ = events.send(BackendEvent::Busy(
                        "Recalculating supported integrations".to_string(),
                    ));
                    let result = (|| -> Result<IntegrationPlan> {
                        let runtime = control.manager().runtime(None)?;
                        integration_plan(control.manager(), &runtime)
                    })()
                    .map_err(format_error);
                    let _ = events.send(BackendEvent::IntegrationPlan(result));
                }
                BackendRequest::ApplyIntegrations(plan) => {
                    let label = format!("Apply {} integration change(s)", plan.changes.len());
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .apply_integrations(&plan, "tui")
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let refreshed = (|| -> Result<IntegrationPlan> {
                        let runtime = control.manager().runtime(None)?;
                        integration_plan(control.manager(), &runtime)
                    })()
                    .map_err(format_error);
                    let _ = events.send(BackendEvent::IntegrationPlan(refreshed));
                    let _ = events.send(BackendEvent::Runtime(
                        control.manager().runtime(None).map_err(format_error),
                    ));
                }
                BackendRequest::LoadBackups => {
                    let _ = events.send(BackendEvent::BackupsLoaded(load_backup_catalog(&control)));
                }
                BackendRequest::CreateBackup => {
                    let label = "Create manual setup bundle".to_string();
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .create_backup("manual", "tui")
                        .map(|record| format!("Created {}", record.path.display()))
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let _ = events.send(BackendEvent::BackupsLoaded(load_backup_catalog(&control)));
                }
                BackendRequest::PruneBackups => {
                    let label = "Prune setup bundle retention".to_string();
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .prune_backups("tui")
                        .map(|removed| format!("Removed {removed} expired setup bundle(s)"))
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let _ = events.send(BackendEvent::BackupsLoaded(load_backup_catalog(&control)));
                }
                BackendRequest::DeleteBackup(id) => {
                    let label = format!("Delete setup bundle {id}");
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .delete_backup(&id, "tui")
                        .map(|()| format!("Deleted setup bundle {id}"))
                        .map_err(format_error);
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let _ = events.send(BackendEvent::BackupsLoaded(load_backup_catalog(&control)));
                }
                BackendRequest::InspectBackup(path) => {
                    let result = control.backup_store().inspect(&path).map_err(format_error);
                    let _ = events.send(BackendEvent::BackupInspected {
                        path,
                        temporary: false,
                        result,
                    });
                }
                BackendRequest::InspectSource(source) => {
                    let (path, temporary, result) = inspect_setup_source(&control, &source);
                    let _ = events.send(BackendEvent::BackupInspected {
                        path,
                        temporary,
                        result,
                    });
                }
                BackendRequest::RestoreBackup { path, temporary } => {
                    let label = "Restore setup bundle".to_string();
                    let _ = events.send(BackendEvent::Busy(label.clone()));
                    let result = control
                        .restore_bundle(&path, "tui")
                        .map(|report| report.message)
                        .map_err(format_error);
                    if result.is_ok() && temporary {
                        let _ = fs::remove_file(&path);
                    }
                    let _ = events.send(BackendEvent::OperationFinished { label, result });
                    let _ = events.send(BackendEvent::BackupsLoaded(load_backup_catalog(&control)));
                    let _ = events.send(BackendEvent::Runtime(
                        control.manager().runtime(None).map_err(format_error),
                    ));
                }
                BackendRequest::Shutdown => break,
            }
        }
    });
}

fn load_backup_catalog(
    control: &ControlPlane,
) -> std::result::Result<(BackupHealth, Vec<BackupRecord>), String> {
    (|| -> Result<(BackupHealth, Vec<BackupRecord>)> {
        Ok((control.backup_health()?, control.backup_store().list()?))
    })()
    .map_err(format_error)
}

fn inspect_setup_source(
    control: &ControlPlane,
    source: &str,
) -> (PathBuf, bool, std::result::Result<BundleInspection, String>) {
    let source = source.trim();
    if source.starts_with("https://") {
        let staged = (|| -> Result<(PathBuf, BundleInspection)> {
            let (path, file) = control.backup_store().create_staging_file()?;
            drop(file);
            if let Err(error) = crate::web::download_https_bundle(
                source,
                &path,
                control.backup_store().policy().max_import_bytes,
            ) {
                let _ = fs::remove_file(&path);
                return Err(error);
            }
            let inspection = control.backup_store().inspect(&path)?;
            Ok((path, inspection))
        })();
        return match staged {
            Ok((path, inspection)) => (path, true, Ok(inspection)),
            Err(error) => (PathBuf::new(), true, Err(format_error(error))),
        };
    }
    let path = PathBuf::from(source);
    let result = if !path.is_absolute() {
        Err("local setup paths must be absolute".to_string())
    } else {
        control.backup_store().inspect(&path).map_err(format_error)
    };
    (path, false, result)
}

fn format_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(INK)), area);
    if area.width < 40 || area.height < 16 {
        let warning = Paragraph::new(Text::from(vec![
            Line::styled("TERMINAL TOO SMALL", Style::default().fg(BAD).bold()),
            Line::raw(""),
            Line::raw(format!("Current: {}×{}", area.width, area.height)),
            Line::raw("Minimum: 40×16"),
            Line::raw("Resize the terminal or press q to exit."),
        ]))
        .block(instrument_block("ARRMAN WATCH", SEA_GLASS))
        .alignment(Alignment::Center);
        frame.render_widget(warning, area);
        return;
    }

    let header_height = if area.height < 25 { 4 } else { 5 };
    let footer_height = 2;
    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(8),
            Constraint::Length(footer_height),
        ])
        .split(area);
    draw_header(frame, shell[0], app);
    match app.tab {
        Tab::Services => draw_services(frame, shell[1], app),
        Tab::Integrations => draw_integrations(frame, shell[1], app),
        Tab::Diagnostics => draw_diagnostics(frame, shell[1], app),
        Tab::Recovery => draw_recovery(frame, shell[1], app),
        Tab::Activity => draw_activity(frame, shell[1], app, true),
    }
    draw_footer(frame, shell[2], app);

    if let InputMode::Modal(modal) = &app.input_mode {
        draw_modal(frame, area, modal);
    }
}

fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if area.height >= 5 {
            vec![
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(2),
            ]
        } else {
            vec![
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(2),
            ]
        })
        .split(area);
    let running = app.services.iter().filter(|row| row.running).count();
    let flagged = app
        .services
        .iter()
        .map(|row| row.issue_count)
        .sum::<usize>();
    let title = Line::from(vec![
        Span::styled(" ARRMAN ", Style::default().fg(INK).bg(SEA_GLASS).bold()),
        Span::styled("  ENGINEERING WATCH", Style::default().fg(TEXT).bold()),
        Span::styled(
            format!(
                "   {} RUNNING / {} TOTAL / {} FLAGGED",
                running,
                app.services.len(),
                flagged
            ),
            Style::default().fg(if flagged > 0 { WARN } else { MUTED }),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title).style(Style::default().bg(PANEL)),
        rows[0],
    );

    let stats = &app.stats;
    let memory = percentage(stats.memory_used, stats.memory_total);
    let disk_used = stats
        .disk_total
        .zip(stats.disk_free)
        .map(|(total, free)| total.saturating_sub(free));
    let disk = percentage(disk_used, stats.disk_total);
    let metrics = Line::from(vec![
        metric("HOST", &stats.ip, INFO),
        separator(),
        metric(
            "CPU",
            stats
                .cpu_percent
                .map(|value| format!("{value:>4.1}%"))
                .unwrap_or_else(|| "sample".to_string()),
            level_color(stats.cpu_percent.unwrap_or(0.0)),
        ),
        separator(),
        metric("LOAD", format_optional(stats.load_1m, 2), TEXT),
        separator(),
        metric(
            "MEM",
            memory
                .map(|value| format!("{value:>3.0}%"))
                .unwrap_or_else(|| "-".to_string()),
            level_color(memory.unwrap_or(0.0)),
        ),
        separator(),
        metric(
            if disk.unwrap_or(0.0) >= 90.0 {
                "DISK!"
            } else {
                "DISK"
            },
            disk.map(|value| format!("{value:>3.0}%"))
                .unwrap_or_else(|| "-".to_string()),
            level_color(disk.unwrap_or(0.0)),
        ),
        separator(),
        metric("UP", format_uptime(stats.uptime_seconds), MUTED),
    ]);
    frame.render_widget(
        Paragraph::new(metrics).style(Style::default().fg(TEXT).bg(PANEL)),
        rows[1],
    );

    let status_index = if area.height >= 5 { 2 } else { 1 };
    let status_color = if app.status_error {
        BAD
    } else if app.busy.is_some() {
        WARN
    } else {
        MUTED
    };
    let status = if let Some(busy) = &app.busy {
        format!(
            "{} {} · {}s · navigation remains live",
            spinner(app.frame),
            busy.label,
            busy.started.elapsed().as_secs()
        )
    } else if matches!(app.input_mode, InputMode::Filter) {
        format!("FILTER / {}_", app.filter)
    } else if !app.filter.is_empty() {
        format!("{} · filter: {}", app.status, app.filter)
    } else {
        app.status.clone()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" WATCH ", Style::default().fg(INK).bg(status_color).bold()),
            Span::styled(format!(" {status}"), Style::default().fg(status_color)),
        ]))
        .style(Style::default().bg(PANEL)),
        rows[status_index],
    );

    let tabs_index = status_index + 1;
    let titles = [
        "1 SERVICES",
        "2 INTEGRATIONS",
        "3 DIAGNOSTICS",
        "4 RECOVERY",
        "5 ACTIVITY",
    ];
    frame.render_widget(
        Tabs::new(titles)
            .select(app.tab.index())
            .style(Style::default().fg(MUTED).bg(INK))
            .highlight_style(Style::default().fg(SEA_GLASS).bold())
            .divider(Span::styled("  ·  ", Style::default().fg(STEEL)))
            .padding(" ", " "),
        rows[tabs_index],
    );
}

fn draw_services(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let activity_height = if area.height >= 28 {
        7
    } else if area.height >= 21 {
        5
    } else {
        3
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(activity_height)])
        .split(area);
    let wide = area.width >= 120;
    let medium = area.width >= 88;
    if app.compact_detail && !medium {
        draw_inspector(frame, rows[0], app.selected_row(), app.detail_scroll);
    } else if wide {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(61),
                Constraint::Length(1),
                Constraint::Min(38),
            ])
            .split(rows[0]);
        draw_service_table(frame, columns[0], app, true);
        draw_inspector(frame, columns[2], app.selected_row(), app.detail_scroll);
    } else if medium {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(57),
                Constraint::Length(1),
                Constraint::Min(36),
            ])
            .split(rows[0]);
        draw_service_table(frame, columns[0], app, false);
        draw_inspector(frame, columns[2], app.selected_row(), app.detail_scroll);
    } else {
        let stacked = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(rows[0]);
        draw_service_table(frame, stacked[0], app, false);
        draw_compact_inspector(frame, stacked[1], app.selected_row());
    }
    draw_activity(frame, rows[1], app, false);
}

fn draw_service_table(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App, show_url: bool) {
    let indices = app.filtered_indices();
    let rows = indices.iter().map(|index| {
        let row = &app.services[*index];
        let (badge, color) = status_badge(row);
        let issue = if row.issue_count > 0 {
            format!("!{}", row.issue_count)
        } else if !row.dependencies_ready {
            "!dep".to_string()
        } else {
            String::new()
        };
        let mut cells = vec![
            Cell::from(Span::styled(badge, Style::default().fg(color).bold())),
            Cell::from(row.service.clone()),
            Cell::from(row.purpose.clone()),
            Cell::from(Span::styled(issue, Style::default().fg(WARN).bold())),
        ];
        if show_url {
            cells.push(Cell::from(
                row.urls.first().cloned().unwrap_or_else(|| "-".to_string()),
            ));
        }
        Row::new(cells).style(Style::default().fg(TEXT))
    });
    let mut widths = vec![
        Constraint::Length(8),
        Constraint::Length(if show_url { 19 } else { 17 }),
        Constraint::Min(18),
        Constraint::Length(5),
    ];
    if show_url {
        widths.push(Constraint::Length(31));
    }
    let header_cells = if show_url {
        vec!["STATE", "SERVICE", "ROLE", "ISS", "ENDPOINT"]
    } else {
        vec!["STATE", "SERVICE", "ROLE", "ISS"]
    };
    let table = Table::new(rows, widths)
        .header(
            Row::new(header_cells)
                .style(Style::default().fg(MUTED).bold())
                .bottom_margin(1),
        )
        .block(instrument_block(
            format!("SERVICE WATCH · {} VISIBLE", indices.len()),
            SEA_GLASS,
        ))
        .row_highlight_style(
            Style::default()
                .bg(PANEL_ACTIVE)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");
    let mut state = TableState::default().with_selected(
        (!indices.is_empty()).then_some(app.selected.min(indices.len().saturating_sub(1))),
    );
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_inspector(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    row: Option<&ServiceRow>,
    scroll: u16,
) {
    let Some(row) = row else {
        frame.render_widget(
            Paragraph::new("No service matches the current filter.")
                .style(Style::default().fg(MUTED))
                .block(instrument_block("INTERLOCK INSPECTOR", STEEL)),
            area,
        );
        return;
    };
    let (badge, color) = status_badge(row);
    let dependencies = if row.depends_on.is_empty() {
        "none".to_string()
    } else {
        row.depends_on
            .iter()
            .map(|name| {
                format!(
                    "{} [{}]",
                    name,
                    row.dependency_status
                        .get(name)
                        .map(|value| concise_status(value))
                        .unwrap_or("missing")
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {badge} "),
                Style::default().fg(INK).bg(color).bold(),
            ),
            Span::styled(
                format!("  {}", row.service),
                Style::default().fg(TEXT).bold(),
            ),
        ]),
        Line::raw(""),
        field_line("ROLE", &row.purpose),
        field_line("STATUS", &row.status),
        field_line("URL", row.urls.join(", ")),
        field_line("IMAGE", &row.image),
        field_line("PORTS", display_or(&row.ports, "none")),
        field_line("MOUNTS", row.mounts.to_string()),
        field_line(
            "HEALTH",
            if row.healthcheck_configured {
                "declared"
            } else {
                "not declared"
            },
        ),
        field_line(
            "INTERLOCK",
            if row.dependencies_ready {
                "ready"
            } else {
                "blocked"
            },
        ),
        field_line("DEPENDS", &dependencies),
        Line::raw(""),
        Line::styled(
            if row.issue_count == 0 {
                "No detected log issues in the current scan.".to_string()
            } else {
                format!(
                    "{} detected issue(s) · press l for evidence",
                    row.issue_count
                )
            },
            Style::default().fg(if row.issue_count == 0 { GOOD } else { WARN }),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(TEXT))
            .block(instrument_block("INTERLOCK INSPECTOR", INFO))
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        area,
    );
}

fn draw_compact_inspector(frame: &mut ratatui::Frame<'_>, area: Rect, row: Option<&ServiceRow>) {
    let Some(row) = row else {
        return;
    };
    let (badge, color) = status_badge(row);
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(badge, Style::default().fg(color).bold()),
            Span::styled(
                format!("  {} · {}", row.service, row.purpose),
                Style::default().fg(TEXT).bold(),
            ),
        ]),
        Line::styled(
            format!(
                "{} · {} · {}",
                row.status,
                row.urls
                    .first()
                    .map(String::as_str)
                    .unwrap_or("no endpoint"),
                if row.dependencies_ready {
                    "interlocks ready"
                } else {
                    "dependency blocked"
                }
            ),
            Style::default().fg(MUTED),
        ),
        Line::styled("Enter opens full detail", Style::default().fg(STEEL)),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .block(instrument_block("SELECTED CHANNEL", INFO))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_activity(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App, full: bool) {
    let visible = area.height.saturating_sub(2) as usize;
    let skip = app.activity_scroll as usize;
    let lines = app
        .activity
        .iter()
        .rev()
        .skip(skip)
        .take(visible.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| {
            Line::from(vec![
                Span::styled(format!("{}  ", line.at), Style::default().fg(STEEL)),
                Span::styled(
                    line.text.clone(),
                    Style::default().fg(if line.error { BAD } else { TEXT }),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let empty = if app.busy.is_some() {
        vec![Line::styled(
            "Command accepted; waiting for process output…",
            Style::default().fg(WARN),
        )]
    } else {
        vec![Line::styled(
            "No command activity yet. Service actions stream here without leaving the TUI.",
            Style::default().fg(MUTED),
        )]
    };
    frame.render_widget(
        Paragraph::new(if lines.is_empty() { empty } else { lines })
            .block(instrument_block(
                if full {
                    "WATCH LOG · FULL"
                } else {
                    "LIVE COMMAND CHANNEL"
                },
                if app.busy.is_some() { WARN } else { STEEL },
            ))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_integrations(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(plan) = &app.integration_plan else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Reading supported service APIs…", Style::default().fg(WARN)),
                Line::raw(""),
                Line::styled(
                    "Navigation remains available while the plan is assembled.",
                    Style::default().fg(MUTED),
                ),
            ])
            .block(instrument_block("INTEGRATION INTERLOCKS", SEA_GLASS)),
            area,
        );
        return;
    };
    let sections = if area.width >= 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(area)
    };
    let pending = if plan.changes.is_empty() {
        vec![ListItem::new(Line::styled(
            "[ALIGNED] No supported changes are pending.",
            Style::default().fg(GOOD),
        ))]
    } else {
        plan.changes
            .iter()
            .map(|change| {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("[{}] ", change.action.to_ascii_uppercase()),
                            Style::default().fg(WARN).bold(),
                        ),
                        Span::styled(change.target.clone(), Style::default().fg(TEXT).bold()),
                    ]),
                    Line::styled(format!("  {}", change.summary), Style::default().fg(MUTED)),
                ])
            })
            .collect()
    };
    frame.render_widget(
        List::new(pending).block(instrument_block(
            format!("PENDING · {}", plan.changes.len()),
            if plan.changes.is_empty() { GOOD } else { WARN },
        )),
        sections[0],
    );
    let blocked = if plan.blocked.is_empty() {
        vec![ListItem::new(Line::styled(
            "No blocked or manual prerequisites.",
            Style::default().fg(GOOD),
        ))]
    } else {
        plan.blocked
            .iter()
            .map(|item| {
                ListItem::new(vec![
                    Line::styled(item.target.clone(), Style::default().fg(BAD).bold()),
                    Line::styled(format!("  {}", item.reason), Style::default().fg(MUTED)),
                ])
            })
            .collect()
    };
    frame.render_widget(
        List::new(blocked).block(instrument_block(
            format!("BLOCKED / MANUAL · {}", plan.blocked.len()),
            if plan.blocked.is_empty() { GOOD } else { BAD },
        )),
        sections[1],
    );
}

fn draw_diagnostics(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let Some(diagnosis) = app.diagnoses.get(&row.service) else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    format!("No log scan is cached for {}.", row.service),
                    Style::default().fg(MUTED),
                ),
                Line::raw(""),
                Line::styled(
                    "Press r to scan this service.",
                    Style::default().fg(SEA_GLASS),
                ),
                Line::styled(
                    "Press 1, select another service, then l to inspect it.",
                    Style::default().fg(MUTED),
                ),
            ])
            .block(instrument_block("FAULT ANALYSIS", INFO)),
            area,
        );
        return;
    };
    let sections = Layout::default()
        .direction(if area.width >= 100 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(area);
    let issue_items = if diagnosis.issues.is_empty() {
        vec![ListItem::new(vec![
            Line::styled(
                "[CLEAR] No known issue patterns",
                Style::default().fg(GOOD).bold(),
            ),
            Line::styled(diagnosis.manual.clone(), Style::default().fg(MUTED)),
        ])]
    } else {
        diagnosis
            .issues
            .iter()
            .map(|issue| {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("[{}] ", issue.kind),
                            Style::default().fg(BAD).bold(),
                        ),
                        Span::styled(issue.summary.clone(), Style::default().fg(TEXT)),
                    ]),
                    Line::styled(
                        format!("Evidence · {}", issue.evidence),
                        Style::default().fg(WARN),
                    ),
                    Line::styled(
                        format!("Recovery · {}", issue.manual),
                        Style::default().fg(MUTED),
                    ),
                ])
            })
            .collect()
    };
    frame.render_widget(
        List::new(issue_items).block(instrument_block(
            format!("FAULTS · {} · {}", diagnosis.issues.len(), row.service),
            if diagnosis.issues.is_empty() {
                GOOD
            } else {
                BAD
            },
        )),
        sections[0],
    );
    let logs = diagnosis
        .logs
        .iter()
        .map(|line| Line::styled(line.clone(), Style::default().fg(MUTED)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(logs)
            .block(instrument_block("RECENT LOG TAIL", STEEL))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        sections[1],
    );
}

fn draw_recovery(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(8)])
        .split(area);
    let health_lines = if let Some(health) = &app.backup_health {
        let (state, color) = if health.last_error.is_some() {
            ("[FAILED]", BAD)
        } else if health.overdue {
            ("[OVERDUE]", WARN)
        } else {
            ("[ON SCHEDULE]", GOOD)
        };
        let last = health
            .last_success
            .as_ref()
            .map(|record| {
                format!(
                    "{} · {}",
                    format_rfc3339(&record.created_at),
                    record.file_name
                )
            })
            .unwrap_or_else(|| "No successful bundle yet".to_string());
        let next = health
            .next_due_at
            .as_deref()
            .map(format_rfc3339)
            .unwrap_or_else(|| "due now".to_string());
        let attempt = health
            .last_attempt_at
            .as_deref()
            .map(format_rfc3339)
            .unwrap_or_else(|| "No attempt in this session".to_string());
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{state} "), Style::default().fg(color).bold()),
                Span::styled(
                    format!(
                        "every {} · keep {} · next {}",
                        human_seconds(health.interval_seconds),
                        health.retention,
                        next
                    ),
                    Style::default().fg(TEXT),
                ),
            ]),
            Line::styled(format!("Last success · {last}"), Style::default().fg(MUTED)),
            Line::styled(
                format!("Last attempt · {attempt}"),
                Style::default().fg(MUTED),
            ),
            Line::styled(
                format!("Storage · {}", health.directory),
                Style::default().fg(INFO),
            ),
        ];
        if let Some(error) = &health.last_error {
            lines.push(Line::styled(
                format!("Failure · {error}"),
                Style::default().fg(BAD),
            ));
            lines.push(Line::styled(
                health.recovery.clone().unwrap_or_else(|| {
                    "Check storage and Activity, then retry a manual bundle.".to_string()
                }),
                Style::default().fg(WARN),
            ));
        }
        lines
    } else {
        vec![Line::styled(
            "Reading recurring-backup policy and retained bundles…",
            Style::default().fg(WARN),
        )]
    };
    frame.render_widget(
        Paragraph::new(health_lines)
            .block(instrument_block("RECURRING BACKUP HEALTH", SEA_GLASS))
            .wrap(Wrap { trim: true }),
        rows[0],
    );

    let columns = Layout::default()
        .direction(if area.width >= 100 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(rows[1]);
    let records = if app.backups.is_empty() {
        vec![ListItem::new(Line::styled(
            "No retained bundle. Press c to create one.",
            Style::default().fg(MUTED),
        ))]
    } else {
        app.backups
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let selected = index == app.selected_backup;
                let style = if selected {
                    Style::default().fg(INK).bg(SEA_GLASS).bold()
                } else {
                    Style::default().fg(TEXT)
                };
                ListItem::new(vec![
                    Line::styled(
                        format!(
                            "{} {} · {}",
                            if selected { ">" } else { " " },
                            format_rfc3339(&record.created_at),
                            human_bytes(record.size_bytes)
                        ),
                        style,
                    ),
                    Line::styled(
                        format!(
                            "  {} · {} state mounts · {} services",
                            record.kind, record.mount_count, record.service_count
                        ),
                        if selected {
                            Style::default().fg(INK).bg(SEA_GLASS)
                        } else {
                            Style::default().fg(MUTED)
                        },
                    ),
                ])
            })
            .collect()
    };
    frame.render_widget(
        List::new(records).block(instrument_block(
            format!("SETUP BUNDLES · {}", app.backups.len()),
            if app.backups.is_empty() { WARN } else { GOOD },
        )),
        columns[0],
    );

    let detail = if let Some(inspected) = &app.inspected_setup {
        let manifest = &inspected.inspection.manifest;
        let mut lines = vec![
            Line::styled(
                "[INSPECTED · NOT APPLIED]",
                Style::default().fg(WARN).bold(),
            ),
            Line::styled(
                inspected.inspection.file_name.clone(),
                Style::default().fg(TEXT).bold(),
            ),
            Line::styled(
                format!("Source host · {}", manifest.host),
                Style::default().fg(MUTED),
            ),
            Line::styled(
                format!(
                    "{} services · {} state mounts · {} excluded",
                    manifest.services.len(),
                    manifest.mounts.iter().filter(|mount| mount.present).count(),
                    manifest.excluded_mounts.len() + manifest.excluded_paths.len()
                ),
                Style::default().fg(TEXT),
            ),
            Line::styled(
                format!("Consistency · {}", manifest.consistency),
                Style::default().fg(WARN),
            ),
            Line::styled(
                format!("SHA-256 · {}", inspected.inspection.sha256),
                Style::default().fg(INFO),
            ),
            Line::raw(""),
            Line::styled("WARNINGS", Style::default().fg(WARN).bold()),
        ];
        lines.extend(
            inspected
                .inspection
                .warnings
                .iter()
                .map(|warning| Line::styled(format!("· {warning}"), Style::default().fg(WARN))),
        );
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "EXCLUDED MOUNTS",
            Style::default().fg(MUTED).bold(),
        ));
        if manifest.excluded_mounts.is_empty() {
            lines.push(Line::styled("· none", Style::default().fg(MUTED)));
        } else {
            lines.extend(manifest.excluded_mounts.iter().map(|mount| {
                Line::styled(
                    format!("· {} · {}", mount.source, mount.reason),
                    Style::default().fg(MUTED),
                )
            }));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "EXCLUDED TRANSIENT PATHS",
            Style::default().fg(MUTED).bold(),
        ));
        if manifest.excluded_paths.is_empty() {
            lines.push(Line::styled("· none", Style::default().fg(MUTED)));
        } else {
            lines.extend(manifest.excluded_paths.iter().map(|item| {
                Line::styled(
                    format!("· {} · {} · {}", item.mount_id, item.path, item.reason),
                    Style::default().fg(MUTED),
                )
            }));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Press R, then type RESTORE exactly. Progress and rollback report in Activity.",
            Style::default().fg(BAD).bold(),
        ));
        lines
    } else if let Some(record) = app.selected_backup() {
        vec![
            Line::styled(record.file_name.clone(), Style::default().fg(TEXT).bold()),
            Line::styled(
                format!("Created · {}", format_rfc3339(&record.created_at)),
                Style::default().fg(MUTED),
            ),
            Line::styled(
                format!(
                    "Kind · {} · consistency {}",
                    record.kind, record.consistency
                ),
                Style::default().fg(MUTED),
            ),
            Line::styled(
                format!("Size · {}", human_bytes(record.size_bytes)),
                Style::default().fg(TEXT),
            ),
            Line::styled(
                format!("Export file · {}", record.path.display()),
                Style::default().fg(INFO),
            ),
            Line::raw(""),
            Line::styled(
                "Press Enter/i to inspect its manifest before restore.",
                Style::default().fg(SEA_GLASS),
            ),
        ]
    } else {
        vec![
            Line::styled("No setup bundle selected.", Style::default().fg(MUTED)),
            Line::raw(""),
            Line::styled(
                "c creates an online bundle; u inspects a local file or HTTPS URL.",
                Style::default().fg(SEA_GLASS),
            ),
        ]
    };
    frame.render_widget(
        Paragraph::new(detail)
            .block(instrument_block("INSPECTION / RESTORE", WARN))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        columns[1],
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let hints = match app.tab {
        Tab::Services if area.width >= 105 => {
            "↑↓/jk move  / filter  Enter detail  e enable  d disable  R restart  p port  l logs  L scan all  ? help  q quit"
        }
        Tab::Services => "↑↓ move  / filter  e enable  d disable  p port  l logs  ? help  q quit",
        Tab::Integrations => {
            "a apply with confirmation  r recalculate  ↑↓ scroll  1 services  ? help  q quit"
        }
        Tab::Diagnostics => {
            "r rescan selected  f suggested fix  ↑↓ scroll  1 services  ? help  q quit"
        }
        Tab::Recovery => {
            "↑↓ bundles  c create  Enter inspect  u file/URL  R restore  e export path  p prune  d delete  r refresh"
        }
        Tab::Activity => {
            "↑↓/PgUp scroll  End latest  c clear when idle  1 services  ? help  q quit"
        }
    };
    frame.render_widget(
        Paragraph::new(Line::styled(hints, Style::default().fg(MUTED)))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(STEEL)),
            )
            .alignment(Alignment::Center),
        area,
    );
}

fn draw_modal(frame: &mut ratatui::Frame<'_>, area: Rect, modal: &Modal) {
    let (width, height) = match modal {
        Modal::Help => (72, 27),
        Modal::Confirm { detail, .. } => (76, (detail.len() as u16 + 8).clamp(11, 20)),
        Modal::PortSelect { ports, .. } => (58, (ports.len() as u16 + 7).clamp(10, 18)),
        Modal::PortInput { .. } => (58, 11),
        Modal::TextInput { .. } => (84, 14),
    };
    let popup = centered_rect(
        width.min(area.width.saturating_sub(4)),
        height.min(area.height.saturating_sub(2)),
        area,
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(Block::default().style(Style::default().bg(PANEL)), popup);
    match modal {
        Modal::Help => draw_help_modal(frame, popup),
        Modal::Confirm { title, detail, .. } => {
            let mut lines = vec![
                Line::styled(title.clone(), Style::default().fg(TEXT).bold()),
                Line::raw(""),
            ];
            lines.extend(
                detail
                    .iter()
                    .map(|line| Line::styled(line.clone(), Style::default().fg(MUTED))),
            );
            lines.extend([
                Line::raw(""),
                Line::from(vec![
                    Span::styled(" y / Enter ", Style::default().fg(INK).bg(SEA_GLASS).bold()),
                    Span::styled(" confirm    ", Style::default().fg(TEXT)),
                    Span::styled(" n / Esc ", Style::default().fg(TEXT).bg(STEEL)),
                    Span::styled(" cancel", Style::default().fg(TEXT)),
                ]),
            ]);
            frame.render_widget(
                Paragraph::new(lines)
                    .block(instrument_block("CONFIRM OPERATION", WARN))
                    .wrap(Wrap { trim: true }),
                popup,
            );
        }
        Modal::PortSelect {
            service,
            ports,
            selected,
        } => {
            let items = ports.iter().enumerate().map(|(index, port)| {
                let style = if index == *selected {
                    Style::default().fg(INK).bg(SEA_GLASS).bold()
                } else {
                    Style::default().fg(TEXT)
                };
                ListItem::new(Line::styled(
                    format!(
                        " {:>5}:{} / {} ",
                        port.published, port.target, port.protocol
                    ),
                    style,
                ))
            });
            frame.render_widget(
                List::new(items).block(instrument_block(
                    format!("SELECT {service} PORT · ↑↓ ENTER · ESC"),
                    SEA_GLASS,
                )),
                popup,
            );
        }
        Modal::PortInput {
            service,
            current,
            input,
            ..
        } => {
            let lines = vec![
                Line::styled(
                    format!("Service · {service}"),
                    Style::default().fg(TEXT).bold(),
                ),
                Line::styled(
                    format!("Current host port · {current}"),
                    Style::default().fg(MUTED),
                ),
                Line::raw(""),
                Line::from(vec![
                    Span::styled(" NEW PORT ", Style::default().fg(INK).bg(SEA_GLASS).bold()),
                    Span::styled(format!("  {input}_"), Style::default().fg(TEXT).bold()),
                ]),
                Line::raw(""),
                Line::styled(
                    "Enter validates, backs up Compose, and recreates only when enabled. Esc cancels.",
                    Style::default().fg(MUTED),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines)
                    .block(instrument_block("CHANGE PUBLISHED PORT", SEA_GLASS))
                    .wrap(Wrap { trim: true }),
                popup,
            );
        }
        Modal::TextInput {
            title,
            detail,
            prompt,
            input,
            ..
        } => {
            let lines = vec![
                Line::styled(detail.clone(), Style::default().fg(MUTED)),
                Line::raw(""),
                Line::styled(prompt.clone(), Style::default().fg(SEA_GLASS).bold()),
                Line::styled(format!("{input}_"), Style::default().fg(TEXT).bold()),
                Line::raw(""),
                Line::styled(
                    "Enter continues · Esc cancels · bracketed paste is supported",
                    Style::default().fg(MUTED),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines)
                    .block(instrument_block(title, WARN))
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
    }
}

fn draw_help_modal(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let lines = vec![
        Line::styled("Navigation", Style::default().fg(SEA_GLASS).bold()),
        help_line("↑ / ↓  j / k", "move immediately from cached state"),
        help_line("PgUp / PgDn", "move one viewport"),
        help_line("Home / End  g / G", "first or last service"),
        help_line("/", "filter by service, role, or state"),
        help_line("1 · 2 · 3 · 4 · 5 / Tab", "switch watch surfaces"),
        Line::raw(""),
        Line::styled("Service operations", Style::default().fg(SEA_GLASS).bold()),
        help_line("e / d / R", "enable, disable, or restart with confirmation"),
        help_line("p", "validated port editor with automatic backup"),
        help_line("l / L", "scan selected service or the fleet"),
        help_line("Enter", "open compact detail on narrow terminals"),
        Line::raw(""),
        Line::styled("Recovery", Style::default().fg(SEA_GLASS).bold()),
        help_line(
            "c / Enter / u",
            "create, inspect retained, or inspect file/URL",
        ),
        help_line(
            "R / p / d",
            "restore with phrase, prune retention, or delete",
        ),
        Line::raw(""),
        Line::styled("Safety", Style::default().fg(SEA_GLASS).bold()),
        Line::styled(
            "Actions run off the render thread. Output stays in the command channel; failures never tear down the terminal.",
            Style::default().fg(MUTED),
        ),
        Line::raw(""),
        Line::styled(
            "Press ? / Enter / Esc to close",
            Style::default().fg(TEXT).bold(),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(instrument_block("KEYBOARD WATCH CARD", INFO))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn instrument_block(title: impl AsRef<str>, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(STEEL))
        .title(Span::styled(
            format!(" {} ", title.as_ref()),
            Style::default().fg(color).bold(),
        ))
        .style(Style::default().bg(PANEL))
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(height) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(width) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1])[1]
}

fn status_badge(row: &ServiceRow) -> (&'static str, Color) {
    let status = row.status.to_ascii_lowercase();
    if row.disabled {
        ("[OFF]", MUTED)
    } else if status.contains("unhealthy")
        || status.contains("exited")
        || status.contains("restarting")
    {
        ("[FAULT]", BAD)
    } else if status.contains("starting") || (!row.dependencies_ready && !row.depends_on.is_empty())
    {
        ("[WAIT]", WARN)
    } else if row.running && status.contains("healthy") {
        ("[READY]", GOOD)
    } else if row.running {
        ("[UP]", GOOD)
    } else if status.contains("not created") {
        ("[NEW]", MUTED)
    } else {
        ("[DOWN]", WARN)
    }
}

fn concise_status(status: &str) -> &str {
    if status.contains("healthy") {
        "ready"
    } else if status.contains("running") {
        "up"
    } else if status.contains("disabled") {
        "off"
    } else if status.contains("not created") {
        "new"
    } else if status.contains("exited") {
        "fault"
    } else {
        status
    }
}

fn field_line(label: &str, value: impl AsRef<str>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(STEEL).bold()),
        Span::styled(
            if value.as_ref().is_empty() {
                "-".to_string()
            } else {
                value.as_ref().to_string()
            },
            Style::default().fg(TEXT),
        ),
    ])
}

fn help_line(key: &str, description: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<20}"), Style::default().fg(TEXT).bold()),
        Span::styled(description.to_string(), Style::default().fg(MUTED)),
    ])
}

fn metric(label: &str, value: impl AsRef<str>, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} {} ", value.as_ref()),
        Style::default().fg(color),
    )
}

fn separator() -> Span<'static> {
    Span::styled("│", Style::default().fg(STEEL))
}

fn level_color(percent: f64) -> Color {
    if percent >= 90.0 {
        BAD
    } else if percent >= 75.0 {
        WARN
    } else {
        GOOD
    }
}

fn percentage(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    used.zip(total)
        .and_then(|(used, total)| (total > 0).then(|| used as f64 / total as f64 * 100.0))
}

fn format_optional(value: Option<f64>, decimals: usize) -> String {
    value
        .map(|value| format!("{value:.decimals$}"))
        .unwrap_or_else(|| "-".to_string())
}

fn format_uptime(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "-".to_string();
    };
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    format!("{days}d {hours:02}h {minutes:02}m")
}

fn format_rfc3339(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|value| {
            value
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

fn human_seconds(seconds: u64) -> String {
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

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn display_or(values: &[String], fallback: &str) -> String {
    if values.is_empty() {
        fallback.to_string()
    } else {
        values.join(", ")
    }
}

fn spinner(frame: usize) -> &'static str {
    ["|", "/", "-", "\\"][(frame / 2) % 4]
}

fn demo_services() -> Vec<ServiceRow> {
    let rows = [
        (
            "audiobookshelf",
            "Audiobook and podcast server",
            "running",
            true,
            false,
            0,
        ),
        ("bazarr", "Subtitle manager", "running", true, false, 0),
        (
            "flaresolverr",
            "Cloudflare challenge proxy",
            "running",
            true,
            false,
            0,
        ),
        (
            "homarr",
            "Media-server dashboard",
            "running",
            true,
            false,
            0,
        ),
        (
            "jellyfin",
            "Media server and streaming",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "jellyseerr",
            "Jellyfin media request manager",
            "disabled",
            false,
            true,
            0,
        ),
        ("lidarr", "Music library manager", "running", true, false, 0),
        (
            "prowlarr",
            "Indexer manager and proxy",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "radarr",
            "Movie library manager",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "sabnzbd",
            "Usenet downloader",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "seerr",
            "Media request and discovery manager",
            "disabled",
            false,
            true,
            0,
        ),
        (
            "sonarr",
            "TV series library manager",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "spotweb",
            "Spotweb Usenet indexer",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "tracearr",
            "Playback monitoring and analytics",
            "running (healthy)",
            true,
            false,
            0,
        ),
        (
            "watcharr",
            "Watch tracking and lists",
            "running",
            true,
            false,
            0,
        ),
        (
            "wizarr",
            "Jellyfin invite and onboarding",
            "running (healthy)",
            true,
            false,
            0,
        ),
    ];
    rows.into_iter()
        .enumerate()
        .map(
            |(index, (name, purpose, status, running, disabled, issues))| ServiceRow {
                service: name.to_string(),
                purpose: purpose.to_string(),
                image: format!("ghcr.io/example/{name}:latest"),
                status: status.to_string(),
                running,
                disabled,
                configured: true,
                container: (!disabled).then(|| name.to_string()),
                ports: if index % 3 == 0 {
                    vec![(8000 + index).to_string()]
                } else {
                    Vec::new()
                },
                urls: if index % 3 == 0 {
                    vec![format!("http://192.168.1.50:{}", 8000 + index)]
                } else {
                    Vec::new()
                },
                mounts: 2,
                depends_on: if name == "tracearr" {
                    vec![
                        "jellyfin".to_string(),
                        "tracearr-redis".to_string(),
                        "tracearr-timescale".to_string(),
                    ]
                } else {
                    Vec::new()
                },
                dependencies_ready: true,
                healthcheck_configured: status.contains("healthy"),
                issue_count: issues,
                ..ServiceRow::default()
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_snapshots_fit_all_breakpoints() {
        for (width, height) in [(160, 50), (100, 40), (80, 28)] {
            let output = demo_snapshot(width, height).unwrap();
            assert!(output.contains("ENGINEERING WATCH"));
            assert!(output.contains("DEMO DATA"));
            assert!(output.contains("SERVICE WATCH"));
            assert!(!output.contains("TERMINAL TOO SMALL"));
            assert!(output.lines().count() <= height as usize);
        }
    }

    #[test]
    fn status_badges_are_textual_and_color_independent() {
        let mut row = ServiceRow {
            running: true,
            status: "running (healthy)".into(),
            ..ServiceRow::default()
        };
        assert_eq!(status_badge(&row).0, "[READY]");
        row.disabled = true;
        assert_eq!(status_badge(&row).0, "[OFF]");
    }

    #[test]
    fn filtering_preserves_fast_cached_navigation() {
        let mut app = App::demo();
        app.filter = "arr".to_string();
        let visible = app.filtered_indices();
        assert!(visible.len() >= 5);
        app.select_first();
        assert!(app.selected_row().unwrap().service.contains("arr"));
        app.move_selection(1);
        assert!(app.selected_row().unwrap().service.contains("arr"));
    }

    #[test]
    fn recovery_watch_surfaces_schedule_and_restore_guard() {
        let mut app = App::demo();
        app.tab = Tab::Recovery;
        app.backup_health = Some(BackupHealth {
            directory: "/nfs/jellyfin/.docker-services-backups".into(),
            interval_seconds: 86_400,
            retention: 14,
            last_success: None,
            last_attempt_at: Some("2026-07-31T20:15:00Z".into()),
            next_due_at: None,
            overdue: true,
            last_error: None,
            recovery: None,
        });
        let output = render_snapshot(app, 120, 40).unwrap();
        assert!(output.contains("RECURRING BACKUP HEALTH"));
        assert!(output.contains("[OVERDUE]"));
        assert!(output.contains("Last attempt"));
        assert!(output.contains("INSPECTION / RESTORE"));
        assert!(output.contains("u file/URL"));
    }
}
