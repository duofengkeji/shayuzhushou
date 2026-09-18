use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{fs, sync::Mutex};
use tauri::Manager;
use uuid::Uuid;

struct AppState {
    db: Mutex<Connection>,
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Product {
    id: String,
    account_id: String,
    title: String,
    price: f64,
    stock: i64,
    status: String,
    updated_at: String,
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Order {
    id: String,
    account_id: String,
    order_no: String,
    product_title: String,
    buyer_masked_name: String,
    amount: f64,
    status: String,
    created_at: String,
    note: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardStats {
    total_accounts: i64,
    healthy_accounts: i64,
    active_products: i64,
    pending_orders: i64,
}

fn to_error(error: impl std::fmt::Display) -> String {
    error.to_string()
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
        "SELECT id, display_name, alias, platform, status, last_sync_at FROM accounts WHERE id = ?1",
        [account_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            ))
        },
    )?;
    Ok(Account {
        id: account.0,
        display_name: account.1,
        alias: account.2,
        platform: account.3,
        status: account.4,
        last_sync_at: account.5,
        product_count: count_for(conn, "products", account_id)?,
        order_count: count_for(conn, "orders", account_id)?,
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
          price REAL NOT NULL, stock INTEGER NOT NULL, status TEXT NOT NULL,
          updated_at TEXT NOT NULL, tags TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS orders (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, order_no TEXT NOT NULL,
          product_title TEXT NOT NULL, buyer_masked_name TEXT NOT NULL,
          amount REAL NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, note TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_jobs (
          id TEXT PRIMARY KEY, account_id TEXT NOT NULL, resource TEXT NOT NULL,
          status TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT, error_message TEXT
        );
        ",
    )?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?;
    if count == 0 {
        let first = create_demo_account(conn, "鲨鱼精选店", "主运营账号", "授权有效")?;
        let _second = create_demo_account(conn, "海风数码店", "华东账号", "即将过期")?;
        seed_demo_records(conn, &first.id)?;
    }
    Ok(())
}

fn create_demo_account(conn: &Connection, display_name: &str, alias: &str, status: &str) -> rusqlite::Result<Account> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO accounts (id, display_name, alias, platform, status, last_sync_at) VALUES (?1, ?2, ?3, '闲鱼（演示）', ?4, ?5)",
        params![id, display_name, alias, status, now],
    )?;
    get_account(conn, &id)
}

fn seed_demo_records(conn: &Connection, account_id: &str) -> rusqlite::Result<()> {
    let now = Utc::now().to_rfc3339();
    let product_data = [
        ("便携式蓝牙耳机｜降噪长续航", 89.0, 36, "已上架", "数码,热销"),
        ("全新机械键盘｜青轴 87 键", 128.0, 12, "已上架", "数码,现货"),
        ("桌面氛围灯｜暖白三档可调", 49.0, 0, "已下架", "家居,缺货"),
    ];
    for (index, (title, price, stock, status, tags)) in product_data.iter().enumerate() {
        let product_id = format!("DEMO-P-{:03}", index + 1);
        conn.execute(
            "INSERT OR IGNORE INTO products (id, account_id, title, price, stock, status, updated_at, tags) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![product_id, account_id, title, price, stock, status, now, tags],
        )?;
    }
    let order_data = [
        ("XY202609180001", "便携式蓝牙耳机｜降噪长续航", "林**", 89.0, "待发货", "请尽快发货"),
        ("XY202609180002", "全新机械键盘｜青轴 87 键", "陈**", 128.0, "待付款", ""),
        ("XY202609170031", "便携式蓝牙耳机｜降噪长续航", "周**", 89.0, "已完成", "已确认收货"),
    ];
    for (index, (order_no, product_title, buyer, amount, status, note)) in order_data.iter().enumerate() {
        conn.execute(
            "INSERT OR IGNORE INTO orders (id, account_id, order_no, product_title, buyer_masked_name, amount, status, created_at, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![format!("DEMO-O-{:03}", index + 1), account_id, order_no, product_title, buyer, amount, status, now, note],
        )?;
    }
    Ok(())
}

