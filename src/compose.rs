use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use regex::Regex;
use serde_json::{Map, Value};

use crate::command::{CommandSpec, run, run_json, run_streaming};
use crate::diagnostics::scan_all;
use crate::integrations::{
    build_scraparr_config, integration_plan, reconcile_integrations, reconcile_scraparr,
};
use crate::model::{
    CpuTimes, DependencySummary, HostStats, IntegrationSummary, LogScanSummary, PendingIntegration,
    PortSpec, RuntimeData, ServiceAction, ServiceRow, Snapshot,
};

const DISABLED_STATE_FILENAME: &str = ".docker-services-disabled.json";

const PURPOSES: &[(&str, &str)] = &[
    ("audiobookshelf", "Audiobook and podcast server"),
    ("autopulse", "Media automation and monitoring"),
    ("autopulse-postgres", "Autopulse database dependency"),
    ("bazarr", "Subtitle manager"),
    ("decluttarr", "Cleans stalled and failed downloads"),
    ("flaresolverr", "Cloudflare challenge proxy"),
    ("floppy", "Self-hosted media tracking"),
    ("floppy-redis", "Floppy cache dependency"),
    ("homarr", "Media-server dashboard"),
    ("jellyfin", "Media server and streaming"),
    ("jellyseerr", "Jellyfin media request manager"),
    ("kapowarr", "Comic library manager"),
    ("lidarr", "Music library manager"),
    ("maintainerr", "Media library maintenance and cleanup"),
    (
        "posterizarr",
        "Automated media poster and artwork generator",
    ),
    (
        "prefetcharr",
        "On-demand episode prefetcher for active playback",
    ),
    ("prowlarr", "Indexer manager and proxy"),
    ("pulsarr", "Plex watchlist and request manager"),
    ("radarr", "Movie library manager"),
    ("sabnzbd", "Usenet downloader"),
    ("scraparr", "Prometheus metrics exporter for media services"),
    ("seerr", "Media request and discovery manager"),
    ("sonarr", "TV series library manager"),
    ("spotweb", "Spotweb Usenet indexer and Newznab API"),
    ("spotweb-mysql", "Spotweb database dependency"),
    ("tdarr", "Media transcoding and processing"),
    ("tdarr-node", "Tdarr transcoding worker"),
    (
        "tracearr",
        "Real-time media playback monitoring and analytics",
    ),
    ("tracearr-redis", "Tracearr cache and job queue dependency"),
    (
        "tracearr-timescale",
        "Tracearr time-series database dependency",
    ),
    ("trailarr", "Trailer downloader and manager"),
    ("unpackarr", "Download extraction helper"),
    ("watcharr", "Watch tracking and lists"),
    ("wizarr", "Jellyfin invite and onboarding"),
];

const EXCLUSIVE_DEPENDENCIES: &[(&str, &[&str])] = &[
    ("autopulse", &["autopulse-postgres"]),
    ("floppy", &["floppy-redis"]),
    ("spotweb", &["spotweb-mysql"]),
    ("tracearr", &["tracearr-redis", "tracearr-timescale"]),
];

#[derive(Clone, Debug)]
pub struct Manager {
    compose_file: PathBuf,
    host: String,
    project_name: Option<String>,
}

impl Manager {
    pub fn new(compose_file: PathBuf, host: String) -> Self {
        Self {
            compose_file,
            host,
            project_name: None,
        }
    }

    pub fn with_project_name(mut self, project_name: Option<String>) -> Self {
        self.project_name = project_name;
        self
    }

    pub fn compose_file(&self) -> &Path {
        &self.compose_file
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn compose_command<I, S>(&self, args: I) -> CommandSpec
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        let mut command = CommandSpec::new("docker").arg("compose");
        let env_file = self.env_file();
        if env_file.is_file() {
            command = command.args(["--env-file".into(), env_file.into_os_string()]);
        }
        if let Some(project_name) = &self.project_name {
            command = command.arg("--project-name").arg(project_name.as_str());
        }
        command
            .args(["--profile", "optional", "-f"])
            .arg(self.compose_file.as_os_str())
            .args(args)
    }

    pub fn compose_config(&self) -> Result<Value> {
        let value = run_json(&self.compose_command(["config", "--format", "json"]))?;
        if !value.is_object() {
            bail!("Docker Compose returned an invalid configuration");
        }
        Ok(value)
    }

    pub fn volume_mountpoint(&self, volume_name: &str) -> Result<Option<PathBuf>> {
        let output = run(
            &CommandSpec::new("docker").args([
                "volume",
                "inspect",
                "--format",
                "{{json .Mountpoint}}",
                volume_name,
            ]),
            false,
        )?;
        if !output.status.success() {
            let detail = output.combined();
            if detail.contains("No such volume") || detail.contains("no such volume") {
                return Ok(None);
            }
            bail!("could not inspect Docker volume {volume_name:?}: {detail}");
        }
        let mountpoint = serde_json::from_str::<String>(output.stdout.trim())
            .context("Docker returned an invalid volume mountpoint")?;
        let path = PathBuf::from(mountpoint);
        if !path.is_absolute() || path.file_name().and_then(|name| name.to_str()) != Some("_data") {
            bail!(
                "Docker volume {volume_name:?} returned an unsafe mountpoint {}",
                path.display()
            );
        }
        Ok(Some(path))
    }

