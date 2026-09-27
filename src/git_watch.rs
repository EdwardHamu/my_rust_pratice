//! 内网 Git 仓库提交监控。
//!
//! 每 4 小时检查一次 `http://git.newtopiot.com/Newtopp/SPC.M.git` 有没有作者 `Cloud` 的新提交，
//! 有则用 Windows 托盘气泡（`controllers::me::show_windows_toast`）弹出通知，并在控制台打印明细。
//!
//! 实现：
//! - 只依赖系统 `git` 命令，在本地维护一个 bare 镜像（`git init --bare` + `git fetch --prune`）；
//! - 每次检查先记录镜像里所有远端分支/标签的提交 ID 作为基线，fetch 后用
//!   `git log <新 tip> --not <旧 tips>` 找出“这次新到达”的提交，再按作者过滤。
//!
//! 取舍：
//! - 判定依据是“提交是否首次可达”，不看提交时间：离线补推、rebase 后强推、新建分支都能检出，也不会重复通知。
//! - 基线就是磁盘上的镜像，程序重启后自然延续；首次运行（还没有镜像）只建立基线，不把历史提交当新提交。
//! - 同一提交同时出现在多个分支上只通知一次，通知里列出全部分支名。
//! - fetch 失败在一次检查内重试 3 次；连续失败只在第一次弹一条告警，恢复后再弹一条恢复通知，中间不刷屏。
//! - 凭据（可选）只经子进程环境变量交给 git 的 credential helper，不进命令行参数、日志和状态接口。
//!   未配置时交给 Git for Windows 自带的凭据管理器，并禁止其弹交互窗口（`GCM_INTERACTIVE=never`）。
//!
//! 可用环境变量覆盖默认值（都可选）：`GIT_WATCH_ENABLED`(true/false)、`GIT_WATCH_REPO_URL`、
//! `GIT_WATCH_AUTHOR`（逗号分隔多个，名字或邮箱包含即匹配、不区分大小写）、`GIT_WATCH_USERNAME` /
//! `GIT_WATCH_PASSWORD`、`GIT_WATCH_INTERVAL_MINUTES`、`GIT_WATCH_DIR`（镜像所在目录）。

use chrono::{DateTime, Local, SecondsFormat};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::time::sleep;
use warp::Rejection;

pub const DEFAULT_REPO_URL: &str = "http://git.newtopiot.com/Newtopp/SPC.M.git";
pub const DEFAULT_AUTHOR: &str = "Cloud";
pub const DEFAULT_INTERVAL_MINUTES: u64 = 4 * 60;

/// 首次 fetch 可能要拉整个仓库历史，给足时间；超时后杀掉子进程
const FETCH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const QUICK_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// 单次检查内 fetch 的最多尝试次数与间隔（吸收短暂网络抖动，避免误报）
const FETCH_ATTEMPTS: u32 = 3;
const FETCH_RETRY_DELAY: Duration = Duration::from_secs(30);
/// 单个 ref 单次最多解析的提交数，防止极端情况下（推送了一个全新历史）输出失控
const MAX_LOG_COMMITS: usize = 2000;
/// 控制台明细最多打印的提交条数
const MAX_COMMITS_IN_CONSOLE: usize = 50;
/// 托盘气泡正文里最多列出的提交条数（气泡只有 255 个字符）
const MAX_COMMITS_IN_TOAST: usize = 3;
/// Windows 托盘气泡限制：标题 63、正文 255（UTF-16 单元），留一点余量
const MAX_TOAST_TITLE_UTF16: usize = 60;
const MAX_TOAST_BODY_UTF16: usize = 240;
/// 气泡里每条提交说明的最大字符数（条数在标题里，正文放不下时截断即可）
const MAX_TOAST_SUBJECT_CHARS: usize = 40;
const MAX_ERROR_CHARS: usize = 400;

/// 运行配置
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub repo_url: String,
    pub authors: Vec<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub dir: PathBuf,
    pub interval: Duration,
}

impl Config {
    /// 常量默认值 + `GIT_WATCH_*` 环境变量覆盖。返回 `Ok(None)` 表示功能被显式关闭。
    pub fn from_env() -> Result<Option<Self>, String> {
        match env_non_empty("GIT_WATCH_ENABLED").as_deref() {
            None | Some("true") => {}
            Some("false") => return Ok(None),
            Some(_) => return Err("GIT_WATCH_ENABLED 只能是 true 或 false".to_string()),
        }
        let repo_url =
            env_non_empty("GIT_WATCH_REPO_URL").unwrap_or_else(|| DEFAULT_REPO_URL.to_string());
        validate_repo_url(&repo_url)?;
        let authors = parse_authors(
            &env_non_empty("GIT_WATCH_AUTHOR").unwrap_or_else(|| DEFAULT_AUTHOR.to_string()),
        );
        if authors.is_empty() {
            return Err("GIT_WATCH_AUTHOR 不能为空".to_string());
        }
        let interval_minutes = match env_non_empty("GIT_WATCH_INTERVAL_MINUTES") {
            None => DEFAULT_INTERVAL_MINUTES,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|m| (1..=7 * 24 * 60).contains(m))
                .ok_or_else(|| {
                    "GIT_WATCH_INTERVAL_MINUTES 必须是 1 ~ 10080 之间的整数（分钟）".to_string()
                })?,
        };
        let username = env_non_empty("GIT_WATCH_USERNAME");
        let password = env_non_empty("GIT_WATCH_PASSWORD");
        if username.is_some() != password.is_some() {
            return Err("GIT_WATCH_USERNAME 与 GIT_WATCH_PASSWORD 必须同时配置".to_string());
        }
        Ok(Some(Config {
            repo_url,
            authors,
            username,
            password,
            dir: env_non_empty("GIT_WATCH_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_dir),
            interval: Duration::from_secs(interval_minutes * 60),
        }))
    }

    /// 仓库显示名，如 `SPC.M`
    pub fn repo_name(&self) -> String {
        repo_name_from_url(&self.repo_url)
    }

    /// 本地 bare 镜像路径，如 `<dir>\SPC.M.git`
    pub fn repo_dir(&self) -> PathBuf {
        self.dir
            .join(format!("{}.git", sanitize_file_name(&self.repo_name())))
    }

