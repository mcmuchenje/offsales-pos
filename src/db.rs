//! SQLite data layer. All money is stored as integer cents.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::auth;

mod sync;

/// Ordered migrations. `PRAGMA user_version` records how many have been applied,
/// so adding a new file to the end of this list upgrades existing databases.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_schema.sql"),
    include_str!("../migrations/seed.sql"),
    include_str!("../migrations/003_users.sql"),
    include_str!("../migrations/004_network.sql"),
    include_str!("../migrations/005_till_products.sql"),
];

#[derive(Clone, Debug)]
pub struct Product {
    pub id: i64,
    pub name: String,
    pub price_cents: i64,
    pub stock_qty: i64,
    pub active: bool,
    /// Tills (device ids) that sell this product; empty = every till.
    pub tills: Vec<i64>,
}

#[derive(Clone, Debug)]
pub struct CartLine {
    pub product_id: i64,
    pub name: String,
    pub qty: i64,
    pub unit_price_cents: i64,
}

#[derive(Clone, Debug)]
pub struct SaleRow {
    pub id: i64,
    pub created_at: String,
    pub total_cents: i64,
    pub items: String,
    pub voided: bool,
    /// Who rang it up; None for sales imported from the spreadsheet.
    pub cashier: Option<String>,
    pub voided_by: Option<String>,
    /// On a till: the sale (or its void) hasn't reached the server yet.
    pub unsent: bool,
}

#[derive(Clone, Debug)]
pub struct ProductSummary {
    pub name: String,
    pub qty: i64,
    pub total_cents: i64,
}

#[derive(Clone, Debug)]
pub struct DaySummary {
    pub day: String,
    pub sales: i64,
    pub items: i64,
    pub total_cents: i64,
}

#[derive(Clone, Debug)]
pub struct CashierSummary {
    /// "Imported" for sales that predate till users.
    pub name: String,
    pub sales: i64,
    pub total_cents: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Manager,
    Cashier,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Manager => "manager",
            Role::Cashier => "cashier",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Role::Manager => "Manager",
            Role::Cashier => "Cashier",
        }
    }
    fn parse(s: &str) -> Role {
        if s == "manager" {
            Role::Manager
        } else {
            Role::Cashier
        }
    }
}

#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub role: Role,
    pub active: bool,
}

