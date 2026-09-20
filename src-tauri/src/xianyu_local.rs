use base64::Engine as _;
use reqwest::header::{HeaderMap, ACCEPT, COOKIE, ORIGIN, REFERER, SET_COOKIE, USER_AGENT};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

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

async fn follow_redirects(
    client: &reqwest::Client,
    url: &str,
    cookies: &mut HashMap<String, String>,
    referer: &str,
) -> Result<(String, String), String> {
    let mut current = reqwest::Url::parse(url).map_err(|_| "身份验证地址格式异常".to_owned())?;
    for _ in 0..8 {
        let response = client
            .get(current.clone())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .header(COOKIE, cookie_string(cookies))
            .header(REFERER, referer)
            .send()
            .await
            .map_err(|error| format!("请求闲鱼身份验证失败：{error}"))?;
        merge_set_cookies(cookies, response.headers());
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or("身份验证跳转地址缺失")?;
            current = reqwest::Url::parse(location)
                .or_else(|_| current.join(location))
                .map_err(|_| "身份验证跳转地址异常".to_owned())?;
            continue;
        }
        let final_url = current.to_string();
        let html = response
            .text()
            .await
            .map_err(|_| "读取身份验证页面失败".to_owned())?;
        return Ok((final_url, html));
    }
    Err("身份验证跳转次数过多".to_owned())
}

