# cloudflared 端口监控与 sync-port.ps1 自动同步

## 需求

每隔 10 秒执行

```powershell
Get-CimInstance Win32_Process -Filter "name='cloudflared.exe'" | Select-Object -ExpandProperty CommandLine
```

检查 cloudflared 的本地端口有无变化；有变化时执行

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\11038\mcp-agent\sync-port.ps1
```

本机实测 cloudflared 的命令行形如 `cloudflared tunnel --url http://127.0.0.1:54571`，
「端口」即 `--url` 中的本地端口。

## 实现

新增 `src/cloudflared_port_monitor.rs`，在 `src/main.rs` 中与其它后台任务一样用
`tokio::spawn` 启动 `start_cloudflared_port_monitor()`（`_handle8`），Debug 与 Release 都运行。

### 轮询与判定

| 函数 | 作用 |
|---|---|
| `query_command_lines()` | `spawn_blocking` 中调用 `powershell.exe -NoProfile -NonInteractive -Command <CIM 查询>`，返回 stdout |
| `extract_ports()` | 逐行解析 `--url VALUE` / `--url=VALUE`，取端口，去重升序 |
| `should_sync()` | 有基准、当前端口非空、且与基准不同时为真 |
| `next_baseline()` | 首次观察原样记录；之后只用非空结果覆盖基准 |
| `run_sync_port_script()` | `spawn_blocking` 中执行 `powershell -NoProfile -ExecutionPolicy Bypass -File <脚本>`，等待结束并记录退出状态与输出 |

`extract_ports()` 兼容：带引号的可执行路径、带引号的 URL、URL 结尾斜杠、`host:port` 裸写法、
IPv6（`[::1]:8080`）、多进程多行（合并为集合）。URL 未写端口时按 scheme 取 80 / 443；
既无 scheme 也无端口的值忽略。命令行里没有 `--url`（如 `tunnel run <name>` 的配置文件模式）
视为无端口。

### 取舍

- **首次读到只记基准，不同步**：hello_cargo 自身重启不等于端口变化。
- **cloudflared 短暂不在不算变化**：读到空列表（通常是重启中）既不触发同步也不更新基准，
  等它带着端口回来再与旧基准比对。这样一次 cloudflared 重启最多触发一次同步，且端口没变时一次都不触发。
- **启动时 cloudflared 尚未运行**：基准记为空集合，之后 cloudflared 出现即视为变化并同步一次。
- **同步脚本失败只记日志**，不影响下一轮检查；脚本运行期间不会并发再起一次（等待其结束后才继续轮询）。
- 基准只保存在内存里，不落盘；程序停止期间发生的端口变化在重启后不会被追认。

日志前缀统一为 `[cloudflared]`：开始监控（含当前端口）、端口变化（旧 -> 新）、脚本执行结果、查询失败。

## 测试

`cargo test cloudflared_port_monitor`：12 个单元测试通过，覆盖端口解析的各种命令行形态与判定/基准逻辑。
另有 1 个 `#[ignore]` 的手动测试 `manual_query_cloudflared_command_lines_on_this_machine`，
用 `cargo test cloudflared_port_monitor -- --ignored --nocapture` 在本机实跑 CIM 查询，
本次输出 `cloudflared tunnel --url http://127.0.0.1:54571` → `ports: [54571]`。

`rustfmt --check src/cloudflared_port_monitor.rs` 通过（按 `AGENTS.md` 只格式化本次改动的文件）；
`get_diagnostics` 对 `src/cloudflared_port_monitor.rs` 与 `src/main.rs` 均为 0 条。

## 注意

- Release 构建以管理员身份运行，`Get-CimInstance` 能读到所有进程的命令行；Debug 非提权时若
  cloudflared 以管理员启动，`CommandLine` 可能为空，会被当作「无端口」处理。
- 目标脚本路径 `C:\Users\11038\mcp-agent\sync-port.ps1` 与 `file_monitor.rs` 一样硬编码在常量里。
