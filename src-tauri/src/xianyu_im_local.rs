use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use reqwest::multipart::{Form, Part};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::xianyu_local::mtop_call;

const WS_URL: &str = "wss://wss-goofish.dingtalk.com/";
const IM_APP_KEY: &str = "444e9908a51d1cb236a27862abc769c9";

/// A request submitted to the account's one persistent LWP connection.
/// Keeping requests on this channel avoids the gateway closing the IM socket
/// when a page opens another WebSocket with the same device id.
pub struct ImRequest {
    pub lwp: String,
    pub body: Value,
    pub response: oneshot::Sender<Result<Value, String>>,
}

pub type ImRequestSender = mpsc::Sender<ImRequest>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContact {
    pub account_id: String,
    pub chat_id: String,
    pub other_user_id: String,
    pub other_user_name: String,
    pub avatar_url: String,
    pub item_id: String,
    pub item_title: String,
    pub item_image_url: String,
    pub order_status: String,
    pub buyer_tag: String,
    pub latest_message: String,
    pub latest_message_time: String,
    pub unread_count: i64,
    #[serde(skip)]
    pub profile_synced_at: String,
}

#[derive(Debug, Clone)]
pub struct CachedChatProfile {
    pub display_name: String,
    pub avatar_url: String,
    pub buyer_tag: String,
    pub profile_synced_at: String,
}

