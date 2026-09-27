use crate::controllers::{self, file};
use crate::get_pot_player;
use crate::mcgs_control;
use std::io;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration as StdDuration;
use tokio::time::{sleep, Duration};
use winapi::um::winuser::{GetAsyncKeyState, VK_MENU};

static MCGS_RESTART_STOP_FLAG: OnceLock<Mutex<Option<Arc<AtomicBool>>>> = OnceLock::new();
static ALT_Q_LISTENER_STARTED: OnceLock<()> = OnceLock::new();
static ALT_Q_ACTION_STACK: OnceLock<Mutex<Vec<AltQAction>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AltQAction {
    WechatCapture,
    McgsRestart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum McgsRestartToggleAction {
    Start,
    StopRequested,
    AlreadyStopping,
}

const VK_Q: i32 = 0x51;
const MCGS_RESTART_INTERVAL_SECS: u64 = 10 * 60;
const MCGS_STOP_CHECK_INTERVAL_MS: u64 = 200;
const MCGS_HOTKEY_POLL_INTERVAL_MS: u64 = 50;

pub fn init_menu() -> () {
    ensure_menu_alt_q_listener();

    loop {
        println!("\n请选择一个选项:");
        println!("1. get_play_list");
        println!("2. start_soft_server");
        println!("3. show mouse xy");
        println!("4. test");
        println!("5. 下载pot NT");
        println!("6. 下载pot ME");
        println!("7. 开始/停止采集微信聊天");
        println!("8. 开始/停止定时重启 MCGS 下位机(停止+启动)");
        println!("9. 手动记录freemodel额度");
        println!("10. 立即检查 SPC.M 有无 Cloud 的新提交");
        println!("11. 退出");

        print!("请输入您的选择: ");
        io::stdout().flush().unwrap(); // 确保提示信息立即显示

        let mut choice = String::new();
        io::stdin().read_line(&mut choice).expect("读取输入失败");

        let choice = choice.trim(); // 移除输入中的换行符和空格

        match choice {
            "1" => {
                tokio::spawn(async {
                    get_pot_player::get_player_list_file().await.unwrap();
                });
                ()
            }
            "2" => {
                tokio::spawn(async {
                    file::axum_init().await;
                });
                ()
                // 在这里添加执行操作 B 的代码
            }
            "3" => {
                println!("等待5秒");
                tokio::spawn(async {
                    tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;
                    controllers::me::show_mouse_xy();
                });
            }
            "4" => {
                tokio::spawn(async {
                    get_pot_player::upload_play_list().await.unwrap();
                });
            }
            "5" => {
                tokio::spawn(async {
                    get_pot_player::down_server_play_list("nt".to_string())
                        .await
                        .unwrap();
                });
                ()
            }
            "6" => {
                tokio::spawn(async {
                    get_pot_player::down_server_play_list("me".to_string())
                        .await
                        .unwrap();
                });
                ()
            }
            "7" => {
                controllers::wechat_capture::toggle_wechat_capture();
            }
            "8" => {
                toggle_mcgs_restart_loop();
            }
            "9" => {
                println!("正在调用 freemodel_usage.py 记录额度，请稍候……");
                tokio::task::spawn_blocking(|| {
                    match crate::file_monitor::run_freemodel_usage_log_once() {
                        Ok(()) => println!("✅ freemodel 额度已手动记录。"),
                        Err(e) => eprintln!("❌ freemodel 额度记录失败: {}", e),
                    }
                });
            }
            "10" => {
                tokio::spawn(async {
                    crate::git_watch::run_manual_check().await;
                });
            }
            "11" => {
                println!("退出程序。");
                break; // 退出循环
            }
            _ => {
                println!("无效的选择，请重新输入。");
            }
        }
    }
}

pub(crate) fn register_alt_q_action(action: AltQAction) {
    let stack = ALT_Q_ACTION_STACK.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = stack.lock().unwrap();
    guard.retain(|registered| *registered != action);
    guard.push(action);
}

pub(crate) fn unregister_alt_q_action(action: AltQAction) {
    let stack = ALT_Q_ACTION_STACK.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = stack.lock().unwrap();
    guard.retain(|registered| *registered != action);
}

pub(crate) fn ensure_menu_alt_q_listener() {
    ALT_Q_LISTENER_STARTED.get_or_init(|| {
        thread::spawn(|| {
            let mut was_alt_q_down = is_key_down(VK_MENU) && is_key_down(VK_Q);

            loop {
                if alt_q_pressed_edge(is_key_down(VK_MENU), is_key_down(VK_Q), &mut was_alt_q_down)
                {
                    handle_menu_alt_q();
                }

                thread::sleep(StdDuration::from_millis(MCGS_HOTKEY_POLL_INTERVAL_MS));
            }
        });
    });
}

fn handle_menu_alt_q() {
    loop {
        let action = next_alt_q_action();

        match action {
            Some(AltQAction::WechatCapture) => {
                if controllers::wechat_capture::request_stop_wechat_capture() {
                    println!("Alt+Q pressed: stop wechat OCR capture");
                    unregister_alt_q_action(AltQAction::WechatCapture);
                    return;
                }

                unregister_alt_q_action(AltQAction::WechatCapture);
            }
            Some(AltQAction::McgsRestart) => {
                if request_stop_mcgs_restart_loop() {
                    println!("Alt+Q pressed: stop MCGS restart loop");
                    unregister_alt_q_action(AltQAction::McgsRestart);
                    return;
                }

                unregister_alt_q_action(AltQAction::McgsRestart);
            }
            None => return,
        }
    }
}

fn next_alt_q_action() -> Option<AltQAction> {
    let stack = ALT_Q_ACTION_STACK.get_or_init(|| Mutex::new(Vec::new()));
    let guard = stack.lock().unwrap();
    guard.last().copied()
}

fn toggle_mcgs_restart_loop() {
    ensure_menu_alt_q_listener();

    let state = MCGS_RESTART_STOP_FLAG.get_or_init(|| Mutex::new(None));
    let mut guard = state.lock().unwrap();

    match toggle_mcgs_restart_state(&mut guard) {
        McgsRestartToggleAction::Start => {
            let stop_flag = Arc::new(AtomicBool::new(false));
            *guard = Some(Arc::clone(&stop_flag));
            register_alt_q_action(AltQAction::McgsRestart);
            drop(guard);

            println!(
                "已启动 MCGS 下位机定时重启：立即执行一次，之后每 10 分钟执行一次。再次选择菜单 8 或按 Alt+Q 停止。"
            );

            tokio::spawn(async move {
                run_mcgs_restart_loop(Arc::clone(&stop_flag)).await;
                if clear_mcgs_restart_state_if_current(&stop_flag) {
                    unregister_alt_q_action(AltQAction::McgsRestart);
                }
            });
        }
        McgsRestartToggleAction::StopRequested => {
            unregister_alt_q_action(AltQAction::McgsRestart);
            println!("已请求停止 MCGS 定时重启任务；若当前正在重启，将在本轮启动运行完成后停止。");
        }
        McgsRestartToggleAction::AlreadyStopping => {
            println!("MCGS 定时重启任务正在停止，请等待当前任务退出。");
        }
    }
}

fn toggle_mcgs_restart_state(state: &mut Option<Arc<AtomicBool>>) -> McgsRestartToggleAction {
    match state.as_ref() {
        Some(stop_flag) if !stop_flag.swap(true, Ordering::SeqCst) => {
            McgsRestartToggleAction::StopRequested
        }
        Some(_) => McgsRestartToggleAction::AlreadyStopping,
        None => McgsRestartToggleAction::Start,
    }
}

async fn run_mcgs_restart_loop(stop_flag: Arc<AtomicBool>) {
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        mcgs_control::restart_lower_computer().await;

        if wait_for_mcgs_restart_interval_or_stop(&stop_flag).await {
            break;
        }
    }

    println!("MCGS 下位机定时重启任务已停止。");
}

