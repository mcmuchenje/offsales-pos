CREATE TABLE products (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL UNIQUE,
    price_cents INTEGER NOT NULL DEFAULT 0 CHECK (price_cents >= 0),
    stock_qty   INTEGER NOT NULL DEFAULT 0,
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE TABLE sales (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now', 'localtime')),
    total_cents    INTEGER NOT NULL,
    tendered_cents INTEGER NOT NULL,
    change_cents   INTEGER NOT NULL,
    payment_method TEXT    NOT NULL DEFAULT 'cash',
    note           TEXT,
    voided         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_sales_created ON sales(created_at);

CREATE TABLE sale_items (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    sale_id          INTEGER NOT NULL REFERENCES sales(id) ON DELETE CASCADE,
    product_id       INTEGER NOT NULL REFERENCES products(id),
    qty              INTEGER NOT NULL CHECK (qty > 0),
    unit_price_cents INTEGER NOT NULL,
    line_total_cents INTEGER NOT NULL
);
CREATE INDEX idx_sale_items_sale ON sale_items(sale_id);
CREATE INDEX idx_sale_items_product ON sale_items(product_id);

-- Ledger of every stock change: opening, sale, restock, adjust, void.
CREATE TABLE stock_movements (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id INTEGER NOT NULL REFERENCES products(id),
    change_qty INTEGER NOT NULL,
    reason     TEXT    NOT NULL,
    ref_id     INTEGER,
    note       TEXT,
    created_at TEXT    NOT NULL DEFAULT (datetime('now', 'localtime'))
);
CREATE INDEX idx_stock_movements_product ON stock_movements(product_id);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO settings (key, value) VALUES ('shop_name', 'Off Sales'), ('currency', '$');
