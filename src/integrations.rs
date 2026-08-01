use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::command::{CommandSpec, run};
use crate::compose::{Manager, service_internal_url};
use crate::model::{
    BlockedIntegration, IntegrationChange, IntegrationPlan, RuntimeData, ServiceRow,
};

const SCRAPARR_CONFIG_PATH: &str = "/nfs/scraparr/config.yaml";

pub fn read_arr_api_key(service: &str) -> String {
    let path = format!("/nfs/{service}/config/config.xml");
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    Regex::new(r"(?s)<ApiKey>\s*([^<]+?)\s*</ApiKey>")
        .ok()
        .and_then(|pattern| pattern.captures(&text))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_string())
        .unwrap_or_default()
}

pub fn read_env_value(manager: &Manager, key: &str) -> String {
    let Ok(text) = fs::read_to_string(manager.env_file()) else {
        return String::new();
    };
    let prefix = format!("{key}=");
    text.lines()
        .rev()
        .find_map(|line| {
            line.strip_prefix(&prefix)
                .map(|value| value.trim().trim_matches(['\'', '"']).to_string())
        })
        .unwrap_or_default()
}

fn read_bazarr_api_key() -> String {
    let pattern = Regex::new(r#"(?m)^\s*apikey:\s*['"]?([^'"\s#]+)"#)
        .expect("static Bazarr key pattern must compile");
    [
        "/nfs/bazarr/config/config/config.yaml",
        "/nfs/bazarr/config/config.yaml",
    ]
    .into_iter()
    .find_map(|path| {
        let text = fs::read_to_string(path).ok()?;
        pattern
            .captures(&text)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_string())
    })
    .unwrap_or_default()
}

fn read_sabnzbd_api_key() -> String {
    let Ok(text) = fs::read_to_string("/nfs/sabnzbd/config/sabnzbd.ini") else {
        return String::new();
    };
    Regex::new(r"(?m)^api_key\s*=\s*(.*)$")
        .ok()
        .and_then(|pattern| pattern.captures(&text))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_string())
        .unwrap_or_default()
}

struct SabWhitelist {
    path: PathBuf,
    text: String,
    hosts: Vec<String>,
}

