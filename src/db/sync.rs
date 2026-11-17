//! Database side of server/till sync.
//!
//! Server: accepts sales and voids from tills (idempotent by sale uuid), bumps a
//! revision counter on every change, and exports snapshots.
//! Till: records sales as `pending` (negative ids), queues voids, and applies
//! snapshots from the server, keeping its own unsent work on top.

use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, Transaction};

use super::{CartLine, Db};
use crate::net::protocol::{
    ProductChange, ProductRecord, SaleLine, SaleRecord, Snapshot, SyncedSale, UserRecord, VoidRecord, API_VERSION, SHARED_SETTINGS,
};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn next_rev(tx: &Transaction) -> rusqlite::Result<i64> {
    tx.query_row("SELECT COALESCE(MAX(rev), 0) + 1 FROM sales", [], |r| r.get(0))
}

/// Inserts sale header, lines, stock decrement and ledger. Returns the sale id.
fn insert_sale(tx: &Transaction, s: &SaleRecord, id: Option<i64>, rev: i64, pending: bool) -> Result<i64> {
    tx.execute(
        "INSERT INTO sales (id, created_at, total_cents, tendered_cents, change_cents, payment_method, note,
                            user_id, uuid, till, rev, pending)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            s.created_at,
            s.total_cents,
            s.tendered_cents,
            s.change_cents,
            s.payment_method,
            s.note,
            s.user_id,
            s.uuid,
            s.till,
            rev,
            pending as i64
        ],
    )?;
    let sale_id = tx.last_insert_rowid();
    for l in &s.lines {
        tx.execute(
            "INSERT INTO sale_items (sale_id, product_id, qty, unit_price_cents, line_total_cents)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![sale_id, l.product_id, l.qty, l.unit_price_cents, l.qty * l.unit_price_cents],
        )?;
        tx.execute("UPDATE products SET stock_qty = stock_qty - ?2 WHERE id = ?1", params![l.product_id, l.qty])?;
        tx.execute(
            "INSERT INTO stock_movements (product_id, change_qty, reason, ref_id, user_id, created_at)
             VALUES (?1, ?2, 'sale', ?3, ?4, ?5)",
            params![l.product_id, -l.qty, sale_id, s.user_id, s.created_at],
        )?;
    }
    Ok(sale_id)
}