struct FetchedUserInfo {
    avatar_url: String,
    display_name: String,
    fans_tag: String,
    trade_status: String,
    cookie: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub account_id: String,
    pub chat_id: String,
    pub sender_user_id: String,
    pub sender_user_name: String,
    pub direction: String,
    pub content_kind: String,
    pub text: String,
    pub media_url: String,
    pub sent_at: String,
    pub send_status: String,
    /// Official receiver receipt state. `readStatus`: 2 = read, other values = unread.
    /// `unknown` means the server did not return a receipt; `unsupported` means
    /// `msgReadStatusSetting` explicitly disables receipts for this message.
    pub read_status: String,
    pub card_title: String,
    pub card_subtitle: String,
    pub card_price: String,
    pub target_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEmoji {
    pub icon_alias: String,
    pub icon_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadReceipt {
    pub chat_id: String,
    pub message_ids: Vec<String>,
    pub status: i64,
    pub timestamp: String,
}

pub struct ContactPage {
    pub items: Vec<ChatContact>,
    pub next_cursor: Option<i64>,
    pub has_more: bool,
    pub cookie: String,
}

pub struct MessagePage {
    pub items: Vec<ChatMessage>,
    pub next_cursor: Option<i64>,
    pub has_more: bool,
    pub cookie: String,
    pub own_avatar_url: String,
}

fn cookie_value(cookie: &str, names: &[&str]) -> String {
    cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            names
                .contains(&name.trim())
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

fn mid() -> String {
    format!(
        "{}{} 0",
        fastrand::u32(0..1000),
        chrono::Utc::now().timestamp_millis()
    )
}

fn device_id(user_id: &str) -> String {
    // The official web client keeps a device id in its local settings and
    // reuses it across reconnects. A fresh UUID on every retry makes the IM
    // gateway treat one desktop session as a stream of new devices, which in
    // turn causes short-lived server-side resets. Keep a *valid UUID* that is
    // deterministic per local account instead. The gateway accepts a UUID,
    // not a UUID with an account-id suffix.
    let mut bytes = md5::compute(format!("io.sunkit.shayuzhushou:im:{user_id}")).0;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn register_message(token: &str, device_id: &str, request_mid: &str) -> String {
    serde_json::json!({
        "lwp": "/reg",
        "headers": {
            "cache-header": "app-key token ua wv", "app-key": IM_APP_KEY, "token": token,
            "ua": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "dt": "j", "wv": "im:3,au:3,sy:6", "sync": "0,0;0;0;",
            "did": device_id, "mid": request_mid
        }
    }).to_string()
}

fn heartbeat_message(request_mid: &str) -> String {
    serde_json::json!({
        "lwp": "/!",
        "headers": { "mid": request_mid },
    })
    .to_string()
}

fn sync_state_message(request_mid: &str) -> String {
    serde_json::json!({
        "lwp": "/r/SyncStatus/getState",
        "headers": { "mid": request_mid },
        "body": [{ "topic": "sync" }],
    })
    .to_string()
}

fn sync_ack_diff_message(request_mid: &str, state: Value) -> String {
    serde_json::json!({
        "lwp": "/r/SyncStatus/ackDiff",
        "headers": { "mid": request_mid },
        "body": [state],
    })
    .to_string()
}

fn needs_sync_recovery(value: &Value) -> bool {
    matches!(
        value.pointer("/body/syncExtraType/type").and_then(Value::as_i64),
        Some(1 | 2)
    )
}

fn ack_message(value: &Value) -> Option<String> {
    // LWP replies are `code + headers` and are matched to an outstanding
    // request. Only server initiated `lwp + headers` pushes require an ACK.
    // ACKing heartbeat/RPC responses creates a response loop that the official
    // client deliberately avoids.
    if value.get("code").is_some() || value.get("lwp").is_none() {
        return None;
    }
    let headers = value.get("headers")?.as_object()?;
    Some(serde_json::json!({ "code": 200, "headers": headers }).to_string())
}

async fn im_token(cookie: &str, device_id: &str) -> Result<(String, String), String> {
    let (body, cookie) = mtop_call(
        cookie,
        "mtop.taobao.idlemessage.pc.login.token",
        "1.0",
        "originaljson",
        &serde_json::json!({ "appKey": IM_APP_KEY, "deviceId": device_id }),
    )
    .await?;
    let token = body
        .pointer("/data/accessToken")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("闲鱼 IM Token 返回缺少 accessToken")?
        .to_owned();
    Ok((token, cookie))
}

async fn request_on_account_channel(
    cookie: &str,
    sender: Option<&ImRequestSender>,
    lwp: &str,
    body: Value,
) -> Result<(Value, String), String> {
    let Some(sender) = sender else {
        // This fallback is used only before an account's IM singleton has
        // been started (for example, a first-time sync during setup).
        return ws_request(cookie, lwp, body).await;
    };
    let (reply, response) = oneshot::channel();
    sender
        .send(ImRequest {
            lwp: lwp.to_owned(),
            body,
            response: reply,
        })
        .await
        .map_err(|_| "闲鱼 IM 单例连接不可用，正在重连".to_owned())?;
    let response = tokio::time::timeout(std::time::Duration::from_secs(25), response)
        .await
        .map_err(|_| "闲鱼 IM 单例请求超时".to_owned())?
        .map_err(|_| "闲鱼 IM 单例连接已断开，正在重连".to_owned())??;
    Ok((response, cookie.to_owned()))
}

async fn ws_request(cookie: &str, lwp: &str, body: Value) -> Result<(Value, String), String> {
    let user_id = cookie_value(cookie, &["unb", "munb"]);
    if user_id.is_empty() {
        return Err("当前登录会话缺少闲鱼用户标识，请重新扫码登录".to_owned());
    }
    let did = device_id(&user_id);
    let (token, renewed_cookie) = im_token(cookie, &did).await?;
    let (stream, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .map_err(|error| format!("闲鱼 IM WebSocket 连接失败：{error}"))?;
    let (mut write, mut read) = stream.split();
    let register_mid = mid();
    write
        .send(Message::Text(
            register_message(&token, &did, &register_mid).into(),
        ))
        .await
        .map_err(|error| format!("闲鱼 IM 注册失败：{error}"))?;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while let Some(frame) = read.next().await {
            let frame = frame.map_err(|error| error.to_string())?;
            let text = match frame {
                Message::Text(value) => value.to_string(),
                Message::Binary(value) => String::from_utf8_lossy(&value).to_string(),
                _ => continue,
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if let Some(ack) = ack_message(&value) {
                let _ = write.send(Message::Text(ack.into())).await;
            }
            if value.pointer("/headers/mid").and_then(Value::as_str) == Some(register_mid.as_str())
            {
                return Ok::<(), String>(());
            }
        }
        Err("闲鱼 IM 注册连接已关闭".to_owned())
    })
    .await;
    let request_mid = mid();
    write
        .send(Message::Text(
            serde_json::json!({ "lwp": lwp, "headers": { "mid": request_mid }, "body": body })
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| format!("闲鱼 IM 请求失败：{error}"))?;
    let response = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(frame) = read.next().await {
            let frame = frame.map_err(|error| format!("闲鱼 IM 接收失败：{error}"))?;
            let text = match frame {
                Message::Text(value) => value.to_string(),
                Message::Binary(value) => String::from_utf8_lossy(&value).to_string(),
                Message::Close(_) => return Err("闲鱼 IM 连接已关闭".to_owned()),
                _ => continue,
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if let Some(ack) = ack_message(&value) {
                let _ = write.send(Message::Text(ack.into())).await;
            }
            if value.pointer("/headers/mid").and_then(Value::as_str) == Some(request_mid.as_str()) {
                return Ok(value);
            }
        }
        Err("闲鱼 IM 连接已关闭".to_owned())
    })
    .await
    .map_err(|_| "闲鱼 IM 请求超时".to_owned())??;
    Ok((response, renewed_cookie))
}

fn is_im_push(value: &Value) -> bool {
    let lwp = value.get("lwp").and_then(Value::as_str).unwrap_or_default();
    let serialized = value.to_string();
    // Realtime seller notifications are delivered on the `/s/sync` stream.
    // They are not `/r/*` request responses, so filtering only by Message or
    // Conversation silently drops every push event.
    lwp == "/s/sync"
        || lwp.contains("Message")
        || lwp.contains("Conversation")
        || value.pointer("/body/syncPushPackage/data").is_some()
        || value.pointer("/body/message").is_some()
        || value.pointer("/body/userMessageModels").is_some()
        || value.pointer("/body/userConvs").is_some()
        || value.get("1").is_some()
        || value.get("4").is_some()
        || serialized.contains("syncPushPackage")
        || serialized.contains("userMessageModels")
        || serialized.contains("userConvs")
}

/// Keep one registered IM WebSocket open until it disconnects. Pushes are
/// forwarded to the callback; the outer account task reconnects only after a
/// real socket/token failure instead of reconnecting after every message.
pub async fn listen_for_push<F, G, H>(
    cookie: &str,
    on_connected: F,
    on_push: G,
    on_trace: H,
    requests: &mut mpsc::Receiver<ImRequest>,
) -> Result<String, String>
where
    F: Fn() + Send + Sync + 'static,
    G: Fn(&Value) + Send + Sync + 'static,
    H: Fn(&str) + Send + Sync + 'static,
{
    let user_id = cookie_value(cookie, &["unb", "munb"]);
    if user_id.is_empty() {
        return Err("当前登录会话缺少闲鱼用户标识，请重新扫码登录".to_owned());
    }
    let did = device_id(&user_id);
    on_trace("正在获取 IM 访问令牌");
    let (token, renewed_cookie) = im_token(cookie, &did).await?;
    on_trace("IM 访问令牌已获取，正在建立 WebSocket");
    let (stream, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .map_err(|error| format!("闲鱼 IM WebSocket 连接失败：{error}"))?;
    let (mut write, mut read) = stream.split();
    let register_mid = mid();
    on_trace("WebSocket 已建立，正在发送 IM 注册请求");
    write
        .send(Message::Text(register_message(&token, &did, &register_mid).into()))
        .await
        .map_err(|error| format!("闲鱼 IM 注册失败：{error}"))?;
    let registered = tokio::time::timeout(std::time::Duration::from_secs(12), async {
        while let Some(frame) = read.next().await {
            let frame = frame.map_err(|error| error.to_string())?;
            let text = match frame {
                Message::Text(value) => value.to_string(),
                Message::Binary(value) => String::from_utf8_lossy(&value).to_string(),
                _ => continue,
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
            if let Some(ack) = ack_message(&value) {
                let _ = write.send(Message::Text(ack.into())).await;
            }
            if value.pointer("/headers/mid").and_then(Value::as_str) == Some(register_mid.as_str()) {
                return Ok::<(), String>(());
            }
        }
        Err("闲鱼 IM 注册连接已关闭".to_owned())
    })
    .await
    .map_err(|_| "闲鱼 IM 注册超时".to_owned())??;
    let _ = registered;
    on_trace("IM 注册成功，开始接收实时推送");
    on_connected();
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
    // Tokio intervals tick immediately on creation. Consume that initial tick
    // so the first heartbeat follows the official 15-second cadence instead
    // of being sent immediately after registration/ackDiff.
    heartbeat.tick().await;
    let mut heartbeat_request: Option<(String, std::time::Instant)> = None;
    let mut sync_state_request: Option<String> = None;
    let mut sync_ack_request: Option<String> = None;
    let mut pending_requests = HashMap::<String, oneshot::Sender<Result<Value, String>>>::new();
    loop {
        tokio::select! {
            Some(request) = requests.recv() => {
                let request_mid = mid();
                on_trace(&format!("单例连接正在发送请求：{}", request.lwp));
                let payload = serde_json::json!({
                    "lwp": request.lwp,
                    "headers": { "mid": request_mid },
                    "body": request.body,
                });
                match write.send(Message::Text(payload.to_string().into())).await {
                    Ok(()) => {
                        pending_requests.insert(request_mid, request.response);
                    }
                    Err(error) => {
                        let _ = request.response.send(Err(format!("闲鱼 IM 单例请求发送失败：{error}")));
                        return Err(format!("闲鱼 IM 单例连接写入失败：{error}"));
                    }
                }
            }
            _ = heartbeat.tick() => {
                if let Some((_, started_at)) = heartbeat_request.as_ref() {
                    if started_at.elapsed() >= std::time::Duration::from_secs(30) {
                        return Err("闲鱼 IM 心跳响应超时".to_owned());
                    }
                    continue;
                }
                let request_mid = mid();
                on_trace("正在发送 IM 心跳");
                write
                    .send(Message::Text(heartbeat_message(&request_mid).into()))
                    .await
                    .map_err(|error| format!("闲鱼 IM 心跳失败：{error}"))?;
                heartbeat_request = Some((request_mid, std::time::Instant::now()));
            }
            Some(frame) = read.next() => {
                let frame = frame.map_err(|error| format!("闲鱼 IM 接收失败：{error}"))?;
                let text = match frame {
                    Message::Text(value) => value.to_string(),
                    Message::Binary(value) => String::from_utf8_lossy(&value).to_string(),
                    Message::Ping(payload) => {
                        write
                            .send(Message::Pong(payload))
                            .await
                            .map_err(|error| format!("闲鱼 IM Pong 应答失败：{error}"))?;
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => return Ok(renewed_cookie),
                    _ => continue,
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(request_mid) = value.pointer("/headers/mid").and_then(Value::as_str) {
                    if let Some(request) = pending_requests.remove(request_mid) {
                        let _ = request.send(Ok(value));
                        continue;
                    }
                    if heartbeat_request.as_ref().is_some_and(|(mid, _)| mid == request_mid) {
                        heartbeat_request = None;
                        on_trace("IM 心跳响应正常");
                        continue;
                    }
                    if sync_state_request.as_deref() == Some(request_mid) {
                        sync_state_request = None;
                        if value.get("code").and_then(Value::as_i64).unwrap_or(500) != 200 {
                            return Err("闲鱼 IM 同步状态请求失败".to_owned());
                        }
                        let state = value.get("body").cloned().ok_or("闲鱼 IM 同步状态缺少响应内容")?;
                        let ack_mid = mid();
                        on_trace("IM 同步状态已返回，正在确认同步游标");
                        write
                            .send(Message::Text(sync_ack_diff_message(&ack_mid, state).into()))
                            .await
                            .map_err(|error| format!("闲鱼 IM 同步确认失败：{error}"))?;
                        sync_ack_request = Some(ack_mid);
                        continue;
                    }
                    if sync_ack_request.as_deref() == Some(request_mid) {
                        sync_ack_request = None;
                        if value.get("code").and_then(Value::as_i64).unwrap_or(500) != 200 {
                            return Err("闲鱼 IM 同步确认被拒绝".to_owned());
                        }
                        on_trace("IM 同步游标确认完成");
                        continue;
                    }
                }
                if needs_sync_recovery(&value) {
                    // The official seller workbench does not ACK a sync reset
                    // directly. It first requests the current sync cursor and
                    // acknowledges that exact state through `ackDiff`.
                    if sync_state_request.is_none() && sync_ack_request.is_none() {
                        let state_mid = mid();
                        on_trace("服务端要求重置同步游标，正在读取同步状态");
                        write
                            .send(Message::Text(sync_state_message(&state_mid).into()))
                            .await
                            .map_err(|error| format!("闲鱼 IM 同步状态请求失败：{error}"))?;
                        sync_state_request = Some(state_mid);
                    }
                    continue;
                }
                if let Some(ack) = ack_message(&value) {
                    write
                        .send(Message::Text(ack.into()))
                        .await
                        .map_err(|error| format!("闲鱼 IM 推送确认失败：{error}"))?;
                }
                if is_im_push(&value) {
                    let push_lwp = value.get("lwp").and_then(Value::as_str).unwrap_or("未知通道");
                    let sync_type = value.pointer("/body/syncExtraType/type").and_then(Value::as_i64);
                    on_trace(&format!(
                        "收到 IM 推送：{}{}",
                        push_lwp,
                        sync_type.map(|kind| format!("（同步类型 {kind}）")).unwrap_or_default()
                    ));
                    #[cfg(debug_assertions)]
                    eprintln!("[im] push lwp={push_lwp}");
                    on_push(&value);
                    // `/s/sync` and `/s/vulcan` are unsolicited server pushes. The
                    // official client feeds them into its LWP request state machine
                    // before issuing any follow-up RPC. Sending a hand-rolled
                    // `getUserMessageById` frame here races that state machine and
                    // makes the server reset the socket after a new message. The
                    // push callback immediately performs the normal local-first
                    // conversation sync instead, which also persists the message.
                }
            }
            else => return Ok(renewed_cookie),
        }
    }
}

/// Clear the seller-workbench red point for one conversation.  The official
/// web client calls `/r/Conversation/clearRedPoint` with a list containing the
/// conversation cid and the last message id currently visible to the seller.
pub async fn clear_red_point(
    cookie: &str,
    chat_id: &str,
    message_id: &str,
    sender: Option<&ImRequestSender>,
) -> Result<(Value, String), String> {
    let cid = if chat_id.contains("@goofish") {
        chat_id.to_owned()
    } else {
        format!("{chat_id}@goofish")
    };
    request_on_account_channel(
        cookie,
        sender,
        "/r/Conversation/clearRedPoint",
        serde_json::json!([[{ "messageId": message_id, "cid": cid }]]),
    )
    .await
}

pub async fn set_conversation_top(
    cookie: &str,
    chat_id: &str,
    pinned: bool,
    sender: Option<&ImRequestSender>,
) -> Result<(Value, String), String> {
    let cid = if chat_id.contains("@goofish") { chat_id.to_owned() } else { format!("{chat_id}@goofish") };
    request_on_account_channel(cookie, sender, "/r/Conversation/setTop", serde_json::json!([cid, pinned])).await
}

pub async fn hide_conversation(
    cookie: &str,
    chat_id: &str,
    sender: Option<&ImRequestSender>,
) -> Result<(Value, String), String> {
    let cid = if chat_id.contains("@goofish") { chat_id.to_owned() } else { format!("{chat_id}@goofish") };
    request_on_account_channel(cookie, sender, "/r/Conversation/hide", serde_json::json!([cid])).await
}

fn value_object(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => serde_json::json!({}),
    }
}

fn value_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim_matches('"').trim().to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn parse_payload(message: &Value) -> Value {
    let content = value_object(message.get("content"));
    let custom = value_object(content.get("custom"));
    let raw = custom
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if raw.is_empty() {
        return content;
    }
    if let Ok(value) = serde_json::from_str(raw) {
        return value;
    }
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(content)
}

fn decode_push_payload(raw: &str) -> Option<Value> {
    // The sync service uses both plain JSON and base64-encoded MessagePack.
    // Read receipts are frequently delivered as the plain JSON variant.
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Some(value);
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(raw))
        .ok()?;
    if let Ok(text) = String::from_utf8(decoded.clone()) {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return Some(value);
        }
    }
    let mut cursor = std::io::Cursor::new(decoded);
    let value = rmpv::decode::read_value(&mut cursor).ok()?;
    Some(rmpv_to_json(value))
}

fn parse_numeric_read_receipt(value: &Value) -> Option<ReadReceipt> {
    let message_ids = value.get("1")?.as_array()?;
    if value_i64(value.get("2")) != Some(2) {
        return None;
    }
    let chat_id = value_string(value.get("3"))
        .trim_end_matches("@goofish")
        .to_owned();
    if chat_id.is_empty() {
        return None;
    }
    let message_ids = message_ids
        .iter()
        .map(|value| value_string(Some(value)))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if message_ids.is_empty() {
        return None;
    }
    Some(ReadReceipt {
        chat_id,
        message_ids,
        status: value_i64(value.get("4")).unwrap_or_default(),
        timestamp: value_string(value.get("5")),
    })
}

pub fn parse_push_read_receipts(value: &Value) -> Vec<ReadReceipt> {
    let mut decoded_items = Vec::new();
    if let Some(items) = value
        .pointer("/body/syncPushPackage/data")
        .and_then(Value::as_array)
    {
        for item in items {
            let Some(data) = item.get("data").and_then(Value::as_str) else { continue };
            if let Some(decoded) = decode_push_payload(data) {
                if let Some(items) = decoded.as_array() {
                    decoded_items.extend(items.iter().cloned());
                } else {
                    decoded_items.push(decoded);
                }
            }
        }
    } else {
        decoded_items.push(value.clone());
    }
    decoded_items
        .iter()
        .filter_map(parse_numeric_read_receipt)
        .collect()
}

fn rmpv_to_json(value: rmpv::Value) -> Value {
    match value {
        rmpv::Value::Nil => Value::Null,
        rmpv::Value::Boolean(value) => Value::Bool(value),
        rmpv::Value::Integer(value) => value
            .as_i64()
            .map(serde_json::Number::from)
            .or_else(|| value.as_u64().map(serde_json::Number::from))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        rmpv::Value::F32(value) => serde_json::Number::from_f64(value as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        rmpv::Value::F64(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        rmpv::Value::String(value) => Value::String(value.as_str().unwrap_or_default().to_owned()),
        rmpv::Value::Binary(value) => Value::String(String::from_utf8_lossy(&value).to_string()),
        rmpv::Value::Array(items) => Value::Array(items.into_iter().map(rmpv_to_json).collect()),
        rmpv::Value::Map(items) => Value::Object(
            items
                .into_iter()
                .map(|(key, value)| (rmpv_key(key), rmpv_to_json(value)))
                .collect(),
        ),
        rmpv::Value::Ext(_, value) => Value::String(String::from_utf8_lossy(&value).to_string()),
    }
}

fn rmpv_key(value: rmpv::Value) -> String {
    match rmpv_to_json(value) {
        Value::String(value) => value,
        Value::Number(value) => value.to_string(),
        value => value.to_string(),
    }
}

fn push_content_payload(model: &Value) -> Value {
    let envelope = model.pointer("/1/6/3");
    for key in ["5", "1"] {
        let Some(value) = envelope.and_then(|value| value.get(key)) else {
            continue;
        };
        if value.is_object() {
            return value.clone();
        }
        let raw = value_string(Some(value));
        if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
            return parsed;
        }
        if let Some(parsed) = decode_push_payload(&raw) {
            return parsed;
        }
    }
    serde_json::json!({})
}

fn parse_numeric_push_message(
    model: &Value,
    account_id: &str,
    own_id: &str,
) -> Option<ChatMessage> {
    let one = model.get("1")?.as_object()?;
    let body = one.get("10").unwrap_or(&Value::Null);
    let chat_id = value_string(one.get("2"))
        .split('@')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let sender_user_id = find_string(body, &["senderUserId"])
        .split('@')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if chat_id.is_empty() || sender_user_id.is_empty() {
        return None;
    }
    let fallback = find_string(body, &["reminderContent"]);
    let payload = push_content_payload(model);
    let (content_kind, text) = payload_text(&payload, &fallback);
    let (card_title, card_subtitle, card_price) = payload_card(&payload, &content_kind);
    let sent = value_i64(one.get("5")).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let extension = value_object(body.get("extJson"));
    let id = find_string(&extension, &["messageId", "message_id", "msgId", "msg_id"])
        .or_else(|| find_string(model, &["messageId", "message_id", "msgId", "msg_id"]))
        .if_empty_then(|| format!("push-{}-{:x}", sent, md5::compute(model.to_string())));
    Some(ChatMessage {
        id,
        account_id: account_id.to_owned(),
        chat_id,
        sender_user_id: sender_user_id.clone(),
        sender_user_name: find_string(body, &["senderNick", "reminderTitle"]),
        direction: if sender_user_id == own_id { "outgoing" } else { "incoming" }.to_owned(),
        content_kind,
        text,
        media_url: payload_media_url(&payload),
        sent_at: millis_rfc3339(sent),
        send_status: "sent".to_owned(),
        // Push payloads normally carry no receiver receipt. Wait for the
        // official history model rather than assuming a state.
        read_status: "unknown".to_owned(),
        card_title,
        card_subtitle,
        card_price,
        target_url: payload_target_url(&payload),
    })
}

pub fn push_message_refs(value: &Value) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    let Some(items) = value
        .pointer("/body/syncPushPackage/data")
        .and_then(Value::as_array)
    else {
        return refs;
    };
    for item in items {
        let Some(raw) = item.get("data").and_then(Value::as_str) else { continue };
        let Some(decoded) = decode_push_payload(raw) else { continue };
        let Some(one) = decoded.get("1") else { continue };
        let chat_id = value_string(Some(one))
            .split('@')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let message_id = value_string(decoded.get("3"));
        if !chat_id.is_empty() && !message_id.is_empty() && decoded.get("10").is_none() {
            refs.push((chat_id, message_id));
        }
    }
    refs
}

/// `/s/para` is the seller IM "typing" push. It does not contain a normal
/// message model, but it does carry the conversation cid. Keep this parser
/// deliberately tolerant because the gateway has returned both plain JSON
/// objects and numeric MessagePack maps for this push.
pub fn parse_typing_push_chat_ids(value: &Value) -> Vec<String> {
    if value.get("lwp").and_then(Value::as_str) != Some("/s/para") {
        return Vec::new();
    }
    let mut roots = vec![value.clone()];
    if let Some(items) = value.pointer("/body/syncPushPackage/data").and_then(Value::as_array) {
        for item in items {
            if let Some(raw) = item.get("data").and_then(Value::as_str) {
                if let Some(decoded) = decode_push_payload(raw) {
                    roots.push(decoded);
                }
            }
        }
    }
    collect_typing_push_payloads(value, &mut roots);
    let mut ids = Vec::new();
    for root in roots {
        collect_typing_chat_ids(&root, &mut ids);
    }
    ids.sort();
    ids.dedup();
    ids
}

fn collect_typing_push_payloads(value: &Value, roots: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            if let Some(raw) = map.get("data").and_then(Value::as_str) {
                if let Some(decoded) = decode_push_payload(raw) {
                    roots.push(decoded);
                }
            }
            for child in map.values() {
                collect_typing_push_payloads(child, roots);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_typing_push_payloads(item, roots);
            }
        }
        _ => {}
    }
}

fn collect_typing_chat_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in ["cid", "chatId", "chat_id", "conversationId", "conversation_id", "conversationCid", "conversation_cid"] {
                if let Some(raw) = map.get(key).and_then(Value::as_str) {
                    let chat_id = raw.trim().trim_end_matches("@goofish");
                    if !chat_id.is_empty() {
                        ids.push(chat_id.to_owned());
                    }
                }
            }
            // Numeric MessagePack schemas use field 1 for the conversation cid.
            if let Some(raw) = map.get("1").and_then(Value::as_str) {
                let chat_id = raw.trim().trim_end_matches("@goofish");
                if !chat_id.is_empty() {
                    ids.push(chat_id.to_owned());
                }
            }
            for child in map.values() {
                collect_typing_chat_ids(child, ids);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_typing_chat_ids(item, ids);
            }
        }
        _ => {}
    }
}

