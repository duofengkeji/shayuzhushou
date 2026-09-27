use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{ACCEPT, CONTENT_TYPE, ORIGIN, REFERER, USER_AGENT};
use rsa::pkcs8::DecodePublicKey;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPublicKey};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

use crate::xianyu_local;

const LOGIN_URL: &str = "https://passport.goofish.com/newlogin/login.do?appName=xianyu&fromSite=77";
// Keep the password form aligned with the browser that hosts the official
// seller login. On macOS that is the native Safari/WebKit context; other
// hosts use the reference project's stable Chrome/Win32 fingerprint.
#[cfg(target_os = "macos")]
const LOGIN_UA: &str = xianyu_local::USER_AGENT_VALUE;
#[cfg(target_os = "macos")]
const LOGIN_PLATFORM: &str = "MacIntel";
#[cfg(not(target_os = "macos"))]
const LOGIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
#[cfg(not(target_os = "macos"))]
const LOGIN_PLATFORM: &str = "Win32";
#[cfg(not(target_os = "macos"))]
const LOGIN_SEC_CH_UA: &str = "\"Google Chrome\";v=\"146\", \"Not=A?Brand\";v=\"8\", \"Chromium\";v=\"146\"";
const LOGIN_DOCUMENT_REFERER: &str = "https://seller.goofish.com/";
const LOGIN_APP_ENTRANCE: &str = "seller_web";
const LOGIN_BIZ_PARAMS: &str = "";
const LOGIN_MODULUS: &str = concat!(
    "d3bcef1f00424f3261c89323fa8cdfa12bbac400d9fe8bb627e8d27a44bd5d59",
    "dce559135d678a8143beb5b8d7056c4e1f89c4e1f152470625b7b41944a97f",
    "02da6f605a49a93ec6eb9cbaf2e7ac2b26a354ce69eb265953d2c29e395d6d8",
    "c1cdb688978551aa0f7521f290035fad381178da0bea8f9e6adce39020f513133fb"
);

fn business_data(body: &Value) -> Result<&Value, String> {
    let data = body.get("data").ok_or("平台接口缺少 data".to_owned())?;
    if data.get("code").and_then(Value::as_str) != Some("success") {
        return Err(data.get("msg").and_then(Value::as_str).unwrap_or("平台接口未返回成功").to_owned());
    }
    Ok(data.get("data").unwrap_or(&Value::Null))
}

fn encrypt(password: &str, public_key: &RsaPublicKey) -> Result<Vec<u8>, String> {
    public_key.encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, password.as_bytes())
        .map_err(|_| "密码加密失败".to_owned())
}

