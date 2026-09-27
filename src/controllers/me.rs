// use std::sync::{Arc, Mutex};
use crate::controllers::abogen_tts::{self, AbogenSpeakOptions};
use crate::get_pot_player;
use crate::uitl;
use chrono::Local;
use chrono::Timelike;
use edge_tts_rust::Boundary;
use edge_tts_rust::EdgeTtsClient;
use edge_tts_rust::SpeakOptions;
use enigo::*;
use futures::FutureExt;
use rsautogui::mouse;
use rust_socketio::{asynchronous::ClientBuilder, Payload};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tokio::task;
use tokio::time::{sleep, Duration};
use warp::Rejection;
use winapi::shared::minwindef::{BOOL, LPARAM};
use winapi::shared::windef::{HWND, RECT};
use winapi::um::winuser::{
    EnumWindows, GetClassNameW, GetWindowRect, IsIconic, IsWindow, IsWindowVisible, PostMessageW,
    SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW, WM_CLOSE,
};

use rodio::Decoder;
use std::fs::{self, File};
use std::io::Cursor;
use winapi::um::winuser::{GetAsyncKeyState, VK_MENU};

const VK_B: i32 = 0x42;
const MAHJONG_SOUL_URL: &str = "https://game.mahjongsoul.com";
const BRAVE_LOAD_DELAY: Duration = Duration::from_secs(270);
const WINDOW_DISCOVERY_ATTEMPTS: usize = 20;
const WINDOW_DISCOVERY_INTERVAL: Duration = Duration::from_millis(500);
const CLICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const REPEATED_CLICK_COUNT: usize = 10;
const FIRST_CLICK_OFFSET: (i32, i32) = (200, 550);
const CHROMIUM_WINDOW_CLASS: &str = "Chrome_WidgetWin_1";
const NOTIFICATION_SOCKET_URL: &str = "http://meamoe.top:3100/";
const DEFAULT_NOTIFICATION_TITLE: &str = "hello_cargo";
const CHARGING_COMPLETE_KEYWORD: &str = "充电完成";
const CHARGING_COMPLETE_NOTIFICATION_DELAY: Duration = Duration::from_secs(5 * 60);
const CDXPP_TOKEN_PATH: &str = r"E:\chatE\cdxpp.token";

static BRAVE_AUTOMATION_RUNNING: AtomicBool = AtomicBool::new(false);

fn brave_log(message: impl AsRef<str>) {
    println!(
        "[{}] [start_brave] {}",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        message.as_ref()
    );
}

fn brave_error(message: impl AsRef<str>) {
    eprintln!(
        "[{}] [start_brave] ERROR: {}",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        message.as_ref()
    );
}

fn format_window_handles(windows: &HashSet<usize>) -> String {
    let mut handles: Vec<_> = windows.iter().copied().collect();
    handles.sort_unstable();
    format!("{:X?}", handles)
}

pub async fn charge() -> Result<String, Rejection> {
    let now = Local::now();
    println!("当前系统时间: {:?}", now);
    tokio::spawn(async {
        let script = r#"$ConfirmPreference = 'None';$ws = New-Object -ComObject WScript.Shell;$wsr = $ws.popup("The software has installed successfully, please restart your computer to take effect. Press OK to restart later.",0,"Reboot Attention!",0 + 64)"#;
        sleep(Duration::from_secs(270)).await;
        let output = Command::new("powershell.exe")
            .args(&["-Command", &script])
            .output()
            .expect("执行失败");
    });

    Ok(format!("charge up"))
}

pub async fn start_brave() -> Result<String, Rejection> {
    brave_log(format!("收到启动请求，目标地址: {MAHJONG_SOUL_URL}"));

    if BRAVE_AUTOMATION_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        brave_log("拒绝重复启动：自动化任务当前正在运行");
        return Ok("Brave 自动化任务已在运行".to_string());
    }

    brave_log("已取得任务运行锁，准备创建后台任务");
    tokio::spawn(async {
        let _running_guard = BraveAutomationRunningGuard;
        brave_log("后台任务开始执行");
        match run_brave_automation().await {
            Ok(()) => brave_log("自动化流程执行完成"),
            Err(error) => brave_error(format!("自动化流程失败: {error}")),
        }
    });
    brave_log("后台任务创建成功，接口即将返回");

    Ok("Brave 自动化任务已启动".to_string())
}

struct BraveAutomationRunningGuard;

impl Drop for BraveAutomationRunningGuard {
    fn drop(&mut self) {
        BRAVE_AUTOMATION_RUNNING.store(false, Ordering::Release);
    }
}

