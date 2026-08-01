use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct HostStats {
    pub ip: String,
    pub cpu_cores: Option<usize>,
    pub cpu_percent: Option<f64>,
    pub load_1m: Option<f64>,
    pub memory_total: Option<u64>,
    pub memory_available: Option<u64>,
    pub memory_used: Option<u64>,
    pub disk_total: Option<u64>,
    pub disk_free: Option<u64>,
    pub uptime_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortSpec {
    pub published: String,
    pub target: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    #[serde(rename = "type")]
    pub kind: String,
    pub summary: String,
    pub evidence: String,
    pub manual: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuggestedFix {
    pub action: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnosis {
    pub service: String,
    pub status: String,
    pub logs: Vec<String>,
    pub issues: Vec<Issue>,
    pub safe_fix: Option<SuggestedFix>,
    pub manual: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ServiceRow {
    pub service: String,
    pub purpose: String,
    pub image: String,
    pub status: String,
    pub running: bool,
    pub disabled: bool,
    pub configured: bool,
    pub container: Option<String>,
    pub network_mode: Option<String>,
    pub ports: Vec<String>,
    pub port_specs: Vec<PortSpec>,
    pub urls: Vec<String>,
    pub mounts: usize,
    pub depends_on: Vec<String>,
    pub dependency_conditions: BTreeMap<String, String>,
    pub dependency_status: BTreeMap<String, String>,
    pub dependencies_ready: bool,
    pub healthcheck: Option<Value>,
    pub healthcheck_configured: bool,
    #[serde(default)]
    pub issue_count: usize,
    #[serde(default)]
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedIntegration {
    pub target: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct IntegrationChange {
    pub target: String,
    pub service: String,
    pub kind: String,
    pub action: String,
    pub method: String,
    pub endpoint: String,
    #[serde(skip_serializing)]
    pub api_key: String,
    pub payload: Value,
    pub previous_payload: Option<Value>,
    pub summary: String,
    pub manual: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct IntegrationPlan {
    pub changes: Vec<IntegrationChange>,
    pub blocked: Vec<BlockedIntegration>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingIntegration {
    pub target: String,
    pub kind: String,
    pub action: String,
    pub summary: String,
}

impl From<&IntegrationChange> for PendingIntegration {
    fn from(value: &IntegrationChange) -> Self {
        Self {
            target: value.target.clone(),
            kind: value.kind.clone(),
            action: value.action.clone(),
            summary: value.summary.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct IntegrationSummary {
    pub pending: Vec<PendingIntegration>,
    pub blocked: Vec<BlockedIntegration>,
    pub scraparr_connectors: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogScanSummary {
    pub scanned_services: usize,
    pub issue_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DependencySummary {
    pub depends_on: BTreeMap<String, String>,
    pub healthcheck: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub host: String,
    pub compose_file: String,
    pub project: Option<String>,
    pub host_stats: HostStats,
    pub log_scan: LogScanSummary,
    pub integrations: IntegrationSummary,
    pub dependency_graph: BTreeMap<String, DependencySummary>,
    pub services: Vec<ServiceRow>,
}

#[derive(Clone, Debug)]
pub struct RuntimeData {
    pub config: Value,
    pub containers: Vec<Value>,
    pub disabled: std::collections::BTreeSet<String>,
    pub project: Option<String>,
    pub stats: HostStats,
    pub cpu_times: Option<CpuTimes>,
    pub services: Vec<ServiceRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Enable,
    Disable,
    Restart,
}

impl ServiceAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Enable => "Enable",
            Self::Disable => "Disable",
            Self::Restart => "Restart",
        }
    }
}
