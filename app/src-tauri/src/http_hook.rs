//! 本地 `127.0.0.1` 轻量 HTTP 触发接口（零外部 Cargo 依赖）。
//!
//! 供 Claude Code Hook / Git Hook / CI 脚本触发桌宠动作、气泡与语音播报。
//! 默认监听 `127.0.0.1:19820`（可通过环境变量 `DESKPET_HOOK_PORT` 覆盖）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Emitter;

use crate::scheduler::{pick_by_category, pick_for_binding, Rng, SchedulerEvent};
use crate::state::Shared;
use crate::stats::now_ms;

pub const DEFAULT_HOOK_PORT: u16 = 19820;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookTriggerRequest {
    pub clip_id: Option<String>,
    pub binding: Option<String>,
    pub category: Option<String>,
    pub scene: Option<String>,
    pub event: Option<String>,
    #[serde(alias = "hook_event_name")]
    pub hook_event_name: Option<String>,
    pub bubble: Option<String>,
    pub ttl_ms: Option<u64>,
    pub speak: Option<bool>,
    pub pomodoro: Option<String>,
}

impl HookTriggerRequest {
    /// 将 Claude Code Hook / CI 事件名归一化为 `(scene, default_clip, default_bubble)`。
    pub fn resolve_event_defaults(&self) -> (Option<&str>, Option<&str>, Option<&str>) {
        let ev = self
            .event
            .as_deref()
            .or(self.hook_event_name.as_deref())
            .map(str::trim)
            .unwrap_or("");
        match ev {
            "Stop" | "SubagentStop" | "ci_pass" | "git_commit" => (
                Some("done"),
                Some("工作状态-雀跃庆祝"),
                Some("任务已顺利完成，干得漂亮！"),
            ),
            "Notification" | "PermissionRequest" | "claude_approval" => (
                Some("approval"),
                Some("招手过来"),
                Some("收到外部确认请求，快去看看吧～"),
            ),
            "ci_fail" | "error" => (
                Some("error"),
                Some("工作状态-垂头叹气冒汗"),
                Some("检测到异常或构建失败，需要排查一下哦。"),
            ),
            _ => (self.scene.as_deref(), None, None),
        }
    }
}