trait StringFallback {
    fn or_else(self, fallback: impl FnOnce() -> String) -> String;
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String;
}

impl StringFallback for String {
    fn or_else(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() { fallback() } else { self }
    }

    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() { fallback() } else { self }
    }
}

/// Decode the same push envelopes consumed by the official seller page. This
/// is intentionally synchronous so the listener can persist the message before
/// notifying the UI; remote history sync then enriches the contact in the
/// background.
pub fn parse_push_messages(value: &Value, account_id: &str, cookie: &str) -> Vec<ChatMessage> {
    let own_id = cookie_value(cookie, &["unb", "munb"])
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut models = Vec::new();
    if let Some(items) = value
        .pointer("/body/syncPushPackage/data")
        .and_then(Value::as_array)
    {
        for item in items {
            let Some(data) = item.get("data").and_then(Value::as_str) else { continue };
            if let Some(decoded) = decode_push_payload(data) {
                if let Some(items) = decoded.as_array() {
                    models.extend(items.iter().cloned());
                } else {
                    models.push(decoded);
                }
            }
        }
    } else if let Some(items) = value
        .pointer("/body/userMessageModels")
        .and_then(Value::as_array)
    {
        models.extend(items.iter().cloned());
    } else if let Some(model) = value.pointer("/body/userMessageModel") {
        models.push(model.clone());
    } else if let Some(body) = value.get("body") {
        if body.get("message").is_some() {
            models.push(body.clone());
        } else if let Some(items) = body.as_array() {
            models.extend(items.iter().cloned());
        }
    } else if value.get("1").is_some() || value.get("4").is_some() {
        models.push(value.clone());
    }
    let hinted_chat_id = value_string(value.get("_pushChatId"));
    let mut seen = HashSet::new();
    models
        .iter()
        .filter_map(|model| {
            parse_numeric_push_message(model, account_id, &own_id).or_else(|| {
                parse_chat_message(model, account_id, &hinted_chat_id, &own_id)
            })
        })
        .filter(|message| seen.insert(message.id.clone()))
        .collect()
}

