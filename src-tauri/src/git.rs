//! Git through the `git` CLI (the user's own config, credentials and hooks apply).

use std::{collections::HashMap, path::Path};

use serde::Serialize;

use crate::core::{Res, command, err};

async fn git(cwd: &str, args: &[&str]) -> Res<String> {
    let out = command("git").args(args).current_dir(cwd).output().await?;
    if !out.status.success() {
        return Err(err(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFile {
    rel: String,
    abs: String,
    status: &'static str,
    staged: bool,
    added: u32,
    deleted: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitState {
    is_repo: bool,
    root: Option<String>,
    branch: Option<String>,
    files: Vec<GitFile>,
}

/// `added\tdeleted\tpath` records of `--numstat -z` → path → (added, deleted), summed.
fn numstats<'a>(outs: impl IntoIterator<Item = &'a str>) -> HashMap<String, (u32, u32)> {
    let mut stats: HashMap<String, (u32, u32)> = HashMap::new();
    for out in outs {
        for rec in out.split('\0') {
            let mut it = rec.splitn(3, '\t');
            let (Some(a), Some(d), Some(p)) = (it.next(), it.next(), it.next()) else { continue };
            if p.is_empty() {
                continue;
            }
            let e = stats.entry(p.to_owned()).or_default();
            e.0 += a.parse().unwrap_or(0);
            e.1 += d.parse().unwrap_or(0);
        }
    }
    stats
}

#[tauri::command]
pub async fn git_state(folder: String) -> Res<GitState> {
    let Ok(root) = git(&folder, &["rev-parse", "--show-toplevel"]).await.map(|s| s.trim().to_owned()) else {
        return Ok(GitState { is_repo: false, root: None, branch: None, files: vec![] });
    };
    let (branch, status, num, num_cached) = tokio::join!(
        git(&root, &["branch", "--show-current"]),
        // no index refresh: that write fires the watcher, which runs yet another status
        git(&root, &["--no-optional-locks", "status", "--porcelain=v1", "-z", "-uall"]),
        git(&root, &["diff", "--numstat", "-z"]),
        git(&root, &["diff", "--numstat", "--cached", "-z"]),
    );
    let (status, num, num_cached) = (status.unwrap_or_default(), num.unwrap_or_default(), num_cached.unwrap_or_default());
    let stats = numstats([num.as_str(), num_cached.as_str()]);

    let mut files = Vec::new();
    let mut recs = status.split('\0');
    while let Some(rec) = recs.next() {
        if rec.len() < 4 {
            continue;
        }
        let b = rec.as_bytes();
        let (x, y) = (b[0], b[1]);
        let rel = &rec[3..];
        if x == b'R' || x == b'C' {
            recs.next(); // the original path
        }
        let status = match (x, y) {
            (b'?', b'?') => "?",
            (b'U', _) | (_, b'U') => "U",
            (b'D', _) | (_, b'D') => "D",
            (b'A', _) => "A",
            (b'R', _) => "R",
            _ => "M",
        };
        let (added, deleted) = stats.get(rel).copied().unwrap_or_default();
        files.push(GitFile {
            abs: Path::new(&root).join(rel).to_string_lossy().into_owned(),
            rel: rel.to_owned(),
            status,
            staged: x != b' ' && x != b'?',
            added,
            deleted,
        });
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let branch = branch.map(|b| b.trim().to_owned()).ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "HEAD".into());
    Ok(GitState { is_repo: true, root: Some(root), branch: Some(branch), files })
}

/// File content at HEAD; '' when it does not exist there.
#[tauri::command]
pub async fn git_original(root: String, rel: String) -> Res<String> {
    Ok(git(&root, &["show", &format!("HEAD:{rel}")]).await.unwrap_or_default())
}

#[tauri::command]
pub async fn git_init(folder: String) -> Res<()> {
    git(&folder, &["init"]).await.map(drop)
}

#[tauri::command]
pub async fn git_show(root: String, rev: String, rel: String) -> Res<String> {
    Ok(git(&root, &["show", &format!("{rev}:{rel}")]).await.unwrap_or_default())
}

#[derive(Serialize)]
pub struct GitCommit {
    hash: String,
    short: String,
    author: String,
    date: u64,
    subject: String,
}

#[tauri::command]
pub async fn git_log(root: String) -> Res<Vec<GitCommit>> {
    let Ok(out) = git(&root, &["log", "-n200", "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e"]).await else {
        return Ok(vec![]);
    };
    Ok(out
        .split('\x1e')
        .filter_map(|rec| {
            let mut f = rec.trim_start().split('\x1f');
            let (hash, short) = (f.next()?, f.next()?);
            if hash.is_empty() || short.is_empty() {
                return None;
            }
            Some(GitCommit {
                hash: hash.into(),
                short: short.into(),
                author: f.next().unwrap_or_default().into(),
                date: f.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0) * 1000,
                subject: f.next().unwrap_or_default().into(),
            })
        })
        .collect())
}

#[derive(Serialize)]
pub struct GitCommitFile {
    rel: String,
    status: &'static str,
    added: u32,
    deleted: u32,
}

#[tauri::command]
pub async fn git_commit_files(root: String, hash: String) -> Res<Vec<GitCommitFile>> {
    let base = ["diff-tree", "--root", "--no-commit-id", "-r", "--no-renames", "-z"];
    let names_args: Vec<&str> = base.iter().copied().chain(["--name-status", &hash]).collect();
    let num_args: Vec<&str> = base.iter().copied().chain(["--numstat", &hash]).collect();
    let (names, nums) = tokio::join!(git(&root, &names_args), git(&root, &num_args));
    let (names, nums) = (names.unwrap_or_default(), nums.unwrap_or_default());
    let stats = numstats([nums.as_str()]);
    let parts: Vec<&str> = names.split('\0').collect();
    Ok(parts
        .as_chunks::<2>().0.iter()
        .filter_map(|p| {
            let (code, rel) = (p[0].chars().next()?, p[1]);
            if rel.is_empty() {
                return None;
            }
            let status = match code {
                'A' => "A",
                'D' => "D",
                'U' => "U",
                _ => "M",
            };
            let (added, deleted) = stats.get(rel).copied().unwrap_or_default();
            Some(GitCommitFile { rel: rel.into(), status, added, deleted })
        })
        .collect())
}

#[derive(Serialize)]
pub struct GitBranch {
    name: String,
    current: bool,
    /// only on the remote (e.g. `origin/feature`): checking it out makes a local tracking branch
    remote: bool,
}

/// Local branches, then remote ones that have no local branch of the same name.
#[tauri::command]
pub async fn git_branches(root: String) -> Res<Vec<GitBranch>> {
    let out = git(&root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname)%1f%(HEAD)", "refs/heads", "refs/remotes"]).await?;
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in out.lines() {
        let Some((refname, head)) = line.split_once('\x1f') else { continue };
        if let Some(name) = refname.strip_prefix("refs/heads/") {
            local.push(GitBranch { name: name.into(), current: head == "*", remote: false });
        } else if let Some(name) = refname.strip_prefix("refs/remotes/")
            && !name.ends_with("/HEAD")
        {
            remote.push(name.to_owned());
        }
    }
    for name in remote {
        let short = name.split_once('/').map_or(name.as_str(), |(_, b)| b);
        if !local.iter().any(|b| b.name == short) && !local.iter().any(|b| b.name == name) {
            local.push(GitBranch { name, current: false, remote: true });
        }
    }
    Ok(local)
}

/// Switch to a branch; a remote one (`origin/x`) becomes local `x` tracking it. Git refuses when
/// uncommitted changes would be overwritten: its message comes back as the error.
#[tauri::command]
pub async fn git_checkout(root: String, name: String, remote: bool) -> Res<()> {
    if remote {
        let local = name.split_once('/').map_or(name.as_str(), |(_, b)| b);
        git(&root, &["switch", "-c", local, "--track", &name]).await.map(drop)
    } else {
        git(&root, &["switch", &name]).await.map(drop)
    }
}

/// Throw away a file's uncommitted changes (staged and not): back to HEAD, or gone when HEAD
/// doesn't have it (new / untracked files).
#[tauri::command]
pub async fn git_discard(root: String, rel: String, status: String) -> Res<()> {
    match status.as_str() {
        "?" => Ok(tokio::fs::remove_file(Path::new(&root).join(&rel)).await?),
        "A" => git(&root, &["rm", "-f", "--", &rel]).await.map(drop),
        _ => git(&root, &["restore", "--source=HEAD", "--staged", "--worktree", "--", &rel]).await.map(drop),
    }
}
