use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    #[serde(rename = "repo", default)]
    pub repos: Vec<Repo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub url: String,
    pub default_branch: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub last_workspace: Option<String>,
    pub last_instance: Option<String>,
    #[serde(default)]
    pub expanded: Vec<PathBuf>,
    pub selected_file: Option<PathBuf>,
    #[serde(default)]
    pub layouts: Vec<InstanceLayout>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceLayout {
    pub workspace: String,
    pub instance: String,
    pub left_panel_w: f32,
    #[serde(default = "default_right_panel_w")]
    pub right_panel_w: f32,
    pub terminal_panel_size: f32,
    /// "bottom" or "right".
    pub terminal_panel_pos: String,
    #[serde(default)]
    pub left_collapsed: bool,
    #[serde(default)]
    pub right_collapsed: bool,
}

fn default_right_panel_w() -> f32 { 240. }

pub fn protocol_root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".protocol"))
        .unwrap_or_else(|| PathBuf::from(".protocol"))
}

pub fn workspaces_dir() -> PathBuf { protocol_root().join("workspaces") }
pub fn instances_dir() -> PathBuf { protocol_root().join("instances") }
pub fn state_path() -> PathBuf { protocol_root().join("state.toml") }

pub fn ensure_dirs() -> Result<()> {
    fs::create_dir_all(workspaces_dir())?;
    fs::create_dir_all(instances_dir())?;
    Ok(())
}

pub fn load_workspaces() -> Vec<(Workspace, PathBuf, Option<String>)> {
    let dir = workspaces_dir();
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else { return out };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|s| s.to_str()) != Some("toml") { continue; }
        match load_workspace(&path) {
            Ok(w) => out.push((w, path, None)),
            Err(e) => {
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                out.push((
                    Workspace { name, repos: vec![] },
                    path,
                    Some(format!("{e:#}")),
                ));
            }
        }
    }
    out
}

fn load_workspace(path: &Path) -> Result<Workspace> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let w: Workspace = toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(w)
}

pub fn load_app_state() -> AppState {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_app_state(state: &AppState) {
    if let Ok(s) = toml::to_string_pretty(state) {
        let _ = fs::write(state_path(), s);
    }
}

/// Create a new blank workspace TOML on disk. Returns the file's path so the
/// caller can open it in the editor. Uniqueness handled by appending `-N`.
pub fn create_blank_workspace() -> Result<PathBuf> {
    ensure_dirs()?;
    let dir = workspaces_dir();
    let mut name = "new-workspace".to_string();
    let mut path = dir.join(format!("{name}.toml"));
    let mut n = 2;
    while path.exists() {
        name = format!("new-workspace-{n}");
        path = dir.join(format!("{name}.toml"));
        n += 1;
    }
    let template = format!(
        "name = \"{name}\"\n\n# Add repos like:\n# [[repo]]\n# name = \"example\"\n# url = \"git@github.com:org/repo.git\"\n# default_branch = \"main\"\n"
    );
    fs::write(&path, template).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}