async fn wait_for_mcgs_restart_interval_or_stop(stop_flag: &AtomicBool) -> bool {
    let mut elapsed_ms = 0;
    let total_ms = MCGS_RESTART_INTERVAL_SECS * 1000;

    while elapsed_ms < total_ms {
        if stop_flag.load(Ordering::SeqCst) {
            return true;
        }

        sleep(Duration::from_millis(MCGS_STOP_CHECK_INTERVAL_MS)).await;
        elapsed_ms += MCGS_STOP_CHECK_INTERVAL_MS;
    }

    stop_flag.load(Ordering::SeqCst)
}

fn request_stop_mcgs_restart_loop() -> bool {
    let stop_flag = {
        let state = MCGS_RESTART_STOP_FLAG.get_or_init(|| Mutex::new(None));
        let guard = state.lock().unwrap();
        guard.as_ref().cloned()
    };

    request_stop_flag_once(stop_flag)
}

fn request_stop_flag_once(stop_flag: Option<Arc<AtomicBool>>) -> bool {
    match stop_flag {
        Some(flag) => !flag.swap(true, Ordering::SeqCst),
        None => false,
    }
}

fn clear_mcgs_restart_state_if_current(completed_stop_flag: &Arc<AtomicBool>) -> bool {
    let state = MCGS_RESTART_STOP_FLAG.get_or_init(|| Mutex::new(None));
    let mut guard = state.lock().unwrap();

    if is_current_mcgs_restart_flag(guard.as_ref(), completed_stop_flag) {
        *guard = None;
        true
    } else {
        false
    }
}