/// Parse the public key returned by the seller API.
///
/// The endpoint has returned more than one representation over time: PEM,
/// base64 encoded SubjectPublicKeyInfo, PKCS#1 DER, and (for some accounts)
/// a JWK object serialized as a string. Keep the wire-format handling here so
/// the registration flow only has to deal with an RSA key.
fn parse_public_key(value: &str) -> Option<RsaPublicKey> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    // Some responses contain JSON escaped newlines (\\n) even after the
    // outer response has been decoded.
    let unescaped = value.replace("\\r", "\r").replace("\\n", "\n");
    for candidate in [value, unescaped.as_str()] {
        if let Ok(key) = RsaPublicKey::from_public_key_pem(candidate) {
            return Some(key);
        }
        if let Ok(key) = RsaPublicKey::from_pkcs1_pem(candidate) {
            return Some(key);
        }
    }

    // A few versions return a JWK as a JSON string instead of PEM. Accept
    // both the object itself and a nested `publicKey` object.
    if let Ok(object) = serde_json::from_str::<Value>(value) {
        if let Some(key) = object.get("publicKey").and_then(Value::as_str)
            .and_then(parse_public_key)
        {
            return Some(key);
        }
        let modulus = object.get("n").or_else(|| object.get("modulus"));
        let exponent = object.get("e").or_else(|| object.get("exponent"));
        if let (Some(modulus), Some(exponent)) = (modulus, exponent) {
            if let (Some(modulus), Some(exponent)) = (jwk_integer(modulus, false), jwk_integer(exponent, true)) {
                if let Ok(key) = RsaPublicKey::new(modulus, exponent) {
                    return Some(key);
                }
            }
        }
    }

    // Decode unheaded base64 values and try both standard RSA encodings. The
    // URL-safe alphabet is included because JWK and some gateway responses
    // use it without padding.
    let compact: String = value.chars().filter(|character| !character.is_whitespace()).collect();
    for decoded in [
        STANDARD.decode(&compact),
        STANDARD_NO_PAD.decode(&compact),
        URL_SAFE.decode(&compact),
        URL_SAFE_NO_PAD.decode(&compact),
    ].into_iter().flatten() {
        if let Ok(text) = std::str::from_utf8(&decoded) {
            if let Some(key) = parse_public_key(text) {
                return Some(key);
            }
        }
        if let Ok(key) = RsaPublicKey::from_public_key_der(&decoded) {
            return Some(key);
        }
        if let Ok(key) = RsaPublicKey::from_pkcs1_der(&decoded) {
            return Some(key);
        }
        // A bare RSA modulus is also occasionally returned. It is only
        // accepted at RSA-sized lengths to avoid treating arbitrary text as a
        // valid key; the platform uses the conventional exponent 65537.
        if decoded.len() >= 128 {
            if let Ok(key) = RsaPublicKey::new(BigUint::from_bytes_be(&decoded), BigUint::from(65537_u32)) {
                return Some(key);
            }
        }
    }

    // Finally support a bare hexadecimal modulus (the format used by some
    // older seller pages).
    if value.len() >= 256 && value.len() % 2 == 0 && value.chars().all(|character| character.is_ascii_hexdigit()) {
        let bytes = (0..value.len())
            .step_by(2)
            .filter_map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
            .collect::<Vec<_>>();
        if bytes.len() * 2 == value.len() {
            if let Ok(key) = RsaPublicKey::from_public_key_der(&bytes) {
                return Some(key);
            }
            if let Ok(key) = RsaPublicKey::from_pkcs1_der(&bytes) {
                return Some(key);
            }
            if let Ok(key) = RsaPublicKey::new(BigUint::from_bytes_be(&bytes), BigUint::from(65537_u32)) {
                return Some(key);
            }
        }
    }

    None
}

fn jwk_integer(value: &Value, exponent: bool) -> Option<BigUint> {
    match value {
        Value::String(text) if exponent => {
            URL_SAFE_NO_PAD.decode(text).ok()
                .or_else(|| STANDARD.decode(text).ok())
                .map(|bytes| BigUint::from_bytes_be(&bytes))
                .or_else(|| BigUint::parse_bytes(text.as_bytes(), 10))
        }
        Value::String(text) => URL_SAFE_NO_PAD.decode(text).ok()
            .or_else(|| STANDARD.decode(text).ok())
            .map(|bytes| BigUint::from_bytes_be(&bytes))
            .or_else(|| BigUint::parse_bytes(text.as_bytes(), 10)),
        Value::Number(number) => number.as_u64().map(BigUint::from),
        _ => None,
    }
}

fn login_key() -> Result<RsaPublicKey, String> {
    RsaPublicKey::new(BigUint::parse_bytes(LOGIN_MODULUS.as_bytes(), 16).ok_or("登录公钥无效")?,
        BigUint::from(65537_u32)).map_err(|_| "登录公钥无效".to_owned())
}

pub(crate) struct ServiceInfo {
    pub login_name: String,
    pub display_name: String,
    pub mobile: String,
    pub role: String,
    pub platform_sub_id: String,
}

