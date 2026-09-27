# CLAUDE.md

The role of this file is to describe common mistakes andconfusion points that agents might encounter as they work inthis project. If you ever encounter something in the projectthat surprises you, please alert the developer working with youand indicate that this is the case in the AgentMD file to helpprevent future agents from having the same issue.

### Windows-specific dependencies

- `winapi` / `user32-sys`: Win32 SendMessage, FindWindowEx, GetWindowText for PotPlayer IPC
- `enigo` / `rsautogui`: Mouse control (Brave automation)
- `runas` / `is_elevated`: Admin elevation
- `rodio`: Audio playback (MP3 alarm + TTS response audio)
- `edge-tts-rust`: Microsoft Edge TTS for text-to-speech
- `screenshots`: Screen capture
- PowerShell commands: WiFi adapter reset, popup dialogs, process management

## Debug vs Release

- **Debug**: skips UAC elevation, skips PotPlayer monitoring loop, uses port 7655
- **Release**: auto-elevates to admin, enables PotPlayer 5-min monitoring loop, uses port 7654

### MP3 resource

`src/ui/bingbongbangbong.MP3` is the alarm sound played at 21:30. It gets copied to the output directory by `build.rs`. The binary finds it at runtime via `std::env::current_exe()` sibling path.

## Git watch (`git_watch.rs`)

- Every 4 h fetches a local bare mirror of the intranet repo `SPC.M` and toasts when author `Cloud` pushed new commits. New = "not reachable from any ref tip before the fetch" (`git log <new tip> --not <old tips>`), never date-based; the mirror on disk *is* the baseline, so restarts don't re-notify and the first run only establishes a baseline.
- Shells out to `git` on PATH via `tokio::process` (needs tokio features `process` + `sync`). Runs with `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never` so a missing credential fails fast instead of popping a Git Credential Manager window; a 15-minute timeout kills a stuck fetch.
- Mirror lives in `%LOCALAPPDATA%\hello_cargo\git_watch\SPC.M.git`. Optional overrides: `GIT_WATCH_ENABLED/REPO_URL/AUTHOR/USERNAME/PASSWORD/INTERVAL_MINUTES/DIR`. Credentials go to git only through the child's environment (inline credential helper) — never into argv, logs or `/git_watch/status`; run text through `redact()` before printing.
- Toast body is capped at 255 UTF-16 units and cannot contain newlines (`escape_powershell_single_quoted` flattens them), so the balloon lists at most 3 commits and the console prints the full list. Failure toasts are throttled to one per failure streak plus one recovery toast.
- Manual trigger: menu `10`, `GET /git_watch/check`; state: `GET /git_watch/status`. Tests build real `git` repos under `%TEMP%`; the fake remote path must not coincide with the mirror path `<dir>/<name>.git`, or the mirror fetches from itself and "succeeds". Details: `docs/git-watch-spc-monitor.md`.

## MCGS IPC Details (`mcgs_control.rs`)

- Finds MCGS「下载配置」dialog (belongs to `McgsSetPro.exe`, a different process from the
  simulator window `mcgs_app.exe`) by top-level window title match, then finds its
  「停止运行」/「启动运行」child buttons by title match, and clicks via
  `SendMessageA(hwnd, BM_CLICK, 0, 0)`.
- **UIPI gotcha**: `McgsSetPro.exe` normally runs elevated (admin). If `hello_cargo.exe` is
  running in **debug mode** (non-elevated, per this file's Debug/Release section), `BM_CLICK`
  is silently swallowed by Windows' User Interface Privilege Isolation — `SendMessageA` returns
  without error, the program prints "已点击", but the button never actually fires and MCGS's
  「返回信息」 log shows no new entry. This is NOT a bug in window/button lookup — verify by
  checking whether the target process can be opened with `OpenProcess(PROCESS_QUERY_INFORMATION)`
  from a non-elevated process (access denied ⇒ target is elevated ⇒ UIPI blocks the click).
  Only a **release build** (which auto-elevates via UAC) can actually click MCGS's buttons.

## PotPlayer IPC Details

- Finds PotPlayer window by class name `PotPlayer64` (via `FindWindowExA`)
- Sends `WM_USER + 1024` (`REQ_TYPE`) with `WPARAM = 20484` (`POT_GET_PROGRESS_TIME`) via `SendMessageA` to get playback progress in milliseconds
- Reads window title to get the playing file name
- Parses PotPlayer playlist files (`.dpl` format, `@`-delimited) with custom byte-by-byte reading utilities
- Uploads info to `https://meamoe.top/koa/newCen/free/savePotInfo`