    pub fn ensure_volume_mountpoint(&self, volume_name: &str) -> Result<PathBuf> {
        if let Some(path) = self.volume_mountpoint(volume_name)? {
            return Ok(path);
        }
        run(
            &CommandSpec::new("docker").args(["volume", "create", volume_name]),
            true,
        )?;
        self.volume_mountpoint(volume_name)?
            .ok_or_else(|| anyhow!("Docker did not create volume {volume_name:?}"))
    }

    pub fn inspect_containers(&self) -> Result<Vec<Value>> {
        let ids = run(&CommandSpec::new("docker").args(["ps", "-aq"]), true)?
            .stdout
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let value = run_json(&CommandSpec::new("docker").arg("inspect").args(ids))?;
        Ok(value.as_array().cloned().unwrap_or_default())
    }

    pub fn runtime(&self, previous_cpu: Option<CpuTimes>) -> Result<RuntimeData> {
        let config = self.compose_config()?;
        let containers = self.inspect_containers()?;
        let disabled = self.read_disabled_services();
        let (stats, cpu_times) = host_stats(&self.host, previous_cpu);
        let services = all_rows(&config, &containers, &self.host, &disabled);
        let project = config
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(RuntimeData {
            config,
            containers,
            disabled,
            project,
            stats,
            cpu_times,
            services,
        })
    }

    pub fn snapshot(
        &self,
        include_diagnostics: bool,
        include_integrations: bool,
        previous_cpu: Option<CpuTimes>,
    ) -> Result<Snapshot> {
        Ok(self
            .snapshot_with_cpu(include_diagnostics, include_integrations, previous_cpu)?
            .0)
    }

    pub fn snapshot_with_cpu(
        &self,
        include_diagnostics: bool,
        include_integrations: bool,
        previous_cpu: Option<CpuTimes>,
    ) -> Result<(Snapshot, Option<CpuTimes>)> {
        let runtime = self.runtime(previous_cpu)?;
        let next_cpu = runtime.cpu_times;
        let snapshot =
            self.snapshot_from_runtime(runtime, include_diagnostics, include_integrations)?;
        Ok((snapshot, next_cpu))
    }

    fn snapshot_from_runtime(
        &self,
        mut runtime: RuntimeData,
        include_diagnostics: bool,
        include_integrations: bool,
    ) -> Result<Snapshot> {
        let diagnoses = if include_diagnostics {
            scan_all(&runtime.services, &runtime.containers)?
        } else {
            BTreeMap::new()
        };
        for row in &mut runtime.services {
            if let Some(diagnosis) = diagnoses.get(&row.service) {
                row.issue_count = diagnosis.issues.len();
                row.issues = diagnosis.issues.clone();
            }
        }

        let (plan, connectors) = if include_integrations {
            (
                integration_plan(self, &runtime)?,
                build_scraparr_config(self, &runtime)?.1,
            )
        } else {
            (Default::default(), Vec::new())
        };
        let dependency_graph = dependency_graph(&runtime.config)
            .into_iter()
            .map(|(name, depends_on)| {
                let healthcheck =
                    service_config(&runtime.config, &name).and_then(healthcheck_summary);
                (
                    name,
                    DependencySummary {
                        depends_on,
                        healthcheck,
                    },
                )
            })
            .collect();
        Ok(Snapshot {
            host: self.host.clone(),
            compose_file: self.compose_file.display().to_string(),
            project: runtime.project,
            host_stats: runtime.stats,
            log_scan: LogScanSummary {
                scanned_services: diagnoses.len(),
                issue_count: diagnoses.values().map(|item| item.issues.len()).sum(),
            },
            integrations: IntegrationSummary {
                pending: plan.changes.iter().map(PendingIntegration::from).collect(),
                blocked: plan.blocked,
                scraparr_connectors: connectors,
            },
            dependency_graph,
            services: runtime.services,
        })
    }

    pub fn action_scope(&self, service: &str, action: ServiceAction) -> Result<Vec<String>> {
        validate_service_name(service)?;
        let config = self.compose_config()?;
        ensure_service_exists(&config, service)?;
        match action {
            ServiceAction::Enable => dependency_closure(&config, service),
            ServiceAction::Disable => {
                let mut scope = dependent_closure(&config, service)?;
                for dependency in exclusive_dependencies(service) {
                    if service_config(&config, dependency).is_some()
                        && !scope.iter().any(|item| item == dependency)
                    {
                        scope.push((*dependency).to_string());
                    }
                }
                Ok(scope)
            }
            ServiceAction::Restart => Ok(vec![service.to_string()]),
        }
    }

