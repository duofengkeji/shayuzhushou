//! 闲鱼接口适配层。
//!
//! 本模块只负责与闲鱼服务端通信：构造 MTop 请求、维护 Cookie、处理接口重试，
//! 并将商品、订单、退款等官方响应提取为稳定的 JSON 数据。Tauri 命令层不应
//! 自行拼接闲鱼 URL、签名或请求头，新增接口优先在本模块增加带注释的封装函数。

use base64::Engine as _;
use reqwest::{
    cookie::{CookieStore, Jar},
    header::{
        HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, CONNECTION, COOKIE,
        LOCATION, ORIGIN, REFERER, SET_COOKIE, USER_AGENT,
    },
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

// Match the native macOS WebView profile used for in-app verification.
// Sending a Windows Chrome fingerprint from a macOS WebView can make the
// risk service issue a challenge that never clears.
#[cfg(target_os = "macos")]
pub(crate) const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.5 Safari/605.1.15";
#[cfg(not(target_os = "macos"))]
pub(crate) const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

// Carries the short-lived Baxia/punish URL to the IM listener. The listener
// consumes it before writing an application log, keeping the signed URL out
// of persistent logs.
const IM_VALIDATION_ERROR_PREFIX: &str = "__XY_IM_VALIDATION_URL__:";

// A Baxia response may rotate the MTop session in the same response that
// returns the signed challenge URL. Keep that cookie paired with the URL so
// the verification WebView does not open the challenge with a stale session.
fn im_validation_cookies() -> &'static Mutex<HashMap<String, String>> {
    static COOKIES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    COOKIES.get_or_init(|| Mutex::new(HashMap::new()))
}

// All MTop calls for one logged-in account share the same short-lived H5
// signing cookies.  Serialize those calls so a profile/order response cannot
// overwrite a newer `_m_h5_tk` produced by an IM or QR request.
fn mtop_locks() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mtop_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    if let Ok(mut locks) = mtop_locks().lock() {
        return locks
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
    }
    Arc::new(tokio::sync::Mutex::new(()))
}

pub(crate) fn im_validation_url(error: &str) -> Option<&str> {
    error.strip_prefix(IM_VALIDATION_ERROR_PREFIX)
}

pub(crate) fn take_im_validation_cookie(verification_url: &str) -> Option<String> {
    im_validation_cookies().lock().ok()?.remove(verification_url)
}

#[derive(Debug, Clone)]
struct QrSession {
    status: String,
    message: String,
    params: HashMap<String, String>,
    cookies: HashMap<String, String>,
    created_at_ms: u128,
    verification_url: String,
    verification_qr_url: String,
    face_htoken: String,
}

#[derive(Debug, Clone)]
pub struct QrStart {
    pub session_id: String,
    pub qr_code_url: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct QrPoll {
    pub status: String,
    pub message: String,
    pub verification_url: String,
    pub verification_qr_url: String,
    pub account_id: String,
    pub cookie: String,
}

fn sessions() -> &'static Mutex<HashMap<String, QrSession>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, QrSession>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn cookie_string(cookies: &HashMap<String, String>) -> String {
    let mut entries = cookies.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    entries
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cookie_parse(value: &str) -> HashMap<String, String> {
    value
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

/// Match the reference login manager: a QR login is complete only after the
/// final response has produced `unb`.
fn login_account_id(cookies: &HashMap<String, String>) -> String {
    cookies
        .get("unb")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn merge_set_cookies(cookies: &mut HashMap<String, String>, headers: &HeaderMap) {
    for header in headers.get_all(SET_COOKIE).iter() {
        let Ok(value) = header.to_str() else { continue };
        let Some(first) = value.split(';').next() else {
            continue;
        };
        let Some((name, value)) = first.split_once('=') else {
            continue;
        };
        if !name.trim().is_empty() {
            cookies.insert(name.trim().to_owned(), value.trim().to_owned());
        }
    }
}

fn json_param(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        value => value.to_string(),
    }
}

fn qr_svg_data_url(content: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(content.as_bytes()).map_err(|_| "二维码内容无效".to_owned())?;
    let image = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .dark_color(qrcode::render::svg::Color("#111827"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(image.as_bytes())
    ))
}

fn json_truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().unwrap_or_default() != 0,
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "y"
        ),
        _ => false,
    }
}