async fn prepare_face_verification(
    client: &reqwest::Client,
    iframe_url: &str,
    cookies: &mut HashMap<String, String>,
) -> Result<(String, String, String), String> {
    let (_, normal_html) =
        follow_redirects(client, iframe_url, cookies, "https://passport.goofish.com/").await?;
    let verify_modes = regex::Regex::new(
        r#"(?i)window\.location\.href\s*=\s*["']([^"']*?/iv/mini/verify_modes\.htm\?[^"']*)["']"#,
    )
    .map_err(|error| error.to_string())?
    .captures(&normal_html)
    .and_then(|value| value.get(1))
    .map(|value| value.as_str().replace("&amp;", "&"))
    .ok_or("身份验证未返回验证方式地址")?;
    let verify_modes = if verify_modes.ends_with("_umidfg=") {
        format!("{verify_modes}1")
    } else {
        verify_modes
    };
    let (_, identity_html) = follow_redirects(client, &verify_modes, cookies, iframe_url).await?;
    let htoken = regex::Regex::new(r#"(?i)htoken=([A-Za-z0-9_\-]+)"#)
        .map_err(|error| error.to_string())?
        .captures(&normal_html)
        .and_then(|value| value.get(1))
        .map(|value| value.as_str().to_owned())
        .ok_or("身份验证未返回 htoken")?;
    let content = regex::Regex::new(r#"(?i)new\s+Qrcode\s*\(\s*\{\s*text\s*:\s*["']([^"']+)["']"#)
        .map_err(|error| error.to_string())?
        .captures(&identity_html)
        .and_then(|value| value.get(1))
        .map(|value| value.as_str().to_owned())
        .ok_or("身份验证未返回二维码")?;
    Ok((htoken, iframe_url.to_owned(), qr_svg_data_url(&content)?))
}

async fn poll_face_verification(
    client: &reqwest::Client,
    htoken: &str,
    cookies: &mut HashMap<String, String>,
) -> Result<(Option<String>, bool), String> {
    if htoken.trim().is_empty() {
        return Ok((None, false));
    }
    let response = client
        .get("https://passport.goofish.com/iv/photoVerify/check.do")
        .query(&[("htoken", htoken)])
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(COOKIE, cookie_string(cookies))
        .header(ORIGIN, "https://passport.goofish.com")
        .header(
            REFERER,
            format!("https://passport.goofish.com/iv/mini/identity_verify.htm?htoken={htoken}"),
        )
        .header("X-Requested-With", "XMLHttpRequest")
        .send()
        .await
        .map_err(|error| format!("查询身份验证状态失败：{error}"))?;
    merge_set_cookies(cookies, response.headers());
    let body: Value = response
        .json()
        .await
        .map_err(|_| "身份验证状态格式异常".to_owned())?;
    let content = body.get("content").unwrap_or(&Value::Null);
    let code = json_param(content.get("code").unwrap_or(&Value::Null));
    let message = json_param(
        content
            .get("message")
            .or_else(|| body.get("message"))
            .unwrap_or(&Value::Null),
    );
    let illegal = code.eq_ignore_ascii_case("AUTH_TOKEN_ILLEGAL")
        || message.to_ascii_uppercase().contains("AUTH_TOKEN_ILLEGAL")
        || message.contains("核身token不合法");
    Ok((
        if code == "3" {
            content
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        },
        illegal,
    ))
}

async fn poll_face(
    client: &reqwest::Client,
    session_id: &str,
    session: &QrSession,
) -> Result<QrPoll, String> {
    let mut cookies = session.cookies.clone();
    let mut htoken = session.face_htoken.clone();
    let mut verification_url = session.verification_url.clone();
    let mut verification_qr_url = session.verification_qr_url.clone();
    if htoken.is_empty() && !verification_url.is_empty() {
        match prepare_face_verification(client, &verification_url, &mut cookies).await {
            Ok(result) => {
                htoken = result.0;
                verification_url = result.1;
                verification_qr_url = result.2;
            }
            Err(error) => {
                return Ok(QrPoll {
                    status: "verification_required".to_owned(),
                    message: format!("身份验证二维码生成失败：{error}"),
                    verification_url,
                    verification_qr_url,
                    account_id: String::new(),
                    cookie: String::new(),
                })
            }
        }
    }
    let (finish_url, illegal) = poll_face_verification(client, &htoken, &mut cookies).await?;
    if illegal {
        let updated = QrSession {
            status: "verification_required".to_owned(),
            message: "请使用手机闲鱼扫描当前二维码完成验证".to_owned(),
            params: session.params.clone(),
            cookies,
            created_at_ms: session.created_at_ms,
            verification_url: verification_url.clone(),
            verification_qr_url: verification_qr_url.clone(),
            face_htoken: htoken,
        };
        sessions()
            .lock()
            .map_err(|_| "二维码会话锁定失败".to_owned())?
            .insert(session_id.to_owned(), updated.clone());
        return Ok(QrPoll {
            status: updated.status,
            message: updated.message,
            verification_url,
            verification_qr_url,
            account_id: String::new(),
            cookie: String::new(),
        });
    }
    if let Some(url) = finish_url {
        let referer =
            format!("https://passport.goofish.com/iv/mini/identity_verify.htm?htoken={htoken}");
        let _ = follow_redirects(client, &url, &mut cookies, &referer).await?;
        let account_id = cookies
            .get("unb")
            .or_else(|| cookies.get("tracknick"))
            .cloned()
            .unwrap_or_default();
        if !account_id.is_empty() {
            let cookie = cookie_string(&cookies);
            let updated = QrSession {
                status: "success".to_owned(),
                message: "身份验证完成，扫码登录成功".to_owned(),
                params: session.params.clone(),
                cookies,
                created_at_ms: session.created_at_ms,
                verification_url: verification_url.clone(),
                verification_qr_url: verification_qr_url.clone(),
                face_htoken: htoken,
            };
            sessions()
                .lock()
                .map_err(|_| "二维码会话锁定失败".to_owned())?
                .insert(session_id.to_owned(), updated.clone());
            return Ok(QrPoll {
                status: updated.status,
                message: updated.message,
                verification_url,
                verification_qr_url,
                account_id,
                cookie,
            });
        }
    }
    let updated = QrSession {
        status: "verification_required".to_owned(),
        message: "需要身份验证，请使用手机闲鱼扫描二维码".to_owned(),
        params: session.params.clone(),
        cookies,
        created_at_ms: session.created_at_ms,
        verification_url: verification_url.clone(),
        verification_qr_url: verification_qr_url.clone(),
        face_htoken: htoken,
    };
    sessions()
        .lock()
        .map_err(|_| "二维码会话锁定失败".to_owned())?
        .insert(session_id.to_owned(), updated.clone());
    Ok(QrPoll {
        status: updated.status.clone(),
        message: updated.message.clone(),
        verification_url,
        verification_qr_url,
        account_id: String::new(),
        cookie: String::new(),
    })
}

pub async fn generate_qr() -> Result<QrStart, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(35))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()
        .map_err(|error| error.to_string())?;
    let mut cookies = HashMap::new();
    let h5_api =
        "https://h5api.m.goofish.com/h5/mtop.gaia.nodejs.gaia.idle.data.gw.v2.index.get/1.0/";
    let first = client
        .get(h5_api)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/plain, */*")
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
    ];
    let warm = client
        .post(h5_api)
        .query(&warm_params)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(COOKIE, cookie_string(&cookies))
        .form(&[("data", data.as_str())])
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
        .header(COOKIE, cookie_string(&cookies))
        .header(REFERER, "https://passport.goofish.com/")
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
        .header(COOKIE, cookie_string(&cookies))
        .header(REFERER, "https://passport.goofish.com/")
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
    if now_millis().saturating_sub(session.created_at_ms) > 300_000 {
        return Ok(QrPoll {
            status: "expired".to_owned(),
            message: "二维码已过期，请重新生成".to_owned(),
            verification_url: String::new(),
            verification_qr_url: String::new(),
            account_id: String::new(),
            cookie: String::new(),
        });
    }
    if matches!(
        session.status.as_str(),
        "success" | "expired" | "cancelled" | "failed"
    ) {
        let account_id = session
            .cookies
            .get("unb")
            .or_else(|| session.cookies.get("tracknick"))
            .cloned()
            .unwrap_or_default();
        return Ok(QrPoll {
            status: session.status,
            message: session.message,
            verification_url: session.verification_url,
            verification_qr_url: session.verification_qr_url,
            account_id,
            cookie: cookie_string(&session.cookies),
        });
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    if session.status == "verification_required" {
        return poll_face(&client, session_id, &session).await;
    }
    let mut cookies = session.cookies.clone();
    let response = client
        .post("https://passport.goofish.com/newlogin/qrcode/query.do")
        .header(USER_AGENT, USER_AGENT_VALUE)
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(COOKIE, cookie_string(&cookies))
        .header(REFERER, "https://passport.goofish.com/")
        .header(ORIGIN, "https://passport.goofish.com")
        .form(&session.params)
        .send()
        .await
        .map_err(|error| format!("查询扫码状态失败：{error}"))?;
    merge_set_cookies(&mut cookies, response.headers());
    let body: Value = response
        .json()
        .await
        .map_err(|_| "闲鱼扫码状态返回格式异常".to_owned())?;
    let raw = body
        .pointer("/content/data/qrCodeStatus")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let verification_url = body
        .pointer("/content/data/iframeRedirectUrl")
        .or_else(|| body.pointer("/content/data/iframeUrl"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let (status, message) = match raw {
        "NEW" => ("waiting", "请使用闲鱼 App 扫描二维码"),
        "SCANED" => ("scanned", "已扫码，请在手机端确认登录"),
        "EXPIRED" => ("expired", "二维码已过期，请重新生成"),
        "CONFIRMED"
            if json_truthy(body.pointer("/content/data/iframeRedirect"))
                && verification_url.is_empty() =>
        {
            ("failed", "闲鱼要求身份验证，但未返回验证地址")
        }
        "CONFIRMED"
            if json_truthy(body.pointer("/content/data/iframeRedirect"))
                || !verification_url.is_empty() =>
        {
            (
                "verification_required",
                "需要身份验证，请使用手机闲鱼扫描二维码",
            )
        }
        "CONFIRMED" => ("success", "扫码登录成功"),
        "CANCELED" | "CANCELLED" => ("cancelled", "已取消扫码登录"),
        _ => ("waiting", "等待扫码确认"),
    };
    let account_id = cookies
        .get("unb")
        .or_else(|| cookies.get("tracknick"))
        .cloned()
        .unwrap_or_default();
    let mut face_htoken = session.face_htoken;
    let mut verification_qr_url = session.verification_qr_url;
    if status == "verification_required" && !verification_url.is_empty() {
        match prepare_face_verification(&client, &verification_url, &mut cookies).await {
            Ok(result) => {
                face_htoken = result.0;
                verification_qr_url = result.2;
            }
            Err(_) => {
                verification_qr_url = qr_svg_data_url(&verification_url)?;
            }
        }
    }
    let created_at_ms =
        if status == "verification_required" && session.status != "verification_required" {
            now_millis()
        } else {
            session.created_at_ms
        };
    let updated = QrSession {
        status: status.to_owned(),
        message: message.to_owned(),
        params: session.params,
        cookies: cookies.clone(),
        created_at_ms,
        verification_url: verification_url.clone(),
        verification_qr_url: verification_qr_url.clone(),
        face_htoken,
    };
    sessions()
        .lock()
        .map_err(|_| "二维码会话锁定失败".to_owned())?
        .insert(session_id.to_owned(), updated);
    Ok(QrPoll {
        status: status.to_owned(),
        message: message.to_owned(),
        verification_url,
        verification_qr_url,
        account_id,
        cookie: if status == "success" {
            cookie_string(&cookies)
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

fn merchant_order_status(item: &Value) -> String {
    let Some(columns) = item.get("columnVOList").and_then(Value::as_array) else {
        return String::new();
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
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default()
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

pub(crate) async fn mtop_call(
    cookie: &str,
    api_name: &str,
    version: &str,
    response_type: &str,
    data: &Value,
) -> Result<(Value, String), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let mut current_cookie = cookie.to_owned();
    let data_value = data.to_string();
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
        let params = [
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
        let url = format!("https://h5api.m.goofish.com/h5/{api_name}/{version}/");
        let response = client
            .post(url)
            .query(&params)
            .header(ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(COOKIE, &current_cookie)
            .header(ORIGIN, "https://www.goofish.com")
            .header(REFERER, "https://www.goofish.com/")
            // Seller trade actions require the same site context that the
            // official COMMONPRO workbench sends with its MTop requests.
            .header("idle_site_biz_code", "COMMONPRO")
            .header(USER_AGENT, USER_AGENT_VALUE)
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
        if ret.contains("SESSION_EXPIRED") || ret.contains("Session过期") {
            return Err("闲鱼登录已过期，请重新扫码登录".to_owned());
        }
        return Err(if ret.is_empty() {
            "闲鱼接口调用失败".to_owned()
        } else {
            ret.to_owned()
        });
    }
    Err("闲鱼接口重试次数过多".to_owned())
}

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

pub async fn fetch_orders(cookie: &str) -> Result<(Vec<Value>, String), String> {
    let mut current_cookie = cookie.to_owned();
    let mut result = Vec::new();
    for page in 1..=100_i64 {
        let data = serde_json::json!({ "pageNumber": page, "rowsPerPage": 30, "orderIds": "", "queryCode": "ALL", "orderSearchParam": "{}" });
        let (body, updated_cookie) = mtop_call(
            &current_cookie,
            "mtop.taobao.idle.trade.merchant.sold.get",
            "1.0",
            "json",
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
                .get("itemInfoVO")
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
            result.push(serde_json::json!({
                "order_id": order_id, "item_id": item_id, "item_title": if item_title.is_empty() { format!("商品 {item_id}") } else { item_title },
                "buyer_nick": json_string(buyer.get("userNick")), "buyer_id": json_string(buyer.get("buyerId")),
                "amount": json_string(price.get("totalPrice")), "quantity": json_i64(price.get("buyNum")).max(1),
                "status_code": status_code, "status": status, "created_at": json_string(common.get("createTime"))
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
