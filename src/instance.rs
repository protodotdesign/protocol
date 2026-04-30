use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{Workspace, instances_dir};

#[derive(Debug, Clone)]
pub struct Instance {
    pub name: String,
    #[allow(dead_code)]
    pub workspace: String,
    #[allow(dead_code)]
    pub path: PathBuf,
}

pub fn list_instances(workspace: &str) -> Vec<Instance> {
    let root = instances_dir().join(workspace);
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&root) else { return out };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if !path.is_dir() { continue; }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        out.push(Instance {
            name: name.to_string(),
            workspace: workspace.to_string(),
            path,
        });
    }
    out
}

pub fn instance_dir(workspace: &str, instance: &str) -> PathBuf {
    instances_dir().join(workspace).join(instance)
}

pub fn create_instance(workspace: &Workspace, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("instance name cannot be empty"));
    }
    let dir = instance_dir(&workspace.name, name);
    if dir.exists() {
        return Err(anyhow!("instance '{name}' already exists"));
    }
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    for repo in &workspace.repos {
        let target = dir.join(&repo.name);
        let status = Command::new("git")
            .args(["clone", "--branch", &repo.default_branch, "--single-branch", &repo.url])
            .arg(&target)
            .status()
            .with_context(|| format!("spawn git clone for {}", repo.name))?;
        if !status.success() {
            return Err(anyhow!("git clone failed for {}", repo.name));
        }
    }
    Ok(())
}

pub fn delete_instance(workspace: &str, name: &str) -> Result<()> {
    let dir = instance_dir(workspace, name);
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).with_context(|| format!("rm -rf {}", dir.display()))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub fn read_dir_sorted(path: &Path) -> Vec<DirEntry> {
    let mut entries: Vec<DirEntry> = fs::read_dir(path)
        .map(|it| {
            it.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let name = p.file_name()?.to_str()?.to_string();
                    if name == ".git" { return None; }
                    let is_dir = p.is_dir();
                    Some(DirEntry { name, path: p, is_dir })
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    entries
}