impl User {
    pub fn is_manager(&self) -> bool {
        self.role == Role::Manager
    }
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;",
        )?;
        // The screens, the till's sync thread and the server each hold a connection.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut db = Db { conn, path: path.to_path_buf() };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn migrate(&mut self) -> Result<()> {
        let current: usize =
            self.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        if current > MIGRATIONS.len() {
            bail!(
                "Database schema v{current} is newer than this app supports (v{}). Please update the app.",
                MIGRATIONS.len()
            );
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    // ---------- settings ----------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---------- users ----------

    pub fn users(&self, include_inactive: bool) -> Result<Vec<User>> {
        let sql = if include_inactive {
            "SELECT id, name, role, active FROM users ORDER BY active DESC, role = 'cashier', name COLLATE NOCASE"
        } else {
            "SELECT id, name, role, active FROM users WHERE active = 1 ORDER BY role = 'cashier', name COLLATE NOCASE"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(User {
                id: r.get(0)?,
                name: r.get(1)?,
                role: Role::parse(&r.get::<_, String>(2)?),
                active: r.get::<_, i64>(3)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn has_users(&self) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE active = 1)", [], |r| r.get(0))?)
    }

    pub fn create_user(&self, name: &str, role: Role, pin: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Enter the person's name.");
        }
        auth::validate_pin(pin)?;
        let hash = auth::hash_pin(pin)?;
        self.conn
            .execute(
                "INSERT INTO users (name, role, pin_hash) VALUES (?1, ?2, ?3)",
                params![name, role.as_str(), hash],
            )
            .map_err(|_| anyhow!("There's already someone called {name}. Use a different name."))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Changes name, role and active flag. Refuses to leave the till without an active manager.
    pub fn update_user(&mut self, id: i64, name: &str, role: Role, active: bool) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Enter the person's name.");
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE users SET name = ?2, role = ?3, active = ?4 WHERE id = ?1",
            params![id, name, role.as_str(), active as i64],
        )
        .map_err(|_| anyhow!("There's already someone called {name}. Use a different name."))?;
        let managers: i64 =
            tx.query_row("SELECT COUNT(*) FROM users WHERE role = 'manager' AND active = 1", [], |r| r.get(0))?;
        if managers == 0 {
            bail!("The till needs at least one active manager. Make someone else a manager first.");
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_pin(&self, id: i64, pin: &str) -> Result<()> {
        auth::validate_pin(pin)?;
        let hash = auth::hash_pin(pin)?;
        self.conn.execute("UPDATE users SET pin_hash = ?2 WHERE id = ?1", params![id, hash])?;
        Ok(())
    }

    /// Checks one user's PIN. Inactive users can't sign in.
    pub fn verify_pin(&self, id: i64, pin: &str) -> Result<Option<User>> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row("SELECT name, role, pin_hash FROM users WHERE id = ?1 AND active = 1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        Ok(row.and_then(|(name, role, hash)| {
            auth::verify_pin(pin, &hash).then(|| User { id, name, role: Role::parse(&role), active: true })
        }))
    }

    /// Manager approval: finds the active manager whose PIN this is.
    pub fn manager_with_pin(&self, pin: &str) -> Result<Option<User>> {
        for u in self.users(false)?.into_iter().filter(User::is_manager) {
            if let Some(m) = self.verify_pin(u.id, pin)? {
                return Ok(Some(m));
            }
        }
        Ok(None)
    }

    // ---------- products ----------

    pub fn products(&self, include_inactive: bool) -> Result<Vec<Product>> {
        let sql = if include_inactive {
            "SELECT id, name, price_cents, stock_qty, active FROM products ORDER BY name"
        } else {
            "SELECT id, name, price_cents, stock_qty, active FROM products WHERE active = 1 ORDER BY name"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(Product {
                id: r.get(0)?,
                name: r.get(1)?,
                price_cents: r.get(2)?,
                stock_qty: r.get(3)?,
                active: r.get::<_, i64>(4)? != 0,
                tills: vec![],
            })
        })?;
        let mut products: Vec<Product> = rows.collect::<Result<_, _>>()?;
        let mut stmt = self.conn.prepare("SELECT product_id, device_id FROM product_tills ORDER BY device_id")?;
        let links = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for link in links {
            let (pid, till) = link?;
            if let Some(p) = products.iter_mut().find(|p| p.id == pid) {
                p.tills.push(till);
            }
        }
        Ok(products)
    }

    /// What the Sell screen shows: active products, and on a till only those
    /// sold at this till.
    pub fn products_for_sale(&self) -> Result<Vec<Product>> {
        let mut products = self.products(false)?;
        if let Some(me) = self.this_till() {
            products.retain(|p| p.tills.is_empty() || p.tills.contains(&me));
        }
        Ok(products)
    }

    /// Sets which tills sell a product; empty = every till.
    pub fn set_product_tills(&mut self, product_id: i64, tills: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM product_tills WHERE product_id = ?1", [product_id])?;
        for t in tills {
            tx.execute("INSERT OR IGNORE INTO product_tills (product_id, device_id) VALUES (?1, ?2)", params![product_id, t])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// (id, name) of the tills products can be assigned to: on the server its
    /// paired tills, on a till the server's list. Empty when standalone.
    pub fn tills(&self) -> Result<Vec<(i64, String)>> {
        let sql = match self.mode().as_str() {
            "server" => "SELECT id, name FROM devices WHERE revoked = 0 ORDER BY name COLLATE NOCASE",
            "till" => "SELECT id, name FROM known_tills ORDER BY name COLLATE NOCASE",
            _ => return Ok(vec![]),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// On a till: its id on the server. None anywhere else (sells everything).
    pub fn this_till(&self) -> Option<i64> {
        if self.mode() != "till" {
            return None;
        }
        self.setting("this_till").ok().flatten().and_then(|v| v.parse().ok())
    }

    pub fn add_product(&mut self, name: &str, price_cents: i64, opening_stock: i64, user_id: i64) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Product name is required");
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO products (name, price_cents, stock_qty) VALUES (?1, ?2, ?3)",
            params![name, price_cents, opening_stock],
        )
        .context("Could not add product (is the name already used?)")?;
        let id = tx.last_insert_rowid();
        if opening_stock != 0 {
            tx.execute(
                "INSERT INTO stock_movements (product_id, change_qty, reason, user_id) VALUES (?1, ?2, 'opening', ?3)",
                params![id, opening_stock, user_id],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn update_product(&self, id: i64, name: &str, price_cents: i64, active: bool) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Product name is required");
        }
        self.conn
            .execute(
                "UPDATE products SET name = ?2, price_cents = ?3, active = ?4 WHERE id = ?1",
                params![id, name, price_cents, active as i64],
            )
            .context("Could not save product (is the name already used?)")?;
        Ok(())
    }

    /// Positive qty = restock, negative = write-off / correction.
    pub fn adjust_stock(&mut self, id: i64, qty: i64, reason: &str, note: &str, user_id: i64) -> Result<()> {
        if qty == 0 {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE products SET stock_qty = stock_qty + ?2 WHERE id = ?1", params![id, qty])?;
        tx.execute(
            "INSERT INTO stock_movements (product_id, change_qty, reason, note, user_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, qty, reason, note, user_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---------- sales ----------

    /// Records a sale on a standalone till or the server (accepted immediately).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn record_sale(&mut self, cart: &[CartLine], tendered_cents: i64, method: &str, user_id: i64) -> Result<i64> {
        self.record_sale_as(cart, tendered_cents, method, user_id, false)
    }

    /// Voids a sale and returns its items to stock. `by_user` is the manager who approved it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn void_sale(&mut self, sale_id: i64, by_user: i64) -> Result<()> {
        self.void_sale_as(sale_id, by_user, false)
    }

    /// Sales between two dates inclusive (YYYY-MM-DD), optionally only one cashier's.
    pub fn sales_between(&self, from: &str, to: &str, only_user: Option<i64>) -> Result<Vec<SaleRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.created_at, s.total_cents, s.voided,
                    group_concat(si.qty || ' x ' || p.name, ', '),
                    u.name, v.name, (s.pending OR s.void_pending)
             FROM sales s
             JOIN sale_items si ON si.sale_id = s.id
             JOIN products p ON p.id = si.product_id
             LEFT JOIN users u ON u.id = s.user_id
             LEFT JOIN users v ON v.id = s.voided_by
             WHERE date(s.created_at) BETWEEN ?1 AND ?2
               AND (?3 IS NULL OR s.user_id = ?3)
             GROUP BY s.id
             ORDER BY s.created_at DESC, s.id DESC",
        )?;
        let rows = stmt.query_map(params![from, to, only_user], |r| {
            Ok(SaleRow {
                id: r.get(0)?,
                created_at: r.get(1)?,
                total_cents: r.get(2)?,
                voided: r.get::<_, i64>(3)? != 0,
                items: r.get(4)?,
                cashier: r.get(5)?,
                voided_by: r.get(6)?,
                unsent: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn product_summary(&self, from: &str, to: &str) -> Result<Vec<ProductSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.name, SUM(si.qty), SUM(si.line_total_cents)
             FROM sale_items si
             JOIN sales s ON s.id = si.sale_id AND s.voided = 0
             JOIN products p ON p.id = si.product_id
             WHERE date(s.created_at) BETWEEN ?1 AND ?2
             GROUP BY p.id ORDER BY SUM(si.line_total_cents) DESC, p.name",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(ProductSummary { name: r.get(0)?, qty: r.get(1)?, total_cents: r.get(2)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn daily_summary(&self, from: &str, to: &str) -> Result<Vec<DaySummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT date(s.created_at) AS d, COUNT(DISTINCT s.id), SUM(si.qty), SUM(si.line_total_cents)
             FROM sales s JOIN sale_items si ON si.sale_id = s.id
             WHERE s.voided = 0 AND date(s.created_at) BETWEEN ?1 AND ?2
             GROUP BY d ORDER BY d DESC",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(DaySummary { day: r.get(0)?, sales: r.get(1)?, items: r.get(2)?, total_cents: r.get(3)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn cashier_summary(&self, from: &str, to: &str) -> Result<Vec<CashierSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(u.name, 'Imported'), COUNT(*), SUM(s.total_cents)
             FROM sales s LEFT JOIN users u ON u.id = s.user_id
             WHERE s.voided = 0 AND date(s.created_at) BETWEEN ?1 AND ?2
             GROUP BY s.user_id ORDER BY SUM(s.total_cents) DESC",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok(CashierSummary { name: r.get(0)?, sales: r.get(1)?, total_cents: r.get(2)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Writes a consistent copy of the database to `dest`.
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        if dest.exists() {
            std::fs::remove_file(dest)?;
        }
        self.conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
        Ok(())
    }
}

pub fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let c = cents.abs();
    format!("{sign}${}.{:02}", c / 100, c % 100)
}

/// Parses "1.5", "$2", "10.25" into cents.
pub fn parse_money(s: &str) -> Option<i64> {
    let s = s.trim().trim_start_matches('$').trim();
    if s.is_empty() {
        return Some(0);
    }
    let v: f64 = s.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v * 100.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Db {
        let dir = std::env::temp_dir().join(format!("pos-test-{}-{}", std::process::id(), rand_suffix()));
        Db::open(&dir.join("t.db")).unwrap()
    }
    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn seed_matches_spreadsheet_closing_stock() {
        let db = mem();
        let stock: std::collections::HashMap<_, _> =
            db.products(true).unwrap().into_iter().map(|p| (p.name, p.stock_qty)).collect();
        assert_eq!(stock["Two Keys 200ml"], 15);
        assert_eq!(stock["Viceroy 200ml"], 11);
        assert_eq!(stock["Viceroy 750ml"], 1);
        assert_eq!(stock["Two Keys 750ml"], 2);
        assert_eq!(stock["Castle 750ml"], 24);
        assert_eq!(stock["Black Label 750ml"], 294);
        assert_eq!(stock["Dragon"], 26);
        assert_eq!(stock["Hunters 330ml"], 221);
        assert_eq!(stock["Corona"], 47);
    }

    #[test]
    fn sale_and_void_roundtrip_records_users() {
        let mut db = mem();
        let boss = db.create_user("Tendai", Role::Manager, "4821").unwrap();
        let till = db.create_user("Rudo", Role::Cashier, "1357").unwrap();
        let p = db.products(false).unwrap().into_iter().find(|p| p.name == "Castle 750ml").unwrap();
        let cart = vec![CartLine { product_id: p.id, name: p.name.clone(), qty: 3, unit_price_cents: 200 }];
        assert!(db.record_sale(&cart, 500, "cash", till).is_err());
        let id = db.record_sale(&cart, 1000, "cash", till).unwrap();
        let after = db.products(false).unwrap().into_iter().find(|x| x.id == p.id).unwrap();
        assert_eq!(after.stock_qty, p.stock_qty - 3);

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mine = db.sales_between(&today, &today, Some(till)).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].cashier.as_deref(), Some("Rudo"));
        assert!(db.sales_between(&today, &today, Some(boss)).unwrap().is_empty());

        db.void_sale(id, boss).unwrap();
        let restored = db.products(false).unwrap().into_iter().find(|x| x.id == p.id).unwrap();
        assert_eq!(restored.stock_qty, p.stock_qty);
        assert_eq!(db.sales_between(&today, &today, None).unwrap()[0].voided_by.as_deref(), Some("Tendai"));
        assert!(db.void_sale(id, boss).is_err());
    }

    #[test]
    fn pins_and_roles() {
        let mut db = mem();
        assert!(!db.has_users().unwrap());
        assert!(db.create_user("Tendai", Role::Manager, "12a4").is_err());
        assert!(db.create_user("Tendai", Role::Manager, "123").is_err());
        let boss = db.create_user("Tendai", Role::Manager, "4821").unwrap();
        let till = db.create_user("Rudo", Role::Cashier, "1357").unwrap();
        assert!(db.create_user("tendai", Role::Cashier, "9999").is_err(), "names are unique, case-insensitive");
        assert!(db.has_users().unwrap());

        assert!(db.verify_pin(till, "1357").unwrap().is_some());
        assert!(db.verify_pin(till, "1358").unwrap().is_none());
        assert_eq!(db.manager_with_pin("4821").unwrap().map(|u| u.id), Some(boss));
        assert!(db.manager_with_pin("1357").unwrap().is_none(), "a cashier PIN is not manager approval");

        // Can't remove the last manager.
        assert!(db.update_user(boss, "Tendai", Role::Cashier, true).is_err());
        assert!(db.update_user(boss, "Tendai", Role::Manager, false).is_err());
        // Deactivated users can't sign in.
        db.update_user(till, "Rudo", Role::Cashier, false).unwrap();
        assert!(db.verify_pin(till, "1357").unwrap().is_none());

        db.set_pin(boss, "250913").unwrap();
        assert!(db.verify_pin(boss, "4821").unwrap().is_none());
        assert!(db.verify_pin(boss, "250913").unwrap().is_some());
    }

    #[test]
    fn money_parsing() {
        assert_eq!(parse_money("1.5"), Some(150));
        assert_eq!(parse_money("$10"), Some(1000));
        assert_eq!(parse_money("abc"), None);
        assert_eq!(money(150), "$1.50");
    }
}