    fn has_credentials(&self) -> bool {
        self.username.is_some() && self.password.is_some()
    }
}

/// 镜像默认放在 `%LOCALAPPDATA%\hello_cargo\git_watch`；没有该变量时放在 exe 旁边的 `git_watch` 目录
fn default_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(local).join("hello_cargo").join("git_watch");
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("git_watch")))
        .unwrap_or_else(|| PathBuf::from("git_watch"))
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn validate_repo_url(url: &str) -> Result<(), String> {
    let ok = ["http://", "https://", "ssh://", "git://", "file://"]
        .iter()
        .any(|p| url.starts_with(p))
        || (url.contains('@') && url.contains(':') && !url.contains("://"));
    if !ok || url.starts_with('-') || url.chars().any(char::is_whitespace) {
        return Err("GIT_WATCH_REPO_URL 不是合法的 Git 远端地址".to_string());
    }
    Ok(())
}

/// 作者列表：逗号或竖线分隔，去空白、去空项、去重
pub fn parse_authors(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split([',', '|']) {
        let p = part.trim();
        if !p.is_empty() && !out.iter().any(|a| a == p) {
            out.push(p.to_string());
        }
    }
    out
}

pub fn repo_name_from_url(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    let trimmed = no_query.trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\', ':']).next().unwrap_or(trimmed);
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        "repo".to_string()
    } else {
        name.to_string()
    }
}

fn sanitize_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('.').to_string();
    if s.is_empty() {
        "repo".to_string()
    } else {
        s
    }
}

/// 作者匹配：名字或邮箱包含配置的任一模式（不区分大小写）
pub fn author_matches(patterns: &[String], name: &str, email: &str) -> bool {
    let name = name.to_lowercase();
    let email = email.to_lowercase();
    patterns.iter().any(|p| {
        let p = p.to_lowercase();
        name.contains(&p) || email.contains(&p)
    })
}