    pub fn run_service_action<F>(
        &self,
        service: &str,
        action: ServiceAction,
        mut progress: F,
    ) -> Result<String>
    where
        F: FnMut(String),
    {
        validate_service_name(service)?;
        let mut output = Vec::new();
        if action == ServiceAction::Enable && service.starts_with("tracearr") {
            let generated = self.ensure_tracearr_secrets()?;
            if !generated.is_empty() {
                output.push(format!(
                    "Generated protected Tracearr settings: {}",
                    generated.join(", ")
                ));
            }
        }
        let config = self.compose_config()?;
        ensure_service_exists(&config, service)?;
        let previous_disabled = self.read_disabled_services();

        let command_output = match action {
            ServiceAction::Enable => {
                let scope = dependency_closure(&config, service)?;
                progress(format!("Enable scope: {}", scope.join(" → ")));
                self.set_services_disabled(scope.iter(), false)?;
                if scope.iter().any(|item| item == "scraparr") {
                    let runtime = self.runtime(None)?;
                    for line in reconcile_scraparr(self, &runtime)? {
                        progress(line);
                    }
                }
                let spec = self.compose_command(
                    std::iter::once("up".to_string())
                        .chain(std::iter::once("-d".to_string()))
                        .chain(scope.iter().cloned()),
                );
                let result = match run_streaming(&spec, &mut progress) {
                    Ok(result) => result,
                    Err(error) => {
                        self.write_disabled_services(&previous_disabled)?;
                        return Err(error.context("enable failed; disabled state restored"));
                    }
                };
                for item in &scope {
                    let _ = run(
                        &CommandSpec::new("docker").args([
                            "update",
                            "--restart",
                            "unless-stopped",
                            item,
                        ]),
                        false,
                    );
                }
                self.wait_for_services_ready(&config, &scope, &mut progress)?;
                output.push(format!("Enabled dependency closure: {}", scope.join(", ")));
                result
            }
            ServiceAction::Disable => {
                let mut scope = dependent_closure(&config, service)?;
                for dependency in exclusive_dependencies(service) {
                    if service_config(&config, dependency).is_some()
                        && !scope.iter().any(|item| item == dependency)
                    {
                        scope.push((*dependency).to_string());
                    }
                }
                progress(format!("Disable scope: {}", scope.join(" → ")));
                let spec = self.compose_command(
                    std::iter::once("stop".to_string()).chain(scope.iter().cloned()),
                );
                let result = run_streaming(&spec, &mut progress)?;
                for item in &scope {
                    let _ = run(
                        &CommandSpec::new("docker").args(["update", "--restart", "no", item]),
                        false,
                    );
                }
                self.set_services_disabled(scope.iter(), true)?;
                output.push(format!("Disabled dependent closure: {}", scope.join(", ")));
                result
            }
            ServiceAction::Restart => {
                progress(format!("Restarting {service}"));
                run_streaming(&self.compose_command(["restart", service]), &mut progress)?
            }
        };

        let combined = command_output.combined();
        if !combined.is_empty() {
            output.push(combined);
        }
        if matches!(action, ServiceAction::Enable | ServiceAction::Disable) {
            for line in reconcile_integrations(self, &mut progress)? {
                output.push(line);
            }
        }
        Ok(output.join("\n"))
    }

    pub fn change_port<F>(
        &self,
        service: &str,
        port_index: usize,
        new_port: u16,
        mut progress: F,
    ) -> Result<String>
    where
        F: FnMut(String),
    {
        validate_service_name(service)?;
        let runtime = self.runtime(None)?;
        let service_value = service_config(&runtime.config, service)
            .ok_or_else(|| anyhow!("service {service:?} is not present in Compose"))?;
        if string_field(service_value, "network_mode") == Some("host") {
            bail!("host-networked services do not use Docker port mappings");
        }
        for row in &runtime.services {
            if row.service != service && row.ports.iter().any(|port| port == &new_port.to_string())
            {
                bail!("host port {new_port} is already used by {}", row.service);
            }
        }

        let backup = self.backup_compose()?;
        progress(format!("Backup created: {}", backup.display()));
        let (target, published) =
            replace_port_in_compose(&self.compose_file, service, port_index, new_port)?;

        let operation = (|| -> Result<()> {
            run(&self.compose_command(["config", "--quiet"]), true)?;
            if !runtime.disabled.contains(service) {
                run_streaming(
                    &self.compose_command(["up", "-d", "--force-recreate", service]),
                    &mut progress,
                )?;
            } else {
                progress(format!(
                    "{service} is disabled; definition updated without starting it"
                ));
            }
            Ok(())
        })();
        if let Err(error) = operation {
            fs::copy(&backup, &self.compose_file).with_context(|| {
                format!(
                    "port update failed and {} could not be restored",
                    self.compose_file.display()
                )
            })?;
            return Err(error.context("port update failed; Compose file restored"));
        }

        let mut details = vec![format!(
            "Changed {service} host port to {published} (container port {target}); backup: {}",
            backup.display()
        )];
        details.extend(reconcile_integrations(self, &mut progress)?);
        Ok(details.join("\n"))
    }

    pub fn read_disabled_services(&self) -> BTreeSet<String> {
        let Ok(text) = fs::read_to_string(self.disabled_state_path()) else {
            return BTreeSet::new();
        };
        serde_json::from_str::<Vec<String>>(&text)
            .unwrap_or_default()
            .into_iter()
            .filter(|item| valid_service_name(item))
            .collect()
    }

