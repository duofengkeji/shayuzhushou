use aes_gcm::aead::{rand_core::RngCore, Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, fs, sync::Mutex};
use tauri::{Emitter, Manager};
use uuid::Uuid;

mod xianyu_im_local;
mod xianyu_local;

struct AppState {
    db: Mutex<Connection>,
    secret_key: [u8; 32],
    chat_listeners: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
    im_request_senders: Mutex<HashMap<String, xianyu_im_local::ImRequestSender>>,
    im_statuses: Mutex<HashMap<String, String>>,
    im_validation_urls: Mutex<HashMap<String, String>>,
    im_validation_cookies: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatImEvent {
    account_id: String,
    requires_sync: bool,
    chat_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImStatusEvent {
    account_id: String,
    status: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImVerificationState {
    required: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppLog {
    id: i64,
    created_at: String,
    level: String,
    category: String,
    account_id: String,
    message: String,
}

fn append_app_log(app: &tauri::AppHandle, level: &str, category: &str, account_id: &str, message: &str) {
    // Keep diagnostics local and bounded. Messages intentionally contain only
    // protocol stage/error text; cookies, tokens and server payloads never
    // enter the application log.
    let entry = AppLog {
        id: 0,
        created_at: Utc::now().to_rfc3339(),
        level: level.to_owned(),
        category: category.to_owned(),
        account_id: account_id.to_owned(),
        message: message.chars().take(600).collect(),
    };
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(conn) = state.db.lock() {
            let _ = conn.execute(
                "INSERT INTO app_logs (created_at, level, category, account_id, message) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![entry.created_at, entry.level, entry.category, entry.account_id, entry.message],
            );
            let _ = conn.execute(
                "DELETE FROM app_logs WHERE id NOT IN (SELECT id FROM app_logs ORDER BY id DESC LIMIT 2000)",
                [],
            );
        }
    }
    let _ = app.emit("app-log", entry);
}

fn update_im_status(app: &tauri::AppHandle, account_id: &str, status: &str, message: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut statuses) = state.im_statuses.lock() {
            statuses.insert(account_id.to_owned(), status.to_owned());
        }
    }
    let level = if message.contains("失败") || message.contains("断开") || message.contains("错误") || message.contains("拒绝") {
        "error"
    } else if status == "connecting" {
        "warn"
    } else {
        "info"
    };
    append_app_log(app, level, "IM", account_id, message);
    let _ = app.emit(
        "im-status",
        ImStatusEvent {
            account_id: account_id.to_owned(),
            status: status.to_owned(),
            message: message.to_owned(),
        },
    );
}

/// Removes a listener that has terminated permanently.  Keeping its completed
/// JoinHandle in the registry makes later `start_chat_listener` calls think
/// the account is still connected, which prevents IM from restarting after a
/// fresh QR login.
fn clear_terminated_chat_listener(app: &tauri::AppHandle, account_id: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if let Ok(mut listeners) = state.chat_listeners.lock() {
        listeners.remove(account_id);
    }
    if let Ok(mut senders) = state.im_request_senders.lock() {
        senders.remove(account_id);
    };
}

fn validation_window_label(account_id: &str) -> String {
    format!(
        "im-verification-{}",
        account_id.replace(|value: char| !value.is_ascii_alphanumeric(), "-")
    )
}

fn is_xianyu_official_url(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    url.scheme() == "https"
        && (host == "goofish.com"
            || host.ends_with(".goofish.com")
            || host == "taobao.com"
            || host.ends_with(".taobao.com"))
}

fn session_cookie_entries(cookie: &str) -> Vec<(String, String)> {
    cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .filter(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty())
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    id: String,
    display_name: String,
    alias: String,
    platform: String,
    status: String,
    last_sync_at: String,
    product_count: i64,
    order_count: i64,
    source_url: String,
    remote_account_id: String,
    conversation_name: String,
    avatar_url: String,
    member_name: String,
}

#[derive(Debug, Clone, Default)]
struct AccountProfile {
    nickname: String,
    member_name: String,
    avatar_url: String,
    cookie: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Product {
    id: String,
    account_id: String,
    title: String,
    image_url: String,
    price: f64,
    stock: i64,
    status: String,
    updated_at: String,
    tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct QuickReplyImage {
    name: String,
    mime_type: String,
    data_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuickReply {
    id: String,
    account_id: String,
    title: String,
    content: String,
    short_code: String,
    images: Vec<QuickReplyImage>,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Order {
    id: String,
    account_id: String,
    order_no: String,
    item_id: String,
    item_image_url: String,
    product_title: String,
    specification: String,
    buyer_masked_name: String,
    amount: f64,
    status_code: String,
    status: String,
    shipping_refund_status: String,
    refund_amount: f64,
    created_at: String,
    note: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Member {
    id: String,
    account_id: String,
    buyer_id: String,
    display_name: String,
    phone_masked: String,
    address_masked: String,
    phone_available: bool,
    address_available: bool,
    first_order_at: String,
    last_order_at: String,
    order_count: i64,
    paid_order_count: i64,
    total_spend: f64,
    average_order_value: f64,
    last_order_status: String,
    remark: String,
    tags: Vec<String>,
    status: String,
    created_at: String,
    updated_at: String,
    last_synced_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberOrder {
    order: Order,
    matched_by: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrderDetail {
    order: Order,
    paid_at: String,
    shipped_at: String,
    completed_at: String,
    closed_at: String,
    service_fee: Option<f64>,
    refund_amount: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefundDetail {
    order_no: String,
    refund_id: String,
    status: String,
    status_code: String,
    reason: String,
    description: String,
    amount: f64,
    create_time: String,
    timeout_text: String,
    deadline_at: String,
    received_status: String,
    return_goods_status: String,
    buyer_evidence: String,
    freight_status: String,
    customer_service: String,
    buyer_name: String,
    product_title: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefundVerification {
    required: bool,
    verification_url: String,
    auth_token: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CustomerItem {
    item_id: String,
    title: String,
    image_url: String,
    price: String,
    fish_coin: String,
    status: String,
    exposure_count: String,
    view_count: String,
    want_count: String,
    visited_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CustomerProfile {
    account_id: String,
    chat_id: String,
    user_id: String,
    display_name: String,
    avatar_url: String,
    remark: String,
    credit_level: String,
    city: String,
    last_active_text: String,
    good_review_rate: String,
    data_updated_at: String,
    purchase_count: String,
    total_spend: String,
    average_order_value: String,
    current_items: Vec<CustomerItem>,
    favorite_items: Vec<CustomerItem>,
    consulted_items: Vec<CustomerItem>,
    official_synced: bool,
    sync_note: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardStats {
    total_accounts: i64,
    healthy_accounts: i64,
    active_products: i64,
    pending_orders: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncResult {
    account: Account,
    products_changed: usize,
    orders_changed: usize,
    source_connected: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncJob {
    id: String,
    account_id: String,
    resource: String,
    status: String,
    started_at: String,
    finished_at: String,
    error_message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatContactsPage {
    items: Vec<xianyu_im_local::ChatContact>,
    next_cursor: Option<i64>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatMessagesPage {
    items: Vec<xianyu_im_local::ChatMessage>,
    next_cursor: Option<i64>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QrLoginStart {
    success: bool,
    session_id: String,
    qr_code_url: String,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QrLoginStatus {
    success: bool,
    status: String,
    message: String,
    face_qr_url: String,
    verification_url: String,
    account_id: String,
    display_name: String,
    is_new_account: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupData {
    exported_at: String,
    accounts: Vec<Account>,
    products: Vec<Product>,
    orders: Vec<Order>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountInput {
    display_name: String,
    alias: String,
    platform: String,
    status: String,
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    remote_account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductInput {
    account_id: String,
    title: String,
    #[serde(default)]
    image_url: String,
    price: f64,
    stock: i64,
    status: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderInput {
    account_id: String,
    product_title: String,
    buyer_masked_name: String,
    amount: f64,
    status: String,
    note: String,
}

fn to_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn encrypt_secret(secret_key: &[u8; 32], value: &str) -> Result<String, String> {
    let cipher = Aes256Gcm::new_from_slice(secret_key).map_err(to_error)?;
    let mut nonce_bytes = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), value.as_bytes())
        .map_err(|_| "本机会话加密失败".to_owned())?;
    let mut payload = nonce_bytes.to_vec();
    payload.extend(encrypted);
    Ok(format!(
        "enc:v1:{}",
        base64::engine::general_purpose::STANDARD.encode(payload)
    ))
}

fn decrypt_secret(secret_key: &[u8; 32], value: &str) -> Result<String, String> {
    let Some(encoded) = value.strip_prefix("enc:v1:") else {
        return Ok(value.to_owned());
    };
    let payload = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "本机会话密文已损坏".to_owned())?;
    if payload.len() <= 12 {
        return Err("本机会话密文已损坏".to_owned());
    }
    let cipher = Aes256Gcm::new_from_slice(secret_key).map_err(to_error)?;
    let decrypted = cipher
        .decrypt(Nonce::from_slice(&payload[..12]), &payload[12..])
        .map_err(|_| "无法解密本机会话，请重新扫码登录".to_owned())?;
    String::from_utf8(decrypted).map_err(|_| "本机会话内容无效".to_owned())
}

fn mask_phone(value: &str) -> String {
    let digits: String = value.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.len() >= 7 { format!("{}****{}", &digits[..3], &digits[digits.len() - 4..]) } else if value.is_empty() { String::new() } else { "已保存".to_owned() }
}

fn mask_address(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() { return String::new(); }
    if chars.len() <= 8 { return format!("{}******", chars.iter().take(2).collect::<String>()); }
    format!("{}******{}", chars.iter().take(4).collect::<String>(), chars.iter().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect::<String>())
}

fn member_load(conn: &Connection, id: &str, secret_key: &[u8; 32], reveal: bool) -> Result<Member, String> {
    conn.query_row(
        "SELECT id, account_id, buyer_id, display_name, phone_ciphertext, address_ciphertext, first_order_at, last_order_at, order_count, paid_order_count, total_spend, average_order_value, last_order_status, remark, tags, status, created_at, updated_at, last_synced_at FROM members WHERE id = ?1",
        [id],
        |row| {
            let phone: String = row.get(4)?;
            let address: String = row.get(5)?;
            let tags_text: String = row.get(14)?;
            let tags = serde_json::from_str::<Vec<String>>(&tags_text).unwrap_or_default();
            let phone_value = if reveal { decrypt_secret(secret_key, &phone).unwrap_or_default() } else { String::new() };
            let address_value = if reveal { decrypt_secret(secret_key, &address).unwrap_or_default() } else { String::new() };
            Ok(Member {
                id: row.get(0)?, account_id: row.get(1)?, buyer_id: row.get(2)?, display_name: row.get(3)?,
                phone_masked: if reveal { phone_value } else { mask_phone(&decrypt_secret(secret_key, &phone).unwrap_or_default()) },
                address_masked: if reveal { address_value } else { mask_address(&decrypt_secret(secret_key, &address).unwrap_or_default()) },
                phone_available: !phone.is_empty(), address_available: !address.is_empty(),
                first_order_at: row.get(6)?, last_order_at: row.get(7)?, order_count: row.get(8)?, paid_order_count: row.get(9)?,
                total_spend: row.get(10)?, average_order_value: row.get(11)?, last_order_status: row.get(12)?, remark: row.get(13)?, tags,
                status: row.get(15)?, created_at: row.get(16)?, updated_at: row.get(17)?, last_synced_at: row.get(18)?,
            })
        },
    ).map_err(to_error)
}

fn sync_member_from_order(conn: &Connection, secret_key: &[u8; 32], account_id: &str, order_id: &str, order: &Value, display_name: &str, status: &str, created_at: &str) -> Result<(), String> {
    let buyer_id = value_string(order, &["buyer_id", "buyerId", "buyer_user_id", "user_id", "userId"]);
    let phone = nested_value(order, &["receiver_mobile", "receiverMobile", "mobile", "phone", "buyer_phone", "buyerPhone", "tel"]);
    let address = nested_value(order, &["receiver_address", "receiverAddress", "address", "buyer_address", "buyerAddress", "delivery_address"]);
    let name = first_nonempty(nested_value(order, &["receiver_name", "receiverName", "consignee", "receiver", "buyer_name", "buyerName"]), display_name.to_owned());
    if buyer_id.is_empty() && phone.is_empty() && (name.is_empty() || address.is_empty()) { return Ok(()); }
    let mut found: Option<(String, String, String)> = None;
    let mut stmt = conn.prepare("SELECT id, phone_ciphertext, address_ciphertext FROM members WHERE account_id = ?1 AND status <> '隐藏'").map_err(to_error)?;
    let candidates = stmt.query_map([account_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))).map_err(to_error)?;
    for candidate in candidates.flatten() {
        let (id, saved_phone, saved_address) = candidate;
        let phone_match = !phone.is_empty() && decrypt_secret(secret_key, &saved_phone).unwrap_or_default() == phone;
        let address_match = !name.is_empty() && !address.is_empty() && decrypt_secret(secret_key, &saved_address).unwrap_or_default() == address;
        let id_match = !buyer_id.is_empty() && conn.query_row("SELECT buyer_id FROM members WHERE id = ?1", [&id], |row| row.get::<_, String>(0)).unwrap_or_default() == buyer_id;
        if id_match { found = Some((id, "buyer_id".to_owned(), saved_phone)); break; }
        if phone_match { found = Some((id, "phone".to_owned(), saved_phone)); break; }
        if address_match { found = Some((id, "name_address".to_owned(), saved_phone)); break; }
    }
    let now = Utc::now().to_rfc3339();
    let (member_id, matched_by, existing_phone) = found.unwrap_or_else(|| (Uuid::new_v4().to_string(), if !buyer_id.is_empty() { "buyer_id".to_owned() } else if !phone.is_empty() { "phone".to_owned() } else { "name_address".to_owned() }, String::new()));
    let phone_cipher = if phone.is_empty() { existing_phone } else { encrypt_secret(secret_key, &phone)? };
    let address_cipher = if address.is_empty() { String::new() } else { encrypt_secret(secret_key, &address)? };
    conn.execute("INSERT INTO members (id, account_id, buyer_id, display_name, phone_ciphertext, address_ciphertext, first_order_at, last_order_at, last_order_status, created_at, updated_at, last_synced_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?9, ?9) ON CONFLICT(id) DO UPDATE SET buyer_id = CASE WHEN excluded.buyer_id <> '' THEN excluded.buyer_id ELSE members.buyer_id END, display_name = CASE WHEN excluded.display_name <> '' THEN excluded.display_name ELSE members.display_name END, phone_ciphertext = CASE WHEN excluded.phone_ciphertext <> '' THEN excluded.phone_ciphertext ELSE members.phone_ciphertext END, address_ciphertext = CASE WHEN excluded.address_ciphertext <> '' THEN excluded.address_ciphertext ELSE members.address_ciphertext END, last_order_at = CASE WHEN excluded.last_order_at > members.last_order_at THEN excluded.last_order_at ELSE members.last_order_at END, last_order_status = excluded.last_order_status, updated_at = excluded.updated_at, last_synced_at = excluded.last_synced_at", params![member_id, account_id, buyer_id, name, phone_cipher, address_cipher, created_at, status, now]).map_err(to_error)?;
    conn.execute("INSERT OR IGNORE INTO member_order_links (member_id, order_id, matched_by, created_at) VALUES (?1, ?2, ?3, ?4)", params![member_id, order_id, matched_by, now]).map_err(to_error)?;
    let (order_count, paid_count, total): (i64, i64, f64) = conn.query_row("SELECT COUNT(*), SUM(CASE WHEN o.status NOT IN ('待付款','待支付','交易关闭','已关闭','退款关闭','已取消') THEN 1 ELSE 0 END), COALESCE(SUM(CASE WHEN o.status NOT IN ('待付款','待支付','交易关闭','已关闭','退款关闭','已取消') THEN MAX(o.amount - o.refund_amount, 0) ELSE 0 END), 0) FROM member_order_links l JOIN orders o ON o.id = l.order_id WHERE l.member_id = ?1", [&member_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(to_error)?;
    conn.execute("UPDATE members SET order_count = ?1, paid_order_count = ?2, total_spend = ?3, average_order_value = CASE WHEN ?2 > 0 THEN ?3 / ?2 ELSE 0 END WHERE id = ?4", params![order_count, paid_count, total, member_id]).map_err(to_error)?;
    Ok(())
}

fn value_string(value: &Value, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| value.get(*name))
        .map(|value| match value {
            Value::String(text) => text.trim().to_owned(),
            Value::Number(number) => number.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default()
}

fn nested_value_string(value: &Value, names: &[&str]) -> String {
    match value {
        Value::Object(map) => {
            for name in names {
                if let Some(Value::String(text)) = map.get(*name) {
                    let text = text.trim();
                    if !text.is_empty() {
                        return text.to_owned();
                    }
                }
            }
            map.values()
                .map(|value| nested_value_string(value, names))
                .find(|value| !value.is_empty())
                .unwrap_or_default()
        }
        Value::Array(items) => items
            .iter()
            .map(|value| nested_value_string(value, names))
            .find(|value| !value.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn value_number(value: &Value, names: &[&str]) -> f64 {
    names
        .iter()
        .find_map(|name| value.get(*name))
        .and_then(|value| match value {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.parse::<f64>().ok(),
            _ => None,
        })
        .unwrap_or(0.0)
}

fn value_i64(value: &Value, names: &[&str]) -> i64 {
    value_number(value, names).max(0.0) as i64
}

async fn fetch_account_profile(cookie: &str) -> Result<AccountProfile, String> {
    // This is the seller account profile endpoint used after QR login and
    // during sync. It returns the display name, member name and avatar for
    // the currently authenticated account.
    let (body, renewed_cookie) = xianyu_local::mtop_call(
        cookie,
        "mtop.alibaba.idle.seller.platform.query.login.merchant.info",
        "1.0",
        "originaljson",
        &serde_json::json!({}),
    )
    .await?;
    let base = body
        .pointer("/data/data/base")
        .or_else(|| body.pointer("/data/base"))
        .unwrap_or(&body);
    let profile = AccountProfile {
        nickname: nested_value(base, &["displayName"]),
        member_name: nested_value(base, &["displayNick", "nick", "nickname"]),
        avatar_url: nested_value(base, &["avatar", "avatarUrl", "logo"]).replace("http://", "https://"),
        cookie: renewed_cookie,
    };
    if profile.nickname.is_empty() {
        return Err("闲鱼会员资料未返回账号名称".to_owned());
    }
    Ok(profile)
}

fn save_account_profile(
    conn: &Connection,
    account_id: &str,
    profile: &AccountProfile,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO account_profiles (account_id, nickname, member_name, avatar_url, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(account_id) DO UPDATE SET nickname = CASE WHEN excluded.nickname <> '' THEN excluded.nickname ELSE account_profiles.nickname END, member_name = CASE WHEN excluded.member_name <> '' THEN excluded.member_name ELSE account_profiles.member_name END, avatar_url = CASE WHEN excluded.avatar_url <> '' THEN excluded.avatar_url ELSE account_profiles.avatar_url END, updated_at = excluded.updated_at",
        params![account_id, profile.nickname, profile.member_name, profile.avatar_url, now],
    )
    .map_err(to_error)?;
    if !profile.nickname.is_empty() {
        conn.execute(
            "UPDATE accounts SET display_name = ?1 WHERE id = ?2",
            params![profile.nickname, account_id],
        )
        .map_err(to_error)?;
    }
    Ok(())
}

fn value_list(payload: &Value) -> Vec<&Value> {
    if let Some(items) = payload.as_array() {
        return items.iter().collect();
    }
    for key in ["items", "data", "list", "records"] {
        if let Some(items) = payload.get(key).and_then(Value::as_array) {
            return items.iter().collect();
        }
        if let Some(items) = payload
            .get(key)
            .and_then(|value| value.get("items"))
            .and_then(Value::as_array)
        {
            return items.iter().collect();
        }
        if let Some(items) = payload
            .get(key)
            .and_then(|value| value.get("list"))
            .and_then(Value::as_array)
        {
            return items.iter().collect();
        }
    }
    Vec::new()
}

fn nested_value(value: &Value, names: &[&str]) -> String {
    match value {
        Value::Object(map) => {
            for name in names {
                if let Some(value) = map.get(*name) {
                    let text = match value {
                        Value::String(text) => text.trim().to_owned(),
                        Value::Number(number) => number.to_string(),
                        _ => String::new(),
                    };
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
            map.values()
                .map(|value| nested_value(value, names))
                .find(|value| !value.is_empty())
                .unwrap_or_default()
        }
        Value::Array(items) => items
            .iter()
            .map(|value| nested_value(value, names))
            .find(|value| !value.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn item_specification(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            if let Some(lines) = map.get("itemInfoLines").and_then(Value::as_array) {
                let text = lines.iter().filter_map(|line| {
                    let key = nested_value(line, &["key"]);
                    let value = nested_value(line, &["value"]);
                    if key.is_empty() && value.is_empty() { None } else if key.is_empty() { Some(value) } else if value.is_empty() { Some(key) } else { Some(format!("{key}：{value}")) }
                }).collect::<Vec<_>>().join(" / ");
                if !text.is_empty() { return text; }
            }
            map.values().map(item_specification).find(|text| !text.is_empty()).unwrap_or_default()
        }
        Value::Array(items) => items.iter().map(item_specification).find(|text| !text.is_empty()).unwrap_or_default(),
        _ => String::new(),
    }
}

fn first_nonempty(primary: String, fallback: String) -> String {
    if primary.is_empty() { fallback } else { primary }
}

fn customer_items(payload: &Value) -> Vec<CustomerItem> {
    let items = payload
        .pointer("/data/items")
        .and_then(Value::as_array)
        .or_else(|| payload.pointer("/data/data/items").and_then(Value::as_array))
        .or_else(|| payload.pointer("/data/module/items").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter_map(|item| {
            let item_id = nested_value(&item, &["itemId", "item_id", "id"]);
            let title = nested_value(&item, &["title", "itemTitle", "item_title"]);
            let key = if item_id.is_empty() { title.clone() } else { item_id.clone() };
            (!key.is_empty() && seen.insert(key)).then(|| CustomerItem {
                item_id,
                title,
                image_url: nested_value(
                    &item,
                    &[
                        "absoluteMajorPicture",
                        "imageUrl",
                        "image_url",
                        "itemPic",
                        "item_pic",
                    ],
                )
                .replace("http://", "https://"),
                price: nested_value(&item, &["reservePrice", "price", "itemPrice", "item_price"]),
                fish_coin: nested_value(&item, &["xybAmount", "fishCoin", "fish_coin"]),
                status: nested_value(&item, &["itemStatusDesc", "statusDesc", "status"]),
                exposure_count: String::new(),
                view_count: String::new(),
                want_count: String::new(),
                visited_at: nested_value(&item, &["timestamp", "visitTime", "visitedAt", "createdAt"]),
            })
        })
        .collect()
}

fn decoded_json_value(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| value.clone())
}

fn current_customer_item(head_info: &Value, item_stats: &Value, fallback: CustomerItem) -> CustomerItem {
    // `message.headinfo.query` returns the exact model rendered by the
    // official current-item card. Some accounts serialize it as a JSON string.
    let item = decoded_json_value(
        head_info
            .pointer("/data/data")
            .or_else(|| head_info.pointer("/data/module"))
            .unwrap_or(head_info),
    );
    let stats = decoded_json_value(
        item_stats
            .pointer("/data/data")
            .or_else(|| item_stats.pointer("/data/module"))
            .unwrap_or(item_stats),
    );
    // The official response keeps the product title in `itemPreInfo`, while
    // `middle.data.title` is sometimes the formatted price. Read those two
    // fields separately so a price never overwrites the product title.
    let item_pre_info = decoded_json_value(
        item.pointer("/commonData/itemPreInfo")
            .or_else(|| item.pointer("/itemPreInfo"))
            .unwrap_or(&Value::Null),
    );
    let middle = item.pointer("/middle/data").unwrap_or(&item);
    CustomerItem {
        item_id: first_nonempty(nested_value(&item, &["itemId", "item_id", "id"]), fallback.item_id),
        title: first_nonempty(
            first_nonempty(
                nested_value(&item_pre_info, &["title", "itemTitle", "item_title"]),
                nested_value(&item, &["itemTitle", "item_title", "name"]),
            ),
            fallback.title,
        ),
        image_url: first_nonempty(
            first_nonempty(
                nested_value(middle, &["picUrl", "imageUrl", "image_url", "absoluteMajorPicture"]),
                nested_value(&item, &["picUrl", "imageUrl", "image_url", "absoluteMajorPicture"]),
            )
            .replace("http://", "https://"),
            fallback.image_url,
        ),
        price: first_nonempty(
            nested_value(middle, &["skuPrice", "price", "reservePrice", "title"]),
            fallback.price,
        ),
        fish_coin: first_nonempty(
            nested_value(middle, &["xybAmount", "fishCoin", "fish_coin"]),
            fallback.fish_coin,
        ),
        status: first_nonempty(
            nested_value(middle, &["itemStatusDesc", "statusDesc", "status"]),
            fallback.status,
        ),
        exposure_count: nested_value(&stats, &["exposureNum", "exposureCount"]),
        view_count: nested_value(&stats, &["showNum", "viewNum", "viewCount"]),
        want_count: nested_value(&stats, &["wantNum", "wantCount"]),
        visited_at: fallback.visited_at,
    }
}

fn normalize_product_status(raw: String, stock: i64) -> String {
    if stock == 0 {
        return "已下架".to_owned();
    }
    match raw.as_str() {
        "已上架" | "上架" | "online" | "active" | "1" => "已上架".to_owned(),
        "已下架" | "下架" | "offline" | "inactive" | "0" => "已下架".to_owned(),
        _ => "已上架".to_owned(),
    }
}

fn normalize_order_status(raw: String) -> String {
    match raw.as_str() {
        "待付款" | "待支付" | "unpaid" | "pending_payment" => "待付款".to_owned(),
        "待发货" | "待寄件" | "已付款" | "paid" | "pending_shipment" | "TO_DELIVER" | "WAIT_SELLER_SEND_GOODS" | "WAIT_SEND_GOODS" | "WAIT_DELIVERY" => "待发货".to_owned(),
        "已发货" | "已寄件" | "shipped" | "pending_receipt" | "WAIT_BUYER_CONFIRM_GOODS" | "WAIT_BUYER_CONFIRM_RECEIVE" => "待收货".to_owned(),
        "已完成" | "交易成功" | "completed" | "success" | "TRADE_SUCCESS" | "WAIT_SELLER_RATE" => "已完成".to_owned(),
        "退款成功" | "已退款" | "refunded" | "REFUND_SUCCESS" => "已退款".to_owned(),
        "退款中" | "refunding" | "refund" | "REFUNDING" => "退款中".to_owned(),
        "交易关闭" | "已关闭" | "退款关闭" | "cancelled" | "closed" | "TRADE_CLOSED" | "REFUND_CLOSED" => {
            "已关闭".to_owned()
        }
        _ if raw.trim().is_empty() => "待处理".to_owned(),
        _ => raw,
    }
}

fn record_sync_job(
    conn: &Connection,
    account_id: &str,
    status: &str,
    started_at: &str,
    error_message: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_jobs (id, account_id, resource, status, started_at, finished_at, error_message) VALUES (?1, ?2, 'local-connector', ?3, ?4, ?4, ?5)",
        params![Uuid::new_v4().to_string(), account_id, status, started_at, error_message],
    )?;
    Ok(())
}

fn count_for(conn: &Connection, table: &str, account_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE account_id = ?1"),
        [account_id],
        |row| row.get(0),
    )
}

fn get_account(conn: &Connection, account_id: &str) -> rusqlite::Result<Account> {
    let account = conn.query_row(
        "SELECT a.id, a.display_name, a.alias, a.platform, a.status, a.last_sync_at, COALESCE(s.source_url, ''), COALESCE(s.remote_account_id, ''), COALESCE(p.avatar_url, ''), COALESCE(p.member_name, '') FROM accounts a LEFT JOIN account_sources s ON s.account_id = a.id LEFT JOIN account_profiles p ON p.account_id = a.id WHERE a.id = ?1",
        [account_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?, row.get::<_, String>(9)?,
            ))
        },
    )?;
    let conversation_name = conn
        .query_row(
            "SELECT conversation_name FROM conversation_preferences WHERE account_id = ?1",
            [account_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    Ok(Account {
        id: account.0,
        display_name: account.1,
        alias: account.2,
        platform: account.3,
        status: account.4,
        last_sync_at: account.5,
        product_count: count_for(conn, "products", account_id)?,
        order_count: count_for(conn, "orders", account_id)?,
        source_url: account.6,
        remote_account_id: account.7,
        conversation_name,
        avatar_url: account.8,
        member_name: account.9,
    })
}

fn initialize_database(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS accounts (
          id TEXT PRIMARY KEY, display_name TEXT NOT NULL, alias TEXT NOT NULL,
          platform TEXT NOT NULL, status TEXT NOT NULL, last_sync_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS products (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, title TEXT NOT NULL,
          image_url TEXT NOT NULL DEFAULT '',
          price REAL NOT NULL, stock INTEGER NOT NULL, status TEXT NOT NULL,
          updated_at TEXT NOT NULL, tags TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS orders (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, order_no TEXT NOT NULL,
          item_id TEXT NOT NULL DEFAULT '', item_image_url TEXT NOT NULL DEFAULT '',
          buyer_id TEXT NOT NULL DEFAULT '',
          product_title TEXT NOT NULL, specification TEXT NOT NULL DEFAULT '', buyer_masked_name TEXT NOT NULL,
          amount REAL NOT NULL, refund_amount REAL NOT NULL DEFAULT 0, status_code TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, shipping_refund_status TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, note TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS members (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, buyer_id TEXT NOT NULL DEFAULT '',
          display_name TEXT NOT NULL DEFAULT '', phone_ciphertext TEXT NOT NULL DEFAULT '',
          address_ciphertext TEXT NOT NULL DEFAULT '', first_order_at TEXT NOT NULL DEFAULT '',
          last_order_at TEXT NOT NULL DEFAULT '', order_count INTEGER NOT NULL DEFAULT 0,
          paid_order_count INTEGER NOT NULL DEFAULT 0, total_spend REAL NOT NULL DEFAULT 0,
          average_order_value REAL NOT NULL DEFAULT 0, last_order_status TEXT NOT NULL DEFAULT '',
          remark TEXT NOT NULL DEFAULT '', tags TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL DEFAULT '正常',
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_synced_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS member_order_links (
          member_id TEXT NOT NULL, order_id TEXT NOT NULL, matched_by TEXT NOT NULL,
          created_at TEXT NOT NULL, PRIMARY KEY (member_id, order_id)
        );
        CREATE TABLE IF NOT EXISTS member_audit_logs (
          id INTEGER PRIMARY KEY AUTOINCREMENT, member_id TEXT NOT NULL, action TEXT NOT NULL,
          occurred_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_jobs (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, resource TEXT NOT NULL,
          status TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT, error_message TEXT
        );
        CREATE TABLE IF NOT EXISTS account_sources (
          account_id TEXT PRIMARY KEY, source_url TEXT NOT NULL, remote_account_id TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS account_credentials (
          account_id TEXT PRIMARY KEY, cookie TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS account_profiles (
          account_id TEXT PRIMARY KEY, nickname TEXT NOT NULL DEFAULT '', member_name TEXT NOT NULL DEFAULT '',
          avatar_url TEXT NOT NULL DEFAULT '', avatar_source TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS chat_contacts (
          account_id TEXT NOT NULL, chat_id TEXT NOT NULL, other_user_id TEXT NOT NULL,
          other_user_name TEXT NOT NULL, avatar_url TEXT NOT NULL DEFAULT '',
          item_id TEXT NOT NULL, item_title TEXT NOT NULL,
          item_image_url TEXT NOT NULL DEFAULT '', order_status TEXT NOT NULL DEFAULT '',
          buyer_tag TEXT NOT NULL DEFAULT '',
          profile_synced_at TEXT NOT NULL DEFAULT '',
          latest_message TEXT NOT NULL, latest_message_time TEXT NOT NULL, unread_count INTEGER NOT NULL,
          PRIMARY KEY (account_id, chat_id)
        );
        CREATE TABLE IF NOT EXISTS customer_remarks (
          account_id TEXT NOT NULL, chat_id TEXT NOT NULL, remark TEXT NOT NULL DEFAULT '',
          updated_at TEXT NOT NULL, PRIMARY KEY (account_id, chat_id)
        );
        CREATE TABLE IF NOT EXISTS chat_messages (
          account_id TEXT NOT NULL, id TEXT NOT NULL, chat_id TEXT NOT NULL,
          sender_user_id TEXT NOT NULL, sender_user_name TEXT NOT NULL, direction TEXT NOT NULL,
          content_kind TEXT NOT NULL, text TEXT NOT NULL, media_url TEXT NOT NULL DEFAULT '', sent_at TEXT NOT NULL, send_status TEXT NOT NULL,
          read_status TEXT NOT NULL DEFAULT 'unknown', card_title TEXT NOT NULL DEFAULT '',
          card_subtitle TEXT NOT NULL DEFAULT '', card_price TEXT NOT NULL DEFAULT '',
          target_url TEXT NOT NULL DEFAULT '',
          PRIMARY KEY (account_id, id)
        );
        CREATE TABLE IF NOT EXISTS chat_emojis (
          account_id TEXT NOT NULL, icon_alias TEXT NOT NULL, icon_url TEXT NOT NULL,
          updated_at TEXT NOT NULL, PRIMARY KEY (account_id, icon_alias)
        );
        CREATE TABLE IF NOT EXISTS quick_replies (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, title TEXT NOT NULL,
          content TEXT NOT NULL, short_code TEXT NOT NULL, images TEXT NOT NULL DEFAULT '[]',
          updated_at TEXT NOT NULL, UNIQUE(account_id, short_code)
        );
        CREATE TABLE IF NOT EXISTS chat_read_state (
          account_id TEXT NOT NULL, chat_id TEXT NOT NULL, read_at TEXT NOT NULL,
          PRIMARY KEY (account_id, chat_id)
        );
        CREATE TABLE IF NOT EXISTS conversation_preferences (
          account_id TEXT PRIMARY KEY, conversation_name TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS app_logs (
          id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL,
          level TEXT NOT NULL, category TEXT NOT NULL, account_id TEXT NOT NULL DEFAULT '',
          message TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_chat_messages_conversation ON chat_messages(account_id, chat_id, sent_at);
        CREATE INDEX IF NOT EXISTS idx_app_logs_created_at ON app_logs(id DESC);
        ",
    )?;
    let has_avatar_source = conn
        .prepare("PRAGMA table_info(account_profiles)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "avatar_source");
    if !has_avatar_source {
        conn.execute("ALTER TABLE account_profiles ADD COLUMN avatar_source TEXT NOT NULL DEFAULT ''", [])?;
    }
    let has_member_name = conn
        .prepare("PRAGMA table_info(account_profiles)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "member_name");
    if !has_member_name {
        conn.execute("ALTER TABLE account_profiles ADD COLUMN member_name TEXT NOT NULL DEFAULT ''", [])?;
    }
    let _ = conn.execute(
        "ALTER TABLE products ADD COLUMN image_url TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN item_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN buyer_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN item_image_url TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN status_code TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN specification TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN shipping_refund_status TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE orders ADD COLUMN refund_amount REAL NOT NULL DEFAULT 0",
        [],
    );
    conn.execute("CREATE INDEX IF NOT EXISTS idx_members_account ON members(account_id, updated_at DESC)", [])?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_member_links_order ON member_order_links(order_id)", [])?;
    // Older builds did not remove member rows when an account was deleted.
    // Drop those orphaned rows during startup so the UI cannot show an unknown shop.
    conn.execute("DELETE FROM member_order_links WHERE member_id IN (SELECT id FROM members WHERE account_id NOT IN (SELECT id FROM accounts))", [])?;
    conn.execute("DELETE FROM member_audit_logs WHERE member_id IN (SELECT id FROM members WHERE account_id NOT IN (SELECT id FROM accounts))", [])?;
    conn.execute("DELETE FROM members WHERE account_id NOT IN (SELECT id FROM accounts)", [])?;
    // One-time backfill for local records created before status_code existed.
    // Subsequent account syncs overwrite this with the official order status ID.
    conn.execute(
        "UPDATE orders SET status_code = CASE
           WHEN status IN ('待付款', '待支付') THEN 'WAIT_PAY'
           WHEN status IN ('待发货', '待寄件', '已付款') THEN 'WAIT_SHIP'
           WHEN status IN ('待收货', '已发货', '已寄件') THEN 'SHIPPED'
           WHEN status = '退款中' THEN 'REFUNDING'
           WHEN status IN ('交易关闭', '已关闭', '退款关闭') THEN 'CLOSED'
           WHEN status IN ('交易成功', '已完成') THEN 'SUCCESS'
           ELSE status_code END
         WHERE status_code = ''",
        [],
    )?;
    let _ = conn.execute(
        "ALTER TABLE chat_contacts ADD COLUMN avatar_url TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_contacts ADD COLUMN item_image_url TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_contacts ADD COLUMN order_status TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_contacts ADD COLUMN buyer_tag TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_contacts ADD COLUMN profile_synced_at TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN media_url TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN read_status TEXT NOT NULL DEFAULT 'unknown'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN card_title TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN card_subtitle TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN card_price TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN target_url TEXT NOT NULL DEFAULT ''",
        [],
    );
    conn.execute(
        "DELETE FROM chat_messages WHERE rowid IN (
          SELECT CASE
            WHEN older.id LIKE '%.PNM' AND newer.id NOT LIKE '%.PNM' THEN newer.rowid
            WHEN newer.id LIKE '%.PNM' AND older.id NOT LIKE '%.PNM' THEN older.rowid
            ELSE newer.rowid
          END
          FROM chat_messages older
          JOIN chat_messages newer
            ON newer.account_id = older.account_id
           AND newer.chat_id = older.chat_id
           AND newer.direction = older.direction
           AND newer.content_kind = older.content_kind
           AND newer.text = older.text
           AND newer.rowid > older.rowid
           AND ABS((julianday(newer.sent_at) - julianday(older.sent_at)) * 86400.0) < 1.0
        )",
        [],
    )?;
    Ok(())
}

fn save_account_source(
    conn: &Connection,
    account_id: &str,
    source_url: &str,
    remote_account_id: &str,
) -> rusqlite::Result<()> {
    if source_url.trim().is_empty() && remote_account_id.trim().is_empty() {
        conn.execute(
            "DELETE FROM account_sources WHERE account_id = ?1",
            [account_id],
        )?;
    } else {
        conn.execute(
            "INSERT INTO account_sources (account_id, source_url, remote_account_id) VALUES (?1, ?2, ?3) ON CONFLICT(account_id) DO UPDATE SET source_url = excluded.source_url, remote_account_id = excluded.remote_account_id",
            params![account_id, source_url.trim().trim_end_matches('/'), remote_account_id.trim()],
        )?;
    }
    Ok(())
}

fn ensure_account_exists(conn: &Connection, account_id: &str) -> Result<(), String> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM accounts WHERE id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .map_err(to_error)?;
    if count == 0 {
        return Err("账号不存在或已被删除".to_owned());
    }
    Ok(())
}

#[tauri::command]
fn list_accounts(state: tauri::State<'_, AppState>) -> Result<Vec<Account>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut stmt = conn
        .prepare("SELECT id FROM accounts ORDER BY last_sync_at DESC")
        .map_err(to_error)?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(to_error)?;
    ids.map(|id| get_account(&conn, &id?))
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)
}

#[tauri::command]
fn update_conversation_name(
    account_id: String,
    conversation_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Account, String> {
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    let conversation_name = conversation_name.trim();
    if conversation_name.chars().count() > 30 {
        return Err("会话名称不能超过 30 个字符".to_owned());
    }
    if conversation_name.is_empty() {
        conn.execute(
            "DELETE FROM conversation_preferences WHERE account_id = ?1",
            [&account_id],
        )
        .map_err(to_error)?;
    } else {
        conn.execute(
            "INSERT INTO conversation_preferences (account_id, conversation_name) VALUES (?1, ?2) ON CONFLICT(account_id) DO UPDATE SET conversation_name = excluded.conversation_name",
            params![account_id, conversation_name],
        )
        .map_err(to_error)?;
    }
    get_account(&conn, &account_id).map_err(to_error)
}

#[tauri::command]
fn list_products(
    account_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Product>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let query = "SELECT id, account_id, title, image_url, price, stock, status, updated_at, tags FROM products WHERE (?1 IS NULL OR account_id = ?1) ORDER BY updated_at DESC";
    let mut statement = conn.prepare(query).map_err(to_error)?;
    let rows = statement
        .query_map([account_id], |row| {
            let tag_string: String = row.get(8)?;
            Ok(Product {
                id: row.get(0)?,
                account_id: row.get(1)?,
                title: row.get(2)?,
                image_url: row.get(3)?,
                price: row.get(4)?,
                stock: row.get(5)?,
                status: row.get(6)?,
                updated_at: row.get(7)?,
                tags: tag_string.split(',').map(str::to_owned).collect(),
            })
        })
        .map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

fn validate_quick_reply(title: &str, short_code: &str, images: &[QuickReplyImage]) -> Result<String, String> {
    if title.trim().is_empty() {
        return Err("快捷回复标题不能为空".to_owned());
    }
    let code = short_code.trim().trim_start_matches('/').to_lowercase();
    if code.is_empty() || !code.chars().all(|value| value.is_ascii_alphanumeric() || value == '_' || value == '-') {
        return Err("快捷回复简码只能使用字母、数字、下划线或连字符".to_owned());
    }
    if code.len() > 32 {
        return Err("快捷回复简码不能超过 32 个字符".to_owned());
    }
    if images.len() > 6 {
        return Err("每条快捷回复最多添加 6 张图片".to_owned());
    }
    if images.iter().any(|image| !image.mime_type.starts_with("image/") || image.data_url.len() > 8 * 1024 * 1024) {
        return Err("快捷回复图片格式或大小无效".to_owned());
    }
    Ok(code)
}

fn read_quick_replies(conn: &Connection, account_id: &str) -> Result<Vec<QuickReply>, String> {
    let mut statement = conn.prepare("SELECT id, account_id, title, content, short_code, images, updated_at FROM quick_replies WHERE account_id = ?1 ORDER BY updated_at DESC").map_err(to_error)?;
    let rows = statement.query_map([account_id], |row| {
        let serialized_images: String = row.get(5)?;
        Ok(QuickReply {
            id: row.get(0)?, account_id: row.get(1)?, title: row.get(2)?, content: row.get(3)?, short_code: row.get(4)?,
            images: serde_json::from_str(&serialized_images).unwrap_or_default(), updated_at: row.get(6)?,
        })
    }).map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
fn list_quick_replies(account_id: String, state: tauri::State<'_, AppState>) -> Result<Vec<QuickReply>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    read_quick_replies(&conn, &account_id)
}

#[tauri::command]
fn create_quick_reply(account_id: String, title: String, content: String, short_code: String, images: Vec<QuickReplyImage>, state: tauri::State<'_, AppState>) -> Result<QuickReply, String> {
    let code = validate_quick_reply(&title, &short_code, &images)?;
    if content.trim().is_empty() && images.is_empty() { return Err("快捷回复至少需要文字或图片".to_owned()); }
    let id = format!("QR-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let updated_at = Utc::now().to_rfc3339();
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    conn.execute("INSERT INTO quick_replies (id, account_id, title, content, short_code, images, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![id, account_id, title.trim(), content.trim(), code, serde_json::to_string(&images).map_err(to_error)?, updated_at]).map_err(|error| if error.to_string().contains("UNIQUE") { "该简码已被使用".to_owned() } else { error.to_string() })?;
    read_quick_replies(&conn, &account_id)?.into_iter().find(|reply| reply.id == id).ok_or_else(|| "读取快捷回复失败".to_owned())
}

#[tauri::command]
fn update_quick_reply(id: String, account_id: String, title: String, content: String, short_code: String, images: Vec<QuickReplyImage>, state: tauri::State<'_, AppState>) -> Result<QuickReply, String> {
    let code = validate_quick_reply(&title, &short_code, &images)?;
    if content.trim().is_empty() && images.is_empty() { return Err("快捷回复至少需要文字或图片".to_owned()); }
    let updated_at = Utc::now().to_rfc3339();
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    let changed = conn.execute("UPDATE quick_replies SET title = ?1, content = ?2, short_code = ?3, images = ?4, updated_at = ?5 WHERE id = ?6 AND account_id = ?7", params![title.trim(), content.trim(), code, serde_json::to_string(&images).map_err(to_error)?, updated_at, id, account_id]).map_err(|error| if error.to_string().contains("UNIQUE") { "该简码已被使用".to_owned() } else { error.to_string() })?;
    if changed == 0 { return Err("快捷回复不存在".to_owned()); }
    read_quick_replies(&conn, &account_id)?.into_iter().find(|reply| reply.id == id).ok_or_else(|| "读取快捷回复失败".to_owned())
}

#[tauri::command]
fn delete_quick_reply(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(to_error)?;
    conn.execute("DELETE FROM quick_replies WHERE id = ?1", [id]).map_err(to_error)?;
    Ok(())
}

#[tauri::command]
fn list_orders(
    account_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Order>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let query = "SELECT id, account_id, order_no, item_id, item_image_url, product_title, specification, buyer_masked_name, amount, refund_amount, status_code, status, shipping_refund_status, created_at, note FROM orders WHERE (?1 IS NULL OR account_id = ?1) ORDER BY created_at DESC";
    let mut statement = conn.prepare(query).map_err(to_error)?;
    let rows = statement
        .query_map([account_id], |row| {
            Ok(Order {
                id: row.get(0)?,
                account_id: row.get(1)?,
                order_no: row.get(2)?,
                item_id: row.get(3)?,
                item_image_url: row.get(4)?,
                product_title: row.get(5)?,
                specification: row.get(6)?,
                buyer_masked_name: row.get(7)?,
                amount: row.get(8)?,
                refund_amount: row.get(9)?,
                status_code: row.get(10)?,
                status: row.get(11)?,
                shipping_refund_status: row.get(12)?,
                created_at: row.get(13)?,
                note: row.get(14)?,
            })
        })
        .map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
fn list_members(account_id: Option<String>, state: tauri::State<'_, AppState>) -> Result<Vec<Member>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut statement = conn.prepare("SELECT m.id FROM members m JOIN accounts a ON a.id = m.account_id WHERE (?1 IS NULL OR m.account_id = ?1) ORDER BY m.last_order_at DESC, m.updated_at DESC").map_err(to_error)?;
    let ids = statement.query_map([account_id], |row| row.get::<_, String>(0)).map_err(to_error)?.collect::<Result<Vec<_>, _>>().map_err(to_error)?;
    ids.iter().map(|id| member_load(&conn, id, &state.secret_key, false)).collect()
}

#[tauri::command]
fn reveal_member(id: String, state: tauri::State<'_, AppState>) -> Result<Member, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let now = Utc::now().to_rfc3339();
    conn.execute("INSERT INTO member_audit_logs (member_id, action, occurred_at) VALUES (?1, '查看敏感信息', ?2)", params![id, now]).map_err(to_error)?;
    member_load(&conn, &id, &state.secret_key, true)
}

#[tauri::command]
fn update_member(id: String, remark: String, tags: Vec<String>, state: tauri::State<'_, AppState>) -> Result<Member, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let now = Utc::now().to_rfc3339();
    let tags_json = serde_json::to_string(&tags).map_err(to_error)?;
    let changed = conn.execute("UPDATE members SET remark = ?1, tags = ?2, updated_at = ?3 WHERE id = ?4", params![remark.trim(), tags_json, now, id]).map_err(to_error)?;
    if changed == 0 { return Err("会员不存在".to_owned()); }
    member_load(&conn, &id, &state.secret_key, false)
}

#[tauri::command]
fn member_orders(id: String, state: tauri::State<'_, AppState>) -> Result<Vec<MemberOrder>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut statement = conn.prepare("SELECT o.id, o.account_id, o.order_no, o.item_id, o.item_image_url, o.product_title, o.specification, o.buyer_masked_name, o.amount, o.refund_amount, o.status_code, o.status, o.shipping_refund_status, o.created_at, o.note, l.matched_by FROM member_order_links l JOIN orders o ON o.id = l.order_id WHERE l.member_id = ?1 ORDER BY o.created_at DESC").map_err(to_error)?;
    let rows = statement.query_map([id], |row| Ok(MemberOrder { order: Order { id: row.get(0)?, account_id: row.get(1)?, order_no: row.get(2)?, item_id: row.get(3)?, item_image_url: row.get(4)?, product_title: row.get(5)?, specification: row.get(6)?, buyer_masked_name: row.get(7)?, amount: row.get(8)?, refund_amount: row.get(9)?, status_code: row.get(10)?, status: row.get(11)?, shipping_refund_status: row.get(12)?, created_at: row.get(13)?, note: row.get(14)? }, matched_by: row.get(15)? })).map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
async fn order_detail(
    account_id: String,
    order_no: String,
    state: tauri::State<'_, AppState>,
) -> Result<OrderDetail, String> {
    let order_no = order_no.trim().to_owned();
    if order_no.is_empty() {
        return Err("订单编号不能为空".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let (raw, renewed_cookie) = xianyu_local::fetch_order_detail(&cookie, &order_no).await?;
    let conn = state.db.lock().map_err(to_error)?;
    let mut order = conn.query_row(
        "SELECT id, account_id, order_no, item_id, item_image_url, product_title, specification, buyer_masked_name, amount, refund_amount, status_code, status, shipping_refund_status, created_at, note FROM orders WHERE account_id = ?1 AND order_no = ?2",
        params![account_id, order_no],
        |row| Ok(Order {
            id: row.get(0)?, account_id: row.get(1)?, order_no: row.get(2)?, item_id: row.get(3)?,
            item_image_url: row.get(4)?, product_title: row.get(5)?, specification: row.get(6)?, buyer_masked_name: row.get(7)?, amount: row.get(8)?,
            refund_amount: row.get(9)?, status_code: row.get(10)?, status: row.get(11)?, shipping_refund_status: row.get(12)?, created_at: row.get(13)?, note: row.get(14)?,
        }),
    ).map_err(|_| "本地没有找到该订单，请先同步订单".to_owned())?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    let specification = item_specification(&raw);
    if !specification.is_empty() {
        order.specification = specification;
        conn.execute("UPDATE orders SET specification = ?1 WHERE account_id = ?2 AND order_no = ?3", params![order.specification.clone(), account_id, order_no]).map_err(to_error)?;
    }

    let parse_number = |names: &[&str]| {
        let value = nested_value(&raw, names);
        if value.trim().is_empty() { None } else { value.trim().parse::<f64>().ok() }
    };
    let time = |names: &[&str]| nested_value(&raw, names);
    Ok(OrderDetail {
        order,
        paid_at: time(&["paySuccessTime", "payTime", "paidTime", "paymentTime", "payDate", "payTimeStr", "paidTimeStr", "paymentTimeStr", "payDateStr", "paidAt"]),
        shipped_at: time(&["sendTime", "shippingTime", "deliveryTime", "deliverTime", "consignTime", "sendTimeStr", "shippingTimeStr", "deliveryTimeStr"]),
        completed_at: time(&["successTime", "completeTime", "completedTime", "finishTime", "dealTime", "successTimeStr", "completeTimeStr", "finishTimeStr"]),
        closed_at: time(&["closeTime", "closedTime", "closeTimeStr", "closedTimeStr", "交易关闭时间"]),
        service_fee: parse_number(&["softwareServiceFee", "serviceFee", "sellerServiceFee", "platformServiceFee", "platformFee", "idleServiceFee", "commissionFee", "serviceCharge"]),
        refund_amount: parse_number(&["refundAmount", "refundMoney", "refundFee", "refundPrice", "refundInfoVO.refundAmount", "refundInfo.refundAmount"]),
    })
}

#[tauri::command]
async fn refund_detail(
    account_id: String,
    order_no: String,
    state: tauri::State<'_, AppState>,
) -> Result<RefundDetail, String> {
    let order_no = order_no.trim().to_owned();
    if order_no.is_empty() { return Err("订单编号不能为空".to_owned()); }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let (raw, renewed_cookie) = xianyu_local::fetch_refund_detail(&cookie, &order_no).await?;
    let conn = state.db.lock().map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    let status_code = nested_value(&raw, &["disputeStatus", "refundStatus", "status", "statusCode"]);
    let status = match status_code.as_str() {
        "1" | "2" | "3" => "等待卖家处理",
        "5" => "退款成功",
        _ if status_code.to_ascii_lowercase().contains("success") => "退款成功",
        _ => "退款申请",
    };
    let amount = nested_value(&raw, &["applyMoney", "refundAmount", "applyRefundFee", "refundFee", "amount", "auctionPrice"])
        .parse::<f64>().unwrap_or(0.0);
    let returned_order_no = nested_value(&raw, &["orderId", "orderNo"]);
    let buyer_evidence = {
        let value = nested_value(&raw, &["buyerEvidence", "buyerProof", "evidence", "proof"]);
        if !value.is_empty() { value } else {
            let count = raw.pointer("/detail/data/data/components").and_then(Value::as_array).and_then(|components| components.iter().find(|component| component.get("render").and_then(Value::as_str) == Some("basicRefundInfo"))).and_then(|component| component.pointer("/data/refundProof/proofMultiMediaList")).and_then(Value::as_array).map(|items| items.len()).unwrap_or(0);
            if count > 0 { format!("有凭证（{count}项）") } else { String::new() }
        }
    };
    Ok(RefundDetail {
        order_no: if returned_order_no.is_empty() { order_no.clone() } else { returned_order_no },
        refund_id: nested_value(&raw, &["refundId", "refundNo", "disputeId", "refundOrderId"]),
        status: status.to_owned(), status_code, reason: nested_value(&raw, &["refundReason", "reason", "afterSaleReason"]),
        description: nested_value(&raw, &["refundDesc", "description", "buyerDescription", "refundProofDesc", "desc"]),
        amount, create_time: nested_value(&raw, &["gmtCreatedTime", "createTime", "applyTime", "refundCreateTime"]),
        timeout_text: nested_value(&raw, &["timeoutText", "deadlineText", "sellerHandleTimeout"]),
        deadline_at: nested_value(&raw, &["sellerHandleDeadline", "deadlineTime", "refundDeadline", "timeoutTime", "autoRefundTime", "deadline"]),
        received_status: nested_value(&raw, &["goodsStatusDesc", "receiveStatus", "goodsStatus", "receivedStatus"]),
        return_goods_status: nested_value(&raw, &["returnGoodStatus", "returnGoodsStatus", "returnStatus"]),
        buyer_evidence,
        freight_status: nested_value(&raw, &["postFeeBear", "freightStatus", "postageStatus", "freightHandling", "shippingFeeStatus"]),
        customer_service: nested_value(&raw, &["csStatusDesc", "customerService", "csIntervention", "serviceStatus", "platformIntervention"]),
        buyer_name: nested_value(&raw, &["buyerNick", "buyerName", "userNick"]),
        product_title: nested_value(&raw, &["itemTitle", "title", "productTitle"]),
    })
}

#[tauri::command]
async fn refund_verification(
    account_id: String,
    refund_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<RefundVerification, String> {
    let refund_id = refund_id.trim().to_owned();
    if refund_id.is_empty() { return Err("退款申请缺少退款编号，请先刷新退款详情".to_owned()); }
    let cookie = { let conn = state.db.lock().map_err(to_error)?; local_session(&conn, &account_id, &state.secret_key)? };
    let (body, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.idle.alipay.verify.url.query",
        "1.0",
        "originaljson",
        &serde_json::json!({
            "bizId": refund_id,
            "scene": "REFUND_PC",
            "callBackUrl": "https://seller.goofish.com/?site=COMMONPRO#/seller-trade/refund-manage"
        }),
    ).await?;
    let conn = state.db.lock().map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    let verification_url = nested_value(&body, &["verifyUrl", "verificationUrl"]);
    let auth_token = nested_value(&body, &["token", "authToken"]);
    if verification_url.is_empty() && auth_token.is_empty() {
        return Ok(RefundVerification { required: false, verification_url, auth_token, message: "当前账号无需额外身份核验".to_owned() });
    }
    if verification_url.is_empty() || auth_token.is_empty() {
        return Err("闲鱼核身接口返回参数不完整".to_owned());
    }
    Ok(RefundVerification { required: true, verification_url, auth_token, message: "请完成支付宝身份核验，完成后点击确认提交退款".to_owned() })
}

#[tauri::command]
async fn refund_action(
    account_id: String,
    order_no: String,
    refund_id: String,
    action: String,
    auth_token: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let api_name = match action.as_str() {
        "agree" => "mtop.taobao.idle.merchant.refund.agree.refund",
        "refuse" => "mtop.taobao.idle.merchant.refund.refuse.refund",
        _ => return Err("退款操作无效".to_owned()),
    };
    let cookie = { let conn = state.db.lock().map_err(to_error)?; local_session(&conn, &account_id, &state.secret_key)? };
    let (_, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        api_name,
        "1.0",
        "originaljson",
        &serde_json::json!({ "refundId": refund_id, "authToken": auth_token }),
    ).await?;
    let conn = state.db.lock().map_err(to_error)?;
    if action == "agree" {
        conn.execute("UPDATE orders SET status_code = 'REFUNDED', status = '退款成功' WHERE account_id = ?1 AND order_no = ?2", params![account_id, order_no]).map_err(to_error)?;
    }
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

#[tauri::command]
fn list_related_orders(
    account_id: String,
    chat_id: String,
    status: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Order>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    let (buyer_name, buyer_id, item_id): (String, String, String) = conn.query_row(
        "SELECT other_user_name, other_user_id, item_id FROM chat_contacts WHERE account_id = ?1 AND chat_id = ?2",
        params![account_id, chat_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).map_err(|_| "会话不存在，请先同步会话列表".to_owned())?;
    // Buyer identity is authoritative. Matching only by item_id can attach a
    // different buyer's order when several people purchased the same item.
    // Item matching is retained only for old contacts that have no buyer ID
    // or name at all.
    let mut statement = conn.prepare("SELECT id, account_id, order_no, item_id, item_image_url, product_title, specification, buyer_masked_name, amount, refund_amount, status_code, status, shipping_refund_status, created_at, note FROM orders WHERE account_id = ?1 AND ((?3 <> '' AND buyer_id = ?3) OR (?3 = '' AND ?2 <> '' AND buyer_masked_name = ?2) OR (?3 = '' AND ?2 = '' AND ?4 <> '' AND item_id = ?4)) ORDER BY created_at DESC").map_err(to_error)?;
    let rows = statement.query_map(params![account_id, buyer_name, buyer_id, item_id], |row| Ok(Order {
        id: row.get(0)?, account_id: row.get(1)?, order_no: row.get(2)?, item_id: row.get(3)?, item_image_url: row.get(4)?, product_title: row.get(5)?, specification: row.get(6)?, buyer_masked_name: row.get(7)?, amount: row.get(8)?, refund_amount: row.get(9)?, status_code: row.get(10)?, status: row.get(11)?, shipping_refund_status: row.get(12)?, created_at: row.get(13)?, note: row.get(14)?,
    })).map_err(to_error)?;
    let orders = rows.collect::<Result<Vec<_>, _>>().map_err(to_error)?;
    let filtered = orders.into_iter().filter(|order| {
        let code = order.status_code.trim().to_ascii_uppercase();
        match status.as_str() {
            "ALL" | "" => true,
            "WAIT_PAY" => matches!(code.as_str(), "WAIT_PAY" | "WAIT_BUYER_PAY" | "UNPAID"),
            "WAIT_SHIP" => matches!(code.as_str(), "WAIT_SHIP" | "WAIT_SELLER_SEND_GOODS" | "WAIT_SEND_GOODS" | "WAIT_DELIVERY" | "PAID"),
            "SHIPPED" => matches!(code.as_str(), "SHIPPED" | "WAIT_BUYER_CONFIRM_GOODS" | "WAIT_BUYER_CONFIRM_RECEIVE" | "WAIT_RECEIVE"),
            "REFUNDING" => matches!(code.as_str(), "REFUNDING" | "REFUND" | "IN_REFUND"),
            "CLOSED" => matches!(code.as_str(), "CLOSED" | "TRADE_CLOSED" | "REFUND_CLOSED" | "CANCELLED"),
            "SUCCESS" => matches!(code.as_str(), "SUCCESS" | "TRADE_SUCCESS" | "WAIT_SELLER_RATE" | "COMPLETED"),
            _ => false,
        }
    }).collect();
    Ok(filtered)
}

#[tauri::command]
fn dashboard_stats(state: tauri::State<'_, AppState>) -> Result<DashboardStats, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let count = |sql: &str| {
        conn.query_row(sql, [], |row| row.get::<_, i64>(0))
            .map_err(to_error)
    };
    Ok(DashboardStats {
        total_accounts: count("SELECT COUNT(*) FROM accounts")?,
        healthy_accounts: count("SELECT COUNT(*) FROM accounts WHERE status = '授权有效'")?,
        active_products: count("SELECT COUNT(*) FROM products WHERE status = '已上架'")?,
        pending_orders: count("SELECT COUNT(*) FROM orders WHERE status IN ('待付款', '待发货')")?,
    })
}

#[tauri::command]
fn list_sync_jobs(
    account_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SyncJob>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut statement = conn.prepare(
        "SELECT id, account_id, resource, status, started_at, COALESCE(finished_at, ''), COALESCE(error_message, '') FROM sync_jobs WHERE (?1 IS NULL OR account_id = ?1) ORDER BY started_at DESC LIMIT 100"
    ).map_err(to_error)?;
    let rows = statement
        .query_map([account_id], |row| {
            Ok(SyncJob {
                id: row.get(0)?,
                account_id: row.get(1)?,
                resource: row.get(2)?,
                status: row.get(3)?,
                started_at: row.get(4)?,
                finished_at: row.get(5)?,
                error_message: row.get(6)?,
            })
        })
        .map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
async fn generate_qr_login() -> Result<QrLoginStart, String> {
    let result = xianyu_local::generate_qr().await?;
    Ok(QrLoginStart {
        success: true,
        session_id: result.session_id,
        qr_code_url: result.qr_code_url,
        message: result.message,
    })
}

#[tauri::command]
async fn check_qr_login_status(
    session_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<QrLoginStatus, String> {
    if session_id.trim().is_empty() {
        return Err("二维码会话 ID 不能为空".to_owned());
    }
    let result = xianyu_local::poll_qr(session_id.trim()).await?;
    let mut display_name = result.account_id.clone();
    let mut is_new_account = false;
    if result.status == "success" && !result.account_id.is_empty() && !result.cookie.is_empty() {
        // The profile endpoint is the source of truth for the logged-in
        // account's nickname and avatar. A profile failure must not invalidate
        // an otherwise successful QR login, so retain the cookie either way.
        let profile = fetch_account_profile(&result.cookie).await.ok();
        let conn = state.db.lock().map_err(to_error)?;
        let existing = conn
            .query_row(
                "SELECT account_id FROM account_sources WHERE remote_account_id = ?1 LIMIT 1",
                [&result.account_id],
                |row| row.get::<_, String>(0),
            )
            .ok();
        let local_id = if let Some(id) = existing {
            id
        } else {
            is_new_account = true;
            let id = Uuid::new_v4().to_string();
            let now = Utc::now().to_rfc3339();
            conn.execute("INSERT INTO accounts (id, display_name, alias, platform, status, last_sync_at) VALUES (?1, ?2, '本机扫码登录', '闲鱼', '授权有效', ?3)", params![id, result.account_id, now]).map_err(to_error)?;
            save_account_source(&conn, &id, "", &result.account_id).map_err(to_error)?;
            id
        };
        let latest_cookie = profile
            .as_ref()
            .filter(|profile| !profile.cookie.is_empty())
            .map(|profile| profile.cookie.as_str())
            .unwrap_or(&result.cookie);
        let encrypted_cookie = encrypt_secret(&state.secret_key, latest_cookie)?;
        conn.execute("INSERT INTO account_credentials (account_id, cookie, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(account_id) DO UPDATE SET cookie = excluded.cookie, updated_at = excluded.updated_at", params![local_id, encrypted_cookie, Utc::now().to_rfc3339()]).map_err(to_error)?;
        if let Some(profile) = profile.as_ref() {
            save_account_profile(&conn, &local_id, profile)?;
        }
        display_name = get_account(&conn, &local_id)
            .map_err(to_error)?
            .display_name;
    }
    Ok(QrLoginStatus {
        success: true,
        status: result.status,
        message: result.message,
        face_qr_url: result.verification_qr_url,
        verification_url: result.verification_url,
        account_id: result.account_id,
        display_name,
        is_new_account,
    })
}

#[tauri::command]
fn create_account(
    input: AccountInput,
    state: tauri::State<'_, AppState>,
) -> Result<Account, String> {
    if input.display_name.trim().is_empty() {
        return Err("店铺名称不能为空".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO accounts (id, display_name, alias, platform, status, last_sync_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, input.display_name.trim(), input.alias.trim(), input.platform.trim(), &input.status, now],
    ).map_err(to_error)?;
    save_account_source(&conn, &id, &input.source_url, &input.remote_account_id)
        .map_err(to_error)?;
    get_account(&conn, &id).map_err(to_error)
}

#[tauri::command]
fn update_account(
    id: String,
    input: AccountInput,
    state: tauri::State<'_, AppState>,
) -> Result<Account, String> {
    if input.display_name.trim().is_empty() {
        return Err("店铺名称不能为空".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &id)?;
    conn.execute(
        "UPDATE accounts SET display_name = ?1, alias = ?2, platform = ?3, status = ?4 WHERE id = ?5",
        params![input.display_name.trim(), input.alias.trim(), input.platform.trim(), &input.status, id],
    ).map_err(to_error)?;
    save_account_source(&conn, &id, &input.source_url, &input.remote_account_id)
        .map_err(to_error)?;
    get_account(&conn, &id).map_err(to_error)
}

#[tauri::command]
fn delete_account(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut conn = state.db.lock().map_err(to_error)?;
    delete_account_records(&mut conn, &id)
}

fn delete_account_records(conn: &mut Connection, id: &str) -> Result<(), String> {
    ensure_account_exists(&conn, &id)?;
    let transaction = conn.transaction().map_err(to_error)?;
    transaction
        .execute("DELETE FROM products WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM orders WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM member_order_links WHERE member_id IN (SELECT id FROM members WHERE account_id = ?1)", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM member_audit_logs WHERE member_id IN (SELECT id FROM members WHERE account_id = ?1)", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM members WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM sync_jobs WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM chat_messages WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM customer_remarks WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM chat_emojis WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM chat_contacts WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM chat_read_state WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    transaction
        .execute(
            "DELETE FROM conversation_preferences WHERE account_id = ?1",
            [&id],
        )
        .map_err(to_error)?;
    transaction
        .execute(
            "DELETE FROM account_credentials WHERE account_id = ?1",
            [&id],
        )
        .map_err(to_error)?;
    transaction
        .execute("DELETE FROM account_sources WHERE account_id = ?1", [&id])
        .map_err(to_error)?;
    let deleted = transaction
        .execute("DELETE FROM accounts WHERE id = ?1", [&id])
        .map_err(to_error)?;
    if deleted != 1 {
        return Err("账号删除失败，请刷新账号列表后重试".to_owned());
    }
    transaction.commit().map_err(to_error)?;
    Ok(())
}

fn local_session(
    conn: &Connection,
    account_id: &str,
    secret_key: &[u8; 32],
) -> Result<String, String> {
    ensure_account_exists(conn, account_id)?;
    let cookie = conn
        .query_row(
            "SELECT cookie FROM account_credentials WHERE account_id = ?1",
            [account_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    if cookie.trim().is_empty() {
        return Err("当前账号没有本机会话，请先扫码登录".to_owned());
    }
    decrypt_secret(secret_key, &cookie)
}

fn save_renewed_session(
    conn: &Connection,
    account_id: &str,
    cookie: &str,
    secret_key: &[u8; 32],
) -> Result<(), String> {
    let encrypted_cookie = encrypt_secret(secret_key, cookie)?;
    conn.execute(
        "UPDATE account_credentials SET cookie = ?1, updated_at = ?2 WHERE account_id = ?3",
        params![encrypted_cookie, Utc::now().to_rfc3339(), account_id],
    )
    .map_err(to_error)?;
    Ok(())
}

#[tauri::command]
async fn open_product_detail(
    account_id: String,
    url: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let target = reqwest::Url::parse(url.trim()).map_err(|_| "商品详情地址无效".to_owned())?;
    let host = target.host_str().unwrap_or_default().to_ascii_lowercase();
    if target.scheme() != "https"
        || !(host == "goofish.com"
            || host.ends_with(".goofish.com")
            || host == "taobao.com"
            || host.ends_with(".taobao.com"))
    {
        return Err("只允许打开闲鱼官方商品地址".to_owned());
    }
    let cookie_values = cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .filter(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty())
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let label = format!("product-preview-{}", Uuid::new_v4().simple());
    let preview = tauri::WebviewWindowBuilder::new(
        &app,
        label,
        tauri::WebviewUrl::App("index.html".into()),
    )
        .title("闲鱼宝贝详情")
        .inner_size(390.0, 780.0)
        .min_inner_size(390.0, 780.0)
        .max_inner_size(390.0, 780.0)
        .center()
        .resizable(false)
        .maximizable(false)
        .focused(true)
        .user_agent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1")
        .build()
        .map_err(to_error)?;
    for (name, value) in cookie_values {
        for domain in [".goofish.com", ".taobao.com"] {
            let cookie = tauri::webview::Cookie::build((name.clone(), value.clone()))
                .domain(domain)
                .path("/")
                .secure(true)
                .http_only(true)
                .same_site(tauri::webview::cookie::SameSite::None)
                .build();
            preview.set_cookie(cookie).map_err(to_error)?;
        }
    }
    preview.navigate(target).map_err(to_error)?;
    Ok(())
}

#[tauri::command]
async fn ship_order_without_parcel(
    account_id: String,
    order_no: String,
    trade_text: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let order_no = order_no.trim();
    if order_no.len() < 8 || !order_no.chars().all(|value| value.is_ascii_digit()) {
        return Err("订单编号无效".to_owned());
    }
    let trade_text = trade_text.trim().to_owned();
    if trade_text.chars().count() > 200 {
        return Err("相关描述最多 200 个字".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    // Official seller workbench request: mtop.taobao.idle.logistics.merchant.consign.dummy.
    // This is the "无需寄件" path and does not open a browser window.
    let (_, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idle.logistics.merchant.consign.dummy",
        "1.0",
        "originaljson",
        &serde_json::json!({
            "orderId": order_no,
            "tradeText": trade_text,
            "picList": "[]",
            "newUnconsign": true,
        }),
    )
    .await?;
    let conn = state.db.lock().map_err(to_error)?;
    conn.execute(
        "UPDATE orders SET status_code = 'SHIPPED', status = '待收货' WHERE account_id = ?1 AND order_no = ?2",
        params![account_id, order_no],
    )
    .map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

fn offline_order_infos(page: &Value) -> Result<Vec<Value>, String> {
    let orders = page
        .pointer("/data/bizOrderInfoList")
        .and_then(Value::as_array)
        .ok_or_else(|| "未获取到可发货的订单信息，请刷新后重试".to_owned())?;
    let order_infos = orders
        .iter()
        .filter_map(|order| {
            let common = order.get("commonData").unwrap_or(&Value::Null);
            let item = order.get("itemVO").unwrap_or(&Value::Null);
            let price = order.get("priceVO").unwrap_or(&Value::Null);
            let order_id = value_string(common, &["orderId"]);
            (!order_id.is_empty()).then(|| serde_json::json!({
                "id": order_id,
                "orderId": value_string(common, &["orderId"]),
                "itemInfo": {
                    "itemImage": value_string(item, &["itemPicUrl"]),
                    "itemTitle": value_string(item, &["title"]),
                    "itemId": value_string(common, &["itemId"]),
                    "itemInfoLines": item.get("itemInfoLines").cloned().unwrap_or(Value::Null),
                    "tags": item.get("serviceTags").cloned().unwrap_or(Value::Null),
                },
                "extraInfo": { "orderId": value_string(common, &["orderId"]) },
                "priceAndNum": {
                    "price": value_string(price, &["auctionPrice"]),
                    "num": value_string(price, &["buyNum"]),
                    "unit": "¥",
                },
                "shipTime": common.get("consignTimeInfo").cloned().unwrap_or_else(|| serde_json::json!([{ "text": "-", "textColor": "#111" }])),
                "reportUrl": value_string(common, &["reportUrl"]),
                "needUploadReport": common.get("needUploadReport").is_some_and(|value| value.as_bool() == Some(true) || value.as_str() == Some("true")),
                "needUploadTips": value_string(common, &["needUploadTips"]),
                "categoryMsg": {
                    "selectedCategory": { "categoryId": value_string(item, &["channelCategoryId"]) },
                    "brand": { "brand": { "value": value_string(item, &["brandValueId"]), "valueName": "" } },
                    "model": { "value": value_string(item, &["modelValueId"]), "valueName": "" },
                    "stkType": "1",
                    "skuType": "",
                },
            }))
        })
        .collect::<Vec<_>>();
    if order_infos.is_empty() {
        return Err("未获取到可发货的订单信息，请刷新后重试".to_owned());
    }
    Ok(order_infos)
}

#[tauri::command]
async fn ship_order_with_logistics(
    account_id: String,
    order_no: String,
    mail_no: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let order_no = order_no.trim();
    let mail_no = mail_no.trim();
    if order_no.len() < 8 || !order_no.chars().all(|value| value.is_ascii_digit()) {
        return Err("订单编号无效".to_owned());
    }
    if mail_no.len() < 5 || mail_no.chars().count() > 80 {
        return Err("请输入正确的快递单号".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    // First use the official page-render endpoint to obtain the seller's
    // default address and the exact orderInfos structure for this trade.
    let (page, cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idle.logistics.merchant.consign.page.render",
        "1.0",
        "originaljson",
        &serde_json::json!({ "tradeId": order_no }),
    )
    .await?;
    let address_id = page
        .pointer("/data/commonData/addressId")
        .and_then(|value| value.as_i64().or_else(|| value.as_str().and_then(|text| text.parse().ok())))
        .filter(|value| *value > 0)
        .ok_or_else(|| "未获取到默认寄件地址，请先在闲鱼卖家工作台设置寄件地址".to_owned())?;
    let order_infos = offline_order_infos(&page)?;
    let (guess, cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idle.logistics.guess.mailno",
        "1.0",
        "originaljson",
        &serde_json::json!({ "mailNo": mail_no }),
    )
    .await?;
    let cp_code = guess
        .pointer("/data/unionCode")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "未能识别快递单号，请确认单号后重试".to_owned())?;
    let (_, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idle.logistics.merchant.consign.offline",
        "1.0",
        "originaljson",
        &serde_json::json!({
            "orderInfos": order_infos,
            "mailNo": mail_no,
            "cpCode": cp_code,
            "addressId": address_id,
        }),
    )
    .await?;
    let conn = state.db.lock().map_err(to_error)?;
    conn.execute(
        "UPDATE orders SET status_code = 'SHIPPED', status = '待收货' WHERE account_id = ?1 AND order_no = ?2",
        params![account_id, order_no],
    )
    .map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

#[tauri::command]
async fn remind_order_receipt(
    account_id: String,
    order_no: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let order_no = order_no.trim();
    if order_no.len() < 8 || !order_no.chars().all(|value| value.is_ascii_digit()) {
        return Err("订单编号无效".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    // Same request used by the official seller order list's “提醒收货”.
    let (_, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idle.trade.merchant.batch.remind.confirm",
        "1.0",
        "originaljson",
        &serde_json::json!({
            "orderIdList": [order_no],
            "remindAllOrder": false,
        }),
    )
    .await?;
    let conn = state.db.lock().map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

#[tauri::command]
async fn cancel_order_by_seller(
    account_id: String,
    order_no: String,
    reason: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let order_no = order_no.trim();
    if order_no.len() < 8 || !order_no.chars().all(|value| value.is_ascii_digit()) {
        return Err("订单编号无效".to_owned());
    }
    let reason = reason.trim().to_owned();
    if reason.is_empty() || reason.chars().count() > 50 {
        return Err("请选择关闭订单原因".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    // Official seller workbench request: mtop.taobao.idle.trade.merchant.close.by.seller.
    let (_, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idle.trade.merchant.close.by.seller",
        "2.0",
        "originaljson",
        &serde_json::json!({ "tid": order_no, "bizOrderId": order_no, "closeReason": reason }),
    )
    .await?;
    let conn = state.db.lock().map_err(to_error)?;
    conn.execute(
        "UPDATE orders SET status_code = 'CLOSED', status = '交易关闭' WHERE account_id = ?1 AND order_no = ?2",
        params![account_id, order_no],
    )
    .map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

fn read_chat_contacts(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<xianyu_im_local::ChatContact>, String> {
    let mut statement = conn.prepare("SELECT c.account_id, c.chat_id, c.other_user_id, c.other_user_name, c.avatar_url, c.item_id, c.item_title, c.item_image_url, c.order_status, c.buyer_tag, c.latest_message, c.latest_message_time, CASE WHEN c.latest_message_time <= COALESCE(r.read_at, '') THEN 0 ELSE c.unread_count END, c.profile_synced_at FROM chat_contacts c LEFT JOIN chat_read_state r ON r.account_id = c.account_id AND r.chat_id = c.chat_id WHERE c.account_id = ?1 ORDER BY c.latest_message_time DESC").map_err(to_error)?;
    let mut contacts = statement
        .query_map([account_id], |row| {
            Ok(xianyu_im_local::ChatContact {
                account_id: row.get(0)?,
                chat_id: row.get(1)?,
                other_user_id: row.get(2)?,
                other_user_name: row.get(3)?,
                avatar_url: row.get(4)?,
                item_id: row.get(5)?,
                item_title: row.get(6)?,
                item_image_url: row.get(7)?,
                order_status: row.get(8)?,
                buyer_tag: row.get(9)?,
                latest_message: row.get(10)?,
                latest_message_time: row.get(11)?,
                unread_count: row.get(12)?,
                profile_synced_at: row.get(13)?,
            })
        })
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;
    // A buyer may have several local conversation ids.  Older rows can have
    // been saved with our temporary `用户 <id>` label while another row already
    // contains the official nickname.  Normalize the in-memory list by buyer
    // id before it reaches the UI so stale duplicate rows never surface.
    let mut official_names = std::collections::HashMap::<String, String>::new();
    for contact in &contacts {
        let name = contact.other_user_name.trim();
        let is_fallback = name
            .strip_prefix("用户 ")
            .or_else(|| name.strip_prefix("用户_"))
            .is_some_and(|suffix| suffix.chars().all(|character| character.is_ascii_digit()));
        if !name.is_empty() && !is_fallback {
            official_names
                .entry(contact.other_user_id.clone())
                .or_insert_with(|| contact.other_user_name.clone());
        }
    }
    for contact in &mut contacts {
        let is_fallback = contact
            .other_user_name
            .trim()
            .strip_prefix("用户 ")
            .or_else(|| contact.other_user_name.trim().strip_prefix("用户_"))
            .is_some_and(|suffix| suffix.chars().all(|character| character.is_ascii_digit()));
        if is_fallback {
            if let Some(name) = official_names.get(&contact.other_user_id) {
                contact.other_user_name = name.clone();
            }
        }
    }
    Ok(contacts)
}

#[tauri::command]
fn list_chat_contacts(
    account_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<xianyu_im_local::ChatContact>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    read_chat_contacts(&conn, &account_id)
}

#[tauri::command]
async fn customer_profile(
    account_id: String,
    chat_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<CustomerProfile, String> {
    if chat_id.trim().is_empty() {
        return Err("会话 ID 不能为空".to_owned());
    }
    let (contact, cookie, current_item, local_remark) = {
        let conn = state.db.lock().map_err(to_error)?;
        ensure_account_exists(&conn, &account_id)?;
        let contact = conn
            .query_row(
                "SELECT c.other_user_id, c.other_user_name, c.avatar_url, c.item_id, c.item_title, c.item_image_url, c.latest_message_time, COALESCE(r.remark, '') FROM chat_contacts c LEFT JOIN customer_remarks r ON r.account_id = c.account_id AND r.chat_id = c.chat_id WHERE c.account_id = ?1 AND c.chat_id = ?2",
                params![account_id, chat_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .map_err(|_| "会话不存在，请先同步会话列表".to_owned())?;
        let price = if contact.3.is_empty() {
            String::new()
        } else {
            conn.query_row(
                "SELECT price FROM products WHERE account_id = ?1 AND id LIKE ?2 LIMIT 1",
                params![account_id, format!("%-{}", contact.3)],
                |row| row.get::<_, f64>(0),
            )
            .map(|value| value.to_string())
            .unwrap_or_default()
        };
        let current_item = (!contact.3.is_empty() || !contact.4.is_empty()).then(|| CustomerItem {
            item_id: contact.3.clone(),
            title: contact.4.clone(),
            image_url: contact.5.clone(),
            price,
            fish_coin: String::new(),
            status: String::new(),
            exposure_count: String::new(),
            view_count: String::new(),
            want_count: String::new(),
            visited_at: contact.6.clone(),
        });
        let local_remark = contact.7.clone();
        (contact, local_session(&conn, &account_id, &state.secret_key)?, current_item, local_remark)
    };

    // These are the same read-only APIs used by the official IM right panel.
    // Keep each call independent: a non-critical footprint failure must not
    // hide the basic buyer profile or the currently associated item.
    let mut renewed_cookie = cookie;
    let mut sync_note = Vec::new();
    let profile_response = match xianyu_local::mtop_call(
        &renewed_cookie,
        "mtop.idle.web.user.panel.customer",
        "1.0",
        "originaljson",
        &serde_json::json!({ "sessionId": chat_id }),
    )
    .await
    {
        Ok((body, cookie)) => {
            renewed_cookie = cookie;
            Some(body)
        }
        Err(_) => {
            sync_note.push("买家资料暂未返回");
            None
        }
    };
    let shop_stats_response = match xianyu_local::mtop_call(
        &renewed_cookie,
        "mtop.alibaba.idle.seller.pc.shop.stats.query",
        "1.0",
        "originaljson",
        &serde_json::json!({ "sessionId": chat_id }),
    )
    .await
    {
        Ok((body, cookie)) => {
            renewed_cookie = cookie;
            Some(body)
        }
        Err(_) => None,
    };
    let fetch_footprint = |kind: &'static str, cookie: String, session_id: String| async move {
        xianyu_local::mtop_call(
            &cookie,
            "mtop.taobao.idlemessage.tool.item.query",
            "1.0",
            "originaljson",
            &serde_json::json!({
                "sessionId": session_id,
                "type": kind,
                "currentPage": 1,
                "pageSize": 100,
            }),
        )
        .await
    };
    let favorite_response = fetch_footprint("collect", renewed_cookie.clone(), chat_id.clone()).await;
    let (favorite_items, cookie_after_favorite) = match favorite_response {
        Ok((body, cookie)) => (customer_items(&body), cookie),
        Err(_) => {
            sync_note.push("收藏商品暂未返回");
            (Vec::new(), renewed_cookie.clone())
        }
    };
    renewed_cookie = cookie_after_favorite;
    // The official "TA 咨询过的" tab passes `type: consult` to this API.
    let consulted_response = fetch_footprint("consult", renewed_cookie.clone(), chat_id.clone()).await;
    let (consulted_items, cookie_after_consulted) = match consulted_response {
        Ok((body, cookie)) => (customer_items(&body), cookie),
        Err(_) => {
            sync_note.push("咨询商品暂未返回");
            (Vec::new(), renewed_cookie.clone())
        }
    };
    renewed_cookie = cookie_after_consulted;

    let current_item = if let Some(fallback) = current_item {
        let head_info = xianyu_local::mtop_call(
            &renewed_cookie,
            "mtop.idle.trade.pc.message.headinfo.query",
            "1.0",
            "json",
            &serde_json::json!({
                "itemId": fallback.item_id,
                "sessionId": chat_id,
                "sessionType": 1,
            }),
        )
        .await;
        let (head_info, cookie_after_head) = match head_info {
            Ok((body, cookie)) => (body, cookie),
            Err(_) => (serde_json::json!({}), renewed_cookie.clone()),
        };
        renewed_cookie = cookie_after_head;
        let item_stats = xianyu_local::mtop_call(
            &renewed_cookie,
            "mtop.alibaba.idle.seller.pc.item.stats.query",
            "1.0",
            "originaljson",
            &serde_json::json!({ "itemId": fallback.item_id }),
        )
        .await;
        let (item_stats, cookie_after_stats) = match item_stats {
            Ok((body, cookie)) => (body, cookie),
            Err(_) => (serde_json::json!({}), renewed_cookie.clone()),
        };
        renewed_cookie = cookie_after_stats;
        Some(current_customer_item(&head_info, &item_stats, fallback))
    } else {
        None
    };

    let profile = profile_response
        .as_ref()
        .and_then(|body| body.pointer("/data/module"))
        .unwrap_or(&Value::Null);
    let buyer = profile.pointer("/base/buyer").unwrap_or(profile);
    let stats = shop_stats_response
        .as_ref()
        .and_then(|body| body.pointer("/data/data"))
        .unwrap_or(buyer);
    // `shop.stats.query` is the source rendered by the official buyer panel:
    // payOrderCount / payAmount / payAvgAmount, with its own update tip.
    let data_updated_at = first_nonempty(
        nested_value(stats, &["dataUpdateTips"]),
        nested_value(profile, &["dataUpdateTime", "dataUpdatedAt", "updateTime", "updateDate"]),
    );
    let result = CustomerProfile {
        account_id: account_id.clone(),
        chat_id: chat_id.clone(),
        user_id: contact.0,
        display_name: first_nonempty(nested_value(profile, &["displayName", "nick", "nickname"]), contact.1),
        avatar_url: first_nonempty(nested_value(profile, &["avatar", "avatarUrl", "logo"]), contact.2),
        remark: first_nonempty(nested_value(profile_response.as_ref().unwrap_or(&Value::Null), &["remark", "userRemark", "extUserRemark"]), local_remark),
        credit_level: nested_value(buyer, &["levelName", "creditLevel", "creditName"]),
        city: nested_value(profile, &["cityName", "city", "location", "locationName"]),
        last_active_text: nested_value(profile, &["lastActiveText", "lastVisitText", "activeText", "lastActiveTime"]),
        good_review_rate: nested_value(buyer, &["buyerGoodRatio", "goodReviewRate", "goodRate"]),
        data_updated_at: if profile_response.is_some() {
            if data_updated_at.is_empty() { chrono::Local::now().format("%Y-%m-%d").to_string() } else { data_updated_at }
        } else {
            String::new()
        },
        purchase_count: first_nonempty(
            nested_value(stats, &["payOrderCount", "orderCnt", "orderCount"]),
            nested_value(buyer, &["payOrderCount", "orderCnt", "orderCount"]),
        ),
        total_spend: first_nonempty(
            nested_value(stats, &["payAmount", "totalPayAmount", "totalSpend"]),
            nested_value(buyer, &["payAmount", "totalPayAmount", "totalSpend"]),
        ),
        average_order_value: first_nonempty(
            nested_value(stats, &["payAvgAmount", "payAvg", "averageOrderValue", "avgPayAmount"]),
            nested_value(buyer, &["payAvgAmount", "payAvg", "averageOrderValue", "avgPayAmount"]),
        ),
        current_items: current_item.into_iter().collect(),
        favorite_items,
        consulted_items,
        official_synced: profile_response.is_some() || shop_stats_response.is_some(),
        sync_note: sync_note.join("；"),
    };
    let conn = state.db.lock().map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(result)
}

#[tauri::command]
async fn update_customer_remark(
    account_id: String,
    chat_id: String,
    remark: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if chat_id.trim().is_empty() {
        return Err("会话 ID 不能为空".to_owned());
    }
    let remark = remark.trim().to_owned();
    if remark.chars().count() > 50 {
        return Err("备注最多 50 个字".to_owned());
    }
    let (cookie, secret_key) = {
        let conn = state.db.lock().map_err(to_error)?;
        ensure_account_exists(&conn, &account_id)?;
        (local_session(&conn, &account_id, &state.secret_key)?, state.secret_key)
    };
    let (body, renewed_cookie) = xianyu_local::mtop_call(
        &cookie,
        "mtop.taobao.idlemessage.pc.tool.remark",
        "1.0",
        "json",
        &serde_json::json!({
            "sessionType": 1,
            "sessionId": chat_id.clone(),
            "oprType": true,
            "remark": remark.clone(),
        }),
    )
    .await?;
    let conn = state.db.lock().map_err(to_error)?;
    conn.execute(
        "INSERT INTO customer_remarks (account_id, chat_id, remark, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(account_id, chat_id) DO UPDATE SET remark = excluded.remark, updated_at = excluded.updated_at",
        params![account_id, chat_id, remark, Utc::now().to_rfc3339()],
    )
    .map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &secret_key)?;
    if body.get("ret").and_then(Value::as_array).is_some_and(|ret| ret.iter().any(|item| item.as_str().is_some_and(|text| text.contains("FAIL")))) {
        return Err("闲鱼备注保存失败".to_owned());
    }
    Ok(remark)
}

#[tauri::command]
fn chat_unread_totals(
    state: tauri::State<'_, AppState>,
) -> Result<std::collections::HashMap<String, i64>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut statement = conn
        .prepare(
            "SELECT c.account_id, COALESCE(SUM(CASE WHEN c.latest_message_time <= COALESCE(r.read_at, '') THEN 0 ELSE c.unread_count END), 0) \
             FROM chat_contacts c \
             LEFT JOIN chat_read_state r ON r.account_id = c.account_id AND r.chat_id = c.chat_id \
             GROUP BY c.account_id",
        )
        .map_err(to_error)?;
    let totals = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(to_error)?
        .collect::<Result<std::collections::HashMap<_, _>, _>>()
        .map_err(to_error)?;
    Ok(totals)
}

#[tauri::command]
async fn start_chat_listener(
    account_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        match local_session(&conn, &account_id, &state.secret_key) {
            Ok(cookie) => cookie,
            Err(error) => {
                update_im_status(&app, &account_id, "not_logged_in", &error);
                return Err(error);
            }
        }
    };
    let mut listeners = state.chat_listeners.lock().map_err(to_error)?;
    if listeners.contains_key(&account_id) {
        return Ok(());
    }
    let (request_sender, mut request_receiver) = tokio::sync::mpsc::channel(64);
    state
        .im_request_senders
        .lock()
        .map_err(to_error)?
        .insert(account_id.clone(), request_sender);
    update_im_status(&app, &account_id, "connecting", "正在连接闲鱼 IM");
    #[cfg(debug_assertions)]
    eprintln!("[im] account={} starting listener", account_id);
    let listener_account_id = account_id.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut cookie = cookie;
        let mut reconnect_attempt = 0_u32;
        loop {
            // Match the web service behavior: every reconnect starts from the
            // latest persisted session. Token and verification requests can
            // rotate cookies while this task is alive.
            if let Some(state) = app.try_state::<AppState>() {
                if let Ok(conn) = state.db.lock() {
                    if let Ok(latest_cookie) = local_session(&conn, &listener_account_id, &state.secret_key) {
                        cookie = latest_cookie;
                    }
                }
            }
            let connecting_message = if reconnect_attempt == 0 {
                "正在连接闲鱼 IM".to_owned()
            } else {
                format!("闲鱼 IM 重连中（第 {reconnect_attempt} 次）")
            };
            update_im_status(&app, &listener_account_id, "connecting", &connecting_message);
            let connected_app = app.clone();
            let connected_account_id = listener_account_id.clone();
            let push_app = app.clone();
            let push_account_id = listener_account_id.clone();
            let push_cookie = cookie.clone();
            let trace_app = app.clone();
            let trace_account_id = listener_account_id.clone();
            let connection_started_at = std::time::Instant::now();
            match xianyu_im_local::listen_for_push(&cookie, move || {
                update_im_status(&connected_app, &connected_account_id, "connected", "闲鱼 IM 已连接");
                #[cfg(debug_assertions)]
                eprintln!("[im] account={} connected", connected_account_id);
            }, move |value| {
                let message_refs = xianyu_im_local::push_message_refs(value);
                let typing_chat_ids = xianyu_im_local::parse_typing_push_chat_ids(value);
                let requires_sync = !message_refs.is_empty();
                let chat_id = message_refs.first().map(|(chat_id, _)| chat_id.clone()).or_else(|| typing_chat_ids.first().cloned()).unwrap_or_default();
                let _ = ingest_im_push(&push_app, &push_account_id, &push_cookie, value);
                if !typing_chat_ids.is_empty() {
                    if let Ok(read_refs) = mark_typing_chats_read(&push_app, &push_account_id, &typing_chat_ids) {
                        schedule_typing_readback(&push_app, &push_account_id, &push_cookie, read_refs);
                    }
                }
                let _ = push_app.emit(
                    "chat-im-event",
                    ChatImEvent { account_id: push_account_id.clone(), requires_sync, chat_id },
                );
            }, move |message| {
                append_app_log(&trace_app, "info", "IM 协议", &trace_account_id, message);
            }, &mut request_receiver).await {
                Ok(renewed_cookie) => {
                    cookie = renewed_cookie;
                    let persist_error = if let Some(state) = app.try_state::<AppState>() {
                        state.db.lock().ok().and_then(|conn| {
                            save_renewed_session(
                                &conn,
                                &listener_account_id,
                                &cookie,
                                &state.secret_key,
                            )
                            .err()
                        })
                    } else {
                        None
                    };
                    if let Some(error) = persist_error {
                        append_app_log(
                            &app,
                            "warn",
                            "IM",
                            &listener_account_id,
                            &format!("IM 会话 Cookie 持久化失败：{error}"),
                        );
                    }
                    reconnect_attempt = if connection_started_at.elapsed() >= std::time::Duration::from_secs(60) { 1 } else { reconnect_attempt.saturating_add(1) };
                    update_im_status(&app, &listener_account_id, "connecting", "闲鱼 IM 连接已结束，正在重连");
                }
                Err(error) => {
                    if let Some(verification_url) = xianyu_local::im_validation_url(&error) {
                        #[cfg(debug_assertions)]
                        eprintln!("[im] account={} requires user validation", listener_account_id);
                        // The token request may have rotated session cookies
                        // via Set-Cookie immediately before returning the
                        // validation URL. Pair that fresh cookie with the URL;
                        // submitting the challenge with the previous cookie
                        // produces a generic slider failure page.
                        let verification_cookie = xianyu_im_local::take_renewed_cookie(&listener_account_id)
                            .unwrap_or_else(|| cookie.clone());
                        if let Some(state) = app.try_state::<AppState>() {
                            if let Ok(conn) = state.db.lock() {
                                let _ = save_renewed_session(
                                    &conn,
                                    &listener_account_id,
                                    &verification_cookie,
                                    &state.secret_key,
                                );
                            }
                        }
                        if let Some(state) = app.try_state::<AppState>() {
                            if let Ok(mut urls) = state.im_validation_urls.lock() {
                                urls.insert(listener_account_id.clone(), verification_url.to_owned());
                            }
                            if let Ok(mut cookies) = state.im_validation_cookies.lock() {
                                cookies.insert(listener_account_id.clone(), verification_cookie);
                            }
                        }
                        update_im_status(&app, &listener_account_id, "verification_required", "闲鱼 IM 需要完成风控验证");
                        clear_terminated_chat_listener(&app, &listener_account_id);
                        break;
                    }
                    #[cfg(debug_assertions)]
                    eprintln!("[im] account={} disconnected: {}", listener_account_id, error);
                    if error.contains("AUTH_TOKEN_ILLEGAL") || error.contains("SESSION_EXPIRED") {
                        update_im_status(&app, &listener_account_id, "not_logged_in", "闲鱼登录会话已失效，请重新扫码登录");
                        clear_terminated_chat_listener(&app, &listener_account_id);
                        break;
                    }
                    reconnect_attempt = if connection_started_at.elapsed() >= std::time::Duration::from_secs(60) { 1 } else { reconnect_attempt.saturating_add(1) };
                    update_im_status(&app, &listener_account_id, "connecting", &format!("{error}；正在重连"));
                }
            }
            // This mirrors the official web client's capped exponential
            // reconnect backoff (100 ms, 200 ms, … up to 5 s) and prevents a
            // transient gateway reset from leaving the account "offline" for
            // a fixed 15-second interval.
            let exponent = reconnect_attempt.saturating_sub(1).min(6);
            let delay_ms = (100_u64.saturating_mul(1_u64 << exponent)).min(5_000);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    });
    listeners.insert(account_id, handle);
    Ok(())
}

#[tauri::command]
fn stop_chat_listener(account_id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.chat_listeners.lock().map_err(to_error)?.remove(&account_id) {
        handle.abort();
    }
    state.im_request_senders.lock().map_err(to_error)?.remove(&account_id);
    if let Ok(mut statuses) = state.im_statuses.lock() {
        statuses.insert(account_id, "stopped".to_owned());
    }
    Ok(())
}

#[tauri::command]
fn get_im_statuses(state: tauri::State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    state.im_statuses.lock().map(|statuses| statuses.clone()).map_err(to_error)
}

#[tauri::command]
fn get_im_verification_state(
    account_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ImVerificationState, String> {
    let required = state
        .im_validation_urls
        .lock()
        .map_err(to_error)?
        .contains_key(&account_id);
    Ok(ImVerificationState { required })
}

#[tauri::command]
fn open_im_verification(
    account_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let verification_url = state
        .im_validation_urls
        .lock()
        .map_err(to_error)?
        .get(&account_id)
        .cloned()
        .filter(|url| !url.is_empty())
        .ok_or("验证地址已失效。请先重新连接 IM，以获取新的验证请求。")?;
    let cookie = state
        .im_validation_cookies
        .lock()
        .map_err(to_error)?
        .get(&account_id)
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| {
            let conn = state.db.lock().map_err(to_error)?;
            local_session(&conn, &account_id, &state.secret_key)
        })?;
    let target = reqwest::Url::parse(&verification_url)
        .map_err(|_| "闲鱼返回的验证地址无效".to_owned())?;
    if !is_xianyu_official_url(&target) {
        return Err("只允许打开闲鱼官方风控验证地址".to_owned());
    }
    let label = validation_window_label(&account_id);
    if let Some(window) = app.get_webview_window(&label) {
        window.set_focus().map_err(to_error)?;
        return Ok(());
    }
    let bootstrap_url = reqwest::Url::parse("https://passport.goofish.com/")
        .map_err(to_error)?;
    let window = tauri::WebviewWindowBuilder::new(
        &app,
        label,
        tauri::WebviewUrl::External(bootstrap_url),
    )
    .title("闲鱼安全验证")
    .inner_size(480.0, 760.0)
    .min_inner_size(420.0, 640.0)
    .center()
    .focused(true)
    .user_agent(xianyu_local::web_user_agent())
    .build()
    .map_err(to_error)?;
    let target_host = target.host_str().unwrap_or("passport.goofish.com");
    for (name, value) in session_cookie_entries(&cookie) {
        for domain in [target_host, ".goofish.com"] {
            let cookie = tauri::webview::Cookie::build((name.clone(), value.clone()))
                .domain(domain)
                .path("/")
                .secure(true)
                .http_only(true)
                .same_site(tauri::webview::cookie::SameSite::None)
                .build();
            window.set_cookie(cookie).map_err(to_error)?;
        }
    }
    window.navigate(target).map_err(to_error)?;
    Ok(())
}

#[tauri::command]
async fn complete_im_verification(
    account_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let label = validation_window_label(&account_id);
    let window = app
        .get_webview_window(&label)
        .ok_or("验证窗口未打开。请先打开验证页并完成闲鱼安全验证。")?;
    let x5_cookies = window
        .cookies()
        .map_err(to_error)?
        .into_iter()
        .filter_map(|cookie| {
            let name = cookie.name().to_owned();
            name.to_ascii_lowercase()
                .starts_with("x5")
                .then(|| (name, cookie.value().to_owned()))
        })
        .collect::<Vec<_>>();
    if x5_cookies.is_empty() {
        return Err("尚未检测到 x5sec 风控 Cookie。请先在验证窗口完成挑战后再继续。".to_owned());
    }
    {
        let conn = state.db.lock().map_err(to_error)?;
        let current_cookie = local_session(&conn, &account_id, &state.secret_key)?;
        let mut cookies = session_cookie_entries(&current_cookie)
            .into_iter()
            .collect::<HashMap<_, _>>();
        for (name, value) in x5_cookies {
            cookies.insert(name, value);
        }
        let merged_cookie = cookies
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        save_renewed_session(&conn, &account_id, &merged_cookie, &state.secret_key)?;
    }
    if let Ok(mut urls) = state.im_validation_urls.lock() {
        urls.remove(&account_id);
    }
    if let Ok(mut cookies) = state.im_validation_cookies.lock() {
        cookies.remove(&account_id);
    }
    let _ = window.close();
    stop_chat_listener(account_id.clone(), state.clone())?;
    start_chat_listener(account_id, app, state).await
}

fn account_im_sender(
    state: &AppState,
    account_id: &str,
) -> Result<Option<xianyu_im_local::ImRequestSender>, String> {
    state
        .im_request_senders
        .lock()
        .map(|senders| senders.get(account_id).cloned())
        .map_err(to_error)
}

#[tauri::command]
fn list_app_logs(limit: Option<usize>, state: tauri::State<'_, AppState>) -> Result<Vec<AppLog>, String> {
    let limit = limit.unwrap_or(300).clamp(1, 1_000) as i64;
    let conn = state.db.lock().map_err(to_error)?;
    let mut statement = conn
        .prepare("SELECT id, created_at, level, category, account_id, message FROM app_logs ORDER BY id ASC LIMIT ?1")
        .map_err(to_error)?;
    let logs = statement
        .query_map(params![limit], |row| {
            Ok(AppLog {
                id: row.get(0)?,
                created_at: row.get(1)?,
                level: row.get(2)?,
                category: row.get(3)?,
                account_id: row.get(4)?,
                message: row.get(5)?,
            })
        })
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;
    Ok(logs)
}

fn set_chat_read(conn: &Connection, account_id: &str, chat_id: &str) -> Result<(), String> {
    let read_at = conn
        .query_row(
            "SELECT latest_message_time FROM chat_contacts WHERE account_id = ?1 AND chat_id = ?2",
            params![account_id, chat_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| Utc::now().to_rfc3339());
    conn.execute(
        "INSERT INTO chat_read_state (account_id, chat_id, read_at) VALUES (?1, ?2, ?3) ON CONFLICT(account_id, chat_id) DO UPDATE SET read_at = CASE WHEN excluded.read_at > chat_read_state.read_at THEN excluded.read_at ELSE chat_read_state.read_at END",
        params![account_id, chat_id, read_at],
    )
    .map_err(to_error)?;
    conn.execute(
        "UPDATE chat_contacts SET unread_count = 0 WHERE account_id = ?1 AND chat_id = ?2",
        params![account_id, chat_id],
    )
    .map_err(to_error)?;
    Ok(())
}

fn mark_typing_chats_read(
    app: &tauri::AppHandle,
    account_id: &str,
    chat_ids: &[String],
) -> Result<Vec<(String, String)>, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "应用状态尚未初始化".to_owned())?;
    let conn = state.db.lock().map_err(to_error)?;
    let mut refs = Vec::new();
    for chat_id in chat_ids {
        let unread_count = conn
            .query_row(
                "SELECT unread_count FROM chat_contacts WHERE account_id = ?1 AND chat_id = ?2",
                params![account_id, chat_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_default();
        let unread_messages = conn
            .query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE account_id = ?1 AND chat_id = ?2 AND read_status <> 'read'",
                params![account_id, chat_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_default();
        let message_id = conn
            .query_row(
                "SELECT id FROM chat_messages WHERE account_id = ?1 AND chat_id = ?2 ORDER BY sent_at DESC LIMIT 1",
                params![account_id, chat_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        set_chat_read(&conn, account_id, chat_id)?;
        conn.execute(
            "UPDATE chat_messages SET read_status = 'read' WHERE account_id = ?1 AND chat_id = ?2",
            params![account_id, chat_id],
        )
        .map_err(to_error)?;
        if !message_id.is_empty() && (unread_count > 0 || unread_messages > 0) {
            refs.push((chat_id.clone(), message_id));
        }
    }
    Ok(refs)
}

fn schedule_typing_readback(
    app: &tauri::AppHandle,
    account_id: &str,
    cookie: &str,
    refs: Vec<(String, String)>,
) {
    for (chat_id, message_id) in refs {
        let app = app.clone();
        let account_id = account_id.to_owned();
        let cookie = cookie.to_owned();
        tauri::async_runtime::spawn(async move {
            let Some(state) = app.try_state::<AppState>() else { return };
            let Ok(sender) = account_im_sender(&state, &account_id) else { return };
            let Ok((_, renewed_cookie)) = xianyu_im_local::clear_red_point(&cookie, &chat_id, &message_id, sender.as_ref()).await else { return };
            if let Ok(conn) = state.db.lock() {
                let _ = save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key);
            };
        });
    }
}

#[tauri::command]
async fn mark_chat_read(
    account_id: String,
    chat_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Mark locally first so the UI never gets stuck behind a transient IM
    // connection.  The remote call below is best-effort and idempotent.
    let (cookie, message_id) = {
        let conn = state.db.lock().map_err(to_error)?;
        ensure_account_exists(&conn, &account_id)?;
        let cookie = local_session(&conn, &account_id, &state.secret_key)?;
        let message_id = conn
            .query_row(
                "SELECT id FROM chat_messages WHERE account_id = ?1 AND chat_id = ?2 ORDER BY sent_at DESC LIMIT 1",
                params![account_id, chat_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        set_chat_read(&conn, &account_id, &chat_id)?;
        (cookie, message_id)
    };

    if !message_id.trim().is_empty() {
        let sender = account_im_sender(&state, &account_id)?;
        if let Ok((_, renewed_cookie)) = xianyu_im_local::clear_red_point(&cookie, &chat_id, &message_id, sender.as_ref()).await {
            let conn = state.db.lock().map_err(to_error)?;
            let _ = save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key);
        }
    }
    Ok(())
}

#[tauri::command]
async fn set_chat_pinned(
    account_id: String,
    chat_id: String,
    pinned: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let sender = account_im_sender(&state, &account_id)?;
    let (response, renewed_cookie) = xianyu_im_local::set_conversation_top(&cookie, &chat_id, pinned, sender.as_ref()).await?;
    if response.get("code").and_then(Value::as_i64).unwrap_or(200) != 200 {
        return Err("闲鱼置顶会话失败".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

#[tauri::command]
async fn delete_chat_conversation(
    account_id: String,
    chat_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let sender = account_im_sender(&state, &account_id)?;
    let (response, renewed_cookie) = xianyu_im_local::hide_conversation(&cookie, &chat_id, sender.as_ref()).await?;
    if response.get("code").and_then(Value::as_i64).unwrap_or(200) != 200 {
        return Err("闲鱼删除会话失败".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    conn.execute("DELETE FROM chat_messages WHERE account_id = ?1 AND chat_id = ?2", params![account_id, chat_id]).map_err(to_error)?;
    conn.execute("DELETE FROM chat_contacts WHERE account_id = ?1 AND chat_id = ?2", params![account_id, chat_id]).map_err(to_error)?;
    conn.execute("DELETE FROM chat_read_state WHERE account_id = ?1 AND chat_id = ?2", params![account_id, chat_id]).map_err(to_error)?;
    conn.execute("DELETE FROM customer_remarks WHERE account_id = ?1 AND chat_id = ?2", params![account_id, chat_id]).map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(())
}

#[tauri::command]
async fn sync_chat_contacts(
    account_id: String,
    cursor: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<ChatContactsPage, String> {
    let (cookie, cached_profiles) = {
        let conn = state.db.lock().map_err(to_error)?;
        let cookie = local_session(&conn, &account_id, &state.secret_key)?;
        let profiles = read_chat_contacts(&conn, &account_id)?
            .into_iter()
            .fold(std::collections::HashMap::new(), |mut profiles, contact| {
                let profile = xianyu_im_local::CachedChatProfile {
                    display_name: contact.other_user_name,
                    avatar_url: contact.avatar_url,
                    buyer_tag: contact.buyer_tag,
                    profile_synced_at: contact.profile_synced_at,
                };
                // The same buyer can have more than one conversation id. Keep
                // a second cache key by buyer id so a newer conversation can
                // reuse the official nickname/avatar from an older one.
                profiles.insert(contact.chat_id, profile.clone());
                let user_key = format!("user:{}", contact.other_user_id);
                let should_replace_shared = profiles
                    .get(&user_key)
                    .map(|cached| {
                        cached.display_name.trim().is_empty()
                            || cached.display_name.starts_with("用户 ")
                            || cached.display_name.starts_with("用户_")
                    })
                    .unwrap_or(true);
                if should_replace_shared {
                    profiles.insert(user_key, profile);
                }
                profiles
            });
        (cookie, profiles)
    };
    let sender = account_im_sender(&state, &account_id)?;
    let page = xianyu_im_local::fetch_contacts(&account_id, &cookie, &cached_profiles, cursor, sender.as_ref()).await?;
    let conn = state.db.lock().map_err(to_error)?;
    for contact in &page.items {
        conn.execute(
            "INSERT INTO chat_contacts (account_id, chat_id, other_user_id, other_user_name, avatar_url, item_id, item_title, item_image_url, order_status, buyer_tag, latest_message, latest_message_time, unread_count, profile_synced_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) ON CONFLICT(account_id, chat_id) DO UPDATE SET other_user_id = excluded.other_user_id, other_user_name = excluded.other_user_name, avatar_url = CASE WHEN excluded.avatar_url <> '' THEN excluded.avatar_url ELSE chat_contacts.avatar_url END, item_id = CASE WHEN excluded.item_id <> '' THEN excluded.item_id ELSE chat_contacts.item_id END, item_title = CASE WHEN excluded.item_title <> '' THEN excluded.item_title ELSE chat_contacts.item_title END, item_image_url = CASE WHEN excluded.item_image_url <> '' THEN excluded.item_image_url ELSE chat_contacts.item_image_url END, order_status = excluded.order_status, buyer_tag = excluded.buyer_tag, latest_message = excluded.latest_message, latest_message_time = excluded.latest_message_time, unread_count = CASE WHEN EXISTS (SELECT 1 FROM chat_read_state WHERE account_id = excluded.account_id AND chat_id = excluded.chat_id AND read_at >= excluded.latest_message_time) THEN 0 ELSE MAX(chat_contacts.unread_count, excluded.unread_count) END, profile_synced_at = excluded.profile_synced_at",
            params![contact.account_id, contact.chat_id, contact.other_user_id, contact.other_user_name, contact.avatar_url, contact.item_id, contact.item_title, contact.item_image_url, contact.order_status, contact.buyer_tag, contact.latest_message, contact.latest_message_time, contact.unread_count, contact.profile_synced_at],
        ).map_err(to_error)?;
    }
    save_renewed_session(&conn, &account_id, &page.cookie, &state.secret_key)?;
    Ok(ChatContactsPage {
        items: read_chat_contacts(&conn, &account_id)?,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
    })
}

fn read_chat_messages(
    conn: &Connection,
    account_id: &str,
    chat_id: &str,
) -> Result<Vec<xianyu_im_local::ChatMessage>, String> {
    let mut statement = conn.prepare("SELECT id, account_id, chat_id, sender_user_id, sender_user_name, direction, content_kind, text, media_url, sent_at, send_status, read_status, card_title, card_subtitle, card_price, target_url FROM chat_messages WHERE account_id = ?1 AND chat_id = ?2 ORDER BY sent_at ASC").map_err(to_error)?;
    let messages = statement
        .query_map(params![account_id, chat_id], |row| {
            Ok(xianyu_im_local::ChatMessage {
                id: row.get(0)?,
                account_id: row.get(1)?,
                chat_id: row.get(2)?,
                sender_user_id: row.get(3)?,
                sender_user_name: row.get(4)?,
                direction: row.get(5)?,
                content_kind: row.get(6)?,
                text: row.get(7)?,
                media_url: row.get(8)?,
                sent_at: row.get(9)?,
                send_status: row.get(10)?,
                read_status: row.get(11)?,
                card_title: row.get(12)?,
                card_subtitle: row.get(13)?,
                card_price: row.get(14)?,
                target_url: row.get(15)?,
            })
        })
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;
    let mut deduplicated = Vec::<xianyu_im_local::ChatMessage>::with_capacity(messages.len());
    let mut latest_by_content = std::collections::HashMap::<String, (i64, usize)>::new();
    for message in messages {
        let sent_millis = chrono::DateTime::parse_from_rfc3339(&message.sent_at)
            .map(|value| value.timestamp_millis())
            .unwrap_or(i64::MIN);
        let key = format!(
            "{}\u{0}{}\u{0}{}",
            message.direction, message.content_kind, message.text
        );
        if let Some((previous_millis, previous_index)) = latest_by_content.get(&key).copied() {
            if sent_millis != i64::MIN && (sent_millis - previous_millis).abs() < 1_000 {
                let previous_is_remote = deduplicated[previous_index].id.ends_with(".PNM");
                let current_is_remote = message.id.ends_with(".PNM");
                if current_is_remote && !previous_is_remote {
                    deduplicated[previous_index] = message;
                    latest_by_content.insert(key, (sent_millis, previous_index));
                }
                continue;
            }
        }
        let index = deduplicated.len();
        deduplicated.push(message);
        latest_by_content.insert(key, (sent_millis, index));
    }
    Ok(deduplicated)
}

fn read_chat_emojis(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<xianyu_im_local::ChatEmoji>, String> {
    let mut statement = conn
        .prepare("SELECT icon_alias, icon_url FROM chat_emojis WHERE account_id = ?1 ORDER BY icon_alias ASC")
        .map_err(to_error)?;
    let emojis = statement
        .query_map([account_id], |row| {
            Ok(xianyu_im_local::ChatEmoji {
                icon_alias: row.get(0)?,
                icon_url: row.get(1)?,
            })
        })
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;
    Ok(emojis)
}

#[tauri::command]
fn list_chat_emojis(
    account_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<xianyu_im_local::ChatEmoji>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    read_chat_emojis(&conn, &account_id)
}

#[tauri::command]
async fn sync_chat_emojis(
    account_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<xianyu_im_local::ChatEmoji>, String> {
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let (emojis, renewed_cookie) = xianyu_im_local::fetch_chat_emojis(&cookie).await?;
    if emojis.is_empty() {
        return Err("闲鱼未返回表情资源".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    let transaction = conn.unchecked_transaction().map_err(to_error)?;
    transaction
        .execute("DELETE FROM chat_emojis WHERE account_id = ?1", [&account_id])
        .map_err(to_error)?;
    for emoji in &emojis {
        transaction
            .execute(
                "INSERT INTO chat_emojis (account_id, icon_alias, icon_url, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![account_id, emoji.icon_alias, emoji.icon_url, Utc::now().to_rfc3339()],
            )
            .map_err(to_error)?;
    }
    transaction.commit().map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(emojis)
}

fn remove_near_duplicate_messages(
    conn: &Connection,
    account_id: &str,
    chat_id: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM chat_messages WHERE rowid IN (
          SELECT CASE
            WHEN older.id LIKE '%.PNM' AND newer.id NOT LIKE '%.PNM' THEN newer.rowid
            WHEN newer.id LIKE '%.PNM' AND older.id NOT LIKE '%.PNM' THEN older.rowid
            ELSE newer.rowid
          END
          FROM chat_messages older
          JOIN chat_messages newer
            ON newer.account_id = older.account_id
           AND newer.chat_id = older.chat_id
           AND newer.direction = older.direction
           AND newer.content_kind = older.content_kind
           AND newer.text = older.text
           AND newer.rowid > older.rowid
           AND ABS((julianday(newer.sent_at) - julianday(older.sent_at)) * 86400.0) < 1.0
          WHERE older.account_id = ?1 AND older.chat_id = ?2
        )",
        params![account_id, chat_id],
    )
    .map_err(to_error)?;
    Ok(())
}

#[tauri::command]
fn list_chat_messages(
    account_id: String,
    chat_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<xianyu_im_local::ChatMessage>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &account_id)?;
    read_chat_messages(&conn, &account_id, &chat_id)
}

fn upsert_chat_message(
    conn: &Connection,
    message: &xianyu_im_local::ChatMessage,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO chat_messages (account_id, id, chat_id, sender_user_id, sender_user_name, direction, content_kind, text, media_url, sent_at, send_status, read_status, card_title, card_subtitle, card_price, target_url) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) ON CONFLICT(account_id, id) DO UPDATE SET sender_user_name = excluded.sender_user_name, direction = excluded.direction, content_kind = excluded.content_kind, text = excluded.text, media_url = excluded.media_url, sent_at = excluded.sent_at, send_status = excluded.send_status, read_status = CASE WHEN chat_messages.read_status = 'read' THEN 'read' WHEN excluded.read_status = 'unknown' THEN chat_messages.read_status ELSE excluded.read_status END, card_title = excluded.card_title, card_subtitle = excluded.card_subtitle, card_price = excluded.card_price, target_url = excluded.target_url",
        params![message.account_id, message.id, message.chat_id, message.sender_user_id, message.sender_user_name, message.direction, message.content_kind, message.text, message.media_url, message.sent_at, message.send_status, message.read_status, message.card_title, message.card_subtitle, message.card_price, message.target_url],
    ).map_err(to_error)?;
    Ok(())
}

fn ingest_im_push(
    app: &tauri::AppHandle,
    account_id: &str,
    cookie: &str,
    value: &Value,
) -> Result<usize, String> {
    let receipts = xianyu_im_local::parse_push_read_receipts(value);
    let messages = xianyu_im_local::parse_push_messages(value, account_id, cookie);
    if messages.is_empty() && receipts.is_empty() {
        return Ok(0);
    }
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "应用状态尚未初始化".to_owned())?;
    let conn = state.db.lock().map_err(to_error)?;
    let mut receipt_ids = 0_usize;
    let mut receipt_updates = 0_usize;
    for receipt in receipts {
        // The official /s/sync receipt uses status=1 for messages read by the
        // peer. Ignore unknown future statuses instead of accidentally
        // downgrading an already-read message.
        if receipt.status != 1 {
            continue;
        }
        for message_id in receipt.message_ids {
            receipt_ids += 1;
            receipt_updates += conn.execute(
                "UPDATE chat_messages SET read_status = 'read' WHERE account_id = ?1 AND chat_id = ?2 AND id = ?3 AND direction = 'outgoing'",
                params![account_id, receipt.chat_id, message_id],
            )
            .map_err(to_error)?;
        }
    }
    let mut inserted = 0;
    for message in &messages {
        let exists = conn
            .query_row(
                "SELECT 1 FROM chat_messages WHERE account_id = ?1 AND id = ?2 LIMIT 1",
                params![account_id, message.id],
                |row| row.get::<_, i64>(0),
            )
            .is_ok();
        upsert_chat_message(&conn, message)?;
        if !exists {
            inserted += 1;
        }
        let is_incoming = message.direction == "incoming";
        let incoming = i64::from(is_incoming);
        let unread_increment = if exists { 0 } else { incoming };
        let other_user_id = if is_incoming {
            message.sender_user_id.clone()
        } else {
            String::new()
        };
        let other_user_name = if is_incoming && !message.sender_user_name.trim().is_empty() {
            message.sender_user_name.clone()
        } else {
            String::new()
        };
        conn.execute(
            "INSERT INTO chat_contacts (account_id, chat_id, other_user_id, other_user_name, avatar_url, item_id, item_title, item_image_url, order_status, buyer_tag, latest_message, latest_message_time, unread_count, profile_synced_at) VALUES (?1, ?2, ?3, ?4, '', '', '', '', '', '', ?5, ?6, ?7, '') ON CONFLICT(account_id, chat_id) DO UPDATE SET other_user_id = CASE WHEN excluded.other_user_id <> '' THEN excluded.other_user_id ELSE chat_contacts.other_user_id END, other_user_name = CASE WHEN excluded.other_user_name <> '' THEN excluded.other_user_name ELSE chat_contacts.other_user_name END, latest_message = CASE WHEN excluded.latest_message_time >= chat_contacts.latest_message_time THEN excluded.latest_message ELSE chat_contacts.latest_message END, latest_message_time = CASE WHEN excluded.latest_message_time >= chat_contacts.latest_message_time THEN excluded.latest_message_time ELSE chat_contacts.latest_message_time END, unread_count = chat_contacts.unread_count + excluded.unread_count",
            params![account_id, message.chat_id, other_user_id, other_user_name, message.text, message.sent_at, unread_increment],
        ).map_err(to_error)?;
    }
    // `append_app_log` also uses `state.db`. Do not call it while this
    // function still owns the non-reentrant database mutex: an IM read receipt
    // would otherwise deadlock the listener and block every later history or
    // message query from the UI.
    drop(conn);
    if receipt_ids > 0 {
        append_app_log(
            app,
            if receipt_updates > 0 { "info" } else { "warn" },
            "IM 回执",
            account_id,
            &format!("收到已读回执 {receipt_ids} 条，匹配本地消息 {receipt_updates} 条"),
        );
    }
    Ok(inserted)
}

#[tauri::command]
async fn sync_chat_messages(
    account_id: String,
    chat_id: String,
    cursor: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<ChatMessagesPage, String> {
    if chat_id.trim().is_empty() {
        return Err("会话 ID 不能为空".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let sender = account_im_sender(&state, &account_id)?;
    let page = xianyu_im_local::fetch_messages(&account_id, &chat_id, &cookie, cursor, sender.as_ref()).await?;
    // The official client clears the remote red point using the last message
    // currently loaded. This also covers the first open, when the local cache
    // did not yet contain a message id for the conversation.
    let mut renewed_cookie = page.cookie.clone();
    if let Some(last_message) = page.items.last().filter(|message| !message.id.trim().is_empty()) {
        if let Ok((_, remote_cookie)) = xianyu_im_local::clear_red_point(&page.cookie, &chat_id, &last_message.id, sender.as_ref()).await {
            renewed_cookie = remote_cookie;
        }
    }
    let conn = state.db.lock().map_err(to_error)?;
    if !page.own_avatar_url.is_empty() {
        conn.execute("UPDATE account_profiles SET avatar_url = ?1, avatar_source = 'im_message', updated_at = ?2 WHERE account_id = ?3", params![page.own_avatar_url, Utc::now().to_rfc3339(), account_id]).map_err(to_error)?;
    }
    for message in &page.items {
        upsert_chat_message(&conn, message)?;
    }
    remove_near_duplicate_messages(&conn, &account_id, &chat_id)?;
    set_chat_read(&conn, &account_id, &chat_id)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(ChatMessagesPage {
        items: read_chat_messages(&conn, &account_id, &chat_id)?,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
    })
}

#[tauri::command]
async fn send_chat_message(
    account_id: String,
    chat_id: String,
    receiver_user_id: String,
    text: String,
    state: tauri::State<'_, AppState>,
) -> Result<xianyu_im_local::ChatMessage, String> {
    if chat_id.trim().is_empty() || receiver_user_id.trim().is_empty() {
        return Err("会话或收件人信息不完整".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let sender = account_im_sender(&state, &account_id)?;
    let (message, renewed_cookie) =
        xianyu_im_local::send_text(&account_id, &chat_id, &receiver_user_id, &cookie, &text, sender.as_ref())
            .await?;
    let conn = state.db.lock().map_err(to_error)?;
    upsert_chat_message(&conn, &message)?;
    conn.execute("UPDATE chat_contacts SET latest_message = ?1, latest_message_time = ?2 WHERE account_id = ?3 AND chat_id = ?4", params![message.text, message.sent_at, account_id, chat_id]).map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(message)
}

#[tauri::command]
async fn send_chat_image(
    account_id: String,
    chat_id: String,
    receiver_user_id: String,
    file_name: String,
    mime_type: String,
    image_data: String,
    width: u32,
    height: u32,
    state: tauri::State<'_, AppState>,
) -> Result<xianyu_im_local::ChatMessage, String> {
    if chat_id.trim().is_empty() || receiver_user_id.trim().is_empty() {
        return Err("会话或收件人信息不完整".to_owned());
    }
    let encoded = image_data
        .split_once(',')
        .map(|(_, value)| value)
        .unwrap_or(image_data.as_str());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "图片数据格式无效".to_owned())?;
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let sender = account_im_sender(&state, &account_id)?;
    let (message, renewed_cookie) = xianyu_im_local::send_image(
        &account_id,
        &chat_id,
        &receiver_user_id,
        &cookie,
        &file_name,
        &mime_type,
        bytes,
        width,
        height,
        sender.as_ref(),
    )
    .await?;
    let conn = state.db.lock().map_err(to_error)?;
    upsert_chat_message(&conn, &message)?;
    conn.execute("UPDATE chat_contacts SET latest_message = ?1, latest_message_time = ?2 WHERE account_id = ?3 AND chat_id = ?4", params![message.text, message.sent_at, account_id, chat_id]).map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(message)
}

#[tauri::command]
async fn send_chat_product(
    account_id: String,
    chat_id: String,
    receiver_user_id: String,
    item_id: String,
    title: String,
    image_url: String,
    price: f64,
    state: tauri::State<'_, AppState>,
) -> Result<xianyu_im_local::ChatMessage, String> {
    if chat_id.trim().is_empty() || receiver_user_id.trim().is_empty() {
        return Err("会话或收件人信息不完整".to_owned());
    }
    let cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        local_session(&conn, &account_id, &state.secret_key)?
    };
    let sender = account_im_sender(&state, &account_id)?;
    let (message, renewed_cookie) = xianyu_im_local::send_product(
        &account_id, &chat_id, &receiver_user_id, &cookie, &item_id, &title,
        &image_url, price, sender.as_ref(),
    ).await?;
    let conn = state.db.lock().map_err(to_error)?;
    upsert_chat_message(&conn, &message)?;
    conn.execute("UPDATE chat_contacts SET latest_message = ?1, latest_message_time = ?2 WHERE account_id = ?3 AND chat_id = ?4", params![message.text, message.sent_at, account_id, chat_id]).map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    Ok(message)
}

#[tauri::command]
fn create_product(
    input: ProductInput,
    state: tauri::State<'_, AppState>,
) -> Result<Product, String> {
    if input.title.trim().is_empty() {
        return Err("商品标题不能为空".to_owned());
    }
    if input.price < 0.0 || input.stock < 0 {
        return Err("价格和库存不能为负数".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &input.account_id)?;
    let id = format!("LOCAL-P-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let now = Utc::now().to_rfc3339();
    let tags = input
        .tags
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    conn.execute(
        "INSERT INTO products (id, account_id, title, image_url, price, stock, status, updated_at, tags) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![id, input.account_id, input.title.trim(), input.image_url.trim(), input.price, input.stock, input.status, now, tags],
    ).map_err(to_error)?;
    Ok(Product {
        id,
        account_id: input.account_id,
        title: input.title.trim().to_owned(),
        image_url: input.image_url.trim().to_owned(),
        price: input.price,
        stock: input.stock,
        status: input.status,
        updated_at: now,
        tags: input.tags,
    })
}

#[tauri::command]
fn update_product(
    id: String,
    input: ProductInput,
    state: tauri::State<'_, AppState>,
) -> Result<Product, String> {
    if input.title.trim().is_empty() {
        return Err("商品标题不能为空".to_owned());
    }
    if input.price < 0.0 || input.stock < 0 {
        return Err("价格和库存不能为负数".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &input.account_id)?;
    let now = Utc::now().to_rfc3339();
    let tags = input
        .tags
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    let changed = conn.execute(
        "UPDATE products SET account_id = ?1, title = ?2, image_url = ?3, price = ?4, stock = ?5, status = ?6, updated_at = ?7, tags = ?8 WHERE id = ?9",
        params![input.account_id, input.title.trim(), input.image_url.trim(), input.price, input.stock, input.status, now, tags, id],
    ).map_err(to_error)?;
    if changed == 0 {
        return Err("商品不存在或已被删除".to_owned());
    }
    Ok(Product {
        id,
        account_id: input.account_id,
        title: input.title.trim().to_owned(),
        image_url: input.image_url.trim().to_owned(),
        price: input.price,
        stock: input.stock,
        status: input.status,
        updated_at: now,
        tags: input.tags,
    })
}

#[tauri::command]
fn delete_product(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(to_error)?;
    let changed = conn
        .execute("DELETE FROM products WHERE id = ?1", [id])
        .map_err(to_error)?;
    if changed == 0 {
        return Err("商品不存在或已被删除".to_owned());
    }
    Ok(())
}

#[tauri::command]
fn update_products_status(
    ids: Vec<String>,
    status: String,
    state: tauri::State<'_, AppState>,
) -> Result<usize, String> {
    if status != "已上架" && status != "已下架" {
        return Err("不支持的商品状态".to_owned());
    }
    let mut conn = state.db.lock().map_err(to_error)?;
    let transaction = conn.transaction().map_err(to_error)?;
    let now = Utc::now().to_rfc3339();
    let mut changed = 0;
    for id in ids {
        changed += transaction
            .execute(
                "UPDATE products SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status, now, id],
            )
            .map_err(to_error)?;
    }
    transaction.commit().map_err(to_error)?;
    Ok(changed)
}

#[tauri::command]
fn delete_products(ids: Vec<String>, state: tauri::State<'_, AppState>) -> Result<usize, String> {
    let mut conn = state.db.lock().map_err(to_error)?;
    let transaction = conn.transaction().map_err(to_error)?;
    let mut changed = 0;
    for id in ids {
        changed += transaction
            .execute("DELETE FROM products WHERE id = ?1", [id])
            .map_err(to_error)?;
    }
    transaction.commit().map_err(to_error)?;
    Ok(changed)
}

#[tauri::command]
fn create_order(input: OrderInput, state: tauri::State<'_, AppState>) -> Result<Order, String> {
    if input.product_title.trim().is_empty() || input.buyer_masked_name.trim().is_empty() {
        return Err("商品和买家信息不能为空".to_owned());
    }
    if input.amount < 0.0 {
        return Err("订单金额不能为负数".to_owned());
    }
    let conn = state.db.lock().map_err(to_error)?;
    ensure_account_exists(&conn, &input.account_id)?;
    let id = Uuid::new_v4().to_string();
    let order_no = format!(
        "LOCAL-{}",
        &Uuid::new_v4().simple().to_string()[..12].to_uppercase()
    );
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO orders (id, account_id, order_no, item_id, item_image_url, buyer_id, product_title, specification, buyer_masked_name, amount, refund_amount, status, shipping_refund_status, created_at, note) VALUES (?1, ?2, ?3, '', '', '', ?4, '', ?5, ?6, 0, ?7, '', ?8, ?9)",
        params![id, input.account_id, order_no, input.product_title.trim(), input.buyer_masked_name.trim(), input.amount, input.status, now, input.note.trim()],
    ).map_err(to_error)?;
    Ok(Order {
        id,
        account_id: input.account_id,
        order_no,
        item_id: String::new(),
        item_image_url: String::new(),
        product_title: input.product_title.trim().to_owned(),
        specification: String::new(),
        buyer_masked_name: input.buyer_masked_name.trim().to_owned(),
        amount: input.amount,
        status_code: String::new(),
        status: input.status,
        refund_amount: 0.0, shipping_refund_status: String::new(),
        created_at: now,
        note: input.note.trim().to_owned(),
    })
}

#[tauri::command]
fn update_order(
    id: String,
    status: String,
    note: String,
    state: tauri::State<'_, AppState>,
) -> Result<Order, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let changed = conn
        .execute(
            "UPDATE orders SET status = ?1, note = ?2 WHERE id = ?3",
            params![status, note.trim(), id],
        )
        .map_err(to_error)?;
    if changed == 0 {
        return Err("订单不存在或已被删除".to_owned());
    }
    conn.query_row(
        "SELECT id, account_id, order_no, item_id, item_image_url, product_title, specification, buyer_masked_name, amount, refund_amount, status_code, status, shipping_refund_status, created_at, note FROM orders WHERE id = ?1", [id],
        |row| Ok(Order { id: row.get(0)?, account_id: row.get(1)?, order_no: row.get(2)?, item_id: row.get(3)?, item_image_url: row.get(4)?, product_title: row.get(5)?, specification: row.get(6)?, buyer_masked_name: row.get(7)?, amount: row.get(8)?, refund_amount: row.get(9)?, status_code: row.get(10)?, status: row.get(11)?, shipping_refund_status: row.get(12)?, created_at: row.get(13)?, note: row.get(14)? }),
    ).map_err(to_error)
}

#[tauri::command]
fn delete_order(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(to_error)?;
    let changed = conn
        .execute("DELETE FROM orders WHERE id = ?1", [id])
        .map_err(to_error)?;
    if changed == 0 {
        return Err("订单不存在或已被删除".to_owned());
    }
    Ok(())
}

#[tauri::command]
fn update_orders_status(
    ids: Vec<String>,
    status: String,
    state: tauri::State<'_, AppState>,
) -> Result<usize, String> {
    let allowed = ["待付款", "待发货", "待收货", "已完成", "退款中", "已关闭"];
    if !allowed.contains(&status.as_str()) {
        return Err("不支持的订单状态".to_owned());
    }
    let mut conn = state.db.lock().map_err(to_error)?;
    let transaction = conn.transaction().map_err(to_error)?;
    let mut changed = 0;
    for id in ids {
        changed += transaction
            .execute(
                "UPDATE orders SET status = ?1 WHERE id = ?2",
                params![status, id],
            )
            .map_err(to_error)?;
    }
    transaction.commit().map_err(to_error)?;
    Ok(changed)
}

#[tauri::command]
fn export_backup(state: tauri::State<'_, AppState>) -> Result<BackupData, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut account_statement = conn
        .prepare("SELECT id FROM accounts ORDER BY last_sync_at DESC")
        .map_err(to_error)?;
    let account_ids = account_statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;
    let accounts = account_ids
        .iter()
        .map(|id| get_account(&conn, id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;

    let mut product_statement = conn.prepare("SELECT id, account_id, title, image_url, price, stock, status, updated_at, tags FROM products ORDER BY updated_at DESC").map_err(to_error)?;
    let products = product_statement
        .query_map([], |row| {
            let tags: String = row.get(8)?;
            Ok(Product {
                id: row.get(0)?,
                account_id: row.get(1)?,
                title: row.get(2)?,
                image_url: row.get(3)?,
                price: row.get(4)?,
                stock: row.get(5)?,
                status: row.get(6)?,
                updated_at: row.get(7)?,
                tags: tags
                    .split(',')
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;

    let mut order_statement = conn.prepare("SELECT id, account_id, order_no, item_id, item_image_url, product_title, specification, buyer_masked_name, amount, refund_amount, status_code, status, shipping_refund_status, created_at, note FROM orders ORDER BY created_at DESC").map_err(to_error)?;
    let orders = order_statement
        .query_map([], |row| {
            Ok(Order {
                id: row.get(0)?,
                account_id: row.get(1)?,
                order_no: row.get(2)?,
                item_id: row.get(3)?,
                item_image_url: row.get(4)?,
                product_title: row.get(5)?,
                specification: row.get(6)?,
                buyer_masked_name: row.get(7)?,
                amount: row.get(8)?,
                refund_amount: row.get(9)?,
                status_code: row.get(10)?,
                status: row.get(11)?,
                shipping_refund_status: row.get(12)?,
                created_at: row.get(13)?,
                note: row.get(14)?,
            })
        })
        .map_err(to_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(to_error)?;

    Ok(BackupData {
        exported_at: Utc::now().to_rfc3339(),
        accounts,
        products,
        orders,
    })
}

#[tauri::command]
async fn sync_account(
    account_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<SyncResult, String> {
    let now = Utc::now().to_rfc3339();
    let local_cookie = {
        let conn = state.db.lock().map_err(to_error)?;
        ensure_account_exists(&conn, &account_id)?;
        local_session(&conn, &account_id, &state.secret_key).unwrap_or_default()
    };

    if local_cookie.trim().is_empty() {
        let conn = state.db.lock().map_err(to_error)?;
        record_sync_job(
            &conn,
            &account_id,
            "未登录",
            &now,
            "当前账号没有本机会话，请先扫码登录。",
        )
        .map_err(to_error)?;
        return Ok(SyncResult {
            account: get_account(&conn, &account_id).map_err(to_error)?,
            products_changed: 0,
            orders_changed: 0,
            source_connected: false,
        });
    }

    let response = async {
        // Product/order calls refresh a stale MTop token when needed. Query
        // the profile afterwards so existing local sessions can be backfilled
        // even if their saved token had expired.
        let (items, renewed_cookie) = xianyu_local::fetch_products(&local_cookie).await?;
        let (orders, renewed_cookie) = xianyu_local::fetch_orders(&renewed_cookie).await?;
        let profile = fetch_account_profile(&renewed_cookie).await.ok();
        let renewed_cookie = profile
            .as_ref()
            .filter(|profile| !profile.cookie.is_empty())
            .map(|profile| profile.cookie.clone())
            .unwrap_or(renewed_cookie);
        Ok::<(Value, Value, String, Option<AccountProfile>), String>((
            Value::Array(items),
            Value::Array(orders),
            renewed_cookie,
            profile,
        ))
    }
    .await;
    let (items_payload, orders_payload, renewed_cookie, profile) = match response {
        Ok(payload) => payload,
        Err(error) => {
            let conn = state.db.lock().map_err(to_error)?;
            record_sync_job(&conn, &account_id, "失败", &now, &error).map_err(to_error)?;
            return Err(format!("本机闲鱼同步失败：{error}"));
        }
    };

    let conn = state.db.lock().map_err(to_error)?;
    if let Some(profile) = profile.as_ref() {
        save_account_profile(&conn, &account_id, profile)?;
    }
    let mut products_changed = 0;
    for item in value_list(&items_payload) {
        let remote_id = value_string(item, &["item_id", "itemId", "num_iid", "id"]);
        let title = value_string(item, &["title", "item_title", "itemTitle", "name"]);
        if remote_id.is_empty() || title.is_empty() {
            continue;
        }
        let direct_stock = value_i64(
            item,
            &["item_quantity", "stock", "quantity", "num", "inventory"],
        );
        let stock = if direct_stock > 0 {
            direct_stock
        } else {
            item.get("variants")
                .and_then(Value::as_array)
                .map(|variants| {
                    variants
                        .iter()
                        .map(|variant| value_i64(variant, &["stock_count", "quantity", "stock"]))
                        .sum()
                })
                .unwrap_or(0)
        };
        let status = normalize_product_status(
            value_string(
                item,
                &["item_status_desc", "status", "item_status", "itemStatus"],
            ),
            stock,
        );
        let tags = item
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let id = format!("SRC-P-{}-{}", &account_id[..8], remote_id);
        let image_url = nested_value_string(item, &["image_url", "imageUrl", "item_pic", "itemPic", "pic_url", "picUrl", "main_pic", "mainPic", "cover_url", "coverUrl"]);
        products_changed += conn.execute(
            "INSERT INTO products (id, account_id, title, image_url, price, stock, status, updated_at, tags) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(id) DO UPDATE SET account_id = excluded.account_id, title = excluded.title, image_url = CASE WHEN excluded.image_url <> '' THEN excluded.image_url ELSE products.image_url END, price = excluded.price, stock = excluded.stock, status = excluded.status, updated_at = excluded.updated_at, tags = excluded.tags",
            params![id, account_id, title, image_url, value_number(item, &["price", "item_price", "itemPrice", "sale_price"]), stock, status, now, tags],
        ).map_err(to_error)?;
    }
    let mut orders_changed = 0;
    for order in value_list(&orders_payload) {
        let remote_id = value_string(order, &["order_id", "orderId", "order_no", "orderNo", "id"]);
        if remote_id.is_empty() {
            continue;
        }
        let order_no = value_string(order, &["order_no", "orderNo", "order_id", "id"]);
        let id = format!("SRC-O-{}-{}", &account_id[..8], remote_id);
        let order_status = normalize_order_status(value_string(order, &["status", "order_status", "orderStatus", "orderStatusDesc", "statusDesc"]));
        let order_created_at = value_string(order, &["created_at", "createdAt", "create_time", "createTime"]);
        let order_amount = value_number(order, &["actual_amount", "amount", "price", "payment"]);
        let order_refund = value_number(order, &["refund_amount", "refundAmount", "refund_money", "refundMoney", "refund_fee", "refundFee"]);
        orders_changed += conn.execute(
            "INSERT INTO orders (id, account_id, order_no, item_id, item_image_url, buyer_id, product_title, specification, buyer_masked_name, amount, refund_amount, status_code, status, shipping_refund_status, created_at, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) ON CONFLICT(id) DO UPDATE SET order_no = excluded.order_no, item_id = excluded.item_id, item_image_url = excluded.item_image_url, buyer_id = excluded.buyer_id, product_title = excluded.product_title, specification = excluded.specification, buyer_masked_name = excluded.buyer_masked_name, amount = excluded.amount, refund_amount = excluded.refund_amount, status_code = excluded.status_code, status = excluded.status, shipping_refund_status = excluded.shipping_refund_status, created_at = excluded.created_at, note = excluded.note",
            params![id, account_id, order_no, value_string(order, &["item_id", "itemId"]), value_string(order, &["item_image_url", "itemImageUrl"]), value_string(order, &["buyer_id", "buyerId"]), value_string(order, &["item_title", "product_title", "productTitle", "title"]), value_string(order, &["specification", "spec"]), value_string(order, &["buyer_fish_nick", "buyer_nick", "buyer_nickname", "buyer_name", "buyer_id"]), order_amount, order_refund, value_string(order, &["status_code", "statusCode", "order_status_code"]), order_status, value_string(order, &["shipping_refund_status", "shippingRefundStatus"]), order_created_at, value_string(order, &["note", "remark", "message"])],
        ).map_err(to_error)?;
        sync_member_from_order(&conn, &state.secret_key, &account_id, &id, order, &value_string(order, &["buyer_fish_nick", "buyer_nick", "buyer_nickname", "buyer_name", "buyer_id"]), &order_status, &order_created_at)?;
    }
    conn.execute(
        "UPDATE accounts SET last_sync_at = ?1 WHERE id = ?2",
        params![now, account_id],
    )
    .map_err(to_error)?;
    save_renewed_session(&conn, &account_id, &renewed_cookie, &state.secret_key)?;
    record_sync_job(&conn, &account_id, "已完成", &now, "").map_err(to_error)?;
    Ok(SyncResult {
        account: get_account(&conn, &account_id).map_err(to_error)?,
        products_changed,
        orders_changed,
        source_connected: true,
    })
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&app_dir)?;
            let key_path = app_dir.join("local-secret.key");
            let secret_key = if key_path.exists() {
                let bytes = fs::read(&key_path)?;
                <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "本机密钥文件无效")?
            } else {
                let mut key = [0_u8; 32];
                OsRng.fill_bytes(&mut key);
                fs::write(&key_path, key)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
                }
                key
            };
            let conn = Connection::open(app_dir.join("shark-butler.sqlite3"))?;
            initialize_database(&conn)?;
            app.manage(AppState {
                db: Mutex::new(conn),
                secret_key,
                chat_listeners: Mutex::new(HashMap::new()),
                im_request_senders: Mutex::new(HashMap::new()),
                im_statuses: Mutex::new(HashMap::new()),
                im_validation_urls: Mutex::new(HashMap::new()),
                im_validation_cookies: Mutex::new(HashMap::new()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_accounts,
            update_conversation_name,
            list_products,
            list_orders,
            list_members,
            reveal_member,
            update_member,
            member_orders,
            order_detail,
            refund_detail,
            refund_verification,
            refund_action,
            list_related_orders,
            dashboard_stats,
            list_sync_jobs,
            create_account,
            update_account,
            delete_account,
            sync_account,
            generate_qr_login,
            check_qr_login_status,
            list_chat_contacts,
            customer_profile,
            update_customer_remark,
            chat_unread_totals,
            get_im_statuses,
            get_im_verification_state,
            open_im_verification,
            complete_im_verification,
            list_app_logs,
            start_chat_listener,
            stop_chat_listener,
            sync_chat_contacts,
            mark_chat_read,
            set_chat_pinned,
            delete_chat_conversation,
            list_chat_messages,
            list_chat_emojis,
            sync_chat_emojis,
            sync_chat_messages,
            open_product_detail,
            ship_order_without_parcel,
            ship_order_with_logistics,
            remind_order_receipt,
            cancel_order_by_seller,
            send_chat_message,
            send_chat_image,
            send_chat_product,
            list_quick_replies,
            create_quick_reply,
            update_quick_reply,
            delete_quick_reply,
            create_product,
            update_product,
            delete_product,
            update_products_status,
            delete_products,
            create_order,
            update_order,
            delete_order,
            update_orders_status,
            export_backup
        ])
        .run(tauri::generate_context!())
        .expect("启动鲨鱼管家失败");
}

#[cfg(test)]
mod tests {
    use super::{
        decrypt_secret, delete_account_records, encrypt_secret, initialize_database,
        normalize_order_status, normalize_product_status, remove_near_duplicate_messages,
    };
    use rusqlite::Connection;

    #[test]
    fn local_session_secret_round_trip() {
        let key = [23_u8; 32];
        let encrypted = encrypt_secret(&key, "cookie=value; unb=10001").expect("encrypt");
        assert!(encrypted.starts_with("enc:v1:"));
        assert_ne!(encrypted, "cookie=value; unb=10001");
        assert_eq!(
            decrypt_secret(&key, &encrypted).expect("decrypt"),
            "cookie=value; unb=10001"
        );
    }

    #[test]
    fn platform_statuses_are_normalized_for_the_ui() {
        assert_eq!(normalize_product_status("已上架".to_owned(), 1), "已上架");
        assert_eq!(normalize_order_status("已发货".to_owned()), "待收货");
        assert_eq!(normalize_order_status("退款成功".to_owned()), "已退款");
        assert_eq!(normalize_order_status("交易关闭".to_owned()), "已关闭");
    }

    #[test]
    fn deleting_account_removes_all_local_relations() {
        let mut conn = Connection::open_in_memory().expect("open database");
        initialize_database(&conn).expect("initialize database");
        conn.execute_batch(
            "
            INSERT INTO accounts VALUES ('a1','测试账号','','闲鱼','授权有效','2026-01-01T00:00:00Z');
            INSERT INTO products VALUES ('p1','a1','商品','',1,1,'已上架','2026-01-01T00:00:00Z','');
            INSERT INTO orders (id,account_id,order_no,item_id,buyer_id,product_title,buyer_masked_name,amount,status,created_at,note) VALUES ('o1','a1','n1','','','商品','买家',1,'待付款','2026-01-01T00:00:00Z','');
            INSERT INTO sync_jobs VALUES ('s1','a1','all','完成','2026-01-01T00:00:00Z',NULL,NULL);
            INSERT INTO account_sources VALUES ('a1','','remote');
            INSERT INTO account_credentials VALUES ('a1','secret','2026-01-01T00:00:00Z');
            INSERT INTO chat_contacts (account_id,chat_id,other_user_id,other_user_name,avatar_url,item_id,item_title,item_image_url,order_status,buyer_tag,latest_message,latest_message_time,unread_count) VALUES ('a1','c1','u1','买家','','','','','','','消息','2026-01-01T00:00:00Z',1);
            INSERT INTO chat_messages VALUES ('a1','m1','c1','u1','买家','incoming','text','消息','','2026-01-01T00:00:00Z','sent','unknown','','','','');
            INSERT INTO chat_emojis VALUES ('a1','[笑脸]','https://img.alicdn.com/emoji.png','2026-01-01T00:00:00Z');
            INSERT INTO chat_read_state VALUES ('a1','c1','2026-01-01T00:00:00Z');
            INSERT INTO conversation_preferences VALUES ('a1','我的会话');
            ",
        )
        .expect("seed account relations");
        delete_account_records(&mut conn, "a1").expect("delete account");
        for table in [
            "accounts",
            "products",
            "orders",
            "sync_jobs",
            "account_sources",
            "account_credentials",
            "chat_contacts",
            "chat_messages",
            "chat_emojis",
            "chat_read_state",
            "conversation_preferences",
            "quick_replies",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count rows");
            assert_eq!(count, 0, "{table} should be empty");
        }
    }

    #[test]
    fn local_echo_is_replaced_by_the_remote_message() {
        let conn = Connection::open_in_memory().expect("open database");
        initialize_database(&conn).expect("initialize database");
        conn.execute_batch(
            "
            INSERT INTO chat_messages VALUES ('a1','local-uuid','c1','u1','我','outgoing','text','你好','','2026-01-01T00:00:00.100Z','sent','unknown','','','','');
            INSERT INTO chat_messages VALUES ('a1','100.PNM','c1','u1','我','outgoing','text','你好','','2026-01-01T00:00:00.250Z','sent','unread','','','','');
            ",
        )
        .expect("seed duplicate messages");
        remove_near_duplicate_messages(&conn, "a1", "c1").expect("remove duplicate");
        let ids = conn
            .prepare("SELECT id FROM chat_messages ORDER BY id")
            .expect("prepare ids")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("read ids")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ids");
        assert_eq!(ids, vec!["100.PNM"]);
    }
}