/// 去掉文本中所有 URL 的 `user:password@` 部分，用于日志、错误信息与状态接口
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("://") {
        let (head, tail) = rest.split_at(pos + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(at) => {
                out.push_str("***@");
                out.push_str(&authority[at + 1..]);
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 按 UTF-16 编码单元截断（Windows API 按 WCHAR 计数）
fn truncate_utf16(s: &str, max: usize) -> String {
    if s.encode_utf16().count() <= max {
        return s.to_string();
    }
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > max.saturating_sub(1) {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// git 子进程
// ---------------------------------------------------------------------------

async fn git(
    cfg: &Config,
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut cmd = tokio::process::Command::new("git");
    // 绝不交互：终端提示、Git Credential Manager 的登录窗口都关掉，拿不到凭据就直接失败
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .arg("--no-pager");
    if cfg.has_credentials() {
        // 凭据只经子进程环境变量交给 helper，命令行参数中不含任何秘密
        cmd.env("GIT_WATCH_USERNAME", cfg.username.as_deref().unwrap_or_default())
            .env("GIT_WATCH_PASSWORD", cfg.password.as_deref().unwrap_or_default())
            .arg("-c")
            .arg("credential.helper=") // 清空系统/全局 helper，避免读到或写入别处的凭据
            .arg("-c")
            .arg(r#"credential.helper=!f() { printf 'username=%s\npassword=%s\n' "$GIT_WATCH_USERNAME" "$GIT_WATCH_PASSWORD"; }; f"#);
    }
    cmd.args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let subcommand = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .copied()
        .unwrap_or("");
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 git（请确认已安装 Git 且在 PATH 中）：{e}"))?;
    if let Some(input) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(input.as_bytes()).await;
            let _ = pipe.shutdown().await;
        }
    }
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("git {subcommand} 执行失败：{e}")),
        Err(_) => {
            return Err(format!(
                "git {subcommand} 超时（{} 秒），已终止",
                timeout.as_secs()
            ))
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // git 的多行报错里第一行 fatal:/error: 才是原因（后面常是 "Please make sure..." 之类的提示）
        let lines: Vec<&str> = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let detail = lines
            .iter()
            .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
            .or(lines.first())
            .copied()
            .unwrap_or("");
        return Err(redact(&format!(
            "git {subcommand} 失败（{}）：{}",
            output.status,
            truncate_chars(detail, MAX_ERROR_CHARS)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 当前镜像里所有远端分支与标签：ref 全名 -> 对象 ID
async fn snapshot_refs(cfg: &Config, repo: &Path) -> Result<BTreeMap<String, String>, String> {
    let out = git(
        cfg,
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%1f%(objectname)",
            "refs/remotes/origin/",
            "refs/tags/",
        ],
        None,
        QUICK_TIMEOUT,
    )
    .await?;
    let mut map = BTreeMap::new();
    for line in out.lines() {
        if let Some((name, sha)) = line.split_once('\u{1f}') {
            if name == "refs/remotes/origin/HEAD" {
                continue;
            }
            map.insert(name.to_string(), sha.trim().to_string());
        }
    }
    Ok(map)
}

/// `refs/remotes/origin/master` -> `master`；`refs/tags/v1` -> `tag:v1`
pub fn display_ref(full: &str) -> String {
    if let Some(b) = full.strip_prefix("refs/remotes/origin/") {
        b.to_string()
    } else if let Some(t) = full.strip_prefix("refs/tags/") {
        format!("tag:{t}")
    } else {
        full.to_string()
    }
}

/// 一条新提交
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub email: String,
    /// 作者时间，ISO 8601（git `%aI`）
    pub date: String,
    pub subject: String,
    /// 这次检查中该提交所属的分支/标签（可能多个）
    pub refs: Vec<String>,
}

/// 解析 `git log --format=%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e` 输出
pub fn parse_log(out: &str) -> Vec<Commit> {
    out.split('\u{1e}')
        .filter_map(|rec| {
            let rec = rec.trim_matches(|c| c == '\n' || c == '\r');
            if rec.is_empty() {
                return None;
            }
            let mut f = rec.splitn(5, '\u{1f}');
            let sha = f.next()?.trim().to_string();
            if sha.len() < 7 {
                return None;
            }
            Some(Commit {
                short_sha: sha.chars().take(7).collect(),
                sha,
                author: f.next().unwrap_or("").to_string(),
                email: f.next().unwrap_or("").to_string(),
                date: f.next().unwrap_or("").to_string(),
                subject: f
                    .next()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                refs: Vec::new(),
            })
        })
        .collect()
}

/// 一次检查的结果
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// 本次只是建立/重置基线（首次运行或远端地址变更），不产生通知
    pub baseline_only: bool,
    /// 本次发生变化（新建或移动）的分支/标签
    pub changed_refs: Vec<String>,
    /// 目标作者的新提交（按时间倒序）
    pub new_commits: Vec<Commit>,
    /// 本次新到达的全部提交数（含其他作者）
    pub total_new_commits: usize,
    pub tracked_refs: usize,
}

/// 执行一次完整检查：准备镜像 -> 记基线 -> fetch -> 找新提交 -> 按作者过滤。
/// 不弹通知，便于测试与手动触发复用。
pub async fn run_check(cfg: &Config) -> Result<Outcome, String> {
    let repo = cfg.repo_dir();
    let mut baseline_only = false;

    if !repo.join("HEAD").is_file() {
        std::fs::create_dir_all(&cfg.dir)
            .map_err(|e| format!("无法创建目录 {}：{e}", cfg.dir.display()))?;
        let repo_str = repo
            .to_str()
            .ok_or_else(|| "镜像路径包含非 UTF-8 字符".to_string())?;
        git(
            cfg,
            &cfg.dir,
            &["init", "--quiet", "--bare", repo_str],
            None,
            QUICK_TIMEOUT,
        )
        .await?;
        git(
            cfg,
            &repo,
            &["remote", "add", "origin", &cfg.repo_url],
            None,
            QUICK_TIMEOUT,
        )
        .await?;
        baseline_only = true;
    } else {
        let current = git(
            cfg,
            &repo,
            &["remote", "get-url", "origin"],
            None,
            QUICK_TIMEOUT,
        )
        .await
        .unwrap_or_default();
        if current.trim() != cfg.repo_url {
            git(
                cfg,
                &repo,
                &["remote", "set-url", "origin", &cfg.repo_url],
                None,
                QUICK_TIMEOUT,
            )
            .await?;
            baseline_only = true;
        }
    }

    let before = snapshot_refs(cfg, &repo).await?;
    if before.is_empty() {
        // 目录在但从未成功拉取过（例如上次首次 fetch 失败）：这次同样只建立基线
        baseline_only = true;
    }

    fetch_with_retry(cfg, &repo).await?;

    let after = snapshot_refs(cfg, &repo).await?;
    let changed: Vec<(String, String)> = after
        .iter()
        .filter(|(name, sha)| before.get(*name) != Some(*sha))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();

    let mut outcome = Outcome {
        baseline_only,
        changed_refs: changed.iter().map(|(n, _)| display_ref(n)).collect(),
        tracked_refs: after.len(),
        ..Default::default()
    };
    if baseline_only || changed.is_empty() {
        return Ok(outcome);
    }

    // 排除项：fetch 之前所有 tip。只要提交在 fetch 前已可达就不算新，无论它现在挂在哪个分支上。
    let mut exclude = String::new();
    for sha in before.values() {
        exclude.push('^');
        exclude.push_str(sha);
        exclude.push('\n');
    }
    let max_count = format!("--max-count={MAX_LOG_COMMITS}");
    let mut merged: BTreeMap<String, Commit> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for (name, sha) in &changed {
        let args: Vec<&str> = vec![
            "log",
            "--stdin",
            "--no-decorate",
            max_count.as_str(),
            "--format=%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e",
            sha.as_str(),
            // 没有路径参数；显式加上 `--` 避免 sha 被当成文件名
            "--",
        ];
        let out = git(cfg, &repo, &args, Some(&exclude), QUICK_TIMEOUT).await?;
        for c in parse_log(&out) {
            let entry = merged.entry(c.sha.clone()).or_insert_with(|| {
                order.push(c.sha.clone());
                c
            });
            let label = display_ref(name);
            if !entry.refs.contains(&label) {
                entry.refs.push(label);
            }
        }
    }
    outcome.total_new_commits = merged.len();

    let mut commits: Vec<Commit> = order
        .into_iter()
        .filter_map(|sha| merged.remove(&sha))
        .filter(|c| author_matches(&cfg.authors, &c.author, &c.email))
        .collect();
    commits.sort_by(|a, b| b.date.cmp(&a.date));
    outcome.new_commits = commits;
    Ok(outcome)
}

async fn fetch_with_retry(cfg: &Config, repo: &Path) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 1..=FETCH_ATTEMPTS {
        match fetch_once(cfg, repo).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("❌ [git-watch] fetch 失败（第 {attempt}/{FETCH_ATTEMPTS} 次）：{e}");
                last_err = e;
                if attempt < FETCH_ATTEMPTS {
                    sleep(FETCH_RETRY_DELAY).await;
                }
            }
        }
    }
    Err(last_err)
}

/// 只需要提交与树对象即可判断作者，优先用 blob:none 的部分克隆省磁盘；服务端不支持时退回普通 fetch。
async fn fetch_once(cfg: &Config, repo: &Path) -> Result<(), String> {
    let base = ["fetch", "--quiet", "--prune", "--no-recurse-submodules"];
    let mut with_filter: Vec<&str> = base.to_vec();
    with_filter.extend(["--filter=blob:none", "origin"]);
    let filter_err = match git(cfg, repo, &with_filter, None, FETCH_TIMEOUT).await {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    let mut plain: Vec<&str> = base.to_vec();
    plain.push("origin");
    match git(cfg, repo, &plain, None, FETCH_TIMEOUT).await {
        Ok(_) => Ok(()),
        Err(plain_err) if plain_err == filter_err => Err(plain_err),
        Err(plain_err) => Err(format!("{plain_err}（带 --filter 时：{filter_err}）")),
    }
}

// ---------------------------------------------------------------------------
// 通知内容
// ---------------------------------------------------------------------------

/// 一条桌面通知（托盘气泡：标题 ≤ 63、正文 ≤ 255 个 UTF-16 单元，正文不支持换行）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

fn format_commit_date(iso: &str) -> String {
    DateTime::parse_from_rfc3339(iso)
        .map(|d| d.with_timezone(&Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

fn format_refs(refs: &[String]) -> String {
    if refs.is_empty() {
        String::new()
    } else {
        format!(" [{}]", refs.join(", "))
    }
}

/// 新提交通知
pub fn build_commit_notification(cfg: &Config, commits: &[Commit]) -> Notification {
    let title = format!(
        "{} · {} 有 {} 条新提交",
        cfg.repo_name(),
        cfg.authors.join("/"),
        commits.len()
    );
    let mut parts: Vec<String> = commits
        .iter()
        .take(MAX_COMMITS_IN_TOAST)
        .map(|c| {
            format!(
                "{}{} {}",
                c.short_sha,
                format_refs(&c.refs),
                truncate_chars(&c.subject, MAX_TOAST_SUBJECT_CHARS)
            )
        })
        .collect();
    if commits.len() > MAX_COMMITS_IN_TOAST {
        parts.push(format!(
            "…等 {} 条，详见控制台",
            commits.len() - MAX_COMMITS_IN_TOAST
        ));
    }
    // 总条数已在标题里；正文超长时直接截断，不为保留结尾而牺牲前面的提交
    Notification {
        title: truncate_utf16(&title, MAX_TOAST_TITLE_UTF16),
        body: truncate_utf16(&parts.join(" | "), MAX_TOAST_BODY_UTF16),
    }
}

pub fn build_failure_notification(cfg: &Config, error: &str) -> Notification {
    Notification {
        title: truncate_utf16(
            &format!("{} 提交监控失败", cfg.repo_name()),
            MAX_TOAST_TITLE_UTF16,
        ),
        body: truncate_utf16(
            &format!(
                "已重试 {FETCH_ATTEMPTS} 次仍失败，恢复前不再提醒：{}",
                redact(error)
            ),
            MAX_TOAST_BODY_UTF16,
        ),
    }
}

pub fn build_recovery_notification(cfg: &Config, failures: u32) -> Notification {
    Notification {
        title: truncate_utf16(
            &format!("{} 提交监控已恢复", cfg.repo_name()),
            MAX_TOAST_TITLE_UTF16,
        ),
        body: format!("此前连续 {failures} 次检查失败，现已恢复正常。"),
    }
}

/// 控制台明细：完整列出新提交（气泡放不下）
fn print_commits(cfg: &Config, commits: &[Commit]) {
    println!(
        "🔔 [git-watch] {} 有 {} 条 {} 的新提交：",
        cfg.repo_name(),
        commits.len(),
        cfg.authors.join("/")
    );
    for c in commits.iter().take(MAX_COMMITS_IN_CONSOLE) {
        println!(
            "   • {}{} {} <{}> {} — {}",
            c.short_sha,
            format_refs(&c.refs),
            c.author,
            c.email,
            format_commit_date(&c.date),
            c.subject
        );
    }
    if commits.len() > MAX_COMMITS_IN_CONSOLE {
        println!(
            "   …另有 {} 条未列出",
            commits.len() - MAX_COMMITS_IN_CONSOLE
        );
    }
}

/// 真正弹出通知：Windows 托盘气泡（PowerShell 会阻塞约 5.5 秒，放到阻塞线程池里）
fn emit(notification: Notification) {
    #[cfg(windows)]
    {
        let Notification { title, body } = notification;
        tokio::task::spawn_blocking(move || {
            if let Err(error) = crate::controllers::me::show_windows_toast(&title, &body) {
                eprintln!("❌ [git-watch] 显示通知失败: {}", error);
            }
        });
    }
    #[cfg(not(windows))]
    {
        println!(
            "🔔 [git-watch] (非 Windows，仅打印) {} — {}",
            notification.title, notification.body
        );
    }
}

// ---------------------------------------------------------------------------
// 状态与调度
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub checks: u64,
    pub last_check_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub baseline_established: bool,
    pub tracked_refs: usize,
    pub last_changed_refs: Vec<String>,
    pub last_new_commits: Vec<Commit>,
    pub last_notified_at: Option<String>,
    pub notifications_sent: u64,
}

static CONFIG: OnceLock<Option<Config>> = OnceLock::new();
static CONFIG_ERROR: OnceLock<String> = OnceLock::new();
static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
/// 定时检查与手动触发互斥，避免两个 fetch 同时操作同一个镜像
static CHECK_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn status_cell() -> &'static Mutex<Status> {
    STATUS.get_or_init(|| Mutex::new(Status::default()))
}

fn check_lock() -> &'static tokio::sync::Mutex<()> {
    CHECK_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn now_text() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 根据一次检查结果更新状态并决定发哪些通知（通知通过 `emit` 注入，测试里不弹真实气泡）
pub fn apply_outcome(
    cfg: &Config,
    status: &mut Status,
    result: &Result<Outcome, String>,
    emit: &mut dyn FnMut(Notification),
) {
    status.checks += 1;
    status.last_check_at = Some(now_text());
    match result {
        Ok(outcome) => {
            let recovered_from = status.consecutive_failures;
            status.consecutive_failures = 0;
            status.last_error = None;
            status.last_success_at = status.last_check_at.clone();
            status.baseline_established = true;
            status.tracked_refs = outcome.tracked_refs;
            status.last_changed_refs = outcome.changed_refs.clone();
            if recovered_from > 0 {
                emit(build_recovery_notification(cfg, recovered_from));
                status.notifications_sent += 1;
            }
            if !outcome.new_commits.is_empty() {
                emit(build_commit_notification(cfg, &outcome.new_commits));
                status.notifications_sent += 1;
                status.last_notified_at = status.last_check_at.clone();
                status.last_new_commits = outcome.new_commits.clone();
            }
        }
        Err(error) => {
            status.consecutive_failures += 1;
            status.last_error = Some(truncate_chars(&redact(error), MAX_ERROR_CHARS));
            if status.consecutive_failures == 1 {
                emit(build_failure_notification(cfg, error));
                status.notifications_sent += 1;
            }
        }
    }
}

async fn check_and_notify(cfg: &Config) -> Result<Outcome, String> {
    let result = run_check(cfg).await;
    match &result {
        Ok(o) if o.baseline_only => println!(
            "🔎 [git-watch] 已建立基线：{} 个分支/标签，后续只通知新到达的提交",
            o.tracked_refs
        ),
        Ok(o) => {
            println!(
                "🔎 [git-watch] 检查完成：{} 个分支/标签有变化，新提交 {} 条，其中 {} 的 {} 条",
                o.changed_refs.len(),
                o.total_new_commits,
                cfg.authors.join("/"),
                o.new_commits.len()
            );
            if !o.new_commits.is_empty() {
                print_commits(cfg, &o.new_commits);
            }
        }
        Err(e) => eprintln!("❌ [git-watch] 检查失败：{e}"),
    }
    let mut status = status_cell().lock().unwrap_or_else(|p| p.into_inner());
    apply_outcome(cfg, &mut status, &result, &mut emit);
    result
}

/// 后台任务入口（main 里 `tokio::spawn` 一次）：启动后立即检查，之后每 4 小时一次。
/// 未启用或配置无效时只打印提示，不影响其它功能。
pub async fn start_git_watch() {
    let cfg = match Config::from_env() {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            println!("🔎 [git-watch] GIT_WATCH_ENABLED=false，已关闭仓库提交监控");
            let _ = CONFIG.set(None);
            return;
        }
        Err(e) => {
            eprintln!("❌ [git-watch] 配置无效，已关闭仓库提交监控：{e}");
            let _ = CONFIG_ERROR.set(e);
            let _ = CONFIG.set(None);
            return;
        }
    };
    if CONFIG.set(Some(cfg.clone())).is_err() {
        return; // 已经启动过
    }
    println!(
        "🔎 [git-watch] 已启用：每 {} 分钟检查 {} 中 {} 的新提交，镜像 {}",
        cfg.interval.as_secs() / 60,
        redact(&cfg.repo_url),
        cfg.authors.join("/"),
        cfg.repo_dir().display()
    );
    loop {
        {
            let _guard = check_lock().lock().await;
            let _ = check_and_notify(&cfg).await;
        }
        println!(
            "🔎 [git-watch] 下次检查在 {} 分钟后",
            cfg.interval.as_secs() / 60
        );
        sleep(cfg.interval).await;
    }
}

/// 手动立即检查一次（菜单 / HTTP 用）。与定时任务互斥；正在检查时返回 `BUSY`。
pub async fn check_now() -> Result<Outcome, &'static str> {
    let Some(Some(cfg)) = CONFIG.get() else {
        return Err("DISABLED");
    };
    let Ok(_guard) = check_lock().try_lock() else {
        return Err("BUSY");
    };
    check_and_notify(cfg).await.map_err(|_| "CHECK_FAILED")
}

/// 菜单项：立即检查一次并把结果打印到控制台
pub async fn run_manual_check() {
    println!("🔎 [git-watch] 正在检查，请稍候……");
    match check_now().await {
        Ok(o) if o.baseline_only => println!("✅ [git-watch] 已建立基线，本次不通知。"),
        Ok(o) if o.new_commits.is_empty() => {
            println!("✅ [git-watch] 没有目标作者的新提交。")
        }
        Ok(o) => println!(
            "✅ [git-watch] 发现 {} 条新提交，已弹出通知。",
            o.new_commits.len()
        ),
        Err("BUSY") => println!("⏳ [git-watch] 已有检查在进行中，请稍后再试。"),
        Err("DISABLED") => println!("⚠️ [git-watch] 仓库提交监控未启用（见启动日志）。"),
        Err(_) => println!("❌ [git-watch] 检查失败，原因见上方日志。"),
    }
}

/// 当前配置摘要 + 运行状态（不含凭据）
pub fn status_json() -> serde_json::Value {
    let status = status_cell()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let cfg = CONFIG.get().and_then(|c| c.as_ref());
    serde_json::json!({
        "enabled": cfg.is_some(),
        "configError": CONFIG_ERROR.get(),
        "repo": cfg.map(|c| redact(&c.repo_url)),
        "repoName": cfg.map(Config::repo_name),
        "authors": cfg.map(|c| c.authors.clone()),
        "intervalMinutes": cfg.map(|c| c.interval.as_secs() / 60),
        "mirrorDir": cfg.map(|c| c.repo_dir().display().to_string()),
        "credentialsConfigured": cfg.map(Config::has_credentials),
        "status": status,
    })
}

// ---------------------------------------------------------------------------
// HTTP（warp）：GET /git_watch/status、GET /git_watch/check
// ---------------------------------------------------------------------------

pub async fn http_status() -> Result<impl warp::Reply, Rejection> {
    Ok(warp::reply::json(&status_json()))
}

pub async fn http_check() -> Result<impl warp::Reply, Rejection> {
    use warp::http::StatusCode;
    let (status, body) = match check_now().await {
        Ok(outcome) => {
            let msg = if outcome.baseline_only {
                "已建立基线，本次不通知"
            } else if outcome.new_commits.is_empty() {
                "没有目标作者的新提交"
            } else {
                "发现新提交，已弹出通知"
            };
            (
                StatusCode::OK,
                serde_json::json!({"ok": true, "msg": msg, "data": outcome}),
            )
        }
        Err("BUSY") => (
            StatusCode::CONFLICT,
            serde_json::json!({"ok": false, "errorCode": "BUSY", "msg": "正在检查中，请稍后再试"}),
        ),
        Err("DISABLED") => (
            StatusCode::NOT_FOUND,
            serde_json::json!({"ok": false, "errorCode": "DISABLED", "msg": "仓库提交监控未启用"}),
        ),
        Err(code) => (
            StatusCode::BAD_GATEWAY,
            serde_json::json!({"ok": false, "errorCode": code, "msg": "检查失败，详见 /git_watch/status"}),
        ),
    };
    Ok(warp::reply::with_status(warp::reply::json(&body), status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_config(url: &str, dir: &Path, authors: &[&str]) -> Config {
        Config {
            repo_url: url.to_string(),
            authors: authors.iter().map(|s| s.to_string()).collect(),
            username: None,
            password: None,
            dir: dir.to_path_buf(),
            interval: Duration::from_secs(60),
        }
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "hello-cargo-git-watch-{tag}-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// 在临时目录里造一个“远端”仓库，用本地路径当作内网仓库地址（Windows / Linux 都可用）
    struct Remote {
        root: PathBuf,
        work: PathBuf,
    }
    impl Remote {
        fn new() -> Self {
            let root = unique_temp_dir("remote");
            let work = root.join("work");
            std::fs::create_dir_all(&work).unwrap();
            Self::run(&work, &["init", "--quiet"]);
            Self::run(&work, &["symbolic-ref", "HEAD", "refs/heads/master"]);
            Self::run(&work, &["config", "user.name", "Setup"]);
            Self::run(&work, &["config", "user.email", "setup@example.com"]);
            Self::run(&work, &["config", "commit.gpgsign", "false"]);
            Remote { root, work }
        }
        fn run(dir: &Path, args: &[&str]) -> String {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        fn url(&self) -> String {
            self.work.display().to_string()
        }
        fn commit(&self, author: &str, email: &str, subject: &str) -> String {
            let file = self.work.join("log.txt");
            let mut content = std::fs::read_to_string(&file).unwrap_or_default();
            content.push_str(subject);
            content.push('\n');
            std::fs::write(&file, content).unwrap();
            Self::run(&self.work, &["add", "-A"]);
            let out = Command::new("git")
                .args(["commit", "--quiet", "-m", subject])
                .current_dir(&self.work)
                .env("GIT_AUTHOR_NAME", author)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_NAME", "CI Bot")
                .env("GIT_COMMITTER_EMAIL", "ci@example.com")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "commit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            Self::run(&self.work, &["rev-parse", "HEAD"])
        }
        fn mirror_dir(&self) -> PathBuf {
            self.root.join("mirror")
        }
    }
    impl Drop for Remote {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn sample_commit(sha: &str, subject: &str, refs: &[&str]) -> Commit {
        Commit {
            sha: sha.to_string(),
            short_sha: sha.chars().take(7).collect(),
            author: "Cloud".into(),
            email: "cloud@corp.com".into(),
            date: "2026-09-27T10:12:00+08:00".into(),
            subject: subject.into(),
            refs: refs.iter().map(|r| r.to_string()).collect(),
        }
    }

    #[test]
    fn parses_author_lists_and_matches_case_insensitively() {
        assert_eq!(parse_authors("Cloud"), vec!["Cloud"]);
        assert_eq!(
            parse_authors(" Cloud , cloud@corp.com |Cloud| "),
            vec!["Cloud", "cloud@corp.com"]
        );
        assert!(parse_authors(" , | ").is_empty());
        let p = parse_authors("Cloud");
        assert!(author_matches(&p, "Cloud", "c@x.com"));
        assert!(author_matches(&p, "cloud", "c@x.com"));
        assert!(author_matches(&p, "Someone", "cloud.wang@corp.com"));
        assert!(!author_matches(&p, "Alice", "alice@corp.com"));
    }

    #[test]
    fn derives_repo_name_and_mirror_path() {
        assert_eq!(repo_name_from_url(DEFAULT_REPO_URL), "SPC.M");
        assert_eq!(repo_name_from_url("git@host:group/proj.git"), "proj");
        assert_eq!(repo_name_from_url("https://h/x/y/"), "y");
        assert_eq!(repo_name_from_url(r"C:\Users\me\repos\work"), "work");
        let cfg = test_config(DEFAULT_REPO_URL, Path::new("watch"), &["Cloud"]);
        assert_eq!(cfg.repo_dir(), Path::new("watch").join("SPC.M.git"));
        assert_eq!(sanitize_file_name("../evil name"), "_evil_name");
        assert_eq!(sanitize_file_name("SPC.M"), "SPC.M");
        assert_eq!(display_ref("refs/remotes/origin/master"), "master");
        assert_eq!(display_ref("refs/tags/v1.0"), "tag:v1.0");
    }

    #[test]
    fn redacts_credentials_embedded_in_urls() {
        assert_eq!(
            redact("fatal: http://bob:s3cr3t@git.example.com/a.git failed"),
            "fatal: http://***@git.example.com/a.git failed"
        );
        assert_eq!(redact("https://token@host/x"), "https://***@host/x");
        assert_eq!(
            redact("a http://u:p@h1/x and http://h2/y"),
            "a http://***@h1/x and http://h2/y"
        );
        assert_eq!(redact(DEFAULT_REPO_URL), DEFAULT_REPO_URL);
        assert_eq!(redact("git@host:group/proj.git"), "git@host:group/proj.git");
        assert_eq!(redact("no urls here"), "no urls here");
    }

    #[test]
    fn parses_git_log_records() {
        let out = "abc1234567\u{1f}Cloud\u{1f}cloud@x.com\u{1f}2026-09-27T10:00:00+08:00\u{1f}fix: 修复\u{1e}\n\
                   def4567890\u{1f}Alice\u{1f}a@x.com\u{1f}2026-09-27T09:00:00+08:00\u{1f}feat: 新功能\u{1e}\n";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].short_sha, "abc1234");
        assert_eq!(commits[0].author, "Cloud");
        assert_eq!(commits[0].subject, "fix: 修复");
        assert_eq!(commits[1].email, "a@x.com");
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn toast_text_fits_balloon_limits_and_hides_credentials() {
        let cfg = test_config(DEFAULT_REPO_URL, Path::new("w"), &["Cloud"]);
        let one = build_commit_notification(
            &cfg,
            &[sample_commit(
                "0123456789abcdef",
                "fix: 修复价格逻辑",
                &["master"],
            )],
        );
        assert_eq!(one.title, "SPC.M · Cloud 有 1 条新提交");
        assert_eq!(one.body, "0123456 [master] fix: 修复价格逻辑");

        // 短说明：三条都列出，结尾提示剩余条数
        let short: Vec<Commit> = (0..7)
            .map(|i| sample_commit(&format!("{i}{i}{i}{i}{i}{i}{i}{i}"), "fix", &["master"]))
            .collect();
        let toast = build_commit_notification(&cfg, &short);
        assert_eq!(toast.title, "SPC.M · Cloud 有 7 条新提交");
        assert_eq!(
            toast.body,
            "0000000 [master] fix | 1111111 [master] fix | 2222222 [master] fix | …等 4 条，详见控制台"
        );

        // 超长说明：正文按 UTF-16 截断到气泡上限，标题仍带总数，没有换行
        let long: Vec<Commit> = (0..7)
            .map(|i| {
                sample_commit(
                    &format!("{i}{i}{i}{i}{i}{i}{i}{i}"),
                    &"很长的提交说明".repeat(12),
                    &["feature/very-long-branch-name-modbus", "master"],
                )
            })
            .collect();
        let toast = build_commit_notification(&cfg, &long);
        assert!(toast.title.encode_utf16().count() <= 63);
        assert!(toast.body.encode_utf16().count() <= 255, "{}", toast.body);
        assert!(toast
            .body
            .starts_with("0000000 [feature/very-long-branch-name-modbus, master] 很长的提交说明"));
        assert!(toast.body.ends_with('…'));
        assert!(!toast.body.contains('\n'));

        let failure =
            build_failure_notification(&cfg, "git fetch 失败：http://u:p@host/x unreachable");
        assert!(!failure.body.contains("u:p@"));
        assert!(failure.body.contains("***@host"));
        assert!(failure.body.encode_utf16().count() <= 255);
    }

    #[test]
    fn status_transitions_notify_once_per_failure_streak_and_once_per_batch() {
        let cfg = test_config(DEFAULT_REPO_URL, Path::new("w"), &["Cloud"]);
        let mut status = Status::default();
        let sent: std::cell::RefCell<Vec<Notification>> = std::cell::RefCell::new(Vec::new());
        let baseline = Ok(Outcome {
            baseline_only: true,
            tracked_refs: 3,
            ..Default::default()
        });
        let quiet = Ok(Outcome {
            tracked_refs: 3,
            ..Default::default()
        });
        let found = Ok(Outcome {
            tracked_refs: 3,
            changed_refs: vec!["master".into()],
            new_commits: vec![sample_commit("abcdef1234", "s", &[])],
            total_new_commits: 2,
            ..Default::default()
        });
        let failed: Result<Outcome, String> = Err("git fetch 失败".into());

        let mut emit = |n: Notification| sent.borrow_mut().push(n);
        apply_outcome(&cfg, &mut status, &baseline, &mut emit);
        apply_outcome(&cfg, &mut status, &quiet, &mut emit);
        assert!(sent.borrow().is_empty());
        assert!(status.baseline_established);

        apply_outcome(&cfg, &mut status, &found, &mut emit);
        assert_eq!(sent.borrow().len(), 1);
        assert!(sent.borrow()[0].title.contains("有 1 条新提交"));
        assert_eq!(status.last_new_commits.len(), 1);

        apply_outcome(&cfg, &mut status, &failed, &mut emit);
        apply_outcome(&cfg, &mut status, &failed, &mut emit);
        apply_outcome(&cfg, &mut status, &failed, &mut emit);
        assert_eq!(
            sent.borrow().len(),
            2,
            "failure streak must notify exactly once"
        );
        assert!(sent.borrow()[1].title.contains("监控失败"));
        assert_eq!(status.consecutive_failures, 3);
        assert!(status.last_error.is_some());

        apply_outcome(&cfg, &mut status, &quiet, &mut emit);
        assert_eq!(sent.borrow().len(), 3);
        assert!(sent.borrow()[2].title.contains("已恢复"));
        assert_eq!(status.consecutive_failures, 0);
        assert_eq!(status.checks, 7);
        assert_eq!(status.notifications_sent, 3);
    }

    #[tokio::test]
    async fn detects_only_new_commits_by_target_author_across_restarts_and_branches() {
        let remote = Remote::new();
        remote.commit("Cloud", "cloud@corp.com", "old cloud commit");
        remote.commit("Alice", "alice@corp.com", "old alice commit");
        let cfg = test_config(&remote.url(), &remote.mirror_dir(), &["Cloud"]);

        // 首次：只建立基线，历史里的 Cloud 提交不通知
        let first = run_check(&cfg).await.unwrap();
        assert!(first.baseline_only);
        assert!(first.new_commits.is_empty());
        assert_eq!(first.tracked_refs, 1);

        // 没有新提交
        let quiet = run_check(&cfg).await.unwrap();
        assert!(!quiet.baseline_only);
        assert!(quiet.changed_refs.is_empty());
        assert!(quiet.new_commits.is_empty());

        // 其他作者提交 -> ref 变了但目标作者无新提交
        remote.commit("Alice", "alice@corp.com", "alice again");
        let other = run_check(&cfg).await.unwrap();
        assert_eq!(other.changed_refs, vec!["master"]);
        assert_eq!(other.total_new_commits, 1);
        assert!(other.new_commits.is_empty());

        // 目标作者在 master 和一个新分支上各提交一次（新分支包含 master 的提交）
        let c1 = remote.commit("Cloud", "cloud@corp.com", "cloud fix 1");
        Remote::run(&remote.work, &["checkout", "--quiet", "-b", "feature/x"]);
        let c2 = remote.commit("cloud", "cloud@corp.com", "cloud feature work");
        Remote::run(&remote.work, &["checkout", "--quiet", "master"]);
        // 重启场景：用同样的镜像目录重新构造 Config，基线来自磁盘
        let cfg2 = test_config(&remote.url(), &remote.mirror_dir(), &["Cloud"]);
        let found = run_check(&cfg2).await.unwrap();
        assert!(!found.baseline_only);
        assert_eq!(found.changed_refs, vec!["feature/x", "master"]);
        assert_eq!(found.total_new_commits, 2);
        assert_eq!(found.new_commits.len(), 2);
        let shas: Vec<&str> = found.new_commits.iter().map(|c| c.sha.as_str()).collect();
        assert!(shas.contains(&c1.as_str()) && shas.contains(&c2.as_str()));
        let on_master = found.new_commits.iter().find(|c| c.sha == c1).unwrap();
        assert_eq!(
            on_master.refs,
            vec!["feature/x", "master"],
            "commit reachable from both refs is reported once with both labels"
        );
        let on_feature = found.new_commits.iter().find(|c| c.sha == c2).unwrap();
        assert_eq!(on_feature.refs, vec!["feature/x"]);
        assert_eq!(on_feature.subject, "cloud feature work");

        // 同样的提交第二次检查不再出现
        let again = run_check(&cfg2).await.unwrap();
        assert!(again.new_commits.is_empty());
        assert!(again.changed_refs.is_empty());

        // 邮箱匹配也算目标作者；换成不相干的作者名则过滤掉
        remote.commit("Unknown Name", "cloud.zhang@corp.com", "by email");
        let by_email = run_check(&cfg2).await.unwrap();
        assert_eq!(by_email.new_commits.len(), 1);
        remote.commit("Bob", "bob@corp.com", "bob");
        let bob_cfg = test_config(&remote.url(), &remote.mirror_dir(), &["Nobody"]);
        assert!(run_check(&bob_cfg).await.unwrap().new_commits.is_empty());
    }

    #[tokio::test]
    async fn remote_url_change_resets_baseline_instead_of_flooding() {
        let remote_a = Remote::new();
        remote_a.commit("Cloud", "c@x", "a1");
        let remote_b = Remote::new();
        remote_b.commit("Cloud", "c@x", "b1");
        remote_b.commit("Cloud", "c@x", "b2");
        let dir = remote_a.mirror_dir();
        // 两个远端仓库名相同（都叫 work），共用同一个镜像目录
        let cfg_a = test_config(&remote_a.url(), &dir, &["Cloud"]);
        assert!(run_check(&cfg_a).await.unwrap().baseline_only);
        let cfg_b = test_config(&remote_b.url(), &dir, &["Cloud"]);
        let switched = run_check(&cfg_b).await.unwrap();
        assert!(switched.baseline_only);
        assert!(switched.new_commits.is_empty());
        remote_b.commit("Cloud", "c@x", "b3");
        let next = run_check(&cfg_b).await.unwrap();
        assert_eq!(next.new_commits.len(), 1);
        assert_eq!(next.new_commits[0].subject, "b3");
    }

    #[tokio::test]
    async fn unreachable_remote_reports_a_readable_error() {
        let root = unique_temp_dir("missing");
        // 注意远端路径不能与本地镜像路径（<dir>/<name>.git）重合，否则会“从自己 fetch”而成功
        let cfg = test_config(
            &root
                .join("missing")
                .join("remote.git")
                .display()
                .to_string(),
            &root,
            &["Cloud"],
        );
        assert_ne!(cfg.repo_dir(), root.join("missing").join("remote.git"));
        let repo = cfg.repo_dir();
        std::fs::create_dir_all(&cfg.dir).unwrap();
        git(
            &cfg,
            &cfg.dir,
            &["init", "--quiet", "--bare", repo.to_str().unwrap()],
            None,
            QUICK_TIMEOUT,
        )
        .await
        .unwrap();
        git(
            &cfg,
            &repo,
            &["remote", "add", "origin", &cfg.repo_url],
            None,
            QUICK_TIMEOUT,
        )
        .await
        .unwrap();
        // 直接走单次 fetch，避免测试里等 3 次 30 秒重试
        let err = fetch_once(&cfg, &repo).await.unwrap_err();
        assert!(err.starts_with("git fetch 失败"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_from_env_validates_switches_and_credentials() {
        // 环境变量是进程级的，测试串行设置并清理
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _g = ENV_LOCK.lock().unwrap();
        let keys = [
            "GIT_WATCH_ENABLED",
            "GIT_WATCH_REPO_URL",
            "GIT_WATCH_AUTHOR",
            "GIT_WATCH_USERNAME",
            "GIT_WATCH_PASSWORD",
            "GIT_WATCH_INTERVAL_MINUTES",
            "GIT_WATCH_DIR",
        ];
        let clear = || keys.iter().for_each(|k| std::env::remove_var(k));

        clear();
        let cfg = Config::from_env().unwrap().unwrap();
        assert_eq!(cfg.repo_url, DEFAULT_REPO_URL);
        assert_eq!(cfg.authors, vec!["Cloud"]);
        assert_eq!(cfg.interval, Duration::from_secs(4 * 3600));
        assert!(cfg.dir.ends_with("git_watch"));
        assert!(!cfg.has_credentials());

        std::env::set_var("GIT_WATCH_ENABLED", "false");
        assert!(Config::from_env().unwrap().is_none());
        std::env::set_var("GIT_WATCH_ENABLED", "yes");
        assert!(Config::from_env().is_err());
        clear();

        std::env::set_var("GIT_WATCH_INTERVAL_MINUTES", "0");
        assert!(Config::from_env().is_err());
        std::env::set_var("GIT_WATCH_INTERVAL_MINUTES", "30");
        std::env::set_var("GIT_WATCH_USERNAME", "bot");
        assert!(
            Config::from_env().is_err(),
            "username without password is rejected"
        );
        std::env::set_var("GIT_WATCH_PASSWORD", "secret");
        std::env::set_var("GIT_WATCH_AUTHOR", "Cloud, cloud@corp.com");
        std::env::set_var("GIT_WATCH_DIR", "gw");
        let cfg = Config::from_env().unwrap().unwrap();
        assert_eq!(cfg.interval, Duration::from_secs(1800));
        assert!(cfg.has_credentials());
        assert_eq!(cfg.authors, vec!["Cloud", "cloud@corp.com"]);
        assert_eq!(cfg.repo_dir(), Path::new("gw").join("SPC.M.git"));

        std::env::set_var("GIT_WATCH_REPO_URL", "-oProxyCommand=evil");
        assert!(Config::from_env().is_err());
        std::env::set_var("GIT_WATCH_REPO_URL", "http://git.example.com/a b.git");
        assert!(Config::from_env().is_err());
        clear();
    }

    /// 本机实跑：对真实内网仓库做一次检查（需要能访问 git.newtopiot.com）。
    /// `cargo test git_watch -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn manual_check_real_intranet_repo_on_this_machine() {
        let cfg = Config::from_env()
            .unwrap()
            .expect("GIT_WATCH_ENABLED 不能为 false");
        let outcome = run_check(&cfg).await.unwrap();
        println!("{}", serde_json::to_string_pretty(&outcome).unwrap());
    }
}