fn reference_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/plain, */*"),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    headers.insert(
        ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br"),
    );
    headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("empty"));
    headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("cors"));
    headers.insert("Sec-Fetch-Site", HeaderValue::from_static("same-origin"));
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://passport.goofish.com/"),
    );
    headers.insert(
        ORIGIN,
        HeaderValue::from_static("https://passport.goofish.com"),
    );
    headers
}

fn seed_cookie_jar(jar: &Jar, cookies: &HashMap<String, String>, url: &reqwest::Url) {
    for (name, value) in cookies {
        jar.add_cookie_str(&format!("{name}={value}; Path=/"), url);
    }
}

/// Re-establish the short-lived MTop session cookie after a QR/face login.
/// The QR polling response can contain the user session (`unb`) before the
/// H5 signing cookie (`_m_h5_tk`) has been issued on this request path. The
/// official web flow warms the same endpoint before requesting the IM token.
pub(crate) async fn warm_mtop_session(cookie: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(4))
        .default_headers(reference_headers())
        .build()
        .map_err(|error| error.to_string())?;
    let mut cookies = cookie_parse(cookie);
    let url = "https://h5api.m.goofish.com/h5/mtop.gaia.nodejs.gaia.idle.data.gw.v2.index.get/1.0/";
    let response = client
        .get(url)
        .header(COOKIE, cookie)
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .send()
        .await
        .map_err(|error| format!("初始化闲鱼 MTop 会话失败：{error}"))?;
    merge_set_cookies(&mut cookies, response.headers());
    // The web flow follows the bootstrap GET with a signed POST.  The POST is
    // what causes H5 to rotate `_m_h5_tk` for a freshly scanned session; doing
    // only the GET leaves QR logins with a valid `unb` but no token for the
    // IM endpoint.
    let token = cookies
        .get("_m_h5_tk")
        .or_else(|| cookies.get("m_h5_tk"))
        .and_then(|value| value.split('_').next())
        .unwrap_or_default();
    let timestamp = now_millis().to_string();
    let app_key = "34839810";
    let data = serde_json::json!({"bizScene":"home"}).to_string();
    let sign = format!(
        "{:x}",
        md5::compute(format!("{token}&{timestamp}&{app_key}&{data}"))
    );
    let warm_params = [
        ("jsv", "2.7.2"),
        ("appKey", app_key),
        ("t", timestamp.as_str()),
        ("sign", sign.as_str()),
        ("v", "1.0"),
        ("type", "originaljson"),
        ("dataType", "json"),
        ("timeout", "20000"),
        ("api", "mtop.gaia.nodejs.gaia.idle.data.gw.v2.index.get"),
        ("data", data.as_str()),
    ];
    let warm_response = client
        .post(url)
        .query(&warm_params)
        .header(COOKIE, cookie_string(&cookies))
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .send()
        .await
        .map_err(|error| format!("刷新闲鱼 MTop 会话失败：{error}"))?;
    merge_set_cookies(&mut cookies, warm_response.headers());
    Ok(cookie_string(&cookies))
}

fn merge_cookie_jar(
    cookies: &mut HashMap<String, String>,
    jar: &Jar,
    urls: &[&reqwest::Url],
) {
    for url in urls {
        if let Some(header) = jar.cookies(url) {
            if let Ok(value) = header.to_str() {
                cookies.extend(cookie_parse(value));
            }
        }
    }
}

fn face_verification_url(body: &Value) -> Option<String> {
    let data = body.pointer("/content/data")?;
    let redirect_enabled = json_truthy(data.get("iframeRedirect"));
    let redirect_url = data
        .get("iframeRedirectUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // Some responses include the boolean flag, others only include the
    // redirect URL. The reference client treats either form as the face
    // verification branch.
    if redirect_enabled || redirect_url.is_some() {
        redirect_url.map(str::to_owned)
    } else {
        None
    }
}

async fn get_following_redirects(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    cookies: &mut HashMap<String, String>,
) -> Result<(reqwest::Url, Vec<u8>), String> {
    let mut current = reqwest::Url::parse(url).map_err(|_| "身份验证地址格式异常".to_owned())?;
    for _ in 0..10 {
        let response = client
            .get(current.clone())
            .headers(headers.clone())
            .send()
            .await
            .map_err(|error| format!("请求身份验证页面失败：{error}"))?;
        merge_set_cookies(cookies, response.headers());
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or("身份验证跳转地址缺失".to_owned())?;
            current = reqwest::Url::parse(location)
                .or_else(|_| current.join(location))
                .map_err(|_| "身份验证跳转地址异常".to_owned())?;
            continue;
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("读取身份验证页面失败：{error}"))?;
        return Ok((current, body.to_vec()));
    }
    Err("身份验证跳转次数过多".to_owned())
}

async fn run_face_verification(session_id: String, iframe_url: String) {
    let Some(snapshot) = sessions().lock().ok().and_then(|all| all.get(&session_id).cloned()) else {
        return;
    };

    let result = async {
        let passport_url = reqwest::Url::parse("https://passport.goofish.com/")
            .map_err(|error| error.to_string())?;
        let entry_url = reqwest::Url::parse(&iframe_url)
            .map_err(|_| "身份验证地址格式异常".to_owned())?;
        let jar = Arc::new(Jar::default());
        seed_cookie_jar(&jar, &snapshot.cookies, &passport_url);
        seed_cookie_jar(&jar, &snapshot.cookies, &entry_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(reference_headers())
            .cookie_provider(Arc::clone(&jar))
            .build()
            .map_err(|error| error.to_string())?;

        // 1. Follow iframeRedirectUrl to normal_validate.htm using the same
        // cookie jar that will be used by every subsequent verification call.
        let mut cookies = snapshot.cookies.clone();
        let (normal_url, normal_body) =
            get_following_redirects(&client, &iframe_url, &HeaderMap::new(), &mut cookies).await?;
        let normal_html = String::from_utf8_lossy(&normal_body);

        // 2. Extract the server-rendered htoken and verify_modes URL.
        let htoken = regex::Regex::new(r#"(?i)htoken=([A-Za-z0-9_\-]+)"#)
            .map_err(|error| error.to_string())?
            .captures(&normal_html)
            .and_then(|value| value.get(1))
            .map(|value| value.as_str().to_owned())
            .ok_or("人脸验证：未能提取 htoken".to_owned())?;
        let verify_modes = regex::Regex::new(
            r#"(?i)window\.location\.href\s*=\s*[\"'](https://[^\"']*?/iv/mini/verify_modes\.htm\?[^\"']*)[\"']"#,
        )
        .map_err(|error| error.to_string())?
        .captures(&normal_html)
        .and_then(|value| value.get(1))
        .map(|value| value.as_str().replace("&amp;", "&"))
        .ok_or("人脸验证：未能提取 verify_modes 链接".to_owned())?;
        let verify_modes = if verify_modes.ends_with("_umidfg=") {
            format!("{verify_modes}1")
        } else {
            verify_modes
        };

        // 3. Follow verify_modes.htm to identity_verify.htm with the same jar.
        let (identity_url, identity_body) =
            get_following_redirects(&client, &verify_modes, &HeaderMap::new(), &mut cookies)
                .await?;
        let identity_html = String::from_utf8_lossy(&identity_body);

        // 4. Extract and publish the phone face-verification QR code.
        let face_content = regex::Regex::new(
            r#"(?i)new\s+Qrcode\s*\(\s*\{\s*text\s*:\s*[\"']([^\"']+)[\"']"#,
        )
        .map_err(|error| error.to_string())?
        .captures(&identity_html)
        .and_then(|value| value.get(1))
        .map(|value| value.as_str().to_owned())
        .ok_or("人脸验证：未能提取人脸验证二维码 URL".to_owned())?;
        let face_qr_url = qr_svg_data_url(&face_content)?;
        merge_cookie_jar(
            &mut cookies,
            &jar,
            &[&passport_url, &normal_url, &identity_url],
        );
        if let Ok(mut all) = sessions().lock() {
            if let Some(session) = all.get_mut(&session_id) {
                session.cookies = cookies.clone();
                session.face_htoken = htoken.clone();
                session.verification_qr_url = face_qr_url;
                session.message = "需要人脸验证，请使用手机闲鱼扫描二维码".to_owned();
            } else {
                return Ok::<(), String>(());
            }
        }

        // 5. Poll the official check endpoint every two seconds until the
        // phone completes verification or the five-minute session expires.
        let check_referer = format!(
            "https://passport.goofish.com/iv/mini/identity_verify.htm?htoken={htoken}"
        );
        let mut iv_check_url = None;
        loop {
            let expired = sessions()
                .lock()
                .ok()
                .and_then(|all| all.get(&session_id).cloned())
                .map(|session| now_millis().saturating_sub(session.created_at_ms) > 300_000)
                .unwrap_or(true);
            if expired {
                break;
            }
            match client
                .get("https://passport.goofish.com/iv/photoVerify/check.do")
                .query(&[("htoken", htoken.as_str())])
                .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
                .header("X-Requested-With", "XMLHttpRequest")
                .header(REFERER, &check_referer)
                .send()
                .await
            {
                Ok(response) => match response.json::<Value>().await {
                    Ok(body) => {
                        let content = body.get("content").unwrap_or(&Value::Null);
                        let code = json_param(content.get("code").unwrap_or(&Value::Null));
                        if code == "3" {
                            iv_check_url = content
                                .get("url")
                                .and_then(Value::as_str)
                                .filter(|value| !value.is_empty())
                                .map(str::to_owned);
                            if iv_check_url.is_some() {
                                break;
                            }
                        }
                    }
                    Err(_) => {}
                },
                Err(_) => {}
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let iv_check_url = iv_check_url.ok_or("人脸验证超时或未完成".to_owned())?;

        // 6. Follow ivCheckLogin.htm with the same jar, then collect the final
        // login cookie. The reference implementation only succeeds with unb.
        let mut finish_headers = HeaderMap::new();
        finish_headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/javascript, */*; q=0.01"),
        );
        finish_headers.insert(
            "X-Requested-With",
            HeaderValue::from_static("XMLHttpRequest"),
        );
        finish_headers.insert(
            REFERER,
            HeaderValue::from_str(&check_referer)
                .map_err(|_| "身份验证 Referer 格式异常".to_owned())?,
        );
        let (finish_url, _) =
            get_following_redirects(&client, &iv_check_url, &finish_headers, &mut cookies).await?;
        merge_cookie_jar(
            &mut cookies,
            &jar,
            &[&passport_url, &normal_url, &identity_url, &finish_url],
        );
        let account_id = login_account_id(&cookies);
        if account_id.is_empty() {
            return Err("人脸验证完成但未获取到 unb，登录失败".to_owned());
        }

        if let Ok(mut all) = sessions().lock() {
            if let Some(session) = all.get_mut(&session_id) {
                session.status = "success".to_owned();
                session.message = "身份验证完成，扫码登录成功".to_owned();
                session.cookies = cookies;
                session.face_htoken = htoken;
            }
        }
        Ok::<(), String>(())
    }
    .await;

    if let Err(error) = result {
        if let Ok(mut all) = sessions().lock() {
            if let Some(session) = all.get_mut(&session_id) {
                session.status = "expired".to_owned();
                session.message = error;
            }
        }
    }
}