struct BraveProcessGuard {
    pid: Option<u32>,
}

impl BraveProcessGuard {
    fn new(pid: u32) -> Self {
        Self { pid: Some(pid) }
    }

    fn terminate(&mut self) -> Result<(), String> {
        let Some(pid) = self.pid else {
            return Ok(());
        };

        let result = terminate_brave_process(pid);
        if result.is_ok() {
            self.pid = None;
        }
        result
    }
}

impl Drop for BraveProcessGuard {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            brave_error(format!("清理 Brave 进程失败: {error}"));
        }
    }
}

async fn run_brave_automation() -> Result<(), String> {
    let existing_windows = chromium_windows()?;

    let brave_process = task::spawn_blocking(open_brave_window)
        .await
        .map_err(|error| format!("启动 Brave 的任务异常: {error}"))??;

    let brave_window = wait_for_new_chromium_window(&existing_windows).await?;
    sleep(BRAVE_LOAD_DELAY).await;

    let interaction_result = task::spawn_blocking(move || interact_with_brave(brave_window))
        .await
        .map_err(|error| format!("鼠标与截图任务异常: {error}"))
        .and_then(|result| result);

    sleep(Duration::from_secs(5)).await;

    let close_result =
        task::spawn_blocking(move || close_brave_window(brave_window, brave_process))
            .await
            .map_err(|error| format!("关闭 Brave 窗口的任务异常: {error}"))
            .and_then(|result| result);

    interaction_result?;
    close_result
}

fn open_brave_window() -> Result<BraveProcessGuard, String> {
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         $process = Start-Process -FilePath 'brave.exe' \
         -ArgumentList @('--new-window', '{MAHJONG_SOUL_URL}') -PassThru; \
         $process.Id"
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|error| format!("无法启动 PowerShell: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("PowerShell 退出状态: {}", output.status)
        } else {
            stderr
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid = stdout
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("无法解析 Brave 进程 PID {:?}: {error}", stdout.trim()))?;
    brave_log(format!("已启动 Brave 进程，PID: {pid}"));

    Ok(BraveProcessGuard::new(pid))
}

async fn wait_for_new_chromium_window(existing: &HashSet<usize>) -> Result<usize, String> {
    for _ in 0..WINDOW_DISCOVERY_ATTEMPTS {
        let current = chromium_windows()?;
        if let Some(window) = select_new_window(existing, &current) {
            return Ok(window);
        }
        sleep(WINDOW_DISCOVERY_INTERVAL).await;
    }

    Err("未找到新创建的 Brave 窗口".to_string())
}

fn select_new_window(existing: &HashSet<usize>, current: &HashSet<usize>) -> Option<usize> {
    current.difference(existing).copied().next()
}