/// Marks a sale voided and returns its stock.
fn apply_void(tx: &Transaction, sale_id: i64, by_user: i64, at: &str, rev: Option<i64>, void_pending: bool) -> Result<bool> {
    let changed = tx.execute(
        "UPDATE sales SET voided = 1, voided_by = ?2, voided_at = ?3, void_pending = ?4, rev = COALESCE(?5, rev)
         WHERE id = ?1 AND voided = 0",
        params![sale_id, by_user, at, void_pending as i64, rev],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    let items: Vec<(i64, i64)> = {
        let mut stmt = tx.prepare("SELECT product_id, qty FROM sale_items WHERE sale_id = ?1")?;
        let rows = stmt.query_map([sale_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    for (pid, qty) in items {
        tx.execute("UPDATE products SET stock_qty = stock_qty + ?2 WHERE id = ?1", params![pid, qty])?;
        tx.execute(
            "INSERT INTO stock_movements (product_id, change_qty, reason, ref_id, user_id) VALUES (?1, ?2, 'void', ?3, ?4)",
            params![pid, qty, sale_id, by_user],
        )?;
    }
    Ok(true)
}

fn sale_lines(tx: &rusqlite::Connection, sale_id: i64) -> Result<Vec<SaleLine>> {
    let mut stmt = tx.prepare("SELECT product_id, qty, unit_price_cents FROM sale_items WHERE sale_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([sale_id], |r| Ok(SaleLine { product_id: r.get(0)?, qty: r.get(1)?, unit_price_cents: r.get(2)? }))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

const SALE_COLS: &str = "id, uuid, created_at, user_id, till, total_cents, tendered_cents, change_cents, payment_method,
                         voided, voided_by, voided_at, note, rev";

fn read_sale(conn: &rusqlite::Connection, r: &rusqlite::Row) -> Result<(i64, i64, SaleRecord)> {
    let id: i64 = r.get(0)?;
    let rec = SaleRecord {
        uuid: r.get(1)?,
        created_at: r.get(2)?,
        user_id: r.get(3)?,
        till: r.get(4)?,
        total_cents: r.get(5)?,
        tendered_cents: r.get(6)?,
        change_cents: r.get(7)?,
        payment_method: r.get(8)?,
        voided: r.get::<_, i64>(9)? != 0,
        voided_by: r.get(10)?,
        voided_at: r.get(11)?,
        note: r.get(12)?,
        lines: sale_lines(conn, id)?,
    };
    Ok((id, r.get(13)?, rec))
}

impl Db {
    // ------------------------------------------------------------ all modes

    /// Records a sale. `pending` = rung up on a till and not yet on the server:
    /// it gets a negative id until the server's copy arrives.
    pub fn record_sale_as(
        &mut self,
        cart: &[CartLine],
        tendered_cents: i64,
        method: &str,
        user_id: i64,
        pending: bool,
    ) -> Result<i64> {
        if cart.is_empty() {
            bail!("Cart is empty");
        }
        let total: i64 = cart.iter().map(|l| l.qty * l.unit_price_cents).sum();
        if method == "cash" && tendered_cents < total {
            bail!("Amount tendered is less than the total");
        }
        let rec = SaleRecord {
            uuid: uuid::Uuid::new_v4().to_string(),
            created_at: now(),
            user_id: Some(user_id),
            till: None,
            total_cents: total,
            tendered_cents,
            change_cents: (tendered_cents - total).max(0),
            payment_method: method.to_string(),
            lines: cart
                .iter()
                .map(|l| SaleLine { product_id: l.product_id, qty: l.qty, unit_price_cents: l.unit_price_cents })
                .collect(),
            voided: false,
            voided_by: None,
            voided_at: None,
            note: None,
        };
        let tx = self.conn.transaction()?;
        let (id, rev) = if pending {
            let id: i64 = tx.query_row("SELECT MIN(0, COALESCE(MIN(id), 0)) - 1 FROM sales", [], |r| r.get(0))?;
            (Some(id), 0)
        } else {
            (None, next_rev(&tx)?)
        };
        let sale_id = insert_sale(&tx, &rec, id, rev, pending)?;
        tx.commit()?;
        Ok(sale_id)
    }

    /// Voids a sale. On a till (`queue` = true) a sale the server already has is
    /// flagged so the void is sent on the next sync.
    pub fn void_sale_as(&mut self, sale_id: i64, by_user: i64, queue: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        let pending: bool = tx
            .query_row("SELECT pending FROM sales WHERE id = ?1", [sale_id], |r| r.get::<_, i64>(0))
            .optional()?
            .is_some_and(|p| p != 0);
        // A sale still waiting to upload carries its voided flag with it.
        let void_pending = queue && !pending;
        let rev = if queue { None } else { Some(next_rev(&tx)?) };
        if !apply_void(&tx, sale_id, by_user, &now(), rev, void_pending)? {
            bail!("Sale {sale_id} is already voided or does not exist");
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mode(&self) -> String {
        self.setting("mode").ok().flatten().unwrap_or_else(|| "standalone".into())
    }

    // ------------------------------------------------------------ server side

    /// Accepts a sale from a till. Safe to receive twice: the uuid decides.
    pub fn ingest_sale(&mut self, s: &SaleRecord, till_name: &str) -> Result<i64> {
        let tx = self.conn.transaction()?;
        if let Some(id) = tx.query_row("SELECT id FROM sales WHERE uuid = ?1", [&s.uuid], |r| r.get(0)).optional()? {
            return Ok(id);
        }
        for l in &s.lines {
            let known: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM products WHERE id = ?1)", [l.product_id], |r| r.get(0))?;
            if !known {
                bail!("Sale {} refers to a product the server doesn't have (id {}).", s.uuid, l.product_id);
            }
        }
        let mut rec = s.clone();
        rec.till = Some(till_name.to_string());
        rec.voided = false;
        let rev = next_rev(&tx)?;
        let id = insert_sale(&tx, &rec, None, rev, false)?;
        if s.voided {
            let rev = next_rev(&tx)?;
            apply_void(&tx, id, s.voided_by.unwrap_or_default(), s.voided_at.as_deref().unwrap_or(&s.created_at), Some(rev), false)?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Accepts a void from a till. Voiding twice is harmless.
    pub fn ingest_void(&mut self, v: &VoidRecord) -> Result<()> {
        let tx = self.conn.transaction()?;
        let Some(id) = tx.query_row("SELECT id FROM sales WHERE uuid = ?1", [&v.uuid], |r| r.get::<_, i64>(0)).optional()? else {
            bail!("The server doesn't have sale {} yet.", v.uuid);
        };
        let rev = next_rev(&tx)?;
        apply_void(&tx, id, v.by_user, &v.at, Some(rev), false)?;
        tx.commit()?;
        Ok(())
    }

    /// Everything a till needs: all products, staff and shared settings, plus
    /// sales changed after `since`.
    pub fn snapshot(&self, since: i64, this_till: Option<i64>) -> Result<Snapshot> {
        let products = self
            .products(true)?
            .into_iter()
            .map(|p| ProductRecord {
                id: p.id,
                name: p.name,
                price_cents: p.price_cents,
                stock_qty: p.stock_qty,
                active: p.active,
                tills: p.tills,
            })
            .collect();
        let users = {
            let mut stmt = self.conn.prepare("SELECT id, name, role, pin_hash, active FROM users ORDER BY id")?;
            let rows = stmt.query_map([], |r| {
                Ok(UserRecord {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: r.get(2)?,
                    pin_hash: r.get(3)?,
                    active: r.get::<_, i64>(4)? != 0,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut settings = vec![];
        for k in SHARED_SETTINGS {
            if let Some(v) = self.setting(k)? {
                settings.push((k.to_string(), v));
            }
        }
        let mut sales = vec![];
        {
            let mut stmt =
                self.conn.prepare(&format!("SELECT {SALE_COLS} FROM sales WHERE rev > ?1 AND pending = 0 ORDER BY rev"))?;
            let mut rows = stmt.query([since])?;
            while let Some(r) = rows.next()? {
                let (id, rev, sale) = read_sale(&self.conn, r)?;
                sales.push(SyncedSale { id, rev, sale });
            }
        }
        let rev: i64 = self.conn.query_row("SELECT COALESCE(MAX(rev), 0) FROM sales", [], |r| r.get(0))?;
        let tills = self.tills()?;
        Ok(Snapshot { api: API_VERSION, products, users, settings, sales, rev: rev.max(since), tills, this_till })
    }

    /// Adds or edits a product, including which tills sell it.
    pub fn save_product(&mut self, c: &ProductChange, user_id: i64) -> Result<()> {
        let id = match c.id {
            Some(id) => {
                self.update_product(id, &c.name, c.price_cents, c.active)?;
                id
            }
            None => self.add_product(&c.name, c.price_cents, c.opening_stock, user_id)?,
        };
        self.set_product_tills(id, &c.tills)
    }

    // ------------------------------------------------------------ devices (server)

    pub fn add_device(&self, name: &str, token_hash: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO devices (name, token_hash) VALUES (?1, ?2)", params![name.trim(), token_hash])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The active till with this token, if any; records that it was seen.
    pub fn device_for_token(&self, token_hash: &str) -> Result<Option<(i64, String)>> {
        let found: Option<(i64, String)> = self
            .conn
            .query_row("SELECT id, name FROM devices WHERE token_hash = ?1 AND revoked = 0", [token_hash], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((id, _)) = &found {
            self.conn.execute("UPDATE devices SET last_seen = datetime('now', 'localtime') WHERE id = ?1", [id])?;
        }
        Ok(found)
    }

    /// (id, name, last_seen) for every till that hasn't been removed.
    pub fn devices(&self) -> Result<Vec<(i64, String, Option<String>)>> {
        let mut stmt = self.conn.prepare("SELECT id, name, last_seen FROM devices WHERE revoked = 0 ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Removes a till. Products only it sold stay limited to the tills that
    /// remain; a product left with none is sold at every till again.
    pub fn revoke_device(&self, id: i64) -> Result<()> {
        self.conn.execute("UPDATE devices SET revoked = 1 WHERE id = ?1", [id])?;
        self.conn.execute("DELETE FROM product_tills WHERE device_id = ?1", [id])?;
        Ok(())
    }

    // ------------------------------------------------------------ till side

    /// Sales waiting to be sent to the server, oldest first.
    pub fn pending_sales(&self) -> Result<Vec<SaleRecord>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {SALE_COLS} FROM sales WHERE pending = 1 ORDER BY id DESC"))?;
        let mut rows = stmt.query([])?;
        let mut out = vec![];
        while let Some(r) = rows.next()? {
            out.push(read_sale(&self.conn, r)?.2);
        }
        Ok(out)
    }

    pub fn mark_sale_sent(&self, uuid: &str) -> Result<()> {
        self.conn.execute("UPDATE sales SET pending = 0 WHERE uuid = ?1", [uuid])?;
        Ok(())
    }

    pub fn pending_voids(&self) -> Result<Vec<VoidRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, voided_by, COALESCE(voided_at, created_at) FROM sales WHERE void_pending = 1 AND pending = 0",
        )?;
        let rows = stmt.query_map([], |r| Ok(VoidRecord { uuid: r.get(0)?, by_user: r.get(1)?, at: r.get(2)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn mark_void_sent(&self, uuid: &str) -> Result<()> {
        self.conn.execute("UPDATE sales SET void_pending = 0 WHERE uuid = ?1", [uuid])?;
        Ok(())
    }

    /// (sales, voids) not yet on the server.
    pub fn unsent_counts(&self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(pending), 0), COALESCE(SUM(void_pending AND NOT pending), 0) FROM sales",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Clears local business data before this computer becomes a till: the
    /// server's copy is the truth. Settings (including the pairing) are kept.
    pub fn wipe_for_till(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM sale_items; DELETE FROM sales; DELETE FROM stock_movements;
             DELETE FROM product_tills; DELETE FROM known_tills;
             DELETE FROM products; DELETE FROM users;
             DELETE FROM settings WHERE key = 'sync_rev';",
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Applies a server snapshot on a till. Unsent local sales and voids stay
    /// and are reflected in stock until the server has them.
    pub fn apply_snapshot(&mut self, snap: &Snapshot) -> Result<()> {
        if snap.api != API_VERSION {
            bail!("The server uses a different version of the till software. Update the server and this till to the same version.");
        }
        let tx = self.conn.transaction()?;
        for p in &snap.products {
            tx.execute(
                "INSERT INTO products (id, name, price_cents, stock_qty, active) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, price_cents = excluded.price_cents,
                     stock_qty = excluded.stock_qty, active = excluded.active",
                params![p.id, p.name, p.price_cents, p.stock_qty, p.active as i64],
            )?;
        }
        tx.execute("DELETE FROM product_tills", [])?;
        for p in &snap.products {
            for t in &p.tills {
                tx.execute("INSERT OR IGNORE INTO product_tills (product_id, device_id) VALUES (?1, ?2)", params![p.id, t])?;
            }
        }
        tx.execute("DELETE FROM known_tills", [])?;
        for (id, name) in &snap.tills {
            tx.execute("INSERT INTO known_tills (id, name) VALUES (?1, ?2)", params![id, name])?;
        }
        if let Some(me) = snap.this_till {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('this_till', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [me.to_string()],
            )?;
        }
        for u in &snap.users {
            tx.execute(
                "INSERT INTO users (id, name, role, pin_hash, active) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, role = excluded.role,
                     pin_hash = excluded.pin_hash, active = excluded.active",
                params![u.id, u.name, u.role, u.pin_hash, u.active as i64],
            )?;
        }
        for (k, v) in &snap.settings {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v],
            )?;
        }
        for s in &snap.sales {
            // A void made here but not yet sent survives the server's (older) copy.
            let local_void: Option<(i64, String)> = tx
                .query_row(
                    "SELECT voided_by, COALESCE(voided_at, created_at) FROM sales WHERE uuid = ?1 AND void_pending = 1",
                    [&s.sale.uuid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            tx.execute("DELETE FROM sales WHERE uuid = ?1 OR id = ?2", params![s.sale.uuid, s.id])?;
            let mut rec = s.sale.clone();
            let server_voided = rec.voided;
            rec.voided = false;
            let id = insert_sale(&tx, &rec, Some(s.id), s.rev, false)?;
            if server_voided {
                apply_void(&tx, id, rec.voided_by.unwrap_or_default(), rec.voided_at.as_deref().unwrap_or(&rec.created_at), None, false)?;
                tx.execute("UPDATE sales SET voided_by = ?2, voided_at = ?3 WHERE id = ?1", params![id, rec.voided_by, rec.voided_at])?;
            } else if let Some((by, at)) = local_void {
                apply_void(&tx, id, by, &at, None, true)?;
            }
        }
        // Re-inserting sales above moved stock; the server's count is the truth.
        // Stock = server's count, minus what this till sold but hasn't sent,
        // plus what this till voided but hasn't told the server.
        for p in &snap.products {
            tx.execute("UPDATE products SET stock_qty = ?2 WHERE id = ?1", params![p.id, p.stock_qty])?;
        }
        tx.execute_batch(
            "UPDATE products SET stock_qty = stock_qty
                 - COALESCE((SELECT SUM(si.qty) FROM sale_items si JOIN sales s ON s.id = si.sale_id
                             WHERE si.product_id = products.id AND s.pending = 1 AND s.voided = 0), 0)
                 + COALESCE((SELECT SUM(si.qty) FROM sale_items si JOIN sales s ON s.id = si.sale_id
                             WHERE si.product_id = products.id AND s.pending = 0 AND s.void_pending = 1), 0);",
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('sync_rev', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [snap.rev.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn sync_rev(&self) -> i64 {
        self.setting("sync_rev").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Role;

    fn temp_db(tag: &str) -> Db {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("pos-sync-{tag}-{}-{n}", std::process::id()));
        Db::open(&dir.join("t.db")).unwrap()
    }

    fn stock(db: &Db, name: &str) -> i64 {
        db.products(true).unwrap().into_iter().find(|p| p.name == name).unwrap().stock_qty
    }

    fn castle(db: &Db) -> CartLine {
        let p = db.products(true).unwrap().into_iter().find(|p| p.name == "Castle 750ml").unwrap();
        CartLine { product_id: p.id, name: p.name, qty: 1, unit_price_cents: p.price_cents }
    }

    /// One push-then-pull round, exactly as the sync thread does it.
    fn sync(till: &mut Db, server: &mut Db, name: &str) {
        for s in till.pending_sales().unwrap() {
            server.ingest_sale(&s, name).unwrap();
            till.mark_sale_sent(&s.uuid).unwrap();
        }
        for v in till.pending_voids().unwrap() {
            server.ingest_void(&v).unwrap();
            till.mark_void_sent(&v.uuid).unwrap();
        }
        let snap = server.snapshot(till.sync_rev(), None).unwrap();
        till.apply_snapshot(&snap).unwrap();
    }

    fn new_till(server: &mut Db, tag: &str) -> Db {
        let mut till = temp_db(tag);
        till.wipe_for_till().unwrap();
        let snap = server.snapshot(0, None).unwrap();
        till.apply_snapshot(&snap).unwrap();
        till
    }

    #[test]
    fn tills_sell_offline_and_converge() {
        let mut server = temp_db("server");
        let boss = server.create_user("Tendai", Role::Manager, "4821").unwrap();
        let rudo = server.create_user("Rudo", Role::Cashier, "1357").unwrap();
        let start = stock(&server, "Castle 750ml");

        let mut a = new_till(&mut server, "a");
        let mut b = new_till(&mut server, "b");
        // Tills get staff with PIN hashes, so they can sign people in offline.
        assert!(a.verify_pin(rudo, "1357").unwrap().is_some());
        assert_eq!(a.sales_between("2000-01-01", "2999-12-31", None).unwrap().len(), 122, "history mirrored");

        // Both tills sell while "offline".
        let mut cart = vec![castle(&a)];
        cart[0].qty = 2;
        let sa = a.record_sale_as(&cart, 1000, "cash", rudo, true).unwrap();
        assert!(sa < 0, "pending sales use negative ids");
        b.record_sale_as(&[castle(&b)], 500, "cash", rudo, true).unwrap();
        assert_eq!(stock(&a, "Castle 750ml"), start - 2);
        assert_eq!(stock(&b, "Castle 750ml"), start - 1);
        assert_eq!(a.unsent_counts().unwrap(), (1, 0));

        // A pull alone keeps local unsent sales counted.
        let snap = server.snapshot(a.sync_rev(), None).unwrap();
        a.apply_snapshot(&snap).unwrap();
        assert_eq!(stock(&a, "Castle 750ml"), start - 2);

        // Back online: everyone converges on the same numbers.
        sync(&mut a, &mut server, "Till A");
        sync(&mut b, &mut server, "Till B");
        sync(&mut a, &mut server, "Till A");
        for db in [&server, &a, &b] {
            assert_eq!(stock(db, "Castle 750ml"), start - 3);
            assert_eq!(db.cashier_summary("2000-01-01", "2999-12-31").unwrap().iter().find(|c| c.name == "Rudo").unwrap().sales, 2);
        }
        assert_eq!(a.unsent_counts().unwrap(), (0, 0));
        let a_sales = a.sales_between("2000-01-01", "2999-12-31", Some(rudo)).unwrap();
        assert!(a_sales.iter().all(|s| s.id > 0 && !s.unsent), "server ids replace pending ones");

        // Resending is harmless (e.g. the reply was lost).
        let again = server.snapshot(0, None).unwrap().sales.into_iter().find(|s| s.sale.till.as_deref() == Some("Till A")).unwrap();
        assert_eq!(server.ingest_sale(&again.sale, "Till A").unwrap(), again.id);
        assert_eq!(stock(&server, "Castle 750ml"), start - 3);

        // Void on till B of till A's sale, while offline, then sync.
        let target = b.sales_between("2000-01-01", "2999-12-31", Some(rudo)).unwrap().into_iter().find(|s| s.total_cents == 400).unwrap();
        b.void_sale_as(target.id, boss, true).unwrap();
        assert_eq!(stock(&b, "Castle 750ml"), start - 1);
        let snap = server.snapshot(b.sync_rev(), None).unwrap();
        b.apply_snapshot(&snap).unwrap();
        assert_eq!(stock(&b, "Castle 750ml"), start - 1, "queued void survives a pull");
        sync(&mut b, &mut server, "Till B");
        sync(&mut a, &mut server, "Till A");
        for db in [&server, &a, &b] {
            assert_eq!(stock(db, "Castle 750ml"), start - 1);
        }
        let voided = a.sales_between("2000-01-01", "2999-12-31", None).unwrap().into_iter().find(|s| s.voided).unwrap();
        assert_eq!(voided.voided_by.as_deref(), Some("Tendai"));
    }

    #[test]
    fn sale_voided_before_it_was_sent() {
        let mut server = temp_db("server2");
        let boss = server.create_user("Tendai", Role::Manager, "4821").unwrap();
        let start = stock(&server, "Castle 750ml");
        let mut a = new_till(&mut server, "c");
        let id = a.record_sale_as(&[castle(&a)], 500, "cash", boss, true).unwrap();
        a.void_sale_as(id, boss, true).unwrap();
        assert_eq!(stock(&a, "Castle 750ml"), start);
        sync(&mut a, &mut server, "Till A");
        assert_eq!(stock(&server, "Castle 750ml"), start);
        assert_eq!(stock(&a, "Castle 750ml"), start);
        assert_eq!(a.unsent_counts().unwrap(), (0, 0));
    }

    #[test]
    fn devices_and_tokens() {
        let server = temp_db("server3");
        let id = server.add_device("Till 1", "hash-1").unwrap();
        assert_eq!(server.device_for_token("hash-1").unwrap().map(|d| d.0), Some(id));
        assert!(server.device_for_token("nope").unwrap().is_none());
        server.revoke_device(id).unwrap();
        assert!(server.device_for_token("hash-1").unwrap().is_none());
    }

    #[test]
    fn products_sold_at_chosen_tills() {
        let mut server = temp_db("server4");
        server.set_setting("mode", "server").unwrap();
        let boss = server.create_user("Tendai", Role::Manager, "4821").unwrap();
        let t1 = server.add_device("Till 1", "h1").unwrap();
        let t2 = server.add_device("Till 2", "h2").unwrap();
        let castle_id = castle(&server).product_id;
        let all = server.products(false).unwrap().len();
        let only_t1 = ProductChange {
            id: Some(castle_id),
            name: "Castle 750ml".into(),
            price_cents: 200,
            opening_stock: 0,
            active: true,
            tills: vec![t1],
        };
        server.save_product(&only_t1, boss).unwrap();
        // The server itself still sells everything.
        assert_eq!(server.products_for_sale().unwrap().len(), all);

        let till = |server: &Db, me: i64, tag: &str| {
            let mut db = temp_db(tag);
            db.set_setting("mode", "till").unwrap();
            db.wipe_for_till().unwrap();
            db.apply_snapshot(&server.snapshot(0, Some(me)).unwrap()).unwrap();
            db
        };
        let a = till(&server, t1, "pt1");
        let b = till(&server, t2, "pt2");
        let has = |db: &Db| db.products_for_sale().unwrap().iter().any(|p| p.id == castle_id);
        assert!(has(&a));
        assert!(!has(&b));
        assert_eq!(b.products_for_sale().unwrap().len(), all - 1);
        // Both tills know both tills, so a manager can reassign from either.
        assert_eq!(b.tills().unwrap().len(), 2);
        assert_eq!(b.products(true).unwrap().iter().find(|p| p.id == castle_id).unwrap().tills, vec![t1]);

        // Removing Till 1 leaves the product with no till, so it's sold everywhere again.
        server.revoke_device(t1).unwrap();
        let b = till(&server, t2, "pt3");
        assert!(has(&b));
    }
}