fn read_sabnzbd_host_whitelist() -> Option<SabWhitelist> {
    let path = PathBuf::from("/nfs/sabnzbd/config/sabnzbd.ini");
    let text = fs::read_to_string(&path).ok()?;
    let hosts = Regex::new(r"(?m)^host_whitelist\s*=\s*(.*)$")
        .ok()
        .and_then(|pattern| pattern.captures(&text))
        .and_then(|captures| captures.get(1))
        .map(|value| {
            value
                .as_str()
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(SabWhitelist { path, text, hosts })
}

pub fn build_scraparr_config(
    manager: &Manager,
    runtime: &RuntimeData,
) -> Result<(String, Vec<String>)> {
    let rows = runtime
        .services
        .iter()
        .map(|row| (row.service.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut lines = vec![
        "general:".to_string(),
        "  address: 0.0.0.0".to_string(),
        "  port: 7100".to_string(),
        "  path: /metrics".to_string(),
        "  log_level: INFO".to_string(),
    ];
    let mut configured = Vec::new();
    for service in [
        "sonarr", "radarr", "prowlarr", "bazarr", "lidarr", "jellyfin",
    ] {
        let Some(row) = rows.get(service).copied().filter(|row| row.running) else {
            continue;
        };
        let Some(url) = service_internal_url(&runtime.config, service) else {
            continue;
        };
        let api_key = match service {
            "bazarr" => read_bazarr_api_key(),
            "jellyfin" => {
                let primary = read_env_value(manager, "SCRAPARR_JELLYFIN_API_KEY");
                if primary.is_empty() {
                    read_env_value(manager, "PREFETCHARR_MEDIA_API_KEY")
                } else {
                    primary
                }
            }
            _ => read_arr_api_key(service),
        };
        if api_key.is_empty() {
            continue;
        }
        if service == "lidarr" {
            let Some(base) = service_api_base(row) else {
                continue;
            };
            let response =
                integration_request("GET", &format!("{base}/api/v1/rootfolder"), &api_key, None);
            if response.status != Some(200) || response.data.as_array().is_none_or(Vec::is_empty) {
                continue;
            }
        }
        lines.extend([
            format!("{service}:"),
            format!("  url: {}", serde_json::to_string(&url)?),
            format!("  api_key: {}", serde_json::to_string(&api_key)?),
            format!("  alias: {service}"),
            "  interval: 30".to_string(),
            "  detailed: true".to_string(),
        ]);
        let api_version = match service {
            "sonarr" | "radarr" => Some("v3"),
            "prowlarr" | "lidarr" => Some("v1"),
            _ => None,
        };
        if let Some(version) = api_version {
            lines.push(format!("  api_version: {version}"));
        }
        configured.push(service.to_string());
    }
    Ok((format!("{}\n", lines.join("\n")), configured))
}

pub fn reconcile_scraparr(manager: &Manager, runtime: &RuntimeData) -> Result<Vec<String>> {
    if runtime.config.pointer("/services/scraparr").is_none() {
        return Ok(Vec::new());
    }
    let (text, configured) = build_scraparr_config(manager, runtime)?;
    let path = Path::new(SCRAPARR_CONFIG_PATH);
    let previous = fs::read_to_string(path).unwrap_or_default();
    if previous == text {
        return Ok(vec![format!(
            "Scraparr connectors aligned: {}",
            display_list(&configured)
        )]);
    }
    write_private_atomic(path, text.as_bytes())?;
    let scraparr_running = runtime
        .services
        .iter()
        .any(|row| row.service == "scraparr" && row.running)
        && !runtime.disabled.contains("scraparr");
    let restarted = scraparr_running
        && run(
            &CommandSpec::new("docker").args(["restart", "scraparr"]),
            false,
        )
        .map(|result| result.status.success())
        .unwrap_or(false);
    Ok(vec![format!(
        "Scraparr connectors updated: {}{}",
        display_list(&configured),
        if restarted { "; restarted" } else { "" }
    )])
}

pub fn integration_plan(manager: &Manager, runtime: &RuntimeData) -> Result<IntegrationPlan> {
    let rows = runtime
        .services
        .iter()
        .map(|row| (row.service.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut plan = IntegrationPlan::default();
    let sab_key = read_sabnzbd_api_key();
    let arr_keys = ["sonarr", "radarr"]
        .into_iter()
        .map(|service| (service, read_arr_api_key(service)))
        .collect::<BTreeMap<_, _>>();

    match read_sabnzbd_host_whitelist() {
        Some(whitelist) if rows.get("sabnzbd").is_some_and(|row| row.running) => {
            if !whitelist.hosts.iter().any(|host| host == "sabnzbd") {
                let mut desired_hosts = whitelist.hosts.clone();
                desired_hosts.push("sabnzbd".to_string());
                plan.changes.push(IntegrationChange {
                    target: "SABnzbd Docker hostname".to_string(),
                    service: "sabnzbd".to_string(),
                    kind: "SABnzbd host whitelist".to_string(),
                    action: "update".to_string(),
                    method: "FILE".to_string(),
                    endpoint: whitelist.path.display().to_string(),
                    api_key: String::new(),
                    payload: json!({
                        "setting": "host_whitelist",
                        "value": desired_hosts.join(","),
                        "previous_value": whitelist.hosts.join(","),
                    }),
                    previous_payload: Some(json!({
                        "setting": "host_whitelist",
                        "value": whitelist.hosts.join(","),
                    })),
                    summary: "Allow Docker clients to address SABnzbd as sabnzbd:8080.".to_string(),
                    manual: "Add sabnzbd to host_whitelist in /nfs/sabnzbd/config/sabnzbd.ini, restart SABnzbd, then retry the Arr integration.".to_string(),
                });
            }
            // Keep the text read above alive as evidence that the file was readable.
            let _ = whitelist.text.len();
        }
        None if rows.get("sabnzbd").is_some_and(|row| row.running) => {
            block(
                &mut plan,
                "SABnzbd Docker hostname",
                "SABnzbd configuration is not readable",
            );
        }
        _ => {}
    }

    for service in ["sonarr", "radarr"] {
        let row = rows.get(service).copied();
        let sab_row = rows.get("sabnzbd").copied();
        let arr_key = arr_keys.get(service).cloned().unwrap_or_default();
        if row.is_none_or(|row| !row.running) {
            block(&mut plan, service, "Service is not running");
            continue;
        }
        if arr_key.is_empty() {
            block(
                &mut plan,
                &format!("{service} → SABnzbd"),
                "An existing API key is unavailable",
            );
            continue;
        }
        let row = row.expect("row checked above");
        let Some(base) = service_api_base(row) else {
            block(&mut plan, service, "No published API port is available");
            continue;
        };
        let response = integration_request(
            "GET",
            &format!("{base}/api/v3/downloadclient"),
            &arr_key,
            None,
        );
        let Some(items) = response.data.as_array() else {
            block(
                &mut plan,
                &format!("{service} → SABnzbd"),
                &format!(
                    "Could not read {service} download clients ({})",
                    response.description()
                ),
            );
            continue;
        };
        if response.status != Some(200) {
            block(
                &mut plan,
                &format!("{service} → SABnzbd"),
                &format!(
                    "Could not read {service} download clients ({})",
                    response.description()
                ),
            );
            continue;
        }
        let existing = items
            .iter()
            .find(|item| item.get("implementation").and_then(Value::as_str) == Some("Sabnzbd"))
            .cloned();
        let sab_available = sab_row.is_some_and(|row| row.running) && !sab_key.is_empty();
        let mut desired = Map::from_iter([
            ("host".to_string(), json!("sabnzbd")),
            ("port".to_string(), json!(8080)),
            ("useSsl".to_string(), json!(false)),
        ]);
        if !sab_key.is_empty() {
            desired.insert("apiKey".to_string(), json!(sab_key));
        }
        let (payload, method, endpoint, action) = if let Some(existing) = &existing {
            let current = field_values(existing);
            let differs = desired
                .iter()
                .any(|(name, value)| !field_matches(current.get(name), value, name))
                || existing
                    .get("enable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                    != sab_available;
            if !differs {
                continue;
            }
            let mut payload = update_fields(existing, &desired);
            payload["enable"] = json!(sab_available);
            (
                payload,
                "PUT",
                format!(
                    "{base}/api/v3/downloadclient/{}",
                    existing.get("id").unwrap_or(&Value::Null)
                ),
                if sab_available { "enable" } else { "disable" },
            )
        } else {
            if !sab_available {
                block(
                    &mut plan,
                    &format!("{service} → SABnzbd"),
                    "SABnzbd is not running or its API key is unavailable",
                );
                continue;
            }
            let category = if service == "sonarr" {
                ("tvCategory", "tv")
            } else {
                ("movieCategory", "movies")
            };
            let mut fields = desired
                .iter()
                .map(|(name, value)| json!({"name": name, "value": value}))
                .collect::<Vec<_>>();
            fields.push(json!({"name": category.0, "value": category.1}));
            (
                json!({
                    "name": "SABnzbd",
                    "enable": true,
                    "protocol": "usenet",
                    "priority": 1,
                    "removeCompletedFiles": true,
                    "removeFailedFiles": true,
                    "implementationName": "SABnzbd",
                    "implementation": "Sabnzbd",
                    "configContract": "SabnzbdSettings",
                    "fields": fields,
                    "tags": [],
                }),
                "POST",
                format!("{base}/api/v3/downloadclient"),
                "create",
            )
        };
        plan.changes.push(IntegrationChange {
            target: format!("{service} → SABnzbd"),
            service: service.to_string(),
            kind: "Arr download client".to_string(),
            action: action.to_string(),
            method: method.to_string(),
            endpoint,
            api_key: arr_key,
            payload,
            previous_payload: existing,
            summary: format!(
                "{} SABnzbd in {service} using the Docker service name sabnzbd:8080.",
                title_case(action)
            ),
            manual: format!(
                "Open {} → Settings → Download Clients and add or update SABnzbd with host sabnzbd, port 8080, and the existing SABnzbd API key.",
                title_case(service)
            ),
        });
    }

    plan_prowlarr(&mut plan, &rows, &arr_keys);
    plan_tracearr(&mut plan, &rows);
    let _ = manager;
    Ok(plan)
}

fn plan_prowlarr(
    plan: &mut IntegrationPlan,
    rows: &BTreeMap<&str, &ServiceRow>,
    arr_keys: &BTreeMap<&str, String>,
) {
    let Some(prow_row) = rows.get("prowlarr").copied().filter(|row| row.running) else {
        block(plan, "Prowlarr applications", "Prowlarr is not running");
        return;
    };
    let prowlarr_key = read_arr_api_key("prowlarr");
    if prowlarr_key.is_empty() {
        block(
            plan,
            "Prowlarr applications",
            "Prowlarr API key is unavailable",
        );
        return;
    }
    let Some(base) = service_api_base(prow_row) else {
        block(
            plan,
            "Prowlarr applications",
            "Prowlarr has no published API port",
        );
        return;
    };
    let response = integration_request(
        "GET",
        &format!("{base}/api/v1/applications"),
        &prowlarr_key,
        None,
    );
    if response.status != Some(200) || !response.data.is_array() {
        block(
            plan,
            "Prowlarr applications",
            &format!(
                "Could not read Prowlarr applications ({})",
                response.description()
            ),
        );
        return;
    }
    let applications = response.data.as_array().expect("checked above");
    for (service, implementation, internal_port, categories) in [
        (
            "radarr",
            "Radarr",
            7878,
            vec![
                2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060, 2070, 2080, 2090,
            ],
        ),
        (
            "sonarr",
            "Sonarr",
            8989,
            vec![5000, 5010, 5020, 5030, 5040, 5045, 5050, 5090],
        ),
    ] {
        let arr_key = arr_keys.get(service).cloned().unwrap_or_default();
        let target_running = rows.get(service).is_some_and(|row| row.running);
        if arr_key.is_empty() {
            block(
                plan,
                &format!("Prowlarr → {implementation}"),
                &format!("{implementation} API key is unavailable"),
            );
            continue;
        }
        let existing = applications
            .iter()
            .find(|item| item.get("implementation").and_then(Value::as_str) == Some(implementation))
            .cloned();
        let desired = Map::from_iter([
            ("prowlarrUrl".to_string(), json!("http://prowlarr:9696")),
            (
                "baseUrl".to_string(),
                json!(format!("http://{service}:{internal_port}")),
            ),
            ("apiKey".to_string(), json!(arr_key)),
        ]);
        let (payload, method, endpoint, action) = if let Some(existing) = &existing {
            if !target_running {
                if !existing
                    .get("enable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    continue;
                }
                let mut payload = existing.clone();
                payload["enable"] = json!(false);
                (
                    payload,
                    "PUT",
                    format!(
                        "{base}/api/v1/applications/{}",
                        existing.get("id").unwrap_or(&Value::Null)
                    ),
                    "disable",
                )
            } else {
                let current = field_values(existing);
                let differs = desired
                    .iter()
                    .any(|(name, value)| !field_matches(current.get(name), value, name))
                    || existing.get("syncLevel").and_then(Value::as_str) != Some("fullSync")
                    || !existing
                        .get("enable")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                if !differs {
                    continue;
                }
                let was_enabled = existing
                    .get("enable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let mut payload = update_fields(existing, &desired);
                payload["enable"] = json!(true);
                payload["syncLevel"] = json!("fullSync");
                (
                    payload,
                    "PUT",
                    format!(
                        "{base}/api/v1/applications/{}",
                        existing.get("id").unwrap_or(&Value::Null)
                    ),
                    if was_enabled { "update" } else { "enable" },
                )
            }
        } else {
            if !target_running {
                block(
                    plan,
                    &format!("Prowlarr → {implementation}"),
                    &format!(
                        "{implementation} is disabled and has no existing Prowlarr application"
                    ),
                );
                continue;
            }
            let mut fields = desired
                .iter()
                .map(|(name, value)| json!({"name": name, "value": value}))
                .collect::<Vec<_>>();
            fields.push(json!({"name": "syncCategories", "value": categories}));
            if implementation == "Sonarr" {
                fields.push(json!({"name": "animeSyncCategories", "value": [5070]}));
                fields.push(json!({"name": "syncAnimeStandardFormatSearch", "value": true}));
            }
            (
                json!({
                    "name": implementation,
                    "implementation": implementation,
                    "configContract": format!("{implementation}Settings"),
                    "syncLevel": "fullSync",
                    "enable": true,
                    "fields": fields,
                    "tags": [],
                }),
                "POST",
                format!("{base}/api/v1/applications"),
                "create",
            )
        };
        plan.changes.push(IntegrationChange {
            target: format!("Prowlarr → {implementation}"),
            service: "prowlarr".to_string(),
            kind: "Prowlarr application".to_string(),
            action: action.to_string(),
            method: method.to_string(),
            endpoint,
            api_key: prowlarr_key.clone(),
            payload,
            previous_payload: existing,
            summary: format!(
                "{} Prowlarr sync for {implementation} using Docker DNS and full sync.",
                title_case(action)
            ),
            manual: format!(
                "Open Prowlarr → Settings → Apps and add or update {implementation} with URL http://{service}:{internal_port} and full sync."
            ),
        });
    }
}

fn plan_tracearr(plan: &mut IntegrationPlan, rows: &BTreeMap<&str, &ServiceRow>) {
    let Some(tracearr) = rows.get("tracearr").copied().filter(|row| row.running) else {
        return;
    };
    let Some(jellyfin) = rows.get("jellyfin").copied().filter(|row| row.running) else {
        block(plan, "Tracearr → Jellyfin", "Jellyfin is not running");
        return;
    };
    let _ = jellyfin;
    let Some(base) = service_api_base(tracearr) else {
        block(
            plan,
            "Tracearr → Jellyfin",
            "Tracearr has no published API port",
        );
        return;
    };
    let response = integration_request("GET", &format!("{base}/api/v1/setup/status"), "", None);
    if response.status != Some(200) || !response.data.is_object() {
        block(
            plan,
            "Tracearr → Jellyfin",
            &format!(
                "Could not read Tracearr setup status ({})",
                response.description()
            ),
        );
    } else if response
        .data
        .get("needsSetup")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        block(
            plan,
            "Tracearr → Jellyfin",
            "Create the initial Tracearr owner account, then add Jellyfin in Settings → Servers.",
        );
    } else if !response
        .data
        .get("hasJellyfinServers")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        block(
            plan,
            "Tracearr → Jellyfin",
            "Add Jellyfin in Tracearr Settings → Servers using http://host.docker.internal:8096 and a Jellyfin API key.",
        );
    }
}

pub fn apply_integration_plan<F>(
    manager: &Manager,
    plan: &IntegrationPlan,
    mut progress: F,
) -> Result<Vec<String>>
where
    F: FnMut(String),
{
    if plan.changes.is_empty() {
        return Ok(vec!["No integration changes are pending".to_string()]);
    }
    let backup = write_integration_backup(manager, &plan.changes)?;
    let mut results = vec![format!("Backup: {}", backup.display())];
    progress(results[0].clone());
    for change in &plan.changes {
        progress(format!("Applying {}", change.target));
        match apply_integration_change(change) {
            Ok(message) => {
                progress(message.clone());
                results.push(message);
            }
            Err(error) => {
                let message = format!("ERROR: {error:#}");
                progress(message.clone());
                results.push(message);
            }
        }
    }
    Ok(results)
}

pub fn reconcile_integrations<F>(manager: &Manager, mut progress: F) -> Result<Vec<String>>
where
    F: FnMut(String),
{
    let runtime = manager.runtime(None)?;
    let plan = integration_plan(manager, &runtime)?;
    let mut results = reconcile_scraparr(manager, &runtime)?;
    for line in &results {
        progress(line.clone());
    }
    if plan.changes.is_empty() {
        let suffix = if plan.blocked.is_empty() {
            String::new()
        } else {
            format!("; {} prerequisite(s) blocked", plan.blocked.len())
        };
        results.push(format!("Integrations recalculated: aligned{suffix}"));
        return Ok(results);
    }
    let intro = format!(
        "Integrations recalculated: applying {} change(s)",
        plan.changes.len()
    );
    progress(intro.clone());
    results.push(intro);
    results.extend(apply_integration_plan(manager, &plan, &mut progress)?);
    Ok(results)
}

fn apply_integration_change(change: &IntegrationChange) -> Result<String> {
    if change.method == "FILE" && change.kind == "SABnzbd host whitelist" {
        let path = Path::new(&change.endpoint);
        let text = fs::read_to_string(path)
            .with_context(|| format!("{} could not read {}", change.target, path.display()))?;
        let value = change
            .payload
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{} has no whitelist value", change.target))?;
        let pattern = Regex::new(r"(?m)^host_whitelist\s*=\s*.*$")?;
        let updated = if pattern.is_match(&text) {
            pattern
                .replace(&text, format!("host_whitelist = {value}"))
                .into_owned()
        } else {
            format!(
                "{}{}host_whitelist = {value}\n",
                text,
                if text.ends_with('\n') { "" } else { "\n" }
            )
        };
        write_private_atomic(path, updated.as_bytes())?;
        let restart = run(
            &CommandSpec::new("docker").args(["restart", "sabnzbd"]),
            true,
        )?;
        if !restart.status.success() {
            bail!("{} was written but SABnzbd restart failed", change.target);
        }
        return Ok(format!("{}: updated and SABnzbd restarted", change.target));
    }

    let response = integration_request(
        &change.method,
        &change.endpoint,
        &change.api_key,
        Some(&change.payload),
    );
    let Some(status) = response.status else {
        bail!(
            "{} failed (HTTP unavailable: {})",
            change.target,
            response.description()
        );
    };
    if !(200..300).contains(&status) {
        bail!(
            "{} failed (HTTP {status}: {})",
            change.target,
            response.description()
        );
    }
    if change.method == "PUT" && status == 202 {
        let collection = change
            .endpoint
            .rsplit_once('/')
            .map(|(value, _)| value)
            .unwrap_or(&change.endpoint);
        let mut expected = field_values(&change.payload);
        if let Some(value) = change.payload.get("syncLevel") {
            expected.insert("__syncLevel".to_string(), value.clone());
        }
        if let Some(value) = change.payload.get("enable") {
            expected.insert("__enable".to_string(), value.clone());
        }
        let item_id = change.payload.get("id");
        let mut persisted = false;
        for _ in 0..10 {
            thread::sleep(Duration::from_millis(500));
            let current = integration_request("GET", collection, &change.api_key, None);
            let Some(items) = current
                .data
                .as_array()
                .filter(|_| current.status == Some(200))
            else {
                continue;
            };
            let Some(item) = items.iter().find(|item| item.get("id") == item_id) else {
                continue;
            };
            let actual = field_values(item);
            let fields_match = expected.iter().all(|(name, value)| {
                name.starts_with("__") || field_matches(actual.get(name), value, name)
            });
            let sync_match = expected
                .get("__syncLevel")
                .is_none_or(|value| item.get("syncLevel") == Some(value));
            let enable_match = expected
                .get("__enable")
                .is_none_or(|value| item.get("enable") == Some(value));
            if fields_match && sync_match && enable_match {
                persisted = true;
                break;
            }
        }
        if !persisted {
            bail!(
                "{} was accepted but was not visible after 5 seconds",
                change.target
            );
        }
        return Ok(format!("{}: accepted and persisted", change.target));
    }
    Ok(format!(
        "{}: {} completed (HTTP {status})",
        change.target, change.action
    ))
}

fn write_integration_backup(manager: &Manager, changes: &[IntegrationChange]) -> Result<PathBuf> {
    let directory = manager
        .compose_file()
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let path = directory.join(format!(
        "docker-integration-backup.{}.json",
        Local::now().format("%Y%m%d-%H%M%S")
    ));
    let backup = changes
        .iter()
        .map(|change| {
            json!({
                "target": change.target,
                "method": change.method,
                "endpoint": change.endpoint,
                "previous_payload": change.previous_payload,
            })
        })
        .collect::<Vec<_>>();
    let mut bytes = serde_json::to_vec_pretty(&backup)?;
    bytes.push(b'\n');
    write_private_atomic(&path, &bytes)?;
    Ok(path)
}

#[derive(Debug)]
struct HttpResult {
    status: Option<u16>,
    data: Value,
}

impl HttpResult {
    fn description(&self) -> String {
        match &self.data {
            Value::String(value) => clip(value, 300),
            Value::Null => self
                .status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "unavailable".to_string()),
            value => clip(&value.to_string(), 300),
        }
    }
}

fn integration_request(
    method: &str,
    url: &str,
    api_key: &str,
    payload: Option<&Value>,
) -> HttpResult {
    let mut request = ureq::request(method, url)
        .timeout(Duration::from_secs(8))
        .set("Content-Type", "application/json");
    if !api_key.is_empty() {
        request = request.set("X-Api-Key", api_key);
    }
    let response = match payload {
        Some(payload) => request.send_json(payload),
        None => request.call(),
    };
    match response {
        Ok(response) => {
            let status = response.status();
            let data = response.into_json::<Value>().unwrap_or(Value::Null);
            HttpResult {
                status: Some(status),
                data,
            }
        }
        Err(ureq::Error::Status(status, response)) => {
            let text = response.into_string().unwrap_or_default();
            HttpResult {
                status: Some(status),
                data: Value::String(clip(&text, 300)),
            }
        }
        Err(ureq::Error::Transport(error)) => HttpResult {
            status: None,
            data: Value::String(error.to_string()),
        },
    }
}

fn service_api_base(row: &ServiceRow) -> Option<String> {
    row.ports
        .first()
        .map(|port| format!("http://127.0.0.1:{port}"))
}

fn field_values(item: &Value) -> Map<String, Value> {
    item.get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|field| {
            Some((
                field.get("name")?.as_str()?.to_string(),
                field.get("value").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect()
}

fn field_matches(current: Option<&Value>, desired: &Value, name: &str) -> bool {
    if name == "apiKey"
        && current.and_then(Value::as_str).is_some_and(|value| {
            ["********", "**********", "[redacted]", "<redacted>"].contains(&value)
        })
    {
        return true;
    }
    current == Some(desired)
}

fn update_fields(item: &Value, desired: &Map<String, Value>) -> Value {
    let mut updated = item.clone();
    let fields = updated
        .as_object_mut()
        .expect("integration item must be an object")
        .entry("fields")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("integration fields must be an array");
    for (name, value) in desired {
        if let Some(field) = fields
            .iter_mut()
            .find(|field| field.get("name").and_then(Value::as_str) == Some(name.as_str()))
        {
            field["value"] = value.clone();
        } else {
            fields.push(json!({"name": name, "value": value}));
        }
    }
    updated
}

fn block(plan: &mut IntegrationPlan, target: &str, reason: &str) {
    plan.blocked.push(BlockedIntegration {
        target: target.to_string(),
        reason: reason.to_string(),
    });
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid path {}", path.display()))?;
    let temporary = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn display_list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn clip(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_api_keys_compare_equal() {
        assert!(field_matches(
            Some(&json!("********")),
            &json!("secret"),
            "apiKey"
        ));
        assert!(!field_matches(Some(&json!("old")), &json!("new"), "apiKey"));
    }

    #[test]
    fn update_fields_preserves_unknown_fields() {
        let item = json!({
            "id": 4,
            "fields": [
                {"name": "host", "value": "old"},
                {"name": "untouched", "value": 9}
            ]
        });
        let desired = Map::from_iter([
            ("host".to_string(), json!("new")),
            ("port".to_string(), json!(8080)),
        ]);
        let updated = update_fields(&item, &desired);
        let fields = field_values(&updated);
        assert_eq!(fields.get("host"), Some(&json!("new")));
        assert_eq!(fields.get("port"), Some(&json!(8080)));
        assert_eq!(fields.get("untouched"), Some(&json!(9)));
    }
}