    pub fn write_disabled_services(&self, services: &BTreeSet<String>) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&services.iter().collect::<Vec<_>>())?;
        atomic_write(
            &self.disabled_state_path(),
            &[bytes.as_slice(), b"\n"].concat(),
            0o600,
        )
    }

    pub fn set_services_disabled<'a, I>(&self, services: I, disabled: bool) -> Result<()>
    where
        I: IntoIterator<Item = &'a String>,
    {
        let mut current = self.read_disabled_services();
        for service in services {
            if disabled {
                current.insert(service.clone());
            } else {
                current.remove(service);
            }
        }
        self.write_disabled_services(&current)
    }

    pub fn disabled_state_path(&self) -> PathBuf {
        self.compose_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(DISABLED_STATE_FILENAME)
    }

    pub fn env_file(&self) -> PathBuf {
        self.compose_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".env")
    }

    pub fn validate_compose(&self) -> Result<()> {
        run(&self.compose_command(["config", "--quiet"]), true)
            .context("Docker Compose validation failed")?;
        Ok(())
    }

    pub fn stop_stack<F>(&self, mut progress: F) -> Result<()>
    where
        F: FnMut(String),
    {
        let config = self.compose_config()?;
        progress("Stopping configured services".to_string());
        run_streaming(&self.compose_command(["stop"]), &mut progress)?;
        if let Some(items) = services(&config) {
            for name in items.keys() {
                let _ = run(
                    &CommandSpec::new("docker").args(["update", "--restart", "no", name]),
                    false,
                );
            }
        }
        Ok(())
    }

    pub fn start_enabled_services<F>(&self, mut progress: F) -> Result<()>
    where
        F: FnMut(String),
    {
        let config = self.compose_config()?;
        let disabled = self.read_disabled_services();
        let enabled = services(&config)
            .map(|items| {
                items
                    .keys()
                    .filter(|name| !disabled.contains(*name))
                    .cloned()
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        for name in &enabled {
            for dependency in dependency_closure(&config, name)? {
                if disabled.contains(&dependency) {
                    bail!(
                        "enabled service {name} depends on disabled service {dependency}; restore intent is inconsistent"
                    );
                }
            }
        }
        let ordered = dependency_order(&config, &enabled)?;
        if ordered.is_empty() {
            progress("No services are enabled in the restored configuration".to_string());
            return Ok(());
        }
        progress(format!(
            "Starting {} enabled service(s) in dependency order",
            ordered.len()
        ));
        let command = self.compose_command(
            std::iter::once("up".to_string())
                .chain(std::iter::once("-d".to_string()))
                .chain(ordered.iter().cloned()),
        );
        run_streaming(&command, &mut progress)?;
        for name in &ordered {
            let _ = run(
                &CommandSpec::new("docker").args(["update", "--restart", "unless-stopped", name]),
                false,
            );
        }
        for name in &disabled {
            let _ = run(
                &CommandSpec::new("docker").args(["update", "--restart", "no", name]),
                false,
            );
        }
        self.wait_for_services_ready(&config, &ordered, &mut progress)?;
        Ok(())
    }

    fn backup_compose(&self) -> Result<PathBuf> {
        let backup = PathBuf::from(format!(
            "{}.bak.{}",
            self.compose_file.display(),
            Local::now().format("%Y%m%d-%H%M%S")
        ));
        fs::copy(&self.compose_file, &backup)
            .with_context(|| format!("could not back up {}", self.compose_file.display()))?;
        Ok(backup)
    }

    fn ensure_tracearr_secrets(&self) -> Result<Vec<String>> {
        let path = self.env_file();
        let current = match fs::read_to_string(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        let assignment = Regex::new(r"(?m)^([A-Za-z_][A-Za-z0-9_]*)=")?;
        let existing = assignment
            .captures_iter(&current)
            .filter_map(|captures| captures.get(1).map(|item| item.as_str().to_string()))
            .collect::<BTreeSet<_>>();
        let keys = [
            "TRACEARR_DB_PASSWORD",
            "TRACEARR_JWT_SECRET",
            "TRACEARR_COOKIE_SECRET",
            "TRACEARR_BETTER_AUTH_SECRET",
        ];
        let generated = keys
            .into_iter()
            .filter(|key| !existing.contains(*key))
            .map(str::to_string)
            .collect::<Vec<_>>();
        if generated.is_empty() {
            if path.exists() {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            }
            return Ok(generated);
        }
        let mut updated = current;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        for key in &generated {
            updated.push_str(key);
            updated.push('=');
            updated.push_str(&secure_hex(32)?);
            updated.push('\n');
        }
        atomic_write(&path, updated.as_bytes(), 0o600)?;
        Ok(generated)
    }

    fn wait_for_services_ready<F>(
        &self,
        config: &Value,
        services: &[String],
        progress: &mut F,
    ) -> Result<bool>
    where
        F: FnMut(String),
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        progress("Waiting for declared health checks…".to_string());
        loop {
            let containers = self.inspect_containers()?;
            let mut pending = Vec::new();
            for name in services {
                let Some(service) = service_config(config, name) else {
                    continue;
                };
                let container = find_container(name, service, &containers);
                let condition = if service.get("healthcheck").is_some() {
                    "service_healthy"
                } else {
                    "service_started"
                };
                if !container_ready(container, condition) {
                    pending.push(name.clone());
                }
            }
            if pending.is_empty() {
                progress("All services reached their declared ready state".to_string());
                return Ok(true);
            }
            if Instant::now() >= deadline {
                progress(format!(
                    "Readiness wait ended after 30s; still settling: {}",
                    pending.join(", ")
                ));
                return Ok(false);
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}

pub fn detect_host() -> String {
    if let Ok(output) = run(&CommandSpec::new("hostname").arg("-I"), false) {
        if let Some(candidate) = output.stdout.split_whitespace().find(|value| {
            value
                .parse::<IpAddr>()
                .map(|address| !address.is_loopback() && address.is_ipv4())
                .unwrap_or(false)
        }) {
            return candidate.to_string();
        }
    }
    Ipv4Addr::LOCALHOST.to_string()
}

pub fn host_stats(host: &str, previous_cpu: Option<CpuTimes>) -> (HostStats, Option<CpuTimes>) {
    let current_cpu = read_cpu_times();
    let cpu_percent = previous_cpu
        .zip(current_cpu)
        .and_then(|(previous, current)| {
            let total = current.total.saturating_sub(previous.total);
            let idle = current.idle.saturating_sub(previous.idle);
            (total > 0).then(|| ((1.0 - idle as f64 / total as f64) * 100.0).clamp(0.0, 100.0))
        });
    let (memory_total, memory_available) = read_memory();
    let (disk_total, disk_free) = read_disk();
    let load_1m = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse().ok());
    let uptime_seconds = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .map(|value| value.round() as u64);
    (
        HostStats {
            ip: host.to_string(),
            cpu_cores: std::thread::available_parallelism().ok().map(usize::from),
            cpu_percent: cpu_percent.map(|value| (value * 10.0).round() / 10.0),
            load_1m,
            memory_total,
            memory_available,
            memory_used: memory_total
                .zip(memory_available)
                .map(|(total, available)| total.saturating_sub(available)),
            disk_total,
            disk_free,
            uptime_seconds,
        },
        current_cpu,
    )
}

fn read_cpu_times() -> Option<CpuTimes> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let mut fields = text.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values = fields
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    let idle = values.get(3).copied().unwrap_or(0) + values.get(4).copied().unwrap_or(0);
    Some(CpuTimes {
        total: values.iter().sum(),
        idle,
    })
}

fn read_memory() -> (Option<u64>, Option<u64>) {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("MemTotal:") => {
                total = fields
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|value| value * 1024)
            }
            Some("MemAvailable:") => {
                available = fields
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|value| value * 1024)
            }
            _ => {}
        }
    }
    (total, available)
}