fn find_string(value: &Value, keys: &[&str]) -> String {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key) {
                    let text = value_string(Some(value));
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
            map.values()
                .find_map(|value| {
                    let text = find_string(value, keys);
                    (!text.is_empty()).then_some(text)
                })
                .unwrap_or_default()
        }
        Value::Array(items) => items
            .iter()
            .find_map(|value| {
                let text = find_string(value, keys);
                (!text.is_empty()).then_some(text)
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

const INVALID_NICKS: &[&str] = &[
    "交易消息",
    "系统消息",
    "卡片消息",
    "我完成了评价",
    "对方完成了评价",
    "快给ta一个评价吧～",
    "卖家已发货",
    "买家已付款",
    "买家已确认收货",
    "等待您发货",
    "我发起了退款申请",
    "买家申请退款",
    "卖家同意退款",
    "超时未付款，系统关闭了订单",
    "买家已拍下，待付款",
    "我已拍下，待付款",
    "买家已拍下",
];

fn valid_nick(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    // This is the app's own fallback label, not a buyer nickname.  Treat it
    // as invalid so a cached/official profile with the real name can win.
    if let Some(suffix) = value.strip_prefix("用户 ").or_else(|| value.strip_prefix("用户_")) {
        if suffix.chars().all(|character| character.is_ascii_digit()) {
            return false;
        }
    }
    if INVALID_NICKS.contains(&value) {
        return false;
    }
    if (value.contains("待付款") || value.contains("待发货") || value.contains("待收货"))
        && (value.contains("买家") || value.contains("卖家") || value.starts_with("我已"))
    {
        return false;
    }
    !value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .is_some_and(|value| INVALID_NICKS.contains(&value))
}

fn find_key_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key) {
                    return Some(value);
                }
            }
            map.values().find_map(|value| find_key_value(value, keys))
        }
        Value::Array(items) => items.iter().find_map(|value| find_key_value(value, keys)),
        _ => None,
    }
}

fn normalize_avatar_url(value: &str) -> String {
    let value = value.trim();
    if value.starts_with("//") {
        format!("https:{value}")
    } else if let Some(value) = value.strip_prefix("http://") {
        // The app itself is served from a secure WebView origin.  WebKit can
        // reject an otherwise valid http image as mixed content, while the
        // official Alibaba CDN supports HTTPS for these same paths.
        format!("https://{value}")
    } else if value.starts_with("https://") {
        value.to_owned()
    } else {
        String::new()
    }
}

fn avatar_url(value: &Value) -> String {
    let keys = [
        "senderAvatar",
        "senderAvatarUrl",
        "avatarUrl",
        "avatarURL",
        "headPic",
        "headPicUrl",
        "userAvatar",
        "userIcon",
        "portrait",
        "userPic",
    ];
    let direct = normalize_avatar_url(&find_string(value, &keys));
    if !direct.is_empty() {
        return direct;
    }
    find_key_value(value, &["avatar", "senderAvatar", "userAvatar", "userIcon"])
        .map(|value| {
            normalize_avatar_url(&find_string(value, &["url", "picUrl", "imageUrl", "src"]))
        })
        .unwrap_or_default()
}

fn item_image_url(payload: &Value, conversation: &Value) -> String {
    let keys = [
        "itemImage",
        "itemImageUrl",
        "productImage",
        "imageUrl",
        "picUrl",
        "mainPic",
        "itemMainPic",
        "itemMainPicUrl",
        "itemPic",
        "itemPicUrl",
        "mainPicUrl",
        "reminderPicUrl",
        "coverUrl",
        "thumbnailUrl",
    ];
    let from_payload = normalize_avatar_url(&find_string(payload, &keys));
    if !from_payload.is_empty() {
        return from_payload;
    }
    normalize_avatar_url(&find_string(conversation, &keys))
}

fn normalize_conversation_order_status(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let normalized = value.to_ascii_uppercase();
    if value.contains("退款")
        || normalized.contains("REFUND")
        || normalized.contains("RETURN_GOODS")
    {
        "退款中".to_owned()
    } else if value.contains("待付款")
        || value.contains("等待付款")
        || normalized.contains("PENDING_PAYMENT")
        || normalized.contains("WAIT_BUYER_PAY")
    {
        "待付款".to_owned()
    } else if value.contains("待发货")
        || value.contains("等待发货")
        || value.contains("已付款")
        || normalized.contains("PENDING_SHIP")
        || normalized.contains("WAIT_SELLER_SEND")
    {
        "待发货".to_owned()
    } else if value.contains("已发货")
        || value.contains("待收货")
        || normalized.contains("SHIPPED")
        || normalized.contains("WAIT_BUYER_CONFIRM")
    {
        "已发货".to_owned()
    } else if value.contains("成功")
        || value.contains("已完成")
        || normalized.contains("COMPLETED")
        || normalized.contains("SUCCESS")
        || normalized.contains("FINISH")
    {
        "交易成功".to_owned()
    } else if value.contains("关闭")
        || value.contains("取消")
        || normalized.contains("CLOSED")
        || normalized.contains("CANCEL")
    {
        "交易关闭".to_owned()
    } else {
        value.to_owned()
    }
}

fn is_displayed_session_status(value: &str) -> bool {
    matches!(
        value,
        "待付款" | "待发货" | "已发货" | "交易成功" | "交易关闭" | "退款中"
    )
}

fn official_session_status_value(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };

    let direct = normalize_conversation_order_status(&value_string(Some(value)));
    if is_displayed_session_status(&direct) {
        return direct;
    }

    // The seller workbench stores `userInfo.ext.tradeStatus` as a JSON string.
    // Accept its structured variants too, but reject unrelated reminder text such
    // as “发”, which must stay in the message preview rather than become a badge.
    let structured = value_object(Some(value));
    for key in [
        "status",
        "statusDesc",
        "statusText",
        "text",
        "title",
        "value",
    ] {
        let status = normalize_conversation_order_status(&find_string(&structured, &[key]));
        if is_displayed_session_status(&status) {
            return status;
        }
    }
    String::new()
}

fn official_session_order_status(value: &Value, conv: &Value) -> String {
    // These are the fields used by the official seller IM list. In a live
    // `pc.session.sync` record, `userExtension.redReminder` is the label shown
    // after the buyer nickname; `summary.redReminder` is its list-summary copy.
    // Some older records instead carry the same label in userInfo.ext.tradeStatus.
    fn from_container(container: &Value) -> String {
        // The websocket response sometimes serializes these extension objects
        // as JSON strings, while the seller workbench model exposes objects.
        let user_extension = value_object(
            container
                .get("userExtension")
                .or_else(|| container.get("user_extension")),
        );
        let summary = value_object(container.get("summary"));
        let user_info = value_object(
            container
                .get("userInfo")
                .or_else(|| container.get("peerUserInfo")),
        );
        let profile_extension =
            value_object(user_info.get("ext").or_else(|| user_info.get("extension")));
        let status = [
            user_extension.get("redReminder"),
            summary.get("redReminder"),
            profile_extension.get("tradeStatus"),
        ]
        .into_iter()
        .map(official_session_status_value)
        .find(|status| !status.is_empty())
        .unwrap_or_default();
        status
    }

    let status = from_container(conv);
    if status.is_empty() {
        from_container(value)
    } else {
        status
    }
}

fn official_fans_tag(value: &Value, conv: &Value) -> String {
    // Verified against the seller workbench `pc.user.query/4.0` response:
    // `data.userInfo.ext.fansTag` contains the exact display label, e.g. “已购粉”.
    let from_conv = find_string(conv, &["fansTag"]);
    if from_conv.is_empty() {
        find_string(value, &["fansTag"])
    } else {
        from_conv
    }
}

fn parse_fetched_user_info(body: &Value, cookie: String) -> FetchedUserInfo {
    let user_info = value_object(
        body.pointer("/data/userInfo")
            .or_else(|| body.pointer("/data/userProfile"))
            .or_else(|| body.pointer("/data/user")),
    );
    let profile_extension =
        value_object(user_info.get("ext").or_else(|| user_info.get("extension")));
    let avatar_url = normalize_avatar_url(&value_string(user_info.get("logo")));
    let fish_nick = value_string(user_info.get("fishNick"));
    let nick = value_string(user_info.get("nick"));
    let display_name = if valid_nick(&fish_nick) {
        fish_nick
    } else {
        nick
    };
    FetchedUserInfo {
        avatar_url,
        display_name,
        fans_tag: value_string(profile_extension.get("fansTag")),
        trade_status: official_session_status_value(profile_extension.get("tradeStatus")),
        cookie,
    }
}

fn profile_needs_refresh(profile_synced_at: &str) -> bool {
    let Ok(synced_at) = chrono::DateTime::parse_from_rfc3339(profile_synced_at) else {
        return true;
    };
    chrono::Utc::now().signed_duration_since(synced_at.with_timezone(&chrono::Utc))
        >= chrono::Duration::hours(24)
}

fn collect_chat_emojis(value: &Value, items: &mut HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            let alias = map
                .get("iconAlias")
                .or_else(|| map.get("icon_alias"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| value.starts_with('[') && value.ends_with(']'));
            let url = map
                .get("iconUrl")
                .or_else(|| map.get("icon_url"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| value.starts_with("https://") || value.starts_with("http://"));
            if let (Some(alias), Some(url)) = (alias, url) {
                items.insert(alias.to_owned(), url.to_owned());
            }
            for child in map.values() {
                collect_chat_emojis(child, items);
            }
        }
        Value::Array(items_value) => {
            for child in items_value {
                collect_chat_emojis(child, items);
            }
        }
        _ => {}
    }
}

/// Loads the seller workbench's current emoji catalog. The official web
/// client caches this exact `iconAlias`/`iconUrl` payload and renders aliases
/// such as `[笑脸]` inline inside normal text messages.
pub async fn fetch_chat_emojis(cookie: &str) -> Result<(Vec<ChatEmoji>, String), String> {
    let (body, renewed_cookie) = mtop_call(
        cookie,
        "mtop.taobao.idlemessage.face.emoji.load",
        "1.0",
        "originaljson",
        &serde_json::json!({}),
    )
    .await?;
    let mut found = HashMap::new();
    collect_chat_emojis(&body, &mut found);
    let mut emojis = found
        .into_iter()
        .map(|(icon_alias, icon_url)| ChatEmoji { icon_alias, icon_url })
        .collect::<Vec<_>>();
    emojis.sort_by(|left, right| left.icon_alias.cmp(&right.icon_alias));
    Ok((emojis, renewed_cookie))
}

async fn fetch_user_info(cookie: &str, chat_id: &str) -> Result<FetchedUserInfo, String> {
    let (body, renewed_cookie) = mtop_call(
        cookie,
        "mtop.taobao.idlemessage.pc.user.query",
        "4.0",
        "originaljson",
        &serde_json::json!({
            "type": 0,
            "sessionType": 1,
            "sessionId": chat_id,
            "isOwner": false
        }),
    )
    .await?;
    Ok(parse_fetched_user_info(&body, renewed_cookie))
}

