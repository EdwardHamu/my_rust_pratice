//! cloudflared 端口监控。
//!
//! 每 10 秒执行一次
//! `Get-CimInstance Win32_Process -Filter "name='cloudflared.exe'" | Select-Object -ExpandProperty CommandLine`，
//! 从命令行的 `--url` 参数里解析本地端口；端口与上一次不同时执行
//! `powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\11038\mcp-agent\sync-port.ps1`。
//!
//! 取舍：
//! - 首次读到的端口只作为基准，不触发同步（程序重启不等于端口变化）。
//! - cloudflared 短暂不在（读到空列表，通常是重启中）不算变化，也不更新基准；
//!   等它带着端口回来时再和旧基准比对，避免重启过程触发两次同步。
//! - 同步脚本的失败只记录日志，不影响下一轮检查。

use std::process::Command;
use tokio::time::{sleep, Duration};

const POLL_INTERVAL: u64 = 10; // 每10秒检查一次
const PROCESS_NAME: &str = "cloudflared.exe";
const SYNC_PORT_SCRIPT: &str = r"C:\Users\11038\mcp-agent\sync-port.ps1";

/// 每 10 秒检查一次 cloudflared 的本地端口，变化时执行 sync-port.ps1
pub async fn start_cloudflared_port_monitor() {
    // None 表示还没有成功读到过命令行；首次读到只记基准，不触发同步
    let mut last_ports: Option<Vec<u16>> = None;

    loop {
        match query_command_lines().await {
            Ok(command_lines) => {
                let current = extract_ports(&command_lines);

                if last_ports.is_none() {
                    println!("🌐 [cloudflared] 开始监控，当前端口: {:?}", current);
                }

                if should_sync(last_ports.as_deref(), &current) {
                    println!(
                        "🌐 [cloudflared] 端口变化: {:?} -> {:?}，执行 sync-port.ps1",
                        last_ports.as_deref().unwrap_or(&[]),
                        current
                    );
                    match run_sync_port_script().await {
                        Ok(summary) => {
                            println!("🌐 [cloudflared] sync-port.ps1 执行完成{}", summary)
                        }
                        Err(e) => eprintln!("❌ [cloudflared] sync-port.ps1 执行失败: {}", e),
                    }
                }

                last_ports = next_baseline(last_ports, current);
            }
            Err(e) => eprintln!("❌ [cloudflared] 读取进程命令行失败: {}", e),
        }

        sleep(Duration::from_secs(POLL_INTERVAL)).await;
    }
}

/// 是否需要执行同步：有基准、当前有端口、且与基准不同
fn should_sync(previous: Option<&[u16]>, current: &[u16]) -> bool {
    match previous {
        None => false,
        Some(_) if current.is_empty() => false,
        Some(prev) => prev != current,
    }
}

/// 下一轮的基准：首次观察原样记录；之后只用非空结果覆盖
fn next_baseline(previous: Option<Vec<u16>>, current: Vec<u16>) -> Option<Vec<u16>> {
    match previous {
        None => Some(current),
        Some(prev) if current.is_empty() => Some(prev),
        Some(_) => Some(current),
    }
}