fn read_disk() -> (Option<u64>, Option<u64>) {
    let Ok(output) = run(&CommandSpec::new("df").args(["-Pk", "/"]), false) else {
        return (None, None);
    };
    let Some(line) = output.stdout.lines().nth(1) else {
        return (None, None);
    };
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 {
        return (None, None);
    }
    (
        fields[1].parse::<u64>().ok().map(|value| value * 1024),
        fields[3].parse::<u64>().ok().map(|value| value * 1024),
    )
}

pub fn dependency_specs(service: &Value) -> BTreeMap<String, String> {
    match service.get("depends_on") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|name| (name.to_string(), "service_started".to_string()))
            .collect(),
        Some(Value::Object(items)) => items
            .iter()
            .map(|(name, value)| {
                let condition = value
                    .get("condition")
                    .and_then(Value::as_str)
                    .unwrap_or("service_started");
                (name.clone(), condition.to_string())
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

pub fn dependency_graph(config: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
    services(config)
        .map(|items| {
            items
                .iter()
                .map(|(name, service)| (name.clone(), dependency_specs(service)))
                .collect()
        })
        .unwrap_or_default()
}

fn dependency_order(config: &Value, requested: &BTreeSet<String>) -> Result<Vec<String>> {
    let graph = dependency_graph(config);
    let mut ordered = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    fn visit(
        name: &str,
        graph: &BTreeMap<String, BTreeMap<String, String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_string()) {
            bail!("dependency cycle detected at {name}");
        }
        if let Some(dependencies) = graph.get(name) {
            for dependency in dependencies.keys() {
                if graph.contains_key(dependency) {
                    visit(dependency, graph, visiting, visited, ordered)?;
                }
            }
        }
        visiting.remove(name);
        visited.insert(name.to_string());
        ordered.push(name.to_string());
        Ok(())
    }
    for name in requested {
        if graph.contains_key(name) {
            visit(name, &graph, &mut visiting, &mut visited, &mut ordered)?;
        }
    }
    Ok(ordered)
}

fn dependent_order(config: &Value, requested: &BTreeSet<String>) -> Result<Vec<String>> {
    let graph = dependency_graph(config);
    let mut reverse = graph
        .keys()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (service, dependencies) in &graph {
        for dependency in dependencies.keys() {
            reverse
                .entry(dependency.clone())
                .or_default()
                .insert(service.clone());
        }
    }
    let mut ordered = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    fn visit(
        name: &str,
        reverse: &BTreeMap<String, BTreeSet<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_string()) {
            bail!("dependency cycle detected at {name}");
        }
        if let Some(dependents) = reverse.get(name) {
            for dependent in dependents {
                visit(dependent, reverse, visiting, visited, ordered)?;
            }
        }
        visiting.remove(name);
        visited.insert(name.to_string());
        ordered.push(name.to_string());
        Ok(())
    }
    for name in requested {
        if graph.contains_key(name) {
            visit(name, &reverse, &mut visiting, &mut visited, &mut ordered)?;
        }
    }
    Ok(ordered)
}