fn payload_text(payload: &Value, fallback: &str) -> (String, String) {
    let kind = if payload.get("expression").is_some() {
        "expression"
    } else {
        match payload
        .get("contentType")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        {
            2 => "image",
            4 => "location",
            5 => "video",
            7 | 12 | 25 | 26 => "product",
            _ => "text",
        }
    };
    let text = payload
        .pointer("/text/text")
        .and_then(Value::as_str)
        .or_else(|| payload.get("text").and_then(Value::as_str))
        .or_else(|| payload.pointer("/expression/name").and_then(Value::as_str))
        .or_else(|| payload.pointer("/expression/alias").and_then(Value::as_str))
        .unwrap_or(fallback)
        .trim()
        .to_owned();
    let text = if text.is_empty() {
        match kind {
            "image" => "[图片]",
            "location" => "[位置]",
            "video" => "[视频]",
            "product" => "[商品]",
            "expression" => "[表情]",
            _ => "",
        }
        .to_owned()
    } else {
        text
    };
    (kind.to_owned(), text)
}

fn payload_media_url(payload: &Value) -> String {
    let direct = payload
        .pointer("/image/pics/0/url")
        .or_else(|| payload.pointer("/video/coverUrl"))
        .or_else(|| payload.pointer("/expression/iconUrl"))
        .or_else(|| payload.pointer("/expression/imageUrl"))
        .or_else(|| payload.pointer("/expression/url"))
        .or_else(|| payload.pointer("/itemCard/imageUrl"))
        .or_else(|| payload.pointer("/itemCard/image"))
        .or_else(|| payload.pointer("/itemCard/itemPic"))
        .or_else(|| payload.pointer("/itemCard/picUrl"))
        .or_else(|| payload.pointer("/itemCard/coverUrl"))
        .or_else(|| payload.pointer("/itemCard/item/mainPic"))
        .or_else(|| payload.pointer("/itemCard/item/mainPicUrl"))
        .or_else(|| payload.pointer("/imageCard/imageUrl"))
        .or_else(|| payload.pointer("/imageCard/image"))
        .or_else(|| payload.pointer("/dxCard/item/main/exContent/imageUrl"))
        .or_else(|| payload.pointer("/dxCard/item/main/exContent/image"))
        .or_else(|| payload.pointer("/dxCard/item/main/exContent/picUrl"))
        .or_else(|| payload.pointer("/dxCard/item/main/exContent/itemPic"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if direct.is_empty() {
        find_string(payload, &["imageUrl", "itemPic", "picUrl", "coverUrl", "pic"])
    } else {
        direct
    }
}

fn payload_value(payload: &Value, paths: &[&str]) -> String {
    paths
        .iter()
        .map(|path| value_string(payload.pointer(path)))
        .filter(|value| !value.is_empty())
        .next()
        .unwrap_or_default()
}

fn payload_card(payload: &Value, kind: &str) -> (String, String, String) {
    if !matches!(kind, "product" | "location")
        && payload.get("itemCard").is_none()
        && payload.get("imageCard").is_none()
        && payload.get("dxCard").is_none()
    {
        return (String::new(), String::new(), String::new());
    }
    let title = payload_value(payload, &[
        "/itemCard/title", "/itemCard/itemTitle", "/itemCard/name", "/itemCard/item/title",
        "/imageCard/title", "/textCard/title",
        "/dxCard/item/main/exContent/title", "/dxCard/item/main/title", "/title",
    ]);
    let subtitle = payload_value(payload, &[
        "/itemCard/desc", "/itemCard/description", "/itemCard/subTitle",
        "/imageCard/desc", "/textCard/desc",
        "/dxCard/item/main/exContent/desc", "/dxCard/item/main/exContent/subTitle", "/subtitle",
    ]);
    let price = payload_value(payload, &[
        "/itemCard/price", "/itemCard/priceText", "/itemCard/priceDesc", "/itemCard/item/price",
        "/dxCard/item/main/exContent/price", "/dxCard/item/main/exContent/priceText", "/price",
    ]);
    (
        if title.is_empty() { find_string(payload, &["itemTitle", "cardTitle"]) } else { title },
        if subtitle.is_empty() { find_string(payload, &["subTitle", "description"]) } else { subtitle },
        if price.is_empty() { find_string(payload, &["priceText", "priceDesc"]) } else { price },
    )
}

fn payload_target_url(payload: &Value) -> String {
    let direct = payload_value(payload, &[
        "/itemCard/targetUrl", "/itemCard/actionUrl", "/itemCard/jumpUrl", "/itemCard/linkUrl", "/itemCard/itemUrl",
        "/imageCard/targetUrl", "/imageCard/actionUrl", "/imageCard/jumpUrl", "/imageCard/linkUrl",
        "/textCard/targetUrl", "/textCard/actionUrl", "/textCard/jumpUrl", "/textCard/linkUrl",
        "/dxCard/item/main/exContent/targetUrl", "/dxCard/item/main/exContent/actionUrl",
        "/dxCard/item/main/exContent/jumpUrl", "/dxCard/item/main/exContent/linkUrl",
        "/dxCard/item/main/targetUrl", "/dxCard/item/main/actionUrl", "/targetUrl", "/actionUrl", "/jumpUrl", "/linkUrl",
    ]);
    if !direct.is_empty() {
        return direct;
    }
    let fallback = find_string(payload, &["targetUrl", "actionUrl", "jumpUrl", "linkUrl", "itemUrl"]);
    if !fallback.is_empty() {
        return fallback;
    }
    let item_id = payload_value(payload, &["/itemCard/item/itemId", "/itemCard/itemId", "/itemId"]);
    if item_id.is_empty() {
        String::new()
    } else {
        format!("https://www.goofish.com/item?id={item_id}")
    }
}

fn millis_rfc3339(value: i64) -> String {
    chrono::DateTime::from_timestamp_millis(value)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

fn value_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn read_receipt_status(model: &Value, message: &Value) -> Option<&'static str> {
    let candidates = [
        model.get("readStatus"),
        message.get("readStatus"),
        model.pointer("/receiverMessageStatus/readStatus"),
        message.pointer("/receiverMessageStatus/readStatus"),
        model.pointer("/message/readStatus"),
        model.pointer("/message/receiverMessageStatus/readStatus"),
    ];
    candidates.into_iter().flatten().find_map(|value| {
        if value_i64(Some(value)) == Some(2) {
            return Some("read");
        }
        if value_i64(Some(value)).is_some() {
            return Some("unread");
        }
        match value.as_str().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("read") | Some("已读") => Some("read"),
            Some("unread") | Some("未读") => Some("unread"),
            _ => None,
        }
    })
}

fn remote_has_more(body: &Value, item_count: usize, page_size: usize) -> bool {
    body.get("hasMore")
        .or_else(|| body.get("has_more"))
        .and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.as_i64().map(|value| value != 0))
                .or_else(|| {
                    value
                        .as_str()
                        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                })
        })
        .unwrap_or(item_count >= page_size)
}

fn remote_cursor(body: &Value) -> Option<i64> {
    value_i64(body.get("nextCursor").or_else(|| body.get("next_cursor")))
}

fn conversation_time(value: &Value) -> Option<i64> {
    let conv = value.get("singleChatUserConversation").unwrap_or(value);
    value_i64(conv.get("modifyTime").or_else(|| conv.get("modify_time")))
}

fn message_time(value: &Value) -> Option<i64> {
    let message = value.get("message").unwrap_or(value);
    value_i64(message.get("createAt").or_else(|| message.get("time")))
}

fn parse_contact(value: &Value, account_id: &str, my_id: &str) -> Option<ChatContact> {
    let conv = value.get("singleChatUserConversation").unwrap_or(value);
    let single = conv.get("singleChatConversation").unwrap_or(conv);
    let chat_id = value_string(single.get("cid"))
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    if chat_id.is_empty() {
        return None;
    }
    let first = value_string(single.get("pairFirst"))
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    let second = value_string(single.get("pairSecond"))
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    let own = my_id.split('@').next().unwrap_or_default();
    let other_user_id = if first == own { second } else { first };
    if other_user_id.is_empty() || other_user_id == "0" {
        return None;
    }
    let extension = value_object(single.get("extension"));
    let last_message = conv
        .get("lastMessage")
        .and_then(|value| value.get("message").or(Some(value)))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let last_extension = value_object(last_message.get("extension"));
    let payload = parse_payload(&last_message);
    let fallback = value_string(last_extension.get("reminderContent"));
    let (_, latest_message) = payload_text(&payload, &fallback);
    let sender_id = value_string(last_extension.get("senderUserId"))
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut other_user_name = if sender_id == other_user_id {
        value_string(last_extension.get("reminderTitle"))
    } else {
        String::new()
    };
    if !valid_nick(&other_user_name) {
        other_user_name = value_string(extension.get("reminderTitle"));
    }
    if !valid_nick(&other_user_name) {
        other_user_name.clear();
    }
    let timestamp =
        conversation_time(value).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let unread_count =
        value_i64(conv.get("redPoint").or_else(|| conv.get("unreadCount"))).unwrap_or_default();
    let order_status = official_session_order_status(value, conv);
    let buyer_tag = official_fans_tag(value, conv);
    let item_id = {
        let value_from_payload = find_string(&payload, &["itemId", "item_id", "itemid"]);
        if value_from_payload.is_empty() {
            find_string(value, &["itemId", "item_id", "itemid"])
        } else {
            value_from_payload
        }
    };
    let item_title = {
        let value_from_payload = find_string(&payload, &["itemTitle", "productTitle"]);
        if value_from_payload.is_empty() {
            find_string(value, &["itemTitle", "productTitle"])
        } else {
            value_from_payload
        }
    };
    Some(ChatContact {
        account_id: account_id.to_owned(),
        chat_id,
        other_user_id,
        other_user_name,
        avatar_url: avatar_url(value),
        item_id,
        item_title,
        item_image_url: item_image_url(&payload, value),
        // Do not invent labels such as “已购” or “咨询”. The official seller
        // workbench does not show them in its session list.
        buyer_tag,
        order_status,
        latest_message,
        latest_message_time: millis_rfc3339(timestamp),
        unread_count,
        profile_synced_at: String::new(),
    })
}

