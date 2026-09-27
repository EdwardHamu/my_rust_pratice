use warp::Filter;
// use std::sync::{Arc, Mutex};
use crate::controllers::me::{self};
use crate::git_watch;
// count:Arc<Mutex<u32>>
//web路由定义
pub fn get_router() -> impl warp::Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone
{
    let route_of_get = warp::get().and(
        warp::path!("hello" / String)
            .and_then(move |s: String| me::potplay(s))
            .or(warp::path!("charge").and_then(me::charge))
            .or(warp::path!("brave").and_then(me::start_brave))
            .or(warp::path!("playList").and_then(me::play_list))
            .or(warp::path!("toast")
                .and(warp::query::<me::ToastQuery>())
                .and_then(me::toast_notify))
            .or(warp::path!("git_watch" / "status").and_then(git_watch::http_status))
            .or(warp::path!("git_watch" / "check").and_then(git_watch::http_check)),
        // .or(warp::path!("playText" / String).and_then(move |s: String| me::play_text(s))),
    );

    let route_of_post = warp::post()
        .and(warp::path!("test").and_then(me::test))
        .or(warp::path!("playText")
            .and(warp::body::json())
            .and_then(me::play_text))
        .or(warp::path!("getTextAudio")
            .and(warp::body::json())
            .and_then(me::get_text_audio))
        .or(warp::path!("playTextAbogen")
            .and(warp::body::json())
            .and_then(me::play_text_abogen))
        .or(warp::path!("save_cdxpp_token")
            .and(warp::body::content_length_limit(64 * 1024))
            .and(warp::body::bytes())
            .and_then(me::save_cdxpp_token));

    let cors = warp::cors()
        // .allow_origin("https://meamoe.top") // 仅允许特定域名
        // .allow_origin("http://localhost:8820") // 也可以允许多个
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "OPTIONS"]) // 明确加上 OPTIONS
        .allow_headers(vec![
            "content-type",
            "authorization",
            "accept",
            "origin",
            "X-Requested-With",
        ])
        .allow_credentials(true)
        .max_age(3600); // 缓存预检请求（Options）的时间，单位为秒

    route_of_get.or(route_of_post).with(cors)
}