pub fn dependency_closure(config: &Value, service: &str) -> Result<Vec<String>> {
    dependency_order(config, &BTreeSet::from([service.to_string()]))
}

pub fn dependent_closure(config: &Value, service: &str) -> Result<Vec<String>> {
    dependent_order(config, &BTreeSet::from([service.to_string()]))
}

pub fn compose_port_specs(service: &Value) -> Vec<PortSpec> {
    service
        .get("ports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            Value::Object(spec) => Some(PortSpec {
                published: value_string(spec.get("published"))?,
                target: value_string(spec.get("target"))?,
                protocol: spec
                    .get("protocol")
                    .and_then(Value::as_str)
                    .unwrap_or("tcp")
                    .to_string(),
            }),
            Value::String(value) => parse_short_port(value),
            _ => None,
        })
        .collect()
}

fn parse_short_port(value: &str) -> Option<PortSpec> {
    let without_protocol = value.split('/').next()?;
    let parts = without_protocol.split(':').collect::<Vec<_>>();
    let (published, target) = match parts.as_slice() {
        [published, target] => (*published, *target),
        [_, published, target] => (*published, *target),
        _ => return None,
    };
    Some(PortSpec {
        published: published.to_string(),
        target: target.to_string(),
        protocol: value.split('/').nth(1).unwrap_or("tcp").to_string(),
    })
}

pub fn all_rows(
    config: &Value,
    containers: &[Value],
    host: &str,
    disabled: &BTreeSet<String>,
) -> Vec<ServiceRow> {
    let mut rows = services(config)
        .map(|items| {
            items
                .iter()
                .map(|(name, service)| service_row(name, service, containers, host, disabled))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let row_status = rows
        .iter()
        .map(|row| (row.service.clone(), row.status.clone()))
        .collect::<BTreeMap<_, _>>();
    for row in &mut rows {
        let mut ready = true;
        for (dependency, condition) in &row.dependency_conditions {
            row.dependency_status.insert(
                dependency.clone(),
                row_status
                    .get(dependency)
                    .cloned()
                    .unwrap_or_else(|| "missing".to_string()),
            );
            let container = service_config(config, dependency)
                .and_then(|service| find_container(dependency, service, containers));
            if disabled.contains(dependency) || !container_ready(container, condition) {
                ready = false;
            }
        }
        row.dependencies_ready = ready;
    }

    let managed = rows
        .iter()
        .filter_map(|row| row.container.clone())
        .collect::<BTreeSet<_>>();
    let project = config.get("name").and_then(Value::as_str);
    for container in containers {
        let Some(name) = container_name(container) else {
            continue;
        };
        let labels = container
            .pointer("/Config/Labels")
            .and_then(Value::as_object);
        if managed.contains(&name)
            || labels
                .and_then(|labels| labels.get("com.docker.compose.project"))
                .and_then(Value::as_str)
                == project
        {
            continue;
        }
        let image = container
            .pointer("/Config/Image")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let ports = container_ports(container);
        rows.push(ServiceRow {
            service: name.clone(),
            purpose: service_purpose(&name, image),
            image: image.to_string(),
            status: format!("{} (unmanaged)", container_status(Some(container))),
            running: container.pointer("/State/Status").and_then(Value::as_str) == Some("running"),
            disabled: false,
            configured: false,
            container: Some(name),
            network_mode: container
                .pointer("/HostConfig/NetworkMode")
                .and_then(Value::as_str)
                .map(str::to_string),
            urls: ports
                .iter()
                .map(|port| format!("http://{host}:{port}"))
                .collect(),
            ports,
            port_specs: Vec::new(),
            mounts: container
                .get("Mounts")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
            dependencies_ready: true,
            ..ServiceRow::default()
        });
    }
    rows.sort_by(|left, right| left.service.cmp(&right.service));
    rows
}

pub fn service_row(
    name: &str,
    service: &Value,
    containers: &[Value],
    host: &str,
    disabled: &BTreeSet<String>,
) -> ServiceRow {
    let container = find_container(name, service, containers);
    let port_specs = compose_port_specs(service);
    let mut ports = container.map(container_ports).unwrap_or_default();
    if ports.is_empty() {
        ports = port_specs
            .iter()
            .map(|spec| spec.published.clone())
            .collect();
    }
    let is_disabled = disabled.contains(name);
    let dependencies = dependency_specs(service);
    let healthcheck = healthcheck_summary(service);
    ServiceRow {
        service: name.to_string(),
        purpose: service_purpose(
            name,
            service
                .get("image")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        image: service
            .get("image")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        status: if is_disabled {
            "disabled".to_string()
        } else {
            container_status(container)
        },
        running: !is_disabled
            && container
                .and_then(|value| value.pointer("/State/Status"))
                .and_then(Value::as_str)
                == Some("running"),
        disabled: is_disabled,
        configured: true,
        container: container.and_then(container_name),
        network_mode: container
            .and_then(|value| value.pointer("/HostConfig/NetworkMode"))
            .and_then(Value::as_str)
            .or_else(|| service.get("network_mode").and_then(Value::as_str))
            .map(str::to_string),
        urls: ports
            .iter()
            .map(|port| format!("http://{host}:{port}"))
            .collect(),
        ports,
        port_specs,
        mounts: service
            .get("volumes")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        depends_on: dependencies.keys().cloned().collect(),
        dependency_conditions: dependencies,
        dependency_status: BTreeMap::new(),
        dependencies_ready: true,
        healthcheck_configured: healthcheck.is_some(),
        healthcheck,
        issue_count: 0,
        issues: Vec::new(),
    }
}

pub fn service_config<'a>(config: &'a Value, name: &str) -> Option<&'a Value> {
    services(config)?.get(name)
}

pub fn services(config: &Value) -> Option<&Map<String, Value>> {
    config.get("services")?.as_object()
}

pub fn find_container<'a>(
    name: &str,
    service: &Value,
    containers: &'a [Value],
) -> Option<&'a Value> {
    let expected = service
        .get("container_name")
        .and_then(Value::as_str)
        .unwrap_or(name);
    containers.iter().find(|container| {
        container
            .pointer("/Config/Labels/com.docker.compose.service")
            .and_then(Value::as_str)
            == Some(name)
            || container
                .get("Name")
                .and_then(Value::as_str)
                .map(|value| value.trim_start_matches('/'))
                == Some(expected)
    })
}