fn parse_chat_message(
    model: &Value,
    account_id: &str,
    chat_id: &str,
    own_id: &str,
) -> Option<ChatMessage> {
    let message = model.get("message").unwrap_or(model);
    let extension = value_object(message.get("extension"));
    let sender_user_id = value_string(extension.get("senderUserId"))
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    if sender_user_id.is_empty() {
        return None;
    }
    let payload = parse_payload(message);
    let (content_kind, text) =
        payload_text(&payload, &value_string(extension.get("reminderContent")));
    let (card_title, card_subtitle, card_price) = payload_card(&payload, &content_kind);
    let media_url = payload_media_url(&payload);
    let sent = message_time(model).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let resolved_chat_id = if chat_id.trim().is_empty() {
        find_string(model, &["chatId", "chat_id", "cid", "conversationId"])
            .split('@')
            .next()
            .unwrap_or_default()
            .to_owned()
    } else {
        chat_id.trim_end_matches("@goofish").to_owned()
    };
    if resolved_chat_id.is_empty() {
        return None;
    }
    let id = ["messageId", "message_id", "msgId", "msg_id"]
        .iter()
        .find_map(|key| {
            let value = value_string(message.get(*key));
            (!value.is_empty()).then_some(value)
        })
        .unwrap_or_else(|| format!("{}-{:x}", sent, md5::compute(model.to_string())));
    // The seller IM model documents readStatus as 2 for read and any other
    // returned status as unread. msgReadStatusSetting=2 means this message
    // does not participate in read receipts, so never infer an unread state.
    let receipt_setting = value_i64(
        message
            .get("msgReadStatusSetting")
            .or_else(|| model.get("msgReadStatusSetting")),
    );
    let read_status = if receipt_setting == Some(2) {
        "unsupported"
    } else {
        read_receipt_status(model, message).unwrap_or("unknown")
    }
    .to_owned();
    Some(ChatMessage {
        id,
        account_id: account_id.to_owned(),
        chat_id: resolved_chat_id,
        sender_user_name: value_string(extension.get("reminderTitle")),
        direction: if sender_user_id == own_id {
            "outgoing"
        } else {
            "incoming"
        }
        .to_owned(),
        sender_user_id,
        content_kind,
        text,
        media_url,
        sent_at: millis_rfc3339(sent),
        send_status: "sent".to_owned(),
        read_status,
        card_title,
        card_subtitle,
        card_price,
        target_url: payload_target_url(&payload),
    })
}