fn text_field(value: &Value, name: &str) -> String {
    match value.get(name) {
        Some(Value::String(text)) => text.to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

pub(crate) async fn list(cookie: &str) -> Result<(Vec<ServiceInfo>, String), String> {
    let mut cookie = cookie.to_owned();
    let mut all = Vec::new();
    for page_no in 1..=50 {
        let (body, next_cookie) = xianyu_local::mtop_call(&cookie,
            "mtop.alibaba.idle.seller.common.sub.account.info.list", "1.0", "originaljson",
            &json!({"pageNo":page_no,"pageSize":20})).await?;
        cookie = next_cookie;
        let payload = business_data(&body)?;
        let rows = payload.get("list").and_then(Value::as_array).ok_or("子账号列表缺少 list".to_owned())?;
        for row in rows {
            let login_name = text_field(row, "nick");
            if login_name.is_empty() { continue; }
            let role = row.get("roleNames").and_then(Value::as_str).unwrap_or_default().to_owned();
            all.push(ServiceInfo {
                login_name,
                display_name: text_field(row, "customName"),
                mobile: text_field(row, "mobile"),
                role,
                platform_sub_id: text_field(row, "subId"),
            });
        }
        let total = payload.get("total")
            .and_then(|value| value.as_u64().or_else(|| value.as_str().and_then(|text| text.parse().ok())))
            .unwrap_or(all.len() as u64);
        if rows.len() < 20 || all.len() >= total as usize { return Ok((all, cookie)); }
    }
    Err("子账号列表超出分页上限，请稍后重试".to_owned())
}

/// Delete a service account from the official seller console.
///
/// The seller page calls this endpoint with the platform `subId` (the local
/// account UUID is only our own bookkeeping key). Keep the renewed mtop
/// cookie so the caller can persist any token rotation before removing local
/// records.
pub(crate) async fn delete(cookie: &str, platform_sub_id: &str) -> Result<String, String> {
    if platform_sub_id.trim().is_empty() {
        return Err("官方子账号缺少 subId，无法调用远程删除接口".to_owned());
    }
    let (body, renewed_cookie) = xianyu_local::mtop_call(
        cookie,
        "mtop.alibaba.idle.seller.platform.sub.account.delete",
        "1.0",
        "originaljson",
        &json!({"subId": platform_sub_id}),
    ).await?;
    business_data(&body)?;
    Ok(renewed_cookie)
}

pub(crate) async fn register(
    cookie: &str, login_suffix: &str, display_name: &str, mobile: &str, password: &str,
) -> Result<(String, String), String> {
    let mut cookie = cookie.to_owned();
    let (prefix_body, next) = xianyu_local::mtop_call(&cookie,
        "mtop.alibaba.idle.seller.platform.usergroup.nick.get", "1.0", "originaljson", &json!({})).await?;
    cookie = next;
    let prefix_data = business_data(&prefix_body)?;
    let prefix = prefix_data.as_str()
        .or_else(|| prefix_data.get("nick").and_then(Value::as_str))
        .or_else(|| prefix_data.get("userNick").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or("平台未返回子账号登录名前缀".to_owned())?;

    let (role_body, next) = xianyu_local::mtop_call(&cookie,
        "mtop.alibaba.idle.seller.platform.user.role.list", "1.0", "originaljson", &json!({})).await?;
    cookie = next;
    let roles = business_data(&role_body)?.as_array().ok_or("平台未返回角色列表".to_owned())?;
    let role_id = roles.iter().find(|role| role.get("roleName").and_then(Value::as_str) == Some("管理员"))
        .and_then(|role| role.get("roleId"))
        .cloned().ok_or("平台未提供管理员角色".to_owned())?;

    let (key_body, next) = xianyu_local::mtop_call(&cookie,
        "mtop.alibaba.idle.seller.platform.sub.account.sub.pk", "1.0", "originaljson", &json!({})).await?;
    cookie = next;
    let key_data = business_data(&key_body)?;
    let public_key = key_data.get("publicKey").and_then(Value::as_str).ok_or("平台未返回创建子账号公钥".to_owned())?;
    let crypto_key = key_data.get("cryptoKey").and_then(Value::as_str).ok_or("平台未返回创建子账号密钥标识".to_owned())?;
    let key = parse_public_key(public_key)
        .ok_or_else(|| "平台创建子账号公钥格式无效".to_owned())?;
    let encrypted_password = base64::engine::general_purpose::STANDARD.encode(encrypt(password, &key)?);
    let (result, next) = xianyu_local::mtop_call(&cookie,
        "mtop.alibaba.idle.seller.platform.common.sub.account.register", "1.0", "originaljson",
        &json!({"nick":login_suffix,"mobile":mobile,"roleIds":role_id,"customName":display_name,
            "encryptedPassword":encrypted_password,"cryptoKey":crypto_key})).await?;
    business_data(&result)?;
    Ok((format!("{prefix}:{login_suffix}"), next))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasswordLoginResult {
    pub status: String,
    pub message: String,
    pub verification_url: String,
    #[serde(skip_serializing)]
    pub cookie: String,
    #[serde(skip_serializing)]
    pub context_token: String,
    #[serde(skip_serializing)]
    pub sms_token: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
}

fn collect_cookies(jar: &Jar) -> String {
    let mut cookies = HashMap::new();
    for url in ["https://passport.goofish.com/", "https://www.goofish.com/", "https://seller.goofish.com/"] {
        if let Ok(url) = reqwest::Url::parse(url) {
            if let Some(header) = jar.cookies(&url) {
                if let Ok(value) = header.to_str() {
                    for part in value.split(';') {
                        if let Some((name, value)) = part.trim().split_once('=') {
                            if !name.is_empty() && !value.is_empty() { cookies.insert(name.to_owned(), value.to_owned()); }
                        }
                    }
                }
            }
        }
    }
    cookies.into_iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("; ")
}

fn login_verification_url(body: &Value) -> String {
    // login.do puts the slider URL in top-level data.url. Identity checks
    // instead use content.data.iframeRedirectUrl.
    body.pointer("/data/url").and_then(Value::as_str).filter(|value| !value.is_empty())
        .or_else(|| body.pointer("/content/data/url").and_then(Value::as_str).filter(|value| !value.is_empty()))
        .or_else(|| body.pointer("/content/data/iframeRedirectUrl").and_then(Value::as_str))
        .unwrap_or_default().to_owned()
}

fn browser_headers(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(not(target_os = "macos"))]
    {
        builder
            .header("sec-ch-ua", LOGIN_SEC_CH_UA)
            .header("sec-ch-ua-mobile", "?0")
            .header("sec-ch-ua-platform", "\"Win32\"")
    }
    #[cfg(target_os = "macos")]
    {
        builder
    }
}

fn is_sms_identity_redirect(target: &str) -> bool {
    let target_lower = target.to_ascii_lowercase();
    target_lower.contains("identity_verify")
        && (target_lower.contains("tag=8")
            || target_lower.contains("sms")
            || target_lower.contains("phone"))
}

pub(crate) async fn password_login(login_name: &str, password: &str, initial_cookie: Option<&str>) -> Result<PasswordLoginResult, String> {
    let password2 = hex(&encrypt(password, &login_key()?)?);
    let fields = [
        ("keepLogin", "false"), ("isIframe", "true"), ("documentReferer", LOGIN_DOCUMENT_REFERER),
        ("defaultView", "password"), ("appName", "xianyu"), ("appEntrance", LOGIN_APP_ENTRANCE),
        ("bizParams", LOGIN_BIZ_PARAMS),
        ("mainPage", "false"), ("isMobile", "false"), ("lang", "zh_CN"), ("returnUrl", ""),
        ("fromSite", "77"), ("weiBoMpBridge", ""), ("jsVersion", "0.10.36"),
        ("screenPixel", "1920x1080"), ("navlanguage", "zh-CN"), ("navUserAgent", LOGIN_UA),
        ("navPlatform", LOGIN_PLATFORM), ("umidGetStatusVal", "255"), ("umidTag", "SERVER"),
        ("loginId", login_name), ("password2", password2.as_str()),
    ];
    let jar = Arc::new(Jar::default());
    if let Some(cookie) = initial_cookie {
        let passport = reqwest::Url::parse("https://passport.goofish.com/").map_err(|error| error.to_string())?;
        for (name, value) in cookie.split(';').filter_map(|part| part.trim().split_once('=')) {
            if !name.is_empty() && !value.is_empty() {
                jar.add_cookie_str(&format!("{name}={value}; Domain=.goofish.com; Path=/"), &passport);
            }
        }
    }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30))
        .cookie_provider(jar.clone())
        // The reference keeps the response that contains the signed challenge
        // and handles any redirect explicitly. Following it here can discard
        // the Set-Cookie pair that is needed by the slider page.
        .redirect(reqwest::redirect::Policy::none()).build().map_err(|error| error.to_string())?;
    let response = browser_headers(client.post(LOGIN_URL).form(&fields))
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(USER_AGENT, LOGIN_UA)
        .header(ORIGIN, "https://passport.goofish.com")
        .header(REFERER, "https://passport.goofish.com/")
        .header("accept-language", "zh-CN,zh;q=0.9,en;q=0.8")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
        .send().await.map_err(|error| error.to_string())?;
    let body: Value = response.json().await.map_err(|_| "登录接口未返回 JSON".to_owned())?;
    let data = body.pointer("/content/data").unwrap_or(&Value::Null);
    let ret = body.get("ret").and_then(Value::as_array).cloned().unwrap_or_default();
    let context_token = data.get("contextToken").and_then(Value::as_str).unwrap_or_default().to_owned();
    let sms_token = data.get("smsToken").and_then(Value::as_str).unwrap_or_default().to_owned();
    let verification_url = login_verification_url(&body);
    let (status, message) = if ret.iter().any(|item| item.as_str().unwrap_or_default().contains("FAIL_SYS_USER_VALIDATE")) {
        ("verification_required", "平台要求滑块验证；密码登录会话已暂停")
    } else if data.get("iframeRedirect").and_then(Value::as_bool) == Some(true) {
        let target = data.get("iframeRedirectUrl").and_then(Value::as_str).unwrap_or_default();
        // The seller_web login page routes a password login that needs phone
        // verification directly to identity_verify.htm?tag=8. It does not
        // include smsToken in login.do, so checking only smsToken would open
        // the slider dialog for a normal SMS challenge.
        if is_sms_identity_redirect(target) {
            // seller_web does not expose this as the legacy smsToken flow.
            // Safari opens identity_verify.htm?htoken=...&tag=8 and keeps
            // the password session in that same browser context while the
            // user requests and submits the SMS code there. Preserve the
            // complete redirect so the caller can embed that official page.
            ("verification_required", "平台要求在官方身份验证页完成短信核验")
        } else if target.contains("mini_login_check") {
            ("verification_required", "平台要求扫码人脸核验；密码登录会话已暂停")
        } else if !sms_token.is_empty() || target.to_ascii_lowercase().contains("sms") {
            ("sms_required", "平台要求短信核验，请在应用内输入验证码")
        } else {
            ("verification_required", "平台要求进一步身份核验；密码登录会话已暂停")
        }
    } else if data.get("loginResult").and_then(Value::as_str) == Some("success")
        || data.get("st").and_then(Value::as_str) == Some("success") {
        ("success", "客服账号登录成功")
    } else if let Some(title) = data.get("titleMsg").and_then(Value::as_str) {
        if title.contains("短信") || title.contains("手机验证码") { ("sms_required", title) }
        else { ("failed", title) }
    } else {
        ("failed", "登录接口未返回可识别的结果，请检查账号或稍后重试")
    };
    if status == "success" {
        let _ = client.get("https://www.goofish.com/").send().await;
    }
    Ok(PasswordLoginResult { status: status.to_owned(), message: message.to_owned(), verification_url, cookie: collect_cookies(&jar), context_token, sms_token })
}

fn passport_client(cookie: &str) -> Result<(reqwest::Client, Arc<Jar>), String> {
    let jar = Arc::new(Jar::default());
    let passport = reqwest::Url::parse("https://passport.goofish.com/").map_err(|error| error.to_string())?;
    for entry in cookie.split(';') {
        if let Some((name, value)) = entry.trim().split_once('=') {
            if !name.is_empty() && !value.is_empty() {
                jar.add_cookie_str(&format!("{name}={value}; Domain=.goofish.com; Path=/"), &passport);
            }
        }
    }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30))
        .cookie_provider(jar.clone()).redirect(reqwest::redirect::Policy::none())
        .build().map_err(|error| error.to_string())?;
    Ok((client, jar))
}