fn chromium_windows() -> Result<HashSet<usize>, String> {
    unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }

        let mut class_name = [0_u16; 64];
        let length = GetClassNameW(
            hwnd,
            class_name.as_mut_ptr(),
            class_name.len().try_into().unwrap_or(i32::MAX),
        );
        if length > 0
            && String::from_utf16_lossy(&class_name[..length as usize]) == CHROMIUM_WINDOW_CLASS
        {
            let windows = &mut *(lparam as *mut HashSet<usize>);
            windows.insert(hwnd as usize);
        }

        1
    }

    let mut windows = HashSet::new();
    let succeeded = unsafe {
        EnumWindows(
            Some(collect_window),
            &mut windows as *mut HashSet<usize> as LPARAM,
        )
    };

    if succeeded == 0 {
        Err(format!(
            "枚举浏览器窗口失败: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(windows)
    }
}

fn interact_with_brave(window: usize) -> Result<(), String> {
    let hwnd = window as HWND;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    unsafe {
        if IsWindow(hwnd) == 0 {
            return Err("Brave 窗口已经不存在".to_string());
        }

        // 如果窗口最小化，先恢复
        if IsIconic(hwnd) != 0 {
            brave_log("窗口已最小化，正在恢复");
            ShowWindow(hwnd, SW_RESTORE);
            thread::sleep(std::time::Duration::from_millis(500));
        } else {
            // 确保窗口可见
            ShowWindow(hwnd, SW_SHOW);
            thread::sleep(std::time::Duration::from_millis(200));
        }

        // 尝试激活窗口
        if SetForegroundWindow(hwnd) == 0 {
            brave_log("SetForegroundWindow 返回 0，尝试备用方案");

            // 备用方案：使用 Alt 键绕过前台锁定限制
            // 这是 Windows 推荐的方法，模拟用户按下 Alt 键可以临时解除前台锁定
            use winapi::um::winuser::{keybd_event, KEYEVENTF_KEYUP};
            const VK_MENU: u8 = 0x12; // Alt 键

            keybd_event(VK_MENU, 0, 0, 0); // 按下 Alt
            thread::sleep(std::time::Duration::from_millis(50));
            SetForegroundWindow(hwnd); // 再次尝试激活
            thread::sleep(std::time::Duration::from_millis(50));
            keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0); // 释放 Alt

            thread::sleep(std::time::Duration::from_millis(200));

            // 验证窗口是否已成为前台窗口
            use winapi::um::winuser::GetForegroundWindow;
            if GetForegroundWindow() != hwnd {
                return Err(
                    "无法激活 Brave 窗口（已尝试备用方案），为避免误点击已终止任务".to_string(),
                );
            }
            brave_log("备用方案成功激活窗口");
        } else {
            brave_log("SetForegroundWindow 成功");
        }

        if GetWindowRect(hwnd, &mut rect) == 0 {
            return Err(format!(
                "读取 Brave 窗口位置失败: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    thread::sleep(CLICK_INTERVAL);
    click_repeatedly(
        mouse_coordinates(window_point(&rect, FIRST_CLICK_OFFSET))?,
        REPEATED_CLICK_COUNT,
    );
    uitl::screen_shot();

    Ok(())
}

fn window_point(rect: &RECT, offset: (i32, i32)) -> (i32, i32) {
    (rect.left + offset.0, rect.top + offset.1)
}

fn mouse_coordinates(point: (i32, i32)) -> Result<(u16, u16), String> {
    let x = u16::try_from(point.0).map_err(|_| format!("鼠标横坐标超出支持范围: {}", point.0))?;
    let y = u16::try_from(point.1).map_err(|_| format!("鼠标纵坐标超出支持范围: {}", point.1))?;
    Ok((x, y))
}

fn click_repeatedly(point: (u16, u16), count: usize) {
    mouse::move_to(point.0, point.1);
    for click_index in 0..count {
        mouse::click(mouse::Button::Left);
        if click_index + 1 < count {
            thread::sleep(CLICK_INTERVAL);
        }
    }
}

fn close_brave_window(window: usize, mut process: BraveProcessGuard) -> Result<(), String> {
    let window_result = close_window(window);
    thread::sleep(std::time::Duration::from_millis(500));
    let process_result = process.terminate();

    match (window_result, process_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(window_error), Ok(())) => Err(window_error),
        (Ok(()), Err(process_error)) => Err(format!("关闭 Brave 进程失败: {process_error}")),
        (Err(window_error), Err(process_error)) => Err(format!(
            "关闭 Brave 窗口失败: {window_error}; 关闭 Brave 进程失败: {process_error}"
        )),
    }
}

fn close_window(window: usize) -> Result<(), String> {
    let hwnd = window as HWND;
    unsafe {
        if IsWindow(hwnd) == 0 {
            return Ok(());
        }
        if PostMessageW(hwnd, WM_CLOSE, 0, 0) == 0 {
            return Err(format!(
                "关闭 Brave 窗口失败: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    Ok(())
}

fn terminate_brave_process(pid: u32) -> Result<(), String> {
    let script = format!(
        "$process = Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
         if ($null -ne $process -and $process.ProcessName -ieq 'brave') {{ \
             & taskkill.exe /PID {pid} /T /F | Out-Null; \
             if ($LASTEXITCODE -ne 0) {{ throw 'taskkill failed' }} \
         }}"
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|error| format!("无法启动 Brave 清理命令: {error}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("Brave 清理命令退出状态: {}", output.status)
        } else {
            stderr
        })
    }
}

pub async fn play_list() -> Result<String, Rejection> {
    let res = get_pot_player::get_player_list_file().await;
    match res {
        Err(e) => {
            println!("文件读取失败: {}", e);
            return Ok("文件读取失败".to_string());
        }
        Ok(_) => Ok("播放列表更新成功".to_string()),
    }
}

pub async fn potplay(s: String) -> Result<String, Rejection> {
    Ok(s)
}

#[derive(Deserialize)]
pub struct ToastQuery {
    text: String,
}

pub async fn toast_notify(query: ToastQuery) -> Result<String, Rejection> {
    let text = query.text;
    let code = longest_verification_code(&text);

    let res = task::spawn_blocking({
        let text = text.clone();

        move || show_notification_and_copy_code(DEFAULT_NOTIFICATION_TITLE, &text).map(|_| ())
    })
    .await;

    match res {
        Ok(Ok(())) => match code {
            Some(code) => Ok(format!("toast 成功，验证码已复制: {}", code)),
            None if text.contains("验证码") => Ok("toast 成功，未找到验证码数字".to_string()),
            None => Ok("toast 成功".to_string()),
        },
        Ok(Err(e)) => Ok(format!("toast 失败: {}", e)),
        Err(e) => Ok(format!("toast 失败: {}", e)),
    }
}

pub async fn start_notification_socket_listener() {
    loop {
        let connection = ClientBuilder::new(NOTIFICATION_SOCKET_URL)
            .namespace("/")
            .reconnect(true)
            .reconnect_on_disconnect(true)
            .reconnect_delay(1_000, 5_000)
            .on("notification", |payload: Payload, _| {
                async move {
                    let Some(notification) = notification_from_socket_payload(payload) else {
                        eprintln!("收到不支持的二进制 notification 广播，已忽略");
                        return;
                    };

                    let notification_task = tokio::spawn(async move {
                        if should_delay_notification(&notification) {
                            println!("收到充电完成通知，5 分钟后显示");
                            sleep(CHARGING_COMPLETE_NOTIFICATION_DELAY).await;
                        }

                        let toast_task = task::spawn_blocking(move || {
                            if let Err(error) = show_notification_and_copy_code(
                                &notification.title,
                                &notification.body,
                            ) {
                                eprintln!("显示 notification 通知失败: {}", error);
                            }
                        });
                        drop(toast_task);
                    });
                    drop(notification_task);
                }
                .boxed()
            })
            .on("error", |payload: Payload, _| {
                async move {
                    eprintln!("notification Socket.IO 错误: {:?}", payload);
                }
                .boxed()
            })
            .connect()
            .await;

        match connection {
            Ok(_socket) => {
                println!("notification Socket.IO 已连接: {}", NOTIFICATION_SOCKET_URL);
                std::future::pending::<()>().await;
            }
            Err(error) => {
                eprintln!("notification Socket.IO 连接失败: {}；5 秒后重试", error);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DesktopNotification {
    title: String,
    body: String,
}

impl DesktopNotification {
    fn text_for_code_detection(&self) -> String {
        format!("{}\n{}", self.title, self.body)
    }
}

fn should_delay_notification(notification: &DesktopNotification) -> bool {
    notification.body.contains(CHARGING_COMPLETE_KEYWORD)
}

fn notification_from_socket_payload(payload: Payload) -> Option<DesktopNotification> {
    match payload {
        Payload::Text(values) => {
            let value = match values.as_slice() {
                [value] => value.clone(),
                _ => Value::Array(values),
            };
            Some(notification_from_json_value(value))
        }
        #[allow(deprecated)]
        Payload::String(text) => Some(notification_from_json_value(
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )),
        Payload::Binary(_) => None,
    }
}

fn notification_from_json_value(value: Value) -> DesktopNotification {
    match &value {
        Value::String(body) => DesktopNotification {
            title: DEFAULT_NOTIFICATION_TITLE.to_string(),
            body: body.clone(),
        },
        Value::Object(object) => {
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(DEFAULT_NOTIFICATION_TITLE)
                .to_string();
            let body = object
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());

            DesktopNotification { title, body }
        }
        _ => DesktopNotification {
            title: DEFAULT_NOTIFICATION_TITLE.to_string(),
            body: value.to_string(),
        },
    }
}

pub async fn test() -> Result<String, Rejection> {
    Ok(format!("********"))
}
pub async fn test2(s: String) -> Result<String, Rejection> {
    Ok(format!("********"))
}

pub async fn save_cdxpp_token(
    body: warp::hyper::body::Bytes,
) -> Result<impl warp::Reply, Rejection> {
    let token = match parse_cdxpp_token(&body) {
        Ok(token) => token,
        Err(error) => {
            return Ok(warp::reply::with_status(
                error,
                warp::http::StatusCode::BAD_REQUEST,
            ));
        }
    };

    let result = task::spawn_blocking(move || {
        let path = std::path::Path::new(CDXPP_TOKEN_PATH);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, token)
    })
    .await;

    let (message, status) = match result {
        Ok(Ok(())) => (
            "cdxpp token 保存成功".to_string(),
            warp::http::StatusCode::OK,
        ),
        Ok(Err(error)) => (
            format!("cdxpp token 保存失败: {error}"),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ),
        Err(error) => (
            format!("cdxpp token 保存任务失败: {error}"),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ),
    };

    Ok(warp::reply::with_status(message, status))
}

fn parse_cdxpp_token(body: &[u8]) -> Result<String, String> {
    let body = String::from_utf8(body.to_vec())
        .map_err(|_| "cdxpp token 必须是 UTF-8 字符串".to_string())?;

    if let Ok(value) = serde_json::from_str::<Value>(&body) {
        return match value {
            Value::String(token) => Ok(token),
            Value::Object(object) => object
                .get("str")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "JSON 请求体必须包含字符串字段 str".to_string()),
            _ => Err("请求体必须是字符串或包含 str 字段的 JSON 对象".to_string()),
        };
    }

    Ok(body)
}

pub fn show_mouse_xy() -> () {
    let enigo = Enigo::new(&Settings::default()).unwrap();
    let (x, y) = enigo.location().unwrap();
    println!("鼠标坐标: x={}, y={}", x, y);
}

pub fn check_network() -> () {
    // check_network 功能已禁用
    /*
    async fn check_one() -> () {
        let res = uitl::ping("www.baidu.com").unwrap();
        let str2 = String::from_utf8_lossy(&res.stdout).clone();
        let str = str2.trim().to_string();
        if str.len() > 200 {
            // 网络连接正常，无需处理。
        } else {
            relink_wifi().await;
        }
    }
    async fn relink_wifi() -> () {
        // let mut enigo = Enigo::new(&Settings::default()).unwrap();
        // let _ =enigo.key(Key::LWin, Direction::Click);
        // sleep(Duration::from_secs(1)).await;
        // mouse::move_to(1800, 1055);
        // mouse::click(mouse::Button::Left);

        // sleep(Duration::from_secs(1)).await;
        // mouse::move_to(1637, 730);
        // mouse::click(mouse::Button::Left);

        // sleep(Duration::from_secs(1)).await;
        // mouse::move_to(1880, 640);
        // mouse::click(mouse::Button::Left);
        // sleep(Duration::from_secs(5)).await;
        // mouse::click(mouse::Button::Left);

        // return Ok("".to_string());

        tokio::spawn(async {
            //查看instance id方法
            // get-PnpDevice | ? {$_.class -eq "NET"} | sort friendlyname | select friendlyname,instanceid
            let script = r#"Disable-PnpDevice -InstanceId  "PCI\VEN_10EC&DEV_8812&SUBSYS_881210EC&REV_01\4&33186293&0&00E8""#;
            // sleep(Duration::from_secs(270)).await;
            Command::new("powershell.exe")
                .args(&["-Command", &script])
                .output()
                .expect("执行失败");

            sleep(Duration::from_secs(6)).await;

            let script = r#"Enable-PnpDevice -InstanceId  "PCI\VEN_10EC&DEV_8812&SUBSYS_881210EC&REV_01\4&33186293&0&00E8""#;
            // sleep(Duration::from_secs(270)).await;
            Command::new("powershell.exe")
                .args(&["-Command", &script])
                .output()
                .expect("执行失败");
        });
    }
    let handle = tokio::spawn(async {
        loop {
            check_one().await;
            sleep(Duration::from_secs(10 * 60)).await;
        }
    });
    // Ok(format!("********"))
    */
}

pub fn play_bingbong() -> () {
    // 获取当前的本地时间
    let now = Local::now();
    // 获取小时和分钟
    let hour = now.hour();
    let minute = now.minute();
    // println!("当前时间是: {:02}:{:02}", hour, minute);

    // 检查是否是 21:30
    if hour == 21 && minute == 30 {
        println!("到时间了！现在是 21:30。");
        let stream_handle =
            rodio::OutputStreamBuilder::open_default_stream().expect("open default audio stream");
        let sink = rodio::Sink::connect_new(&stream_handle.mixer());

        // 【关键步骤 1】：获取当前可执行文件 (.exe) 的完整路径
        let mut music_path = std::env::current_exe().unwrap();

        // 【关键步骤 2】：去掉文件名，只保留目录路径
        // 例如：从 "C:\Game\release\game.exe" 变成 "C:\Game\release\"
        music_path.pop();

        // 【关键步骤 3】：拼接音频文件名
        // 建议把资源放在一个 assets 文件夹里，更整洁，这里假设就在同级目录
        music_path.push("bingbongbangbong.MP3");
        // Load a sound from a file, using a path relative to Cargo.toml
        let file = File::open(music_path).unwrap();
        // Decode that sound file into a source
        let source = Decoder::try_from(file).unwrap();
        // Play the sound directly on the device
        stream_handle.mixer().add(source);

        // The sound plays in a separate audio thread,
        // so we need to keep the main thread alive while it's playing.
        std::thread::sleep(std::time::Duration::from_secs(5));
    } else {
        // println!("还没到时间，或者已经过了。");
    }
}

#[derive(Deserialize)]
pub struct PlayTextData {
    str: String,
}
pub async fn play_text(dat: PlayTextData) -> Result<String, Rejection> {
    // dat: PlayTextData
    let res = tokio::spawn(async move {
        let client = EdgeTtsClient::new().unwrap();
        let result = client
            .synthesize(
                dat.str.as_str(),
                // "我早已麻痹",
                SpeakOptions {
                    voice: "zh-CN-XiaoxiaoNeural".into(),
                    boundary: Boundary::Sentence,
                    rate: "+20%".into(),
                    volume: "-10%".into(),
                    ..SpeakOptions::default()
                },
            )
            .await
            .unwrap();

        println!("audio bytes: {}", result.audio.len());
        println!("boundaries: {}", result.boundaries.len());
        play_audio_from_vec(result.audio).await;
        // Ok(())
    });

    match res.await {
        Ok(res) => Ok(format!("play_text 成功")),
        Err(e) => {
            println!("play_text 失败: {}", e);
            return Ok(format!("play_text 失败"));
        }
    }
}

// 合成语音后不在服务端播放，直接把音频字节返回给请求方。
pub async fn get_text_audio(dat: PlayTextData) -> Result<impl warp::Reply, Rejection> {
    println!(
        "get_text_audio 收到请求: 字符数={}, 内容={:?}",
        dat.str.chars().count(),
        dat.str
    );

    // 空文本/纯空白会让 Edge TTS 返回 NoAudioReceived，提前返回明确的客户端错误。
    if dat.str.trim().is_empty() {
        let reply = warp::http::Response::builder()
            .status(warp::http::StatusCode::BAD_REQUEST)
            .header("content-type", "text/plain; charset=utf-8")
            .body("get_text_audio 失败: str 为空".to_string().into_bytes())
            .unwrap();
        return Ok(reply);
    }

    let client = EdgeTtsClient::new().unwrap();
    let result = client
        .synthesize(
            dat.str.as_str(),
            SpeakOptions {
                voice: "zh-CN-XiaoxiaoNeural".into(),
                boundary: Boundary::Sentence,
                rate: "+20%".into(),
                volume: "-10%".into(),
                ..SpeakOptions::default()
            },
        )
        .await;

    match result {
        Ok(res) => {
            println!("get_text_audio 音频字节: {}", res.audio.len());
            let reply = warp::http::Response::builder()
                .header("content-type", "audio/mpeg")
                .header("content-length", res.audio.len())
                .body(res.audio)
                .unwrap();
            Ok(reply)
        }
        Err(e) => {
            println!("get_text_audio 失败: {:?}", e);
            let body = format!("get_text_audio 失败: {:?}", e).into_bytes();
            let reply = warp::http::Response::builder()
                .status(warp::http::StatusCode::INTERNAL_SERVER_ERROR)
                .header("content-type", "text/plain; charset=utf-8")
                .body(body)
                .unwrap();
            Ok(reply)
        }
    }
}

pub async fn play_text_abogen(dat: PlayTextData) -> Result<String, Rejection> {
    let res = tokio::spawn(async move {
        abogen_tts::synthesize_and_play(dat.str.as_str(), AbogenSpeakOptions::default()).await
    });

    match res.await {
        Ok(Ok(())) => Ok(format!("play_text_abogen 成功")),
        Ok(Err(e)) => {
            println!("play_text_abogen 失败: {}", e);
            Ok(format!("play_text_abogen 失败: {}", e))
        }
        Err(e) => {
            println!("play_text_abogen 失败: {}", e);
            Ok(format!("play_text_abogen 失败"))
        }
    }
}

async fn play_audio_from_vec(audio_data: Vec<u8>) {
    let res = task::spawn_blocking(move || {
        // 1. 获取默认输出设备的句柄
        // _stream 必须保持存活，否则声音会立即停止
        let stream_handle =
            rodio::OutputStreamBuilder::open_default_stream().expect("open default audio stream");
        let sink = Arc::new(rodio::Sink::connect_new(&stream_handle.mixer()));

        // 3. 将 Vec<u8> 包装在 Cursor 中，因为它需要实现 Read + Seek
        let cursor = Cursor::new(audio_data);

        // 4. 解码音频数据（自动识别 MP3, WAV, Vorbis, Flac 等）
        let source = Decoder::new(cursor).unwrap();

        // 5. 将音频源放入 Sink 播放
        sink.append(source);
        let stop_hotkey_listener = Arc::new(AtomicBool::new(false));
        let hotkey_listener =
            start_audio_hotkey_listener(Arc::clone(&sink), Arc::clone(&stop_hotkey_listener));

        // 6. 阻塞当前线程直到音频播放完毕（否则函数结束释放资源声音就没了）
        sink.sleep_until_end();
        stop_hotkey_listener.store(true, Ordering::SeqCst);
        let _ = hotkey_listener.join();
    });
    res.await.unwrap();
}

fn start_audio_hotkey_listener(sink: Arc<rodio::Sink>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut was_alt_b_down = false;

        while !stop.load(Ordering::SeqCst) {
            if alt_b_pressed_edge(is_key_down(VK_MENU), is_key_down(VK_B), &mut was_alt_b_down) {
                if sink.is_paused() {
                    sink.play();
                    println!("Alt+B pressed: resume audio");
                } else {
                    sink.pause();
                    println!("Alt+B pressed: pause audio");
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    })
}

fn alt_b_pressed_edge(alt_down: bool, b_down: bool, was_down: &mut bool) -> bool {
    let is_down = alt_down && b_down;
    let pressed = is_down && !*was_down;
    *was_down = is_down;
    pressed
}

fn is_key_down(vkey: i32) -> bool {
    unsafe { (GetAsyncKeyState(vkey) as u16 & 0x8000) != 0 }
}

fn longest_verification_code(text: &str) -> Option<String> {
    if !text.contains("验证码") {
        return None;
    }

    let mut best: Option<&str> = None;
    let mut run_start: Option<usize> = None;

    for (idx, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            if run_start.is_none() {
                run_start = Some(idx);
            }
            continue;
        }

        if let Some(start) = run_start.take() {
            best = choose_longer_code(best, &text[start..idx]);
        }
    }

    if let Some(start) = run_start {
        best = choose_longer_code(best, &text[start..]);
    }

    best.map(|code| code.to_string())
}

fn choose_longer_code<'a>(best: Option<&'a str>, candidate: &'a str) -> Option<&'a str> {
    if candidate.len() < 4 {
        return best;
    }

    match best {
        Some(current) if current.len() >= candidate.len() => best,
        _ => Some(candidate),
    }
}

fn show_notification_and_copy_code(title: &str, body: &str) -> Result<Option<String>, String> {
    let code = longest_verification_code(&format!("{}\n{}", title, body));
    show_windows_toast(title, body)?;

    if let Some(code) = &code {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("剪贴板打开失败: {}", e))?;
        clipboard
            .set_text(code)
            .map_err(|e| format!("剪贴板写入失败: {}", e))?;
    }

    Ok(code)
}

pub(crate) fn show_windows_toast(title: &str, body: &str) -> Result<(), String> {
    let script = windows_toast_script(title, body);

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("PowerShell 启动失败: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!("PowerShell 退出码: {}", output.status))
        } else {
            Err(stderr)
        }
    }
}

fn windows_toast_script(title: &str, body: &str) -> String {
    let title = escape_powershell_single_quoted(title);
    let body = escape_powershell_single_quoted(body);

    format!(
        r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$notify = New-Object System.Windows.Forms.NotifyIcon
$notify.Icon = [System.Drawing.SystemIcons]::Information
$notify.BalloonTipIcon = [System.Windows.Forms.ToolTipIcon]::Info
$notify.BalloonTipTitle = '{title}'
$notify.BalloonTipText = '{body}'
$notify.Visible = $true
$notify.ShowBalloonTip(5000)
Start-Sleep -Milliseconds 5500
$notify.Dispose()
"#
    )
}

pub(crate) fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "&apos;").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_edge_triggers_once_while_alt_b_is_held() {
        let mut was_down = false;

        assert!(alt_b_pressed_edge(true, true, &mut was_down));
        assert!(!alt_b_pressed_edge(true, true, &mut was_down));
        assert!(!alt_b_pressed_edge(true, true, &mut was_down));
    }

    #[test]
    fn hotkey_edge_rearms_after_alt_b_is_released() {
        let mut was_down = false;

        assert!(alt_b_pressed_edge(true, true, &mut was_down));
        assert!(!alt_b_pressed_edge(false, false, &mut was_down));
        assert!(alt_b_pressed_edge(true, true, &mut was_down));
    }

    #[test]
    fn new_browser_window_is_selected_from_snapshot_difference() {
        let existing = HashSet::from([10, 20]);
        let current = HashSet::from([10, 20, 30]);

        assert_eq!(select_new_window(&existing, &current), Some(30));
    }

    #[test]
    fn no_browser_window_is_selected_when_snapshot_is_unchanged() {
        let existing = HashSet::from([10, 20]);

        assert_eq!(select_new_window(&existing, &existing), None);
    }

    #[test]
    fn click_offset_is_relative_to_browser_window() {
        let rect = RECT {
            left: 50,
            top: 100,
            right: 1050,
            bottom: 800,
        };

        assert_eq!(window_point(&rect, (200, 550)), (250, 650));
    }

    #[test]
    fn negative_mouse_coordinates_are_rejected() {
        assert!(mouse_coordinates((-1, 550)).is_err());
    }

    #[test]
    fn verification_code_is_none_without_keyword() {
        assert_eq!(longest_verification_code("登录代码 123456"), None);
    }

    #[test]
    fn verification_code_extracts_four_or_more_digits() {
        assert_eq!(
            longest_verification_code("你的验证码是1234，请勿泄露"),
            Some("1234".to_string())
        );
    }

    #[test]
    fn verification_code_chooses_longest_digit_run() {
        assert_eq!(
            longest_verification_code("验证码 1234 订单 987654"),
            Some("987654".to_string())
        );
    }

    #[test]
    fn verification_code_keeps_first_when_lengths_tie() {
        assert_eq!(
            longest_verification_code("验证码 12345 和 67890 都出现"),
            Some("12345".to_string())
        );
    }

    #[test]
    fn verification_code_ignores_short_digit_runs() {
        assert_eq!(longest_verification_code("验证码 12 345"), None);
    }

    #[test]
    fn notification_object_uses_title_and_content() {
        let notification = notification_from_json_value(serde_json::json!({
            "title": "系统通知",
            "content": "你的验证码是 123456",
            "level": "info"
        }));

        assert_eq!(
            notification,
            DesktopNotification {
                title: "系统通知".to_string(),
                body: "你的验证码是 123456".to_string(),
            }
        );
        assert_eq!(
            longest_verification_code(&notification.text_for_code_detection()),
            Some("123456".to_string())
        );
    }

    #[test]
    fn notification_object_falls_back_to_json_for_non_string_content() {
        let value = serde_json::json!({
            "title": "系统通知",
            "content": { "code": 123456 },
            "level": "info"
        });
        let notification = notification_from_json_value(value.clone());

        assert_eq!(notification.title, "系统通知");
        assert_eq!(notification.body, value.to_string());
    }

    #[test]
    fn notification_non_object_uses_default_title_and_json_body() {
        let notification = notification_from_json_value(serde_json::json!(["服务通知", 123]));

        assert_eq!(notification.title, DEFAULT_NOTIFICATION_TITLE);
        assert_eq!(notification.body, "[\"服务通知\",123]");
    }

    #[test]
    fn notification_payload_with_multiple_values_uses_json_array() {
        let notification = notification_from_socket_payload(Payload::Text(vec![
            serde_json::json!("第一条"),
            serde_json::json!("第二条"),
        ]))
        .expect("文本广播应被转换为通知");

        assert_eq!(notification.title, DEFAULT_NOTIFICATION_TITLE);
        assert_eq!(notification.body, "[\"第一条\",\"第二条\"]");
    }

    #[test]
    fn notification_code_can_span_title_and_body() {
        let notification = DesktopNotification {
            title: "验证码".to_string(),
            body: "123456".to_string(),
        };

        assert_eq!(
            longest_verification_code(&notification.text_for_code_detection()),
            Some("123456".to_string())
        );
    }

    #[test]
    fn charging_complete_notification_is_delayed_when_body_matches() {
        let notification = DesktopNotification {
            title: "设备状态".to_string(),
            body: "移动电源充电完成，请拔掉电源".to_string(),
        };

        assert!(should_delay_notification(&notification));
    }

    #[test]
    fn charging_complete_title_alone_does_not_delay_notification() {
        let notification = DesktopNotification {
            title: "充电完成".to_string(),
            body: "设备状态已更新".to_string(),
        };

        assert!(!should_delay_notification(&notification));
    }

    #[test]
    fn toast_script_uses_notify_icon_balloon_tip() {
        let script = windows_toast_script("测试标题", "测试通知");

        assert!(script.contains("System.Windows.Forms"));
        assert!(script.contains("NotifyIcon"));
        assert!(script.contains("ShowBalloonTip"));
        assert!(script.contains("测试标题"));
        assert!(script.contains("测试通知"));
    }

    #[test]
    fn cdxpp_token_parser_accepts_existing_json_request_style() {
        assert_eq!(
            parse_cdxpp_token(br#"{"str":"token-value"}"#),
            Ok("token-value".to_string())
        );
    }

    #[test]
    fn cdxpp_token_parser_accepts_plain_text() {
        assert_eq!(
            parse_cdxpp_token(b"token-value"),
            Ok("token-value".to_string())
        );
    }
}
