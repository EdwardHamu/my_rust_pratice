# 内网仓库 SPC.M 提交监控（git-watch）

## 需求

每隔 10 分钟检查内网仓库 `http://git.newtopiot.com/Newtopp/SPC.M.git` 有没有作者 **Cloud** 的新提交，
有就弹出通知。

## 实现

新增 `src/git_watch.rs`，在 `src/main.rs` 中与其它后台任务一样用 `tokio::spawn` 启动
`start_git_watch()`（`_handle8`），Debug 与 Release 都运行；启动后立即检查一次，之后每 10 分钟一次（2026-09-29 由 4 小时改为 10 分钟）。

通知复用 `controllers::me::show_windows_toast()`（改为 `pub(crate)`）的 Windows 托盘气泡，
同时在控制台打印完整明细（气泡只有 255 个字符，最多列 3 条，总条数在标题里）：

```
标题：SPC.M · Cloud 有 2 条新提交
正文：c857fcf [feature/modbus] feat(modbus): add holding register map | 0009af8 [feature/modbus, master] fix(spc): 修复采样周期计算错误
```

### 检测原理

| 函数 | 作用 |
|---|---|
| `run_check()` | 一次完整检查：准备镜像 → 记基线 → fetch → 找新提交 → 按作者过滤；不弹通知 |
| `snapshot_refs()` | `git for-each-ref` 记录镜像里所有远端分支/标签的提交 ID |
| `fetch_with_retry()` / `fetch_once()` | `git fetch --prune`，一次检查内最多 3 次（间隔 30 秒）；优先 `--filter=blob:none` 省磁盘，服务端不支持时退回普通 fetch |
| `parse_log()` / `author_matches()` | 解析 `git log <新 tip> --not <旧 tips>` 输出；作者名或邮箱包含 `Cloud`（不区分大小写）即算 |
| `apply_outcome()` | 更新状态并决定发哪些通知（通知函数以闭包注入，测试不弹真实气泡） |
| `check_now()` / `run_manual_check()` | 手动立即检查（菜单 10、HTTP），与定时任务互斥 |

本地 bare 镜像默认在 `%LOCALAPPDATA%\hello_cargo\git_watch\SPC.M.git`（没有该变量时在 exe 旁的 `git_watch\`），
只依赖 PATH 里的 `git`。

### 取舍

- **按“提交是否首次可达”判定，不看提交时间**：离线补推、rebase 后强推、新建分支都能检出，且不会重复通知。
- **基线就是磁盘上的镜像**：程序重启后自然延续；首次运行（还没有镜像）只建立基线，不把历史提交当新提交；
  删掉镜像目录等于重新建基线。
- **同一提交出现在多个分支上只通知一次**，通知里列出全部分支名（如 `[feature/modbus, master]`）。
- **失败不刷屏**：连续失败只在第一次弹一条「SPC.M 提交监控失败」，之后只记日志；恢复后弹一条「已恢复」。
  典型场景：笔记本离开内网/VPN 断开时只会收到一次提醒。
- **绝不交互**：`GIT_TERMINAL_PROMPT=0` + `GCM_INTERACTIVE=never`，拿不到凭据直接失败，不会弹出 Git 登录窗口卡住 fetch；
  fetch 超过 15 分钟会被终止。
- 更换仓库地址后第一次检查只重置基线，不会把新仓库的历史当成新提交。

日志前缀统一为 `[git-watch]`：启用信息（含镜像路径）、基线建立、每次检查汇总、新提交明细、fetch 失败原因。
日志与状态接口里的地址都会去掉 `user:password@`。

### 可选配置（环境变量，不设即用默认值）

| 变量 | 默认 | 说明 |
|---|---|---|
| `GIT_WATCH_ENABLED` | `true` | `false` 关闭功能 |
| `GIT_WATCH_REPO_URL` | `http://git.newtopiot.com/Newtopp/SPC.M.git` | 不要把账号密码写进 URL |
| `GIT_WATCH_AUTHOR` | `Cloud` | 多个用逗号分隔，如 `Cloud,cloud@corp.com` |
| `GIT_WATCH_USERNAME` / `GIT_WATCH_PASSWORD` | 空 | 仓库需要 HTTP 认证且 Git 凭据管理器里没有时填写，须同时设置；只经子进程环境变量交给 credential helper |
| `GIT_WATCH_INTERVAL_MINUTES` | `10` | 1 ~ 10080；上线验证时可临时改小 |
| `GIT_WATCH_DIR` | `%LOCALAPPDATA%\hello_cargo\git_watch` | 镜像所在目录 |

### 手动触发与查看

- 菜单 **10. 立即检查 SPC.M 有无 Cloud 的新提交**（原「退出」顺延为 11）。
- `GET /git_watch/status`：配置摘要（不含凭据）与最近一次检查状态（`checks`、`lastCheckAt`、`lastError`、
  `consecutiveFailures`、`lastNewCommits`、`notificationsSent` 等）。
- `GET /git_watch/check`：立即检查一次并按正常规则弹通知；正在检查时返回 409 `BUSY`，未启用 404，失败 502。

## 测试

`cargo test git_watch`：10 个测试通过（1 个 `#[ignore]` 手动测试）。除纯逻辑外，集成测试在临时目录用真实 `git`
造“远端”仓库，覆盖：首次只建基线、其它作者的提交不通知、跨分支同一提交去重并标注分支、重启后用磁盘基线延续、
邮箱匹配、换仓库地址只重置基线、不可达远端的错误文本、失败/恢复通知节流、环境变量校验、气泡长度限制。

手动测试 `manual_check_real_intranet_repo_on_this_machine`（`cargo test git_watch -- --ignored --nocapture`）
在本机对真实内网仓库跑一次 `run_check()` 并打印结果。

本次在 Linux 环境完成：`cargo check --tests --target x86_64-pc-windows-gnu` 与
`cargo build --release --target x86_64-pc-windows-gnu` 均通过（mingw 交叉编译出 `hello_cargo.exe`，0 条新告警）；
模块测试在 Linux 上以独立 crate 引入 `src/git_watch.rs` 全部通过；并以 1 分钟间隔实跑了定时循环与两个 HTTP 接口。
`rustfmt --check` 对本次改动文件通过（`src/main.rs` 会连带报出 `src/file_monitor.rs` 的既有差异，见 AGENTS.md）。

## 注意

- 运行机需要 Git for Windows（`git` 在 PATH 中）。仓库若要登录，优先让 Git 凭据管理器已存有该主机的凭据
  （在本机手动 `git clone` 过一次即可），否则设置 `GIT_WATCH_USERNAME` / `GIT_WATCH_PASSWORD`。
- Release 以管理员身份运行，`%LOCALAPPDATA%` 仍是当前用户的目录；若 UAC 时切换到了别的管理员账号，镜像会落在那个账号下。
- `Cargo.toml` 给 tokio 增加了 `process`、`sync` 两个 feature（子进程超时/终止、异步互斥锁）；`Cargo.lock` 因此新增
  `signal-hook-registry`（仅 unix 目标使用）。