fn sms_form(mobile: &str, context_token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("keepLogin", "false".into()), ("isIframe", "true".into()),
        ("documentReferer", LOGIN_DOCUMENT_REFERER.into()),
        ("defaultView", "smslogin".into()), ("appName", "xianyu".into()),
        ("appEntrance", LOGIN_APP_ENTRANCE.into()), ("fromSite", "77".into()),
        ("bizParams", LOGIN_BIZ_PARAMS.into()),
        ("mainPage", "false".into()), ("isMobile", "false".into()),
        ("lang", "zh_CN".into()), ("jsVersion", "0.10.36".into()),
        ("returnUrl", String::new()), ("weiBoMpBridge", String::new()),
        ("screenPixel", "1920x1080".into()),
        ("navlanguage", "zh-CN".into()), ("navUserAgent", LOGIN_UA.into()),
        ("navPlatform", LOGIN_PLATFORM.into()), ("loginId", mobile.into()),
        ("umidGetStatusVal", "255".into()), ("umidTag", "SERVER".into()),
        ("phoneCode", "86".into()), ("countryCode", "CN".into()),
        ("contextToken", context_token.into()),
    ]
}

async fn sms_post(client: &reqwest::Client, path: &str, form: &[(&str, String)]) -> Result<Value, String> {
    browser_headers(client.post(format!("https://passport.goofish.com{path}?appName=xianyu&fromSite=77")))
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(USER_AGENT, LOGIN_UA)
        .header(ORIGIN, "https://passport.goofish.com")
        .header(REFERER, "https://passport.goofish.com/")
        .header("accept-language", "zh-CN,zh;q=0.9,en;q=0.8")
        .form(form).send().await.map_err(|error| error.to_string())?
        .json().await.map_err(|_| "短信接口未返回 JSON".to_owned())
}

