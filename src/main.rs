#![allow(unused_variables, dead_code)]

use runas::Command;
// use std::io;
// use utf8_slice::slice;
use tokio::time::{sleep, Duration};
use tokio::{self};

// use std::thread::sleep;
// use std::time::Duration;

mod controllers;
mod enums;
mod file_monitor;
mod get_pot_player;
mod mcgs_control;
mod menu;
mod ocr;
mod router;
mod ui;
mod uitl;

#[tokio::main]
async fn main() {
    unsafe {
        enums::set_user();
    }

    if !cfg!(debug_assertions) {
        if enums::USER.get().unwrap().as_str() == enums::HW_USER {
            let handle = tokio::spawn(async {
                loop {
                    get_pot_player::save_pot_play_info().await;
                    sleep(Duration::from_secs(5 * 60)).await;
                }
            });
        }

        if !is_elevated::is_elevated() {
            println!("不是管理员，尝试以管理员权限重新运行...");

            // 重启自己，触发 UAC
            Command::new(std::env::current_exe().unwrap())
                .gui(true) // 避免命令行窗口弹出
                .status()
                .expect("无法重新启动进程");

            return; // 当前进程退出
        }
        println!("已获得管理员权限！");
    }

    // get_pot_player::save_pot_play_info().await;
    // get_pot_player::get_player_list_file().await;
    // let (bv, play_time,now_title)  = get_pot_player::get_pot_first_info();
    // println!("🪵 [main.rs:30]~ token ~ \x1b[0;32mnow_title\x1b[0m = {}", now_title);
    // let split_str = now_title.split(" ").collect::<Vec<_>>();
    // let time_str_list = &split_str[1..3];
    // let now_play_ts = uitl::transform_wuyang_time_ts(time_str_list); //日期

    // get_pot_player::get_player_list_file().await;
    //获取路由k
    // // 定义一个简单的 GET 路由 release go
    // let hello = warp::path!("hello" / String)
    //     .map(|name| format!("Hello, {}!", name));

    // // 组合路由
    // let route = hello;
    let handle = tokio::spawn(async {
        let port: u16 = if cfg!(debug_assertions) { 7655 } else { 7654 };
        let route = router::get_router();
        warp::serve(route).run(([0, 0, 0, 0], port)).await;
    });

    let _notification_listener = tokio::spawn(async {
        controllers::me::start_notification_socket_listener().await;
    });

    // if enums::USER.get().unwrap().as_str() == enums::HW_USER {
    //     controllers::me::check_network();
    // }

    let handle2 = tokio::spawn(async {
        loop {
            controllers::me::play_bingbong();
            sleep(Duration::from_secs(30)).await;
        }
    });

    // 监控 config files 修改并自动备份
    let _handle3 = tokio::spawn(async {
        file_monitor::start_monitoring().await;
    });

    // 每天清理 D:\MCode\Rust 第一层图片，只保留最新10个
    let _handle4 = tokio::spawn(async {
        file_monitor::start_daily_image_cleanup().await;
    });

    // 每天早上8点检查最新截图，OCR识别失败字样并弹窗提醒
    let _handle5 = tokio::spawn(async {
        file_monitor::start_daily_screenshot_ocr_check().await;
    });

    // 每天午夜0点检查微信聊天采集(菜单7)是否仍在运行，若在运行则自动停止
    let _handle6 = tokio::spawn(async {
        file_monitor::start_midnight_wechat_capture_stop().await;
    });

    // 每天早上8点调用 freemodel_usage.py 获取7天已用额度，记录到日志文件
    let _handle7 = tokio::spawn(async {
        file_monitor::start_daily_freemodel_usage_log().await;
    });

    // let path = enums::get_list_local_list();
    // let line_first = uitl::read_lines(path, 0, 37).unwrap();
    // println!("🪵 [main.rs:60]~ token ~ \x1b[0;32mline_first\x1b[0m = {}", line_first);

    menu::start::init_menu();
}

// type File = String;

// fn open(f: &mut File) -> bool {
//     true
// }
// fn close(f: &mut File) -> bool {
//     true
// }

// #[allow(dead_code)]
// fn read(f: &mut File, save_to: &mut Vec<u8>) -> ! {
//     unimplemented!()
// }

// fn main() {
//     let mut f1 = File::from("f1.txt");
//     open(&mut f1);
//     read(&mut f1, &mut vec![]);
//     close(&mut f1);
// }