async fn monitor_qr_status(session_id: String) {
    loop {
        let Some(snapshot) = sessions().lock().ok().and_then(|all| all.get(&session_id).cloned()) else {
            return;
        };
        if now_millis().saturating_sub(snapshot.created_at_ms) > 300_000 {
            if let Ok(mut all) = sessions().lock() {
                if let Some(session) = all.get_mut(&session_id) {
                    session.status = "expired".to_owned();
                    session.message = "二维码已过期，请重新生成".to_owned();
                }
            }
            return;
        }
        if matches!(
            snapshot.status.as_str(),
            "success" | "expired" | "cancelled" | "failed" | "verification_required"
        ) {
            return;
        }

        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::limited(8))
            .default_headers(reference_headers())
            .build();
        let response = match response {
            Ok(client) => {
                client
                    .post("https://passport.goofish.com/newlogin/qrcode/query.do")
                    .header(COOKIE, cookie_string(&snapshot.cookies))
                    .form(&snapshot.params)
                    .send()
                    .await
            }
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        let response = match response {
            Ok(response) => response,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut cookies = snapshot.cookies.clone();
        merge_set_cookies(&mut cookies, response.headers());
        let body = match response.json::<Value>().await {
            Ok(body) => body,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        let raw = body
            .pointer("/content/data/qrCodeStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match raw {
            "NEW" => {}
            "SCANED" => {
                if let Ok(mut all) = sessions().lock() {
                    if let Some(session) = all.get_mut(&session_id) {
                        session.status = "scanned".to_owned();
                        session.message = "已扫码，请在手机端确认登录".to_owned();
                    }
                }
            }
            "CONFIRMED"
                if face_verification_url(&body).is_some()
                    || json_truthy(body.pointer("/content/data/iframeRedirect")) =>
            {
                let iframe_url = face_verification_url(&body).unwrap_or_default();
                if iframe_url.is_empty() {
                    if let Ok(mut all) = sessions().lock() {
                        if let Some(session) = all.get_mut(&session_id) {
                            session.status = "expired".to_owned();
                            session.message = "闲鱼要求身份验证，但未返回验证地址".to_owned();
                        }
                    }
                    return;
                }
                if let Ok(mut all) = sessions().lock() {
                    if let Some(session) = all.get_mut(&session_id) {
                        session.status = "verification_required".to_owned();
                        session.message = "正在生成人脸验证二维码".to_owned();
                        session.cookies = cookies;
                        session.created_at_ms = now_millis();
                        session.verification_url = iframe_url.clone();
                    }
                }
                tauri::async_runtime::spawn(run_face_verification(
                    session_id.clone(),
                    iframe_url,
                ));
                return;
            }
            "CONFIRMED" => {
                let account_id = login_account_id(&cookies);
                if let Ok(mut all) = sessions().lock() {
                    if let Some(session) = all.get_mut(&session_id) {
                        session.cookies = cookies;
                        if !account_id.is_empty() {
                            session.status = "success".to_owned();
                            session.message = "扫码登录成功".to_owned();
                        } else {
                            session.status = "expired".to_owned();
                            session.message = "扫码确认完成但未获取到 unb，登录失败".to_owned();
                        }
                    }
                }
                return;
            }
            "EXPIRED" => {
                if let Ok(mut all) = sessions().lock() {
                    if let Some(session) = all.get_mut(&session_id) {
                        session.status = "expired".to_owned();
                        session.message = "二维码已过期，请重新生成".to_owned();
                    }
                }
                return;
            }
            _ => {
                if let Ok(mut all) = sessions().lock() {
                    if let Some(session) = all.get_mut(&session_id) {
                        session.status = "cancelled".to_owned();
                        session.message = "已取消扫码登录".to_owned();
                    }
                }
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
}

pub async fn generate_qr() -> Result<QrStart, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(35))
        .redirect(reqwest::redirect::Policy::limited(8))
        .default_headers(reference_headers())
        .build()
        .map_err(|error| error.to_string())?;
    let mut cookies = HashMap::new();
    let h5_api =
        "https://h5api.m.goofish.com/h5/mtop.gaia.nodejs.gaia.idle.data.gw.v2.index.get/1.0/";
    let first = client
        .get(h5_api)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Connection", "keep-alive")
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .send()
        .await
        .map_err(|error| format!("获取闲鱼登录令牌失败：{error}"))?;
    merge_set_cookies(&mut cookies, first.headers());
    let token = cookies
        .get("m_h5_tk")
        .or_else(|| cookies.get("_m_h5_tk"))
        .and_then(|value| value.split('_').next())
        .unwrap_or_default();
    let timestamp = now_millis().to_string();
    let app_key = "34839810";
    let data = serde_json::json!({"bizScene":"home"}).to_string();
    let sign = format!(
        "{:x}",
        md5::compute(format!("{token}&{timestamp}&{app_key}&{data}"))
    );
    let warm_params = [
        ("jsv", "2.7.2"),
        ("appKey", app_key),
        ("t", &timestamp),
        ("sign", &sign),
        ("v", "1.0"),
        ("type", "originaljson"),
        ("dataType", "json"),
        ("timeout", "20000"),
        ("api", "mtop.gaia.nodejs.gaia.idle.data.gw.v2.index.get"),
        ("data", data.as_str()),
    ];
    let warm = client
        .post(h5_api)
        .query(&warm_params)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Connection", "keep-alive")
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .header(COOKIE, cookie_string(&cookies))
        .send()
        .await
        .map_err(|error| format!("初始化闲鱼登录会话失败：{error}"))?;
    merge_set_cookies(&mut cookies, warm.headers());

    let mini_params = [
        ("lang", "zh_cn".to_owned()),
        ("appName", "xianyu".to_owned()),
        ("appEntrance", "web".to_owned()),
        ("styleType", "vertical".to_owned()),
        ("bizParams", String::new()),
        ("notLoadSsoView", "false".to_owned()),
        ("notKeepLogin", "false".to_owned()),
        ("isMobile", "false".to_owned()),
        ("qrCodeFirst", "false".to_owned()),
        ("stie", "77".to_owned()),
        ("rnd", fastrand::f64().to_string()),
    ];
    let mini = client
        .get("https://passport.goofish.com/mini_login.htm")
        .query(&mini_params)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Connection", "keep-alive")
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header(COOKIE, cookie_string(&cookies))
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .send()
        .await
        .map_err(|error| format!("获取闲鱼扫码参数失败：{error}"))?;
    merge_set_cookies(&mut cookies, mini.headers());
    let html = mini
        .text()
        .await
        .map_err(|_| "读取闲鱼扫码参数失败".to_owned())?;
    let capture = regex::Regex::new(r#"(?s)window\.viewData\s*=\s*(\{.*?\});"#)
        .map_err(|error| error.to_string())?
        .captures(&html)
        .and_then(|value| value.get(1))
        .map(|value| value.as_str().to_owned())
        .ok_or("闲鱼登录页面未返回 loginFormData")?;
    let view_data: Value =
        serde_json::from_str(&capture).map_err(|_| "闲鱼登录参数格式异常".to_owned())?;
    let form_data = view_data
        .get("loginFormData")
        .and_then(Value::as_object)
        .ok_or("闲鱼登录参数不完整")?;
    let mut params = form_data
        .iter()
        .map(|(key, value)| (key.clone(), json_param(value)))
        .collect::<HashMap<_, _>>();
    params.insert("umidTag".to_owned(), "SERVER".to_owned());
    let generated = client
        .get("https://passport.goofish.com/newlogin/qrcode/generate.do")
        .query(&params)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Connection", "keep-alive")
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .send()
        .await
        .map_err(|error| format!("生成闲鱼二维码失败：{error}"))?;
    merge_set_cookies(&mut cookies, generated.headers());
    let body: Value = generated
        .json()
        .await
        .map_err(|_| "闲鱼二维码接口返回格式异常".to_owned())?;
    let content = body
        .pointer("/content/data/codeContent")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            body.pointer("/content/message")
                .and_then(Value::as_str)
                .unwrap_or("生成闲鱼二维码失败")
                .to_owned()
        })?;
    for key in ["t", "ck"] {
        if let Some(value) = body.pointer(&format!("/content/data/{key}")) {
            params.insert(key.to_owned(), json_param(value));
        }
    }
    let session_id = Uuid::new_v4().to_string();
    let qr_code_url = qr_svg_data_url(content)?;
    let session = QrSession {
        status: "waiting".to_owned(),
        message: "请使用闲鱼 App 扫描二维码".to_owned(),
        params,
        cookies,
        created_at_ms: now_millis(),
        verification_url: String::new(),
        verification_qr_url: String::new(),
        face_htoken: String::new(),
    };
    let mut all = sessions()
        .lock()
        .map_err(|_| "二维码会话锁定失败".to_owned())?;
    all.retain(|_, item| now_millis().saturating_sub(item.created_at_ms) < 600_000);
    all.insert(session_id.clone(), session);
    drop(all);
    tauri::async_runtime::spawn(monitor_qr_status(session_id.clone()));
    Ok(QrStart {
        session_id,
        qr_code_url,
        message: "请使用闲鱼 App 扫描二维码".to_owned(),
    })
}

pub async fn poll_qr(session_id: &str) -> Result<QrPoll, String> {
    let session = sessions()
        .lock()
        .map_err(|_| "二维码会话锁定失败".to_owned())?
        .get(session_id)
        .cloned()
        .ok_or("二维码会话不存在或已过期")?;
    let account_id = if session.status == "success" {
        login_account_id(&session.cookies)
    } else {
        String::new()
    };
    Ok(QrPoll {
        status: session.status.clone(),
        message: session.message,
        verification_url: session.verification_url,
        verification_qr_url: session.verification_qr_url,
        account_id,
        cookie: if session.status == "success" {
            cookie_string(&session.cookies)
        } else {
            String::new()
        },
    })
}

fn json_string(value: Option<&Value>) -> String {
    value
        .and_then(|item| match item {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn nested_json_string(value: &Value, names: &[&str]) -> String {
    match value {
        Value::Object(map) => {
            for name in names {
                if let Some(candidate) = map.get(*name) {
                    let text = json_string(Some(candidate));
                    if !text.trim().is_empty() {
                        return text.trim().to_owned();
                    }
                }
            }
            map.values()
                .map(|child| nested_json_string(child, names))
                .find(|text| !text.is_empty())
                .unwrap_or_default()
        }
        Value::Array(items) => items
            .iter()
            .map(|child| nested_json_string(child, names))
            .find(|text| !text.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn first_order_value(values: &[String]) -> String {
    values.iter().find(|value| !value.trim().is_empty()).cloned().unwrap_or_default()
}

fn order_contact_value(item: &Value, names: &[&str]) -> String {
    for container_name in ["receiverInfoVO", "receiverInfo", "deliveryInfoVO", "deliveryInfo", "addressInfo", "logisticsInfoVO", "buyerInfoVO"] {
        if let Some(container) = item.get(container_name) {
            let value = nested_json_string(container, names);
            if !value.is_empty() {
                return value;
            }
        }
    }
    nested_json_string(item, names)
}

fn json_i64(value: Option<&Value>) -> i64 {
    value
        .and_then(|item| {
            item.as_i64()
                .or_else(|| item.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| item.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0)
}

fn json_bool(value: Option<&Value>) -> bool {
    value
        .and_then(|item| {
            item.as_bool()
                .or_else(|| item.as_str().map(|value| value == "true" || value == "1"))
        })
        .unwrap_or(false)
}

fn merchant_order_statuses(item: &Value) -> Vec<String> {
    let Some(columns) = item.get("columnVOList").and_then(Value::as_array) else {
        return Vec::new();
    };
    columns
        .iter()
        .find(|column| {
            matches!(column.get("name").and_then(Value::as_str), Some("发货/退款状态"))
                || matches!(column.get("num").and_then(Value::as_str), Some("2"))
        })
        .and_then(|column| column.get("contentVOList").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .flat_map(|content| [content.get("value"), content.get("key")])
        .map(|value| json_string(value.and_then(|value| value.get("text")).or(value)))
        .filter(|value| !value.trim().is_empty())
        .collect()
}

fn merchant_order_status(item: &Value) -> String {
    merchant_order_statuses(item).into_iter().next().unwrap_or_default()
}

fn merchant_order_status_code(raw: &str, display: &str, in_refund: bool) -> String {
    if in_refund { return "REFUNDING".to_owned(); }
    let raw = raw.trim().to_ascii_uppercase();
    match raw.as_str() {
        "WAIT_PAY" | "WAIT_BUYER_PAY" | "UNPAID" => "WAIT_PAY".to_owned(),
        "WAIT_SHIP" | "WAIT_SELLER_SEND_GOODS" | "WAIT_SEND_GOODS" | "WAIT_DELIVERY" | "PAID" => "WAIT_SHIP".to_owned(),
        "SHIPPED" | "WAIT_BUYER_CONFIRM_GOODS" | "WAIT_BUYER_CONFIRM_RECEIVE" | "WAIT_RECEIVE" => "SHIPPED".to_owned(),
        "REFUNDING" | "REFUND" | "IN_REFUND" => "REFUNDING".to_owned(),
        "CLOSED" | "TRADE_CLOSED" | "REFUND_CLOSED" | "CANCELLED" => "CLOSED".to_owned(),
        "SUCCESS" | "TRADE_SUCCESS" | "WAIT_SELLER_RATE" | "COMPLETED" => "SUCCESS".to_owned(),
        _ => match display {
            "待付款" | "待支付" => "WAIT_PAY".to_owned(),
            "待发货" | "待寄件" | "已付款" => "WAIT_SHIP".to_owned(),
            "已发货" | "待收货" | "已寄件" => "SHIPPED".to_owned(),
            "退款中" => "REFUNDING".to_owned(),
            "交易关闭" | "已关闭" | "退款关闭" => "CLOSED".to_owned(),
            "交易成功" | "已完成" => "SUCCESS".to_owned(),
            _ => raw,
        },
    }
}

/// 调用闲鱼 MTop 接口并返回响应体与服务端续期后的 Cookie。
///
/// 闲鱼接口使用 `_m_h5_tk` 参与签名；当服务端返回 token 过期且下发了新
/// Cookie 时，本函数会自动重试一次。所有卖家交易接口统一携带 COMMONPRO
/// 站点上下文，避免在业务代码中重复维护请求头。
pub(crate) async fn mtop_call(
    cookie: &str,
    api_name: &str,
    version: &str,
    response_type: &str,
    data: &Value,
) -> Result<(Value, String), String> {
    let parsed_cookie = cookie_parse(cookie);
    let account_key = parsed_cookie
        .get("unb")
        .or_else(|| parsed_cookie.get("munb"))
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "anonymous".to_owned());
    let account_lock = mtop_lock(&account_key);
    let _request_guard = account_lock.lock().await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let mut current_cookie = cookie.to_owned();
    let data_value = data.to_string();
    let mut warmed_session = false;
    for _ in 0..3 {
        let timestamp = now_millis().to_string();
        let cookies = cookie_parse(&current_cookie);
        let token = cookies
            .get("_m_h5_tk")
            .or_else(|| cookies.get("m_h5_tk"))
            .and_then(|value| value.split('_').next())
            .unwrap_or_default();
        let sign = format!(
            "{:x}",
            md5::compute(format!("{token}&{timestamp}&34839810&{data_value}"))
        );
        let is_im_token = api_name == "mtop.taobao.idlemessage.pc.login.token";
        let mut params = vec![
            ("jsv", "2.7.2".to_owned()),
            ("appKey", "34839810".to_owned()),
            ("t", timestamp),
            ("sign", sign),
            ("v", version.to_owned()),
            ("type", response_type.to_owned()),
            ("accountSite", "xianyu".to_owned()),
            ("dataType", "json".to_owned()),
            ("timeout", "20000".to_owned()),
            ("api", api_name.to_owned()),
            ("sessionOption", "AutoLoginOnly".to_owned()),
        ];
        // Keep the IM token request aligned with the reference project's
        // dedicated im_token_api.py request. These risk-context parameters
        // are required for the web token endpoint and are not sent by ordinary
        // product/order MTop calls.
        if is_im_token {
            params.extend([
                ("dangerouslySetWindvaneParams", "%5Bobject%20Object%5D".to_owned()),
                ("smToken", "token".to_owned()),
                ("queryToken", "sm".to_owned()),
                ("sm", "sm".to_owned()),
                ("spm_cnt", "a21ybx.im.0.0".to_owned()),
                ("spm_pre", "a21ybx.home.sidebar.1.4c053da6vYwnmf".to_owned()),
                ("log_id", "4c053da6vYwnmf".to_owned()),
            ]);
        }
        if api_name == "mtop.taobao.idle.trade.merchant.sold.get" {
            // This is the order list request issued by the seller workbench;
            // its page context is part of the normal request fingerprint.
            params.push(("spm_cnt", "a21107h.42826273.0.0".to_owned()));
        }
        let url = format!("https://h5api.m.goofish.com/h5/{api_name}/{version}/");
        let seller_api = api_name.starts_with("mtop.alibaba.idle.seller.")
            || api_name.starts_with("mtop.taobao.idle.merchant.refund.")
            || api_name == "mtop.idle.alipay.verify.url.query"
            || api_name == "mtop.taobao.idle.trade.merchant.sold.get";
        let mut request = client
            .post(url)
            .query(&params)
            .header(ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(COOKIE, &current_cookie)
            .header(ORIGIN, if seller_api { "https://seller.goofish.com" } else { "https://www.goofish.com" })
            .header(REFERER, if seller_api { "https://seller.goofish.com/?site=COMMONPRO" } else { "https://www.goofish.com/" })
            .header(USER_AGENT, if is_im_token {
                USER_AGENT_VALUE
            } else {
                USER_AGENT_VALUE
            });
        if is_im_token {
            request = request
                .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
                .header("cache-control", "no-cache")
                .header("pragma", "no-cache")
                .header("priority", "u=1, i")
                .header("sec-fetch-dest", "empty")
                .header("sec-fetch-mode", "cors")
                .header("sec-fetch-site", "same-site");
        } else {
            // Seller trade actions require the same site context that the
            // official COMMONPRO workbench sends with its MTop requests.
            request = request.header("idle_site_biz_code", "COMMONPRO");
        }
        let response = request
            .form(&[("data", data_value.as_str())])
            .send()
            .await
            .map_err(|error| format!("闲鱼接口请求失败：{error}"))?;
        let mut merged = cookie_parse(&current_cookie);
        merge_set_cookies(&mut merged, response.headers());
        let merged_cookie = cookie_string(&merged);
        let body: Value = response
            .json()
            .await
            .map_err(|_| "闲鱼接口返回格式异常".to_owned())?;
        let ret = body
            .get("ret")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .unwrap_or_default();
        if ret.contains("SUCCESS") {
            return Ok((body, merged_cookie));
        }
        if (ret.contains("TOKEN_EXOIRED") || ret.contains("TOKEN_EXPIRED"))
            && merged_cookie != current_cookie
        {
            current_cookie = merged_cookie;
            continue;
        }
        if (ret.contains("FAIL_SYS_TOKEN_EMPTY") || ret.contains("TOKEN_EMPTY"))
            && !warmed_session
        {
            // A freshly confirmed QR login can have the user session before
            // the H5 signing cookie reaches this request path. Warm the same
            // MTop bootstrap endpoint once, then sign and retry the original
            // call with the refreshed Cookie.
            warmed_session = true;
            let warmed_cookie = warm_mtop_session(&merged_cookie)
                .await
                .unwrap_or_else(|_| merged_cookie.clone());
            if warmed_cookie != current_cookie {
                current_cookie = warmed_cookie;
                continue;
            }
        }
        if ret.contains("SESSION_EXPIRED") || ret.contains("Session过期") {
            return Err("闲鱼登录已过期，请重新扫码登录".to_owned());
        }
        if ret.contains("FAIL_SYS_USER_VALIDATE")
            || ret.contains("RGV587")
            || ret.contains("WUA_IS_MACHINE")
        {
            let verification_url = body
                .pointer("/data/url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !verification_url.is_empty() {
                if let Ok(mut cookies) = im_validation_cookies().lock() {
                    cookies.insert(verification_url.to_owned(), merged_cookie);
                }
            }
            return Err(format!("{IM_VALIDATION_ERROR_PREFIX}{verification_url}"));
        }
        return Err(if ret.is_empty() {
            "闲鱼接口调用失败".to_owned()
        } else {
            ret.to_owned()
        });
    }
    Err("闲鱼接口重试次数过多".to_owned())
}

/// 分页读取当前账号的在售商品，并补充商品详情中的图片和库存信息。
pub async fn fetch_products(cookie: &str) -> Result<(Vec<Value>, String), String> {
    let user_id = cookie_parse(cookie).get("unb").cloned().unwrap_or_default();
    let mut current_cookie = cookie.to_owned();
    let mut result = Vec::new();
    for page in 1..=50_i64 {
        let data = serde_json::json!({ "needGroupInfo": false, "pageNumber": page, "pageSize": 20, "groupName": "在售", "groupId": "58877261", "defaultGroup": true, "userId": user_id });
        let (body, updated_cookie) = mtop_call(
            &current_cookie,
            "mtop.idle.web.xyh.item.list",
            "1.0",
            "originaljson",
            &data,
        )
        .await?;
        current_cookie = updated_cookie;
        let cards = body
            .pointer("/data/cardList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if cards.is_empty() {
            break;
        }
        for card in &cards {
            let Some(item) = card.get("cardData") else {
                continue;
            };
            let item_id = json_string(item.get("id"));
            if item_id.is_empty() || item_id.starts_with("auto_") {
                continue;
            }
            let title = json_string(item.get("title"));
            let price = json_string(item.pointer("/priceInfo/price"));
            let mut image_url = [
                "/picInfo/picUrl", "/picInfo/url", "/imageInfo/imageUrl",
                "/imageUrl", "/picUrl", "/mainPic", "/itemPic",
            ]
            .iter()
            .map(|path| json_string(item.pointer(path)))
            .find(|value| !value.is_empty())
            .unwrap_or_default();
            let mut stock = 0_i64;
            let mut description = String::new();
            if let Ok((detail, updated_cookie)) = mtop_call(
                &current_cookie,
                "mtop.taobao.idle.pc.detail",
                "1.0",
                "originaljson",
                &serde_json::json!({"itemId": item_id}),
            )
            .await
            {
                current_cookie = updated_cookie;
                description = json_string(detail.pointer("/data/itemDO/desc"));
                if image_url.is_empty() {
                    image_url = [
                        "/data/itemDO/imageInfos/0/url", "/data/itemDO/images/0/url",
                        "/data/itemDO/picUrl", "/data/itemDO/itemPic", "/data/itemDO/mainPic",
                    ]
                    .iter()
                    .map(|path| json_string(detail.pointer(path)))
                    .find(|value| !value.is_empty())
                    .unwrap_or_default();
                }
                stock = detail
                    .pointer("/data/itemDO/skuList")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().map(|sku| json_i64(sku.get("quantity"))).sum())
                    .unwrap_or(0);
            }
            if stock == 0 {
                stock = json_i64(item.get("quantity")).max(1);
            }
            result.push(serde_json::json!({ "item_id": item_id, "title": title, "image_url": image_url, "price": price, "stock": stock, "status": "已上架", "description": description }));
        }
        if cards.len() < 20 {
            break;
        }
    }
    Ok((result, current_cookie))
}

/// 分页读取卖家订单列表，保留官方原始时间字符串和商品规格字段。
pub async fn fetch_orders(cookie: &str) -> Result<(Vec<Value>, String), String> {
    let mut current_cookie = cookie.to_owned();
    let mut result = Vec::new();
    for page in 1..=100_i64 {
        let data = serde_json::json!({ "pageNumber": page, "rowsPerPage": 30, "orderIds": "", "queryCode": "ALL", "orderSearchParam": "{}" });
        let (body, updated_cookie) = mtop_call(
            &current_cookie,
            "mtop.taobao.idle.trade.merchant.sold.get",
            "1.0",
            // The seller workbench sends originaljson for this endpoint. The
            // generic `json` response type is treated as a different client
            // context by some accounts and can return a misleading token or
            // permission failure.
            "originaljson",
            &data,
        )
        .await?;
        current_cookie = updated_cookie;
        let module = body.pointer("/data/module");
        let items = module
            .and_then(|value| value.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for item in &items {
            let common = item.get("commonData").unwrap_or(&Value::Null);
            let buyer = item.get("buyerInfoVO").unwrap_or(&Value::Null);
            let price = item.get("priceVO").unwrap_or(&Value::Null);
            let product = item
                .get("merchantItemVO")
                .or_else(|| item.get("itemInfoVO"))
                .or_else(|| item.get("itemInfo"))
                .or_else(|| item.get("itemVO"))
                .unwrap_or(&Value::Null);
            let order_id = json_string(common.get("orderId"));
            if order_id.is_empty() {
                continue;
            }
            let in_refund = json_bool(common.get("inRefund"));
            let status = if in_refund {
                "退款中".to_owned()
            } else {
                [
                    json_string(common.get("orderStatusDesc")),
                    json_string(common.get("statusDesc")),
                    json_string(item.get("orderStatusDesc")),
                    json_string(item.get("statusDesc")),
                    merchant_order_status(item),
                    json_string(common.get("orderStatus")),
                    json_string(common.get("status")),
                    json_string(item.get("orderStatus")),
                    json_string(item.get("status")),
                ]
                .into_iter()
                .find(|value| !value.is_empty())
                .unwrap_or_default()
            };
            let status_code = merchant_order_status_code(&json_string(common.get("orderStatus")), &status, in_refund);
            let item_id = [common.get("itemId"), product.get("itemId"), item.get("itemId")]
                .iter()
                .map(|value| json_string(*value))
                .find(|value| !value.is_empty())
                .unwrap_or_default();
            let item_title = [
                common.get("itemTitle"),
                product.get("itemTitle"),
                product.get("title"),
                product.get("name"),
                item.get("itemTitle"),
            ]
            .iter()
            .map(|value| json_string(*value))
                .find(|value| !value.is_empty())
                .unwrap_or_default();
            let specification = product
                .get("itemInfoLines")
                .and_then(Value::as_array)
                .map(|lines| lines.iter().filter_map(|line| {
                    let key = json_string(line.get("key"));
                    let value = json_string(line.get("value"));
                    if key.is_empty() && value.is_empty() { None } else if key.is_empty() { Some(value) } else if value.is_empty() { Some(key) } else { Some(format!("{key}：{value}")) }
                }).collect::<Vec<_>>().join(" / "))
                .unwrap_or_default();
            let item_image_url = [
                product.get("itemPicUrl"), product.get("picUrl"), product.get("imageUrl"),
                common.get("itemPicUrl"), item.get("itemPicUrl"),
            ]
            .iter()
            .map(|value| json_string(*value))
            .find(|value| !value.is_empty())
            .unwrap_or_default();
            let shipping_refund_status = merchant_order_statuses(item).join(" / ");
            result.push(serde_json::json!({
                "order_id": order_id, "item_id": item_id, "item_image_url": item_image_url, "item_title": if item_title.is_empty() { format!("商品 {item_id}") } else { item_title },
                "specification": specification,
                "buyer_nick": first_order_value(&[
                    json_string(buyer.get("userNick")),
                    nested_json_string(item, &["buyerNick", "buyerNickname", "buyerName", "userNick"]),
                ]),
                "buyer_id": first_order_value(&[
                    json_string(buyer.get("buyerId")),
                    json_string(buyer.get("userId")),
                    nested_json_string(item, &["buyerId", "buyerUserId", "buyerUserIdStr", "userId", "userIdStr"]),
                ]),
                "receiver_name": order_contact_value(item, &["receiverName", "receiver_name", "consignee", "consigneeName", "收货人", "buyerName"]),
                "receiver_mobile": order_contact_value(item, &["receiverMobile", "receiver_mobile", "mobile", "phone", "tel", "consigneeMobile", "收货人电话"]),
                "receiver_address": order_contact_value(item, &["receiverAddress", "receiver_address", "address", "detailAddress", "consigneeAddress", "收货地址"]),
                "amount": json_string(price.get("totalPrice")), "quantity": json_i64(price.get("buyNum")).max(1),
                "status_code": status_code, "status": status, "shipping_refund_status": shipping_refund_status, "created_at": json_string(common.get("createTime"))
            }));
        }
        let next_page = module
            .and_then(|value| value.get("nextPage"))
            .is_some_and(|value| json_bool(Some(value)));
        if !next_page || items.len() < 30 {
            break;
        }
    }
    Ok((result, current_cookie))
}

/// Fetch one seller order from the same official seller order endpoint used by
/// the order synchronizer. The detail drawer calls this once when it opens so
/// timestamps and fee fields are not fabricated from the local order row.
/// 读取单笔订单的官方详情，用于时间、服务费和商品规格展示。
pub async fn fetch_order_detail(cookie: &str, order_no: &str) -> Result<(Value, String), String> {
    let data = serde_json::json!({
        "pageNumber": 1,
        "rowsPerPage": 1,
        "orderIds": order_no,
        "queryCode": "ALL",
        "orderSearchParam": "{}"
    });
    let (body, renewed_cookie) = mtop_call(
        cookie,
        "mtop.taobao.idle.trade.merchant.sold.get",
        "1.0",
        "json",
        &data,
    )
    .await?;
    let items = body
        .pointer("/data/module/items")
        .and_then(Value::as_array)
        .ok_or("官方订单详情响应缺少订单数据".to_owned())?;
    let item = items
        .iter()
        .find(|item| {
            [
                item.pointer("/commonData/orderId"),
                item.pointer("/commonData/orderIdStr"),
                item.get("orderId"),
                item.get("orderNo"),
            ]
            .iter()
            .flatten()
            .any(|value| json_string(Some(value)) == order_no)
        })
        .cloned()
        .ok_or("官方未返回该订单详情".to_owned())?;
    Ok((item, renewed_cookie))
}

/// Fetch the seller-side refund record for an order.  The workbench uses the
/// same refund list endpoint as the official refund-management page; keeping
/// this in the local session layer means the drawer and the future refund
/// management page share cookies, signing and renewal behavior.
/// 读取单笔订单的退款详情及售后服务记录。
pub async fn fetch_refund_detail(cookie: &str, order_no: &str) -> Result<(Value, String), String> {
    let mut current_cookie = cookie.to_owned();
    for dispute_status in ["1", "2", "3", "5"] {
        let data = serde_json::json!({
            "pageNumber": 1,
            "rowsPerPage": 50,
            "queryType": "refund",
            "refundSearchParam": { "disputeStatus": dispute_status, "queryCode": "ALL" }
        });
        let (body, renewed_cookie) = mtop_call(
            &current_cookie,
            "mtop.taobao.idle.merchant.refund.list",
            "1.0",
            "originaljson",
            &data,
        ).await?;
        current_cookie = renewed_cookie;
        let items = body
            .pointer("/data/data/items")
            .or_else(|| body.pointer("/data/module/items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(item) = items.into_iter().find(|item| {
            [
                item.pointer("/commonData/orderId"),
                item.pointer("/commonData/orderIdStr"),
                item.get("orderId"),
                item.get("orderNo"),
            ].iter().flatten().any(|value| json_string(Some(value)) == order_no)
        }) {
            let refund_id = [
                item.pointer("/refundInfoVO/refundId"),
                item.pointer("/refundInfo/refundId"),
                item.get("refundId"),
                item.get("disputeId"),
            ].iter().flatten().map(|value| json_string(Some(value))).find(|value| !value.trim().is_empty()).unwrap_or_default();
            if refund_id.is_empty() {
                return Ok((item, current_cookie));
            }
            let (detail, detail_cookie) = mtop_call(
                &current_cookie,
                "mtop.taobao.idle.merchant.refund.detail",
                "1.0",
                "originaljson",
                &serde_json::json!({ "orderId": order_no, "refundId": refund_id }),
            ).await?;
            let (service_record, service_cookie) = mtop_call(
                &detail_cookie,
                "mtop.taobao.idle.merchant.refund.service.record",
                "1.0",
                "originaljson",
                &serde_json::json!({ "orderId": order_no }),
            ).await.unwrap_or((serde_json::Value::Null, detail_cookie));
            return Ok((serde_json::json!({ "detail": detail, "service_record": service_record, "list": item }), service_cookie));
        }
    }
    Err("官方未返回该订单的退款详情".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{cookie_parse, login_account_id};

    #[test]
    fn login_account_id_accepts_unb() {
        assert_eq!(
            login_account_id(&cookie_parse("munb=mobile-id; unb=web-id")),
            "web-id"
        );
    }

    #[test]
    fn login_account_id_rejects_display_name_only_session() {
        assert!(login_account_id(&cookie_parse("munb=mobile-id; tracknick=buyer-name")).is_empty());
    }
}