pub async fn fetch_contacts(
    account_id: &str,
    cookie: &str,
    cached_profiles: &HashMap<String, CachedChatProfile>,
    cursor: Option<i64>,
    sender: Option<&ImRequestSender>,
) -> Result<ContactPage, String> {
    const PAGE_SIZE: usize = 50;
    let my_id = cookie_value(cookie, &["unb", "munb"]);
    let cursor = cursor.unwrap_or(9_007_199_254_740_991_i64);
    let (response, mut renewed_cookie) = request_on_account_channel(
        cookie,
        sender,
        "/r/Conversation/listNewestPagination",
        serde_json::json!([cursor, PAGE_SIZE]),
    )
    .await?;
    let body = response.get("body").unwrap_or(&response);
    let values = body
        .get("userConvs")
        .or_else(|| body.get("user_convs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut contacts = values
        .iter()
        .filter_map(|value| parse_contact(value, account_id, &my_id))
        .filter(|contact| seen.insert(contact.chat_id.clone()))
        .collect::<Vec<_>>();
    let has_more = remote_has_more(body, values.len(), PAGE_SIZE);
    let next_cursor = if has_more {
        remote_cursor(body)
            .or_else(|| values.iter().filter_map(conversation_time).min())
            .filter(|next| *next < cursor)
    } else {
        None
    };
    for contact in &mut contacts {
        let cached = cached_profiles
            .get(&contact.chat_id)
            .or_else(|| cached_profiles.get(&format!("user:{}", contact.other_user_id)));
        if let Some(cached) = cached {
            if !valid_nick(&contact.other_user_name) && valid_nick(&cached.display_name) {
                contact.other_user_name = cached.display_name.clone();
            }
            if contact.avatar_url.is_empty() && !cached.avatar_url.is_empty() {
                contact.avatar_url = cached.avatar_url.clone();
            }
            if contact.buyer_tag.is_empty() {
                contact.buyer_tag = cached.buyer_tag.clone();
            }
            contact.profile_synced_at = cached.profile_synced_at.clone();
        }
        // Retry profiles which were cached before the image URL mapping was
        // available instead of leaving the avatar placeholder for 24 hours.
        if profile_needs_refresh(&contact.profile_synced_at) || contact.avatar_url.is_empty() {
            if let Ok(profile) = fetch_user_info(&renewed_cookie, &contact.chat_id).await {
                renewed_cookie = profile.cookie;
                if !profile.avatar_url.is_empty() {
                    contact.avatar_url = profile.avatar_url;
                }
                if valid_nick(&profile.display_name) {
                    contact.other_user_name = profile.display_name;
                }
                contact.buyer_tag = profile.fans_tag;
                if contact.order_status.is_empty() && !profile.trade_status.is_empty() {
                    contact.order_status = profile.trade_status;
                }
                contact.profile_synced_at = chrono::Utc::now().to_rfc3339();
            }
        }
        if !valid_nick(&contact.other_user_name) {
            contact.other_user_name = format!("用户 {}", contact.other_user_id);
        }
    }
    Ok(ContactPage {
        items: contacts,
        next_cursor,
        has_more: has_more && next_cursor.is_some(),
        cookie: renewed_cookie,
    })
}

pub async fn fetch_messages(
    account_id: &str,
    chat_id: &str,
    cookie: &str,
    cursor: Option<i64>,
    sender: Option<&ImRequestSender>,
) -> Result<MessagePage, String> {
    const PAGE_SIZE: usize = 50;
    let own_id = cookie_value(cookie, &["unb", "munb"])
        .split('@')
        .next()
        .unwrap_or_default()
        .to_owned();
    let full_chat_id = if chat_id.contains("@goofish") {
        chat_id.to_owned()
    } else {
        format!("{chat_id}@goofish")
    };
    let cursor = cursor.unwrap_or(9_007_199_254_740_991_i64);
    let (response, renewed_cookie) = request_on_account_channel(
        cookie,
        sender,
        "/r/MessageManager/listUserMessages",
        serde_json::json!([full_chat_id, false, cursor, PAGE_SIZE, false]),
    )
    .await?;
    let body = response.get("body").unwrap_or(&response);
    let models = body
        .get("userMessageModels")
        .or_else(|| body.get("user_message_models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let own_avatar_url = models
        .iter()
        .find_map(|model| {
            let message = model.get("message").unwrap_or(model);
            let extension = value_object(message.get("extension"));
            let sender_value = value_string(extension.get("senderUserId"));
            let sender_id = sender_value
                .split('@')
                .next()
                .unwrap_or_default();
            (sender_id == own_id).then(|| avatar_url(&extension)).filter(|value| !value.is_empty())
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut messages = models
        .iter()
        .filter_map(|model| parse_chat_message(model, account_id, chat_id, &own_id))
        .filter(|message| seen.insert(message.id.clone()))
        .collect::<Vec<_>>();
    messages.sort_by(|left, right| left.sent_at.cmp(&right.sent_at));
    let has_more = remote_has_more(body, models.len(), PAGE_SIZE);
    let next_cursor = if has_more {
        remote_cursor(body)
            .or_else(|| models.iter().filter_map(message_time).min())
            .filter(|next| *next < cursor)
    } else {
        None
    };
    Ok(MessagePage {
        items: messages,
        next_cursor,
        has_more: has_more && next_cursor.is_some(),
        cookie: renewed_cookie,
        own_avatar_url,
    })
}

fn sent_message_id(response: &Value, fallback: &str) -> String {
    [
        "/body/sendResultModel/messageId",
        "/body/messageId",
        "/body/message/messageId",
        "/body/result/messageId",
    ]
    .iter()
    .find_map(|path| response.pointer(path).and_then(Value::as_str))
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .unwrap_or(fallback)
    .to_owned()
}

fn rejected_send_error(label: &str, response: &Value) -> String {
    let body = response.get("body").unwrap_or(response);
    let reason = ["reason", "developerMessage", "message", "code"]
        .iter()
        .find_map(|key| {
            let value = value_string(body.get(*key));
            (!value.is_empty()).then_some(value)
        });
    match reason {
        Some(reason) => format!("闲鱼拒绝发送该{label}：{reason}"),
        None => format!("闲鱼拒绝发送该{label}"),
    }
}

pub async fn send_text(
    account_id: &str,
    chat_id: &str,
    receiver_user_id: &str,
    cookie: &str,
    text: &str,
    sender: Option<&ImRequestSender>,
) -> Result<(ChatMessage, String), String> {
    if text.trim().is_empty() {
        return Err("消息内容不能为空".to_owned());
    }
    let own_id = cookie_value(cookie, &["unb", "munb"]);
    let full_chat_id = if chat_id.contains("@goofish") {
        chat_id.to_owned()
    } else {
        format!("{chat_id}@goofish")
    };
    let full_receiver = if receiver_user_id.contains("@goofish") {
        receiver_user_id.to_owned()
    } else {
        format!("{receiver_user_id}@goofish")
    };
    let full_self = if own_id.contains("@goofish") {
        own_id
    } else {
        format!("{own_id}@goofish")
    };
    let uuid = Uuid::new_v4().to_string();
    let payload = serde_json::json!({ "contentType": 1, "text": { "text": text.trim() } });
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string().as_bytes());
    let (response, cookie) = request_on_account_channel(cookie, sender, "/r/MessageSend/sendByReceiverScope", serde_json::json!([
        { "uuid": uuid, "cid": full_chat_id, "conversationType": 1, "content": { "contentType": 101, "custom": { "type": 1, "data": encoded } }, "redPointPolicy": 0, "extension": { "extJson": "{}" }, "ctx": { "appVersion": "1.0", "platform": "web" }, "mtags": {}, "msgReadStatusSetting": 1 },
        { "actualReceivers": [full_receiver, full_self] }
    ])).await?;
    if response.get("code").and_then(Value::as_i64).unwrap_or(200) != 200 {
        return Err(rejected_send_error("消息", &response));
    }
    if let Some(reason) = response
        .pointer("/body/reason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Err(reason.to_owned());
    }
    Ok((
        ChatMessage {
            id: sent_message_id(&response, &uuid),
            account_id: account_id.to_owned(),
            chat_id: chat_id.trim_end_matches("@goofish").to_owned(),
            sender_user_id: full_self.trim_end_matches("@goofish").to_owned(),
            sender_user_name: "我".to_owned(),
            direction: "outgoing".to_owned(),
            content_kind: "text".to_owned(),
            text: text.trim().to_owned(),
            media_url: String::new(),
            sent_at: chrono::Utc::now().to_rfc3339(),
            send_status: "sent".to_owned(),
            // Every message sent by this client opts into receiver receipts
            // with msgReadStatusSetting=1. It is unread until history sync or
            // an IM read-receipt push confirms otherwise.
            read_status: "unread".to_owned(),
            card_title: String::new(),
            card_subtitle: String::new(),
            card_price: String::new(),
            target_url: String::new(),
        },
        cookie,
    ))
}

pub async fn send_product(
    account_id: &str,
    chat_id: &str,
    receiver_user_id: &str,
    cookie: &str,
    item_id: &str,
    title: &str,
    image_url: &str,
    price: f64,
    sender: Option<&ImRequestSender>,
) -> Result<(ChatMessage, String), String> {
    let item_id = item_id.trim();
    let title = title.trim();
    if item_id.is_empty() || title.is_empty() {
        return Err("商品缺少闲鱼商品 ID 或标题，请先同步商品".to_owned());
    }
    let own_id = cookie_value(cookie, &["unb", "munb"]);
    let full_chat_id = if chat_id.contains("@goofish") { chat_id.to_owned() } else { format!("{chat_id}@goofish") };
    let full_receiver = if receiver_user_id.contains("@goofish") { receiver_user_id.to_owned() } else { format!("{receiver_user_id}@goofish") };
    let full_self = if own_id.contains("@goofish") { own_id } else { format!("{own_id}@goofish") };
    // The web seller uses a negative, timestamp-shaped UUID for item cards.
    // Matching it avoids the stricter validation applied to content type 7.
    let uuid = format!("-{}0", chrono::Utc::now().timestamp_millis());
    let price_text = format!("¥{price:.2}");
    // This is the same card payload emitted by the official seller workbench.
    let payload = serde_json::json!({
        "contentType": 7,
        "itemCard": {
            "item": {
                "itemId": item_id,
                "mainPic": image_url.trim(),
                "price": price_text,
                "title": title,
            },
            "itemTip": "商品详情",
        }
    });
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string().as_bytes());
    let (response, renewed_cookie) = request_on_account_channel(cookie, sender, "/r/MessageSend/sendByReceiverScope", serde_json::json!([
        { "uuid": uuid, "cid": full_chat_id, "conversationType": 1, "content": { "contentType": 101, "custom": { "type": 7, "data": encoded } }, "redPointPolicy": 0, "extension": { "extJson": "{}" }, "ctx": { "appVersion": "1.0", "platform": "web" }, "mtags": {}, "msgReadStatusSetting": 1 },
        { "actualReceivers": [full_self.clone(), full_receiver] }
    ])).await?;
    if response.get("code").and_then(Value::as_i64).unwrap_or(200) != 200 {
        return Err(rejected_send_error("商品", &response));
    }
    if let Some(reason) = response.pointer("/body/reason").and_then(Value::as_str).filter(|value| !value.is_empty()) {
        return Err(reason.to_owned());
    }
    Ok((ChatMessage {
        id: sent_message_id(&response, &uuid),
        account_id: account_id.to_owned(),
        chat_id: chat_id.trim_end_matches("@goofish").to_owned(),
        sender_user_id: full_self.trim_end_matches("@goofish").to_owned(),
        sender_user_name: "我".to_owned(),
        direction: "outgoing".to_owned(),
        content_kind: "product".to_owned(),
        text: format!("[链接]{title}"),
        media_url: image_url.trim().to_owned(),
        sent_at: chrono::Utc::now().to_rfc3339(),
        send_status: "sent".to_owned(),
        read_status: "unread".to_owned(),
        card_title: title.to_owned(),
        card_subtitle: String::new(),
        card_price: price_text,
        target_url: format!("https://www.goofish.com/item?id={item_id}"),
    }, renewed_cookie))
}

fn find_http_url(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.starts_with("http://") || text.starts_with("https://") => {
            Some(text.to_owned())
        }
        Value::Array(items) => items.iter().find_map(find_http_url),
        Value::Object(map) => map.values().find_map(find_http_url),
        _ => None,
    }
}

pub async fn send_image(
    account_id: &str,
    chat_id: &str,
    receiver_user_id: &str,
    cookie: &str,
    file_name: &str,
    mime_type: &str,
    image_bytes: Vec<u8>,
    width: u32,
    height: u32,
    sender: Option<&ImRequestSender>,
) -> Result<(ChatMessage, String), String> {
    if image_bytes.is_empty() {
        return Err("图片内容为空".to_owned());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(|error| error.to_string())?;
    let part = Part::bytes(image_bytes)
        .file_name(if file_name.trim().is_empty() {
            "image.jpg".to_owned()
        } else {
            file_name.to_owned()
        })
        .mime_str(if mime_type.starts_with("image/") {
            mime_type
        } else {
            "image/jpeg"
        })
        .map_err(|error| format!("图片类型无效：{error}"))?;
    let upload = client
        .post("https://stream-upload.goofish.com/api/upload.api")
        .query(&[
            ("floderId", "0"),
            ("appkey", "xy_chat"),
            ("_input_charset", "utf-8"),
        ])
        .header("Accept", "*/*")
        .header("Origin", "https://www.goofish.com")
        .header("Referer", "https://www.goofish.com/")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/131 Safari/537.36",
        )
        .header("Cookie", cookie)
        .multipart(Form::new().part("file", part))
        .send()
        .await
        .map_err(|error| format!("图片上传失败：{error}"))?;
    let status = upload.status();
    let body = upload
        .json::<Value>()
        .await
        .map_err(|error| format!("图片上传响应无效：{error}"))?;
    if !status.is_success() {
        return Err(format!("图片上传失败（HTTP {}）", status.as_u16()));
    }
    let image_url = find_http_url(&body).ok_or_else(|| {
        body.pointer("/message")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "图片上传成功但未返回图片地址".to_owned())
    })?;
    let own_id = cookie_value(cookie, &["unb", "munb"]);
    let full_chat_id = if chat_id.contains("@goofish") {
        chat_id.to_owned()
    } else {
        format!("{chat_id}@goofish")
    };
    let full_receiver = if receiver_user_id.contains("@goofish") {
        receiver_user_id.to_owned()
    } else {
        format!("{receiver_user_id}@goofish")
    };
    let full_self = if own_id.contains("@goofish") {
        own_id
    } else {
        format!("{own_id}@goofish")
    };
    let uuid = Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "contentType": 2,
        "image": { "pics": [{ "type": 0, "url": image_url, "width": width.max(1), "height": height.max(1) }] }
    });
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(payload.to_string().as_bytes());
    let (response, renewed_cookie) = request_on_account_channel(cookie, sender, "/r/MessageSend/sendByReceiverScope", serde_json::json!([
        { "uuid": uuid, "cid": full_chat_id, "conversationType": 1, "content": { "contentType": 101, "custom": { "type": 2, "data": encoded } }, "redPointPolicy": 0, "extension": { "extJson": "{}" }, "ctx": { "appVersion": "1.0", "platform": "web" }, "mtags": {}, "msgReadStatusSetting": 1 },
        { "actualReceivers": [full_receiver, full_self.clone()] }
    ])).await?;
    if response.get("code").and_then(Value::as_i64).unwrap_or(200) != 200 {
        return Err(rejected_send_error("图片", &response));
    }
    if let Some(reason) = response
        .pointer("/body/reason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Err(reason.to_owned());
    }
    Ok((
        ChatMessage {
            id: sent_message_id(&response, &uuid),
            account_id: account_id.to_owned(),
            chat_id: chat_id.trim_end_matches("@goofish").to_owned(),
            sender_user_id: full_self.trim_end_matches("@goofish").to_owned(),
            sender_user_name: "我".to_owned(),
            direction: "outgoing".to_owned(),
            content_kind: "image".to_owned(),
            text: "[图片]".to_owned(),
            media_url: image_url,
            sent_at: chrono::Utc::now().to_rfc3339(),
            send_status: "sent".to_owned(),
            read_status: "unread".to_owned(),
            card_title: String::new(),
            card_subtitle: String::new(),
            card_price: String::new(),
            target_url: String::new(),
        },
        renewed_cookie,
    ))
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use super::{ack_message, decode_push_payload, device_id, official_session_order_status, parse_chat_message, parse_contact, parse_fetched_user_info, parse_push_messages, parse_push_read_receipts, parse_typing_push_chat_ids, push_message_refs};
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn maps_official_message_read_receipt_fields() {
        let base = json!({
            "messageId": "m-1",
            "messageTime": 1_700_000_000_000_i64,
            "message": {
                "messageId": "m-1",
                "msgReadStatusSetting": 1,
                "extension": { "senderUserId": "seller@goofish", "reminderTitle": "卖家" }
            }
        });
        let mut read = base.clone();
        read["readStatus"] = json!(2);
        assert_eq!(
            parse_chat_message(&read, "account", "buyer", "seller")
                .expect("read message")
                .read_status,
            "read"
        );

        let mut unread = base.clone();
        unread["readStatus"] = json!(0);
        assert_eq!(
            parse_chat_message(&unread, "account", "buyer", "seller")
                .expect("unread message")
                .read_status,
            "unread"
        );

        let mut unsupported = base;
        unsupported["message"]["msgReadStatusSetting"] = json!(2);
        unsupported["readStatus"] = json!(2);
        assert_eq!(
            parse_chat_message(&unsupported, "account", "buyer", "seller")
                .expect("unsupported message")
                .read_status,
                "unsupported"
        );
    }

    #[test]
    fn maps_nested_and_string_read_receipt_fields() {
        let mut nested = json!({
            "messageId": "m-2",
            "message": {
                "messageId": "m-2",
                "extension": { "senderUserId": "seller@goofish" },
                "receiverMessageStatus": { "readStatus": "2" }
            }
        });
        assert_eq!(
            parse_chat_message(&nested, "account", "buyer", "seller")
                .expect("nested read message")
                .read_status,
            "read"
        );
        nested["message"]["receiverMessageStatus"]["readStatus"] = json!("未读");
        assert_eq!(
            parse_chat_message(&nested, "account", "buyer", "seller")
                .expect("nested unread message")
                .read_status,
            "unread"
        );
    }

    #[test]
    fn keeps_expression_and_product_card_payloads_for_the_ui() {
        let expression = json!({
            "messageTime": 1_700_000_000_000_i64,
            "message": {
                "messageId": "expression-1",
                "extension": { "senderUserId": "buyer@goofish", "reminderTitle": "买家" },
                "content": { "custom": { "data": "{\"contentType\":3,\"expression\":{\"name\":\"[笑脸]\",\"iconUrl\":\"https://img.alicdn.com/face.png\"}}" } }
            }
        });
        let parsed = parse_chat_message(&expression, "account", "buyer", "seller").expect("expression");
        assert_eq!(parsed.content_kind, "expression");
        assert_eq!(parsed.text, "[笑脸]");
        assert_eq!(parsed.media_url, "https://img.alicdn.com/face.png");

        let product = json!({
            "messageTime": 1_700_000_001_000_i64,
            "message": {
                "messageId": "product-1",
                "extension": { "senderUserId": "seller@goofish", "reminderTitle": "卖家" },
                "content": { "custom": { "data": "{\"contentType\":7,\"itemCard\":{\"item\":{\"itemId\":\"123\",\"mainPic\":\"https://img.alicdn.com/item.png\",\"price\":\"¥69.00\",\"title\":\"官方商品\"},\"itemTip\":\"商品详情\"}}" } }
            }
        });
        let parsed = parse_chat_message(&product, "account", "buyer", "seller").expect("product");
        assert_eq!(parsed.content_kind, "product");
        assert_eq!(parsed.card_title, "官方商品");
        assert_eq!(parsed.card_price, "¥69.00");
        assert_eq!(parsed.media_url, "https://img.alicdn.com/item.png");
        assert_eq!(parsed.target_url, "https://www.goofish.com/item?id=123");

        let trade_card = json!({
            "messageTime": 1_700_000_002_000_i64,
            "message": {
                "messageId": "trade-card-1",
                "extension": { "senderUserId": "buyer@goofish", "reminderTitle": "买家" },
                "content": { "custom": { "data": "{\"contentType\":26,\"dxCard\":{\"item\":{\"main\":{\"exContent\":{\"title\":\"我已拍下，待付款\",\"desc\":\"请双方沟通及时确认价格\",\"priceText\":\"58.00\",\"imageUrl\":\"https://img.alicdn.com/trade.png\"}}}}}" } }
            }
        });
        let parsed = parse_chat_message(&trade_card, "account", "buyer", "seller").expect("trade card");
        assert_eq!(parsed.content_kind, "product");
        assert_eq!(parsed.card_title, "我已拍下，待付款");
        assert_eq!(parsed.card_subtitle, "请双方沟通及时确认价格");
        assert_eq!(parsed.card_price, "58.00");
        assert_eq!(parsed.media_url, "https://img.alicdn.com/trade.png");
    }

    #[test]
    fn reads_the_official_red_reminder_as_the_session_trade_status() {
        let session = json!({
            "singleChatUserConversation": {
                "singleChatConversation": {
                    "cid": "57003034974@goofish",
                    "pairFirst": "767300580@goofish",
                    "pairSecond": "2654721200@goofish",
                    "extension": { "itemId": "846460351664" }
                },
                "userExtension": { "redReminder": "交易成功", "redReminderStyle": "10" },
                "summary": { "redReminder": "交易成功", "summaryContent": "发" },
                "lastMessage": {
                    "message": {
                        "extension": { "senderUserId": "767300580", "reminderTitle": "多蜂科技", "reminderContent": "发" },
                        "content": { "contentType": 1, "text": { "text": "发" } }
                    }
                }
            }
        });

        let contact = parse_contact(&session, "account-1", "767300580").expect("contact");
        assert_eq!(contact.order_status, "交易成功");
        assert!(contact.buyer_tag.is_empty());
        assert_eq!(contact.latest_message, "发");
    }

    #[test]
    fn ignores_a_normal_message_reminder_when_it_is_not_a_trade_status() {
        let session = json!({
            "userExtension": { "redReminder": "发" },
            "summary": { "redReminder": "发" }
        });
        assert!(official_session_order_status(&session, &session).is_empty());
    }

    #[test]
    fn supports_the_older_structured_trade_status_extension() {
        let session = json!({
            "userInfo": { "ext": { "tradeStatus": "{\"statusDesc\":\"交易关闭\"}" } }
        });
        assert_eq!(
            official_session_order_status(&session, &session),
            "交易关闭"
        );
    }

    #[test]
    fn reads_json_encoded_official_session_extensions() {
        let session = json!({
            "user_extension": "{\"redReminder\":\"交易关闭\",\"redReminderStyle\":\"11\"}",
            "summary": "{\"redReminder\":\"交易关闭\"}"
        });
        assert_eq!(
            official_session_order_status(&session, &session),
            "交易关闭"
        );
    }

    #[test]
    fn reads_fans_tag_and_fish_nick_from_the_official_user_query_response() {
        let response = json!({
            "data": {
                "userInfo": {
                    "fishNick": "天空朦胧的鳗鱼",
                    "nick": "承***叔",
                    "logo": "//img.alicdn.com/avatar.jpg",
                    "ext": { "fansTag": "已购粉" }
                }
            }
        });
        let profile = parse_fetched_user_info(&response, "cookie".to_owned());
        assert_eq!(profile.display_name, "天空朦胧的鳗鱼");
        assert_eq!(profile.avatar_url, "https://img.alicdn.com/avatar.jpg");
        assert_eq!(profile.fans_tag, "已购粉");
        assert!(profile.trade_status.is_empty());
    }

    #[test]
    fn decodes_numeric_websocket_push_message_before_remote_sync() {
        let push = json!({
            "lwp": "/s/sync",
            "body": {
                "syncPushPackage": {
                    "data": [{
                        "data": base64::engine::general_purpose::STANDARD.encode(
                            r#"{"1":{"2":"2654721200@goofish","5":1789801433358,"6":{"3":{"5":{"contentType":1,"text":{"text":"实时消息"}}}},"10":{"senderUserId":"2654721200","senderNick":"天空朦胧的鳗鱼","reminderContent":"实时消息"}}}"#
                        )
                    }]
                }
            }
        });
        let messages = parse_push_messages(&push, "account-1", "unb=767300580");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].chat_id, "2654721200");
        assert_eq!(messages[0].text, "实时消息");
        assert_eq!(messages[0].direction, "incoming");
    }

    #[test]
    fn finds_conversation_from_typing_push() {
        let push = json!({
            "lwp": "/s/para",
            "body": { "cid": "2654721200@goofish", "userId": "seller" }
        });
        assert_eq!(parse_typing_push_chat_ids(&push), vec!["2654721200"]);
    }

    #[test]
    fn finds_conversation_from_numeric_typing_push_payload() {
        let push = json!({
            "lwp": "/s/para",
            "body": { "syncPushPackage": { "data": [{
                "data": base64::engine::general_purpose::STANDARD.encode(r#"{"1":"2654721200@goofish","2":"typing"}"#)
            }] } }
        });
        assert_eq!(parse_typing_push_chat_ids(&push), vec!["2654721200"]);
    }

    #[test]
    fn finds_conversation_from_direct_typing_data_payload() {
        let push = json!({
            "lwp": "/s/para",
            "body": { "data": base64::engine::general_purpose::STANDARD.encode(r#"{"conversationId":"2654721200@goofish","command":0}"#) }
        });
        assert_eq!(parse_typing_push_chat_ids(&push), vec!["2654721200"]);
    }

    #[test]
    fn decodes_official_msgpack_push_envelope() {
        let raw = "hAGzNjY5MTk1NTUzMzJAZ29vZmlzaAIBA7E0MzEzNjczMTA5ODE5LlBOTQTPAAABoLh6iPQ=";
        let decoded = decode_push_payload(raw).expect("msgpack payload");
        eprintln!("decoded={decoded}");
        let push = json!({
            "lwp": "/s/sync",
            "body": { "syncPushPackage": { "data": [{
                "data": raw
            }] } }
        });
        let refs = push_message_refs(&push);
        assert_eq!(refs, vec![("66919555332".to_owned(), "4313673109819.PNM".to_owned())]);
        assert!(parse_push_messages(&push, "account-1", "unb=767300580").is_empty());
    }

    #[test]
    fn decodes_numeric_read_receipt_push() {
        let decoded = json!({
            "1": ["4077151826249.PNM", "4066820235744.PNM"],
            "2": 2,
            "3": "60585751957@goofish",
            "4": 1,
            "5": "1776770953455"
        });
        let receipts = parse_push_read_receipts(&decoded);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].chat_id, "60585751957");
        assert_eq!(receipts[0].message_ids, ["4077151826249.PNM", "4066820235744.PNM"]);
        assert_eq!(receipts[0].status, 1);
        assert_eq!(receipts[0].timestamp, "1776770953455");
        assert!(parse_push_messages(&decoded, "account-1", "unb=767300580").is_empty());
    }

    #[test]
    fn decodes_plain_json_read_receipt_envelope() {
        let raw = r#"{"1":["4304934168351.PNM"],"2":2,"3":"57003034974@goofish","4":1,"5":"1776770953455"}"#;
        let push = json!({
            "lwp": "/s/sync",
            "body": { "syncPushPackage": { "data": [{ "data": raw }] } }
        });
        let receipts = parse_push_read_receipts(&push);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].chat_id, "57003034974");
        assert_eq!(receipts[0].message_ids, ["4304934168351.PNM"]);
        assert_eq!(receipts[0].status, 1);
    }

    #[test]
    fn reuses_the_same_im_device_id_for_one_account() {
        assert_eq!(device_id("767300580"), device_id("767300580"));
        assert_ne!(device_id("767300580"), device_id("2654721200"));
        assert!(Uuid::parse_str(&device_id("767300580")).is_ok());
    }

    #[test]
    fn only_acknowledges_server_pushes_not_rpc_responses() {
        let response = json!({ "code": 200, "headers": { "mid": "1" } });
        assert!(ack_message(&response).is_none());
        let push = json!({ "lwp": "/s/sync", "headers": { "mid": "2", "sid": "s" } });
        assert_eq!(
            ack_message(&push).expect("push ack"),
            r#"{"code":200,"headers":{"mid":"2","sid":"s"}}"#
        );
    }
}