pub fn spawn_http_hook_server(shared: Shared) {
    let port = std::env::var("DESKPET_HOOK_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_HOOK_PORT);

    std::thread::spawn(move || {
        let addr = format!("127.0.0.1:{port}");
        let Ok(listener) = TcpListener::bind(&addr) else {
            eprintln!("[deskpet] 本地 HTTP Hook 端口 {addr} 绑定失败，已跳过");
            return;
        };
        let _ = listener.set_nonblocking(true);
        eprintln!("[deskpet] 本地 HTTP Hook 已就绪: http://{addr}/trigger");

        loop {
            if shared.is_quitting() {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let s = shared.clone();
                    std::thread::spawn(move || {
                        handle_client(stream, &s);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(80));
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    });
}

fn write_json_response(stream: &mut TcpStream, status: &str, body: &Value) {
    let payload = body.to_string();
    let resp = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n\
         {payload}",
        payload.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn handle_client(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
    let mut buf = [0u8; 16 * 1024];
    let Ok(n) = stream.read(&mut buf) else {
        return;
    };
    if n == 0 {
        return;
    }
    let raw = String::from_utf8_lossy(&buf[..n]);
    let mut lines = raw.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    if method.eq_ignore_ascii_case("OPTIONS") {
        write_json_response(&mut stream, "204 No Content", &json!({}));
        return;
    }

    if method.eq_ignore_ascii_case("GET") && (path == "/health" || path == "/status") {
        let pet = shared.snapshot();
        let ch = shared.character_snapshot();
        let body = json!({
            "ok": true,
            "charId": ch.as_ref().map(|c| c.id.as_str()).unwrap_or(""),
            "charName": ch.as_ref().map(|c| c.name.as_str()).unwrap_or(""),
            "currentClip": shared.current_clip_id().unwrap_or_default(),
            "muted": pet.muted,
            "pomodoro": shared.pomodoro_snapshot(),
        });
        write_json_response(&mut stream, "200 OK", &body);
        return;
    }

    if method.eq_ignore_ascii_case("POST") && (path == "/trigger" || path == "/hook") {
        let body_str = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("").trim();
        let req: HookTriggerRequest = if body_str.is_empty() {
            HookTriggerRequest::default()
        } else {
            match serde_json::from_str(body_str) {
                Ok(v) => v,
                Err(err) => {
                    write_json_response(
                        &mut stream,
                        "400 Bad Request",
                        &json!({ "ok": false, "error": format!("JSON 解析失败: {err}") }),
                    );
                    return;
                }
            }
        };

        if let Some(ref pomo_act) = req.pomodoro {
            let snap = shared.control_pomodoro(pomo_act.trim(), None);
            let _ = shared.inner.app.emit("pet_pomodoro", &snap);
        }

        let (resolved_scene, default_clip, default_bubble) = req.resolve_event_defaults();
        let now = now_ms();
        let mut rng = Rng::new((now as u64) ^ 0x484F_4F4B);
        let ch = shared.character_snapshot();
        let history = shared.history_snapshot();

        let mut played_clip = String::new();
        if let Some(ref character) = ch {
            let chosen = req
                .clip_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && character.clip_map.contains_key(*s))
                .map(str::to_string)
                .or_else(|| {
                    default_clip
                        .filter(|c| character.clip_map.contains_key(*c))
                        .map(str::to_string)
                })
                .or_else(|| {
                    req.binding
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .and_then(|b| pick_for_binding(character, b, &history, now, &mut rng).map(|c| c.id))
                })
                .or_else(|| {
                    req.category
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .and_then(|cat| {
                            pick_by_category(character, &[cat], &history, now, &mut rng).map(|c| c.id)
                        })
                });
            if let Some(cid) = chosen {
                played_clip = cid.clone();
                shared.send(SchedulerEvent::PlayCommand { clip_id: cid });
            }
        }

        let bubble_text = req
            .bubble
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(default_bubble);
        let ttl_ms = req.ttl_ms.unwrap_or(4500).clamp(1000, 15_000);
        let speak = req.speak.unwrap_or(true);

        if let Some(character) = ch.as_ref() {
            if speak && (resolved_scene.is_some() || bubble_text.is_some()) {
                let scene_key = resolved_scene.unwrap_or("done");
                crate::dispatch_daily_scene_voice(
                    shared,
                    &character.id,
                    scene_key,
                    bubble_text,
                    ttl_ms,
                    false,
                );
            } else if let Some(text) = bubble_text {
                shared.emit_bubble(text.to_string(), ttl_ms);
            }
        } else if let Some(text) = bubble_text {
            shared.emit_bubble(text.to_string(), ttl_ms);
        }

        write_json_response(
            &mut stream,
            "200 OK",
            &json!({
                "ok": true,
                "charId": ch.as_ref().map(|c| c.id.as_str()).unwrap_or(""),
                "clipId": played_clip,
                "bubble": bubble_text.unwrap_or(""),
            }),
        );
        return;
    }

    write_json_response(
        &mut stream,
        "404 Not Found",
        &json!({ "ok": false, "error": "仅支持 GET /health 与 POST /trigger" }),
    );
}

#[cfg(test)]
mod tests {
    use super::HookTriggerRequest;

    #[test]
    fn hook_request_maps_claude_code_and_ci_events() {
        let stop_req: HookTriggerRequest =
            serde_json::from_str(r#"{"hook_event_name":"Stop","speak":true}"#).unwrap();
        let (scene, clip, bubble) = stop_req.resolve_event_defaults();
        assert_eq!(scene, Some("done"));
        assert_eq!(clip, Some("工作状态-雀跃庆祝"));
        assert!(bubble.is_some());

        let perm_req: HookTriggerRequest =
            serde_json::from_str(r#"{"hook_event_name":"PermissionRequest"}"#).unwrap();
        assert_eq!(perm_req.resolve_event_defaults().0, Some("approval"));

        let fail_req: HookTriggerRequest =
            serde_json::from_str(r#"{"event":"ci_fail","bubble":"单测挂了"}"#).unwrap();
        assert_eq!(fail_req.resolve_event_defaults().0, Some("error"));
        assert_eq!(fail_req.bubble.as_deref(), Some("单测挂了"));
    }
}