pub(crate) async fn send_sms(cookie: &str, mobile: &str, context_token: &str) -> Result<(String, String), String> {
    let (client, jar) = passport_client(cookie)?;
    let mut form = sms_form(mobile, context_token);
    form.push(("codeLength", "6".into()));
    let body = sms_post(&client, "/newlogin/sms/send.do", &form).await?;
    let data = body.pointer("/content/data").unwrap_or(&Value::Null);
    if data.get("isCheckCodeShowed").and_then(Value::as_bool) == Some(true) {
        return Err("平台要求额外图形验证，短信暂未发送".to_owned());
    }
    let token = data.get("smsToken").and_then(Value::as_str).filter(|value| !value.is_empty())
        .ok_or_else(|| data.get("titleMsg").and_then(Value::as_str)
            .unwrap_or("短信发送未成功，平台未返回 smsToken").to_owned())?;
    Ok((token.to_owned(), collect_cookies(&jar)))
}

pub(crate) struct SmsLoginResult {
    pub cookie: String,
    pub reported_login_id: String,
}

pub(crate) async fn submit_sms(
    cookie: &str, mobile: &str, context_token: &str, sms_token: &str, code: &str,
) -> Result<SmsLoginResult, String> {
    let (client, jar) = passport_client(cookie)?;
    let mut form = sms_form(mobile, context_token);
    form.push(("smsToken", sms_token.into()));
    form.push(("smsCode", code.into()));
    let body = sms_post(&client, "/newlogin/sms/login.do", &form).await?;
    let data = body.pointer("/content/data").unwrap_or(&Value::Null);
    if data.get("smsRegToken").is_some() {
        return Err("手机号登录指向尚未注册的账号，已停止以免误创建账号".to_owned());
    }
    if data.get("loginResult").and_then(Value::as_str) != Some("success")
        && data.get("st").and_then(Value::as_str) != Some("success") {
        return Err(data.get("titleMsg").and_then(Value::as_str).unwrap_or("短信验证未通过").to_owned());
    }
    let _ = client.get("https://www.goofish.com/").send().await;
    let reported_login_id = data.get("loginId").and_then(Value::as_str).unwrap_or_default().to_owned();
    Ok(SmsLoginResult { cookie: collect_cookies(&jar), reported_login_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1::EncodeRsaPublicKey;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::traits::PublicKeyParts;

    #[test]
    fn login_password_uses_reference_rsa_key() {
        let encrypted = encrypt("abc12345", &login_key().unwrap()).unwrap();
        assert_eq!(encrypted.len(), 128);
        assert_eq!(hex(&encrypted).len(), 256);
    }

    #[test]
    fn public_key_parser_accepts_wire_formats() {
        let key = login_key().unwrap();
        let pem = key.to_public_key_pem(LineEnding::LF).unwrap();
        assert!(parse_public_key(&pem).is_some());

        let pkcs1 = key.to_pkcs1_der().unwrap();
        assert!(parse_public_key(&STANDARD.encode(pkcs1.as_bytes())).is_some());

        let jwk = json!({
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(key.e().to_bytes_be()),
        });
        assert!(parse_public_key(&jwk.to_string()).is_some());
    }

    #[test]
    fn seller_business_error_is_not_registration_success() {
        let body = json!({"ret":["SUCCESS::调用成功"],"data":{"code":"failed","msg":"无创建权限"}});
        let result = business_data(&body);
        assert_eq!(result.unwrap_err(), "无创建权限");
    }

    #[test]
    fn password_login_keeps_platform_slider_url() {
        let slider = "https://passport.goofish.com/newlogin/login.do/_____tmd_____/punish?x5step=2";
        assert_eq!(login_verification_url(&json!({"ret": ["FAIL_SYS_USER_VALIDATE::需要验证"], "data": {"url": slider}})), slider);
        assert_eq!(login_verification_url(&json!({"content": {"data": {"url": slider}}})), slider);
        assert_eq!(login_verification_url(&json!({"content": {"data": {"iframeRedirectUrl": slider}}})), slider);
    }

    #[test]
    fn seller_identity_verify_tag_is_embedded_verification_branch() {
        let target = "https://passport.goofish.com/iv/mini/identity_verify.htm?htoken=abc&tag=8";
        assert!(is_sms_identity_redirect(target));
        assert!(!is_sms_identity_redirect(
            "https://passport.goofish.com/iv/mini/identity_verify.htm?htoken=abc&tag=3"
        ));
    }
}