#[tauri::command]
fn list_accounts(state: tauri::State<'_, AppState>) -> Result<Vec<Account>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let mut stmt = conn.prepare("SELECT id FROM accounts ORDER BY last_sync_at DESC").map_err(to_error)?;
    let ids = stmt.query_map([], |row| row.get::<_, String>(0)).map_err(to_error)?;
    ids.map(|id| get_account(&conn, &id?)).collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
fn list_products(account_id: Option<String>, state: tauri::State<'_, AppState>) -> Result<Vec<Product>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let query = "SELECT id, account_id, title, price, stock, status, updated_at, tags FROM products WHERE (?1 IS NULL OR account_id = ?1) ORDER BY updated_at DESC";
    let mut statement = conn.prepare(query).map_err(to_error)?;
    let rows = statement.query_map([account_id], |row| {
        let tag_string: String = row.get(7)?;
        Ok(Product { id: row.get(0)?, account_id: row.get(1)?, title: row.get(2)?, price: row.get(3)?, stock: row.get(4)?, status: row.get(5)?, updated_at: row.get(6)?, tags: tag_string.split(',').map(str::to_owned).collect() })
    }).map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
fn list_orders(account_id: Option<String>, state: tauri::State<'_, AppState>) -> Result<Vec<Order>, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let query = "SELECT id, account_id, order_no, product_title, buyer_masked_name, amount, status, created_at, note FROM orders WHERE (?1 IS NULL OR account_id = ?1) ORDER BY created_at DESC";
    let mut statement = conn.prepare(query).map_err(to_error)?;
    let rows = statement.query_map([account_id], |row| {
        Ok(Order { id: row.get(0)?, account_id: row.get(1)?, order_no: row.get(2)?, product_title: row.get(3)?, buyer_masked_name: row.get(4)?, amount: row.get(5)?, status: row.get(6)?, created_at: row.get(7)?, note: row.get(8)? })
    }).map_err(to_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(to_error)
}

#[tauri::command]
fn dashboard_stats(state: tauri::State<'_, AppState>) -> Result<DashboardStats, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).map_err(to_error);
    Ok(DashboardStats {
        total_accounts: count("SELECT COUNT(*) FROM accounts")?,
        healthy_accounts: count("SELECT COUNT(*) FROM accounts WHERE status = '授权有效'")?,
        active_products: count("SELECT COUNT(*) FROM products WHERE status = '已上架'")?,
        pending_orders: count("SELECT COUNT(*) FROM orders WHERE status IN ('待付款', '待发货')")?,
    })
}

#[tauri::command]
fn add_demo_account(display_name: String, state: tauri::State<'_, AppState>) -> Result<Account, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let account = create_demo_account(&conn, &display_name, "本地演示账号", "授权有效").map_err(to_error)?;
    seed_demo_records(&conn, &account.id).map_err(to_error)?;
    get_account(&conn, &account.id).map_err(to_error)
}

#[tauri::command]
fn sync_account(account_id: String, state: tauri::State<'_, AppState>) -> Result<Account, String> {
    let conn = state.db.lock().map_err(to_error)?;
    let now = Utc::now().to_rfc3339();
    conn.execute("UPDATE accounts SET last_sync_at = ?1 WHERE id = ?2", params![now, account_id]).map_err(to_error)?;
    conn.execute(
        "INSERT INTO sync_jobs (id, account_id, resource, status, started_at, finished_at, error_message) VALUES (?1, ?2, 'local-demo', '已完成', ?3, ?3, '')",
        params![Uuid::new_v4().to_string(), account_id, now],
    ).map_err(to_error)?;
    get_account(&conn, &account_id).map_err(to_error)
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&app_dir)?;
            let conn = Connection::open(app_dir.join("shark-butler.sqlite3"))?;
            initialize_database(&conn)?;
            app.manage(AppState { db: Mutex::new(conn) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_accounts, list_products, list_orders, dashboard_stats, add_demo_account, sync_account
        ])
        .run(tauri::generate_context!())
        .expect("启动鲨鱼管家失败");
}