pub fn container_ready(container: Option<&Value>, condition: &str) -> bool {
    let Some(container) = container else {
        return false;
    };
    if container.pointer("/State/Status").and_then(Value::as_str) != Some("running") {
        return false;
    }
    condition != "service_healthy"
        || container
            .pointer("/State/Health/Status")
            .and_then(Value::as_str)
            == Some("healthy")
}

pub fn container_status(container: Option<&Value>) -> String {
    let Some(container) = container else {
        return "not created".to_string();
    };
    let status = container
        .pointer("/State/Status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if status == "running" {
        if let Some(health) = container
            .pointer("/State/Health/Status")
            .and_then(Value::as_str)
        {
            return format!("running ({health})");
        }
    }
    if status == "exited" {
        let code = container.pointer("/State/ExitCode").unwrap_or(&Value::Null);
        return format!("exited ({code})");
    }
    status.to_string()
}

pub fn container_name(container: &Value) -> Option<String> {
    container
        .get("Name")
        .and_then(Value::as_str)
        .map(|value| value.trim_start_matches('/').to_string())
}

fn container_ports(container: &Value) -> Vec<String> {
    let mut ports = BTreeSet::new();
    if let Some(bindings) = container
        .pointer("/NetworkSettings/Ports")
        .and_then(Value::as_object)
    {
        for values in bindings.values().filter_map(Value::as_array) {
            for binding in values {
                if let Some(port) = binding.get("HostPort").and_then(Value::as_str) {
                    ports.insert(port.to_string());
                }
            }
        }
    }
    ports.into_iter().collect()
}

pub fn healthcheck_summary(service: &Value) -> Option<Value> {
    let healthcheck = service.get("healthcheck")?.as_object()?;
    let mut summary = Map::new();
    for key in ["test", "interval", "timeout", "retries", "start_period"] {
        if let Some(value) = healthcheck.get(key) {
            summary.insert(key.to_string(), value.clone());
        }
    }
    Some(Value::Object(summary))
}

pub fn service_internal_url(config: &Value, service_name: &str) -> Option<String> {
    let service = service_config(config, service_name)?;
    if service.get("network_mode").and_then(Value::as_str) == Some("host") {
        return (service_name == "jellyfin")
            .then(|| "http://host.docker.internal:8096".to_string());
    }
    compose_port_specs(service)
        .first()
        .map(|spec| format!("http://{service_name}:{}", spec.target))
}

fn service_purpose(name: &str, image: &str) -> String {
    let key = name.to_ascii_lowercase();
    if let Some((_, purpose)) = PURPOSES.iter().find(|(candidate, _)| *candidate == key) {
        return (*purpose).to_string();
    }
    let image_name = image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split(':')
        .next()
        .unwrap_or(image)
        .to_ascii_lowercase();
    PURPOSES
        .iter()
        .find(|(candidate, _)| *candidate == image_name)
        .map(|(_, purpose)| (*purpose).to_string())
        .unwrap_or_else(|| "Docker container".to_string())
}

fn ensure_service_exists(config: &Value, service: &str) -> Result<()> {
    if service_config(config, service).is_none() {
        bail!("service {service:?} is not present in Compose");
    }
    Ok(())
}

fn valid_service_name(service: &str) -> bool {
    !service.is_empty()
        && service
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_.-".contains(character))
}

fn validate_service_name(service: &str) -> Result<()> {
    if !valid_service_name(service) {
        bail!("invalid service name {service:?}");
    }
    Ok(())
}

fn exclusive_dependencies(service: &str) -> &'static [&'static str] {
    EXCLUSIVE_DEPENDENCIES
        .iter()
        .find(|(candidate, _)| *candidate == service)
        .map(|(_, dependencies)| *dependencies)
        .unwrap_or(&[])
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field)?.as_str()
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn secure_hex(bytes: usize) -> Result<String> {
    let mut random = vec![0_u8; bytes];
    File::open("/dev/urandom")
        .context("secure random source /dev/urandom is unavailable")?
        .read_exact(&mut random)?;
    Ok(random.iter().map(|value| format!("{value:02x}")).collect())
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid destination path {}", path.display()))?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let operation = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("could not create {}", temporary.display()))?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    })();
    if operation.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    operation
}

