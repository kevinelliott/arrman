pub mod backup;
pub mod command;
pub mod compose;
pub mod control;
pub mod diagnostics;
pub mod integrations;
pub mod model;
pub mod tui;
pub mod web;

pub const DEFAULT_COMPOSE_FILE: &str = "/root/docker-compose.yml";
pub const DEFAULT_REFRESH_SECONDS: f64 = 5.0;
pub const DEFAULT_WEB_BIND: &str = "127.0.0.1:9876";
pub const DEFAULT_BACKUP_DIR: &str = "/nfs/jellyfin/.docker-services-backups";