fn is_current_mcgs_restart_flag(
    current: Option<&Arc<AtomicBool>>,
    completed: &Arc<AtomicBool>,
) -> bool {
    current
        .map(|current_stop_flag| Arc::ptr_eq(current_stop_flag, completed))
        .unwrap_or(false)
}

fn choose_alt_q_action_for_test(stack: &[AltQAction]) -> Option<AltQAction> {
    stack.last().copied()
}

fn alt_q_pressed_edge(alt_down: bool, q_down: bool, was_down: &mut bool) -> bool {
    let is_down = alt_down && q_down;
    let pressed = is_down && !*was_down;
    *was_down = is_down;
    pressed
}

fn is_key_down(vkey: i32) -> bool {
    unsafe { (GetAsyncKeyState(vkey) as u16 & 0x8000) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn alt_q_pressed_edge_only_triggers_on_new_press() {
        let mut was_down = false;

        assert!(!alt_q_pressed_edge(false, false, &mut was_down));
        assert!(!alt_q_pressed_edge(true, false, &mut was_down));
        assert!(!alt_q_pressed_edge(false, true, &mut was_down));

        assert!(alt_q_pressed_edge(true, true, &mut was_down));
        assert!(!alt_q_pressed_edge(true, true, &mut was_down));

        assert!(!alt_q_pressed_edge(false, false, &mut was_down));
        assert!(alt_q_pressed_edge(true, true, &mut was_down));
    }

    #[test]
    fn alt_q_action_stack_uses_last_started_priority() {
        let stack = vec![AltQAction::WechatCapture, AltQAction::McgsRestart];
        assert_eq!(
            choose_alt_q_action_for_test(&stack),
            Some(AltQAction::McgsRestart)
        );

        let stack = vec![AltQAction::McgsRestart, AltQAction::WechatCapture];
        assert_eq!(
            choose_alt_q_action_for_test(&stack),
            Some(AltQAction::WechatCapture)
        );
    }

    #[test]
    fn mcgs_restart_toggle_starts_when_idle() {
        let mut state = None;

        assert_eq!(
            toggle_mcgs_restart_state(&mut state),
            McgsRestartToggleAction::Start
        );
        assert!(state.is_none());
    }

    #[test]
    fn mcgs_restart_toggle_requests_stop_when_running() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut state = Some(Arc::clone(&stop_flag));

        assert_eq!(
            toggle_mcgs_restart_state(&mut state),
            McgsRestartToggleAction::StopRequested
        );
        assert!(stop_flag.load(Ordering::SeqCst));
        assert!(state.is_some());
    }

    #[test]
    fn mcgs_restart_toggle_does_not_restart_while_stopping() {
        let stop_flag = Arc::new(AtomicBool::new(true));
        let mut state = Some(stop_flag);

        assert_eq!(
            toggle_mcgs_restart_state(&mut state),
            McgsRestartToggleAction::AlreadyStopping
        );
        assert!(state.is_some());
    }

    #[test]
    fn request_stop_flag_once_only_reports_first_stop() {
        let flag = Arc::new(AtomicBool::new(false));

        assert!(request_stop_flag_once(Some(Arc::clone(&flag))));
        assert!(!request_stop_flag_once(Some(Arc::clone(&flag))));
        assert!(!request_stop_flag_once(None));
    }

    #[test]
    fn mcgs_restart_cleanup_only_matches_same_task_flag() {
        let old_flag = Arc::new(AtomicBool::new(true));
        let new_flag = Arc::new(AtomicBool::new(false));

        assert!(is_current_mcgs_restart_flag(Some(&old_flag), &old_flag));
        assert!(!is_current_mcgs_restart_flag(Some(&new_flag), &old_flag));
        assert!(!is_current_mcgs_restart_flag(None, &old_flag));
    }
}