/// 从若干行 cloudflared 命令行中解析 `--url` 的端口，去重升序
pub fn extract_ports(command_lines: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = command_lines
        .lines()
        .filter_map(url_argument)
        .filter_map(|url| port_of_url(&url))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// 取出一行命令里 `--url VALUE` 或 `--url=VALUE` 的 VALUE（去掉包裹引号）
fn url_argument(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    while let Some(token) = tokens.next() {
        let token = token.trim_matches(|c| c == '"' || c == '\'');
        if token == "--url" {
            return tokens
                .next()
                .map(|value| value.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
        if let Some(value) = token.strip_prefix("--url=") {
            return Some(value.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    None
}

/// 解析 URL 或 host:port 中的端口；没写端口时按 scheme 取默认值
fn port_of_url(url: &str) -> Option<u16> {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme.to_ascii_lowercase()), rest),
        None => (None, url),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);

    // IPv6 形如 [::1]:8080
    let port_text = if let Some(end) = host_port.rfind(']') {
        host_port[end + 1..].strip_prefix(':')
    } else {
        host_port.rsplit_once(':').map(|(_, port)| port)
    };

    match port_text {
        Some(port) => port.parse::<u16>().ok(),
        None => match scheme.as_deref() {
            Some("http") | Some("ws") => Some(80),
            Some("https") | Some("wss") => Some(443),
            _ => None,
        },
    }
}

async fn query_command_lines() -> Result<String, String> {
    tokio::task::spawn_blocking(query_command_lines_blocking)
        .await
        .map_err(|e| format!("查询任务异常退出: {}", e))?
}

fn query_command_lines_blocking() -> Result<String, String> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"name='{}'\" | Select-Object -ExpandProperty CommandLine",
        PROCESS_NAME
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("无法启动 PowerShell: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("PowerShell 退出状态: {}", output.status)
        } else {
            stderr
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_sync_port_script() -> Result<String, String> {
    tokio::task::spawn_blocking(run_sync_port_script_blocking)
        .await
        .map_err(|e| format!("同步任务异常退出: {}", e))?
}

fn run_sync_port_script_blocking() -> Result<String, String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            SYNC_PORT_SCRIPT,
        ])
        .output()
        .map_err(|e| format!("无法启动 PowerShell: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        return Err(if stderr.is_empty() {
            format!("退出状态: {}", output.status)
        } else {
            format!("退出状态: {}，stderr: {}", output.status, stderr)
        });
    }

    Ok(if stdout.is_empty() {
        String::new()
    } else {
        format!("，输出: {}", stdout)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_port_from_plain_url_argument() {
        let line = r"C:\tools\cloudflared.exe tunnel --url http://localhost:8080";
        assert_eq!(extract_ports(line), vec![8080]);
    }

    #[test]
    fn extracts_port_from_equals_form_and_bare_host_port() {
        assert_eq!(
            extract_ports("cloudflared.exe tunnel --url=localhost:3000"),
            vec![3000]
        );
        assert_eq!(
            extract_ports("cloudflared.exe tunnel --url 127.0.0.1:9000"),
            vec![9000]
        );
    }

    #[test]
    fn extracts_port_from_quoted_path_and_quoted_url_with_trailing_slash() {
        let line = r#""C:\Program Files\cloudflared\cloudflared.exe" tunnel --url "http://127.0.0.1:5173/""#;
        assert_eq!(extract_ports(line), vec![5173]);
    }

    #[test]
    fn extracts_port_from_ipv6_url() {
        assert_eq!(
            extract_ports("cloudflared tunnel --url http://[::1]:4321"),
            vec![4321]
        );
    }

    #[test]
    fn falls_back_to_scheme_default_port_when_url_has_no_port() {
        assert_eq!(
            extract_ports("cloudflared tunnel --url http://localhost"),
            vec![80]
        );
        assert_eq!(
            extract_ports("cloudflared tunnel --url https://localhost/"),
            vec![443]
        );
        assert_eq!(
            extract_ports("cloudflared tunnel --url localhost"),
            Vec::<u16>::new()
        );
    }

    #[test]
    fn merges_multiple_processes_sorted_and_deduplicated() {
        let lines = "cloudflared tunnel --url http://localhost:8080\r\n\
                     cloudflared tunnel --url http://localhost:3000\r\n\
                     cloudflared tunnel --url http://localhost:8080\r\n";
        assert_eq!(extract_ports(lines), vec![3000, 8080]);
    }

    #[test]
    fn ignores_lines_without_url_argument_and_empty_output() {
        assert_eq!(
            extract_ports("cloudflared tunnel run my-tunnel"),
            Vec::<u16>::new()
        );
        assert_eq!(extract_ports(""), Vec::<u16>::new());
        assert_eq!(extract_ports("   \r\n"), Vec::<u16>::new());
    }

    #[test]
    fn first_observation_only_sets_baseline() {
        assert!(!should_sync(None, &[8080]));
        assert_eq!(next_baseline(None, vec![8080]), Some(vec![8080]));
        assert_eq!(next_baseline(None, vec![]), Some(vec![]));
    }

    #[test]
    fn unchanged_port_does_not_sync() {
        assert!(!should_sync(Some(&[8080]), &[8080]));
    }

    #[test]
    fn changed_port_syncs_and_moves_baseline() {
        assert!(should_sync(Some(&[8080]), &[9090]));
        assert_eq!(
            next_baseline(Some(vec![8080]), vec![9090]),
            Some(vec![9090])
        );
    }

    #[test]
    fn process_disappearing_neither_syncs_nor_moves_baseline() {
        assert!(!should_sync(Some(&[8080]), &[]));
        assert_eq!(next_baseline(Some(vec![8080]), vec![]), Some(vec![8080]));
    }

    #[test]
    fn process_appearing_after_empty_baseline_syncs() {
        assert!(should_sync(Some(&[]), &[8080]));
    }

    #[test]
    #[ignore]
    fn manual_query_cloudflared_command_lines_on_this_machine() {
        let lines = query_command_lines_blocking().unwrap();
        println!(
            "cloudflared command lines:\n{}\nports: {:?}",
            lines,
            extract_ports(&lines)
        );
    }
}