fn replace_port_in_compose(
    compose_file: &Path,
    service_name: &str,
    port_index: usize,
    new_host_port: u16,
) -> Result<(String, String)> {
    let text = fs::read_to_string(compose_file)
        .with_context(|| format!("could not read {}", compose_file.display()))?;
    let mut lines = text
        .split_inclusive('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !text.ends_with('\n') && lines.is_empty() {
        lines.push(text);
    }
    let service_pattern = Regex::new(&format!(r"^  {}:\s*$", regex::escape(service_name)))?;
    let next_service_pattern = Regex::new(r"^  [A-Za-z0-9_.-]+:\s*$")?;
    let service_start = lines
        .iter()
        .position(|line| service_pattern.is_match(trim_line(line)))
        .ok_or_else(|| {
            anyhow!(
                "service {service_name:?} is not present in {}",
                compose_file.display()
            )
        })?;
    let service_end = lines
        .iter()
        .enumerate()
        .skip(service_start + 1)
        .find(|(_, line)| next_service_pattern.is_match(trim_line(line)))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    let ports_header = Regex::new(r"^    ports:\s*$")?;
    let next_field = Regex::new(r"^    [A-Za-z0-9_.-]+:")?;
    let port_line = Regex::new(r"^      -\s*")?;
    let mut inside_ports = false;
    let mut port_lines = Vec::new();
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(service_end)
        .skip(service_start + 1)
    {
        let value = trim_line(line);
        if ports_header.is_match(value) {
            inside_ports = true;
            continue;
        }
        if inside_ports && next_field.is_match(value) {
            break;
        }
        if inside_ports && port_line.is_match(value) {
            port_lines.push(index);
        }
    }
    let line_index = *port_lines.get(port_index).ok_or_else(|| {
        anyhow!(
            "service {service_name:?} has no editable port at index {}",
            port_index + 1
        )
    })?;
    let target_pattern = Regex::new(r#":(?:"|')?([0-9]+)(?:"|')?(?:\s*#.*)?\s*$"#)?;
    let target = target_pattern
        .captures(trim_line(&lines[line_index]))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
        .ok_or_else(|| anyhow!("only simple HOST:CONTAINER port mappings can be edited"))?;
    let indent = lines[line_index]
        .chars()
        .take_while(|character| {
            character.is_whitespace() && *character != '\n' && *character != '\r'
        })
        .collect::<String>();
    let newline = if lines[line_index].ends_with("\r\n") {
        "\r\n"
    } else if lines[line_index].ends_with('\n') {
        "\n"
    } else {
        ""
    };
    lines[line_index] = format!("{indent}- {new_host_port}:{target}{newline}");
    atomic_write(compose_file, lines.concat().as_bytes(), 0o600)?;
    Ok((target, new_host_port.to_string()))
}

fn trim_line(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn config() -> Value {
        serde_json::json!({
            "name": "root",
            "services": {
                "db": {"image": "postgres", "healthcheck": {"test": ["CMD", "true"]}},
                "api": {"image": "api", "depends_on": {"db": {"condition": "service_healthy"}}},
                "web": {"image": "web", "depends_on": {"api": {"condition": "service_started"}}}
            }
        })
    }

    #[test]
    fn dependency_and_dependent_orders_are_safe() {
        assert_eq!(
            dependency_closure(&config(), "web").unwrap(),
            ["db", "api", "web"]
        );
        assert_eq!(
            dependent_closure(&config(), "db").unwrap(),
            ["web", "api", "db"]
        );
    }

    #[test]
    fn targeted_compose_command_has_no_unrelated_scale_flags() {
        let directory = tempdir().unwrap();
        let manager = Manager::new(directory.path().join("docker-compose.yml"), "host".into());
        let command = manager.compose_command(["up", "-d", "audiobookshelf"]);
        assert!(!command.args.iter().any(|item| item == "--scale"));
    }

    #[test]
    fn staged_compose_can_preserve_the_original_project_name() {
        let directory = tempdir().unwrap();
        let manager = Manager::new(directory.path().join("docker-compose.yml"), "host".into())
            .with_project_name(Some("root".into()));
        let command = manager.compose_command(["config", "--quiet"]);
        let args = command
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--project-name", "root"])
        );
    }

    #[test]
    fn disabled_state_is_atomic_and_sorted() {
        let directory = tempdir().unwrap();
        let manager = Manager::new(directory.path().join("docker-compose.yml"), "host".into());
        manager
            .write_disabled_services(&BTreeSet::from(["zeta".into(), "alpha".into()]))
            .unwrap();
        assert_eq!(
            manager.read_disabled_services(),
            BTreeSet::from(["alpha".into(), "zeta".into()])
        );
        let mode = fs::metadata(manager.disabled_state_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn port_edit_preserves_container_port() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("docker-compose.yml");
        fs::write(
            &path,
            "services:\n  app:\n    image: app\n    ports:\n      - 8080:80\n    restart: unless-stopped\n  next:\n    image: next\n",
        )
        .unwrap();
        let result = replace_port_in_compose(&path, "app", 0, 9090).unwrap();
        assert_eq!(result, ("80".into(), "9090".into()));
        assert!(fs::read_to_string(path).unwrap().contains("- 9090:80"));
    }
}
