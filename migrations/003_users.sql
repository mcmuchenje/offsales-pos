-- Till users who sign in with a PIN. Sales and stock changes record who did them.
CREATE TABLE users (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    role       TEXT    NOT NULL CHECK (role IN ('manager', 'cashier')),
    pin_hash   TEXT    NOT NULL,           -- Argon2id PHC string, never the PIN itself
    active     INTEGER NOT NULL DEFAULT 1,
    created_at TEXT    NOT NULL DEFAULT (datetime('now', 'localtime'))
);
CREATE UNIQUE INDEX idx_users_name ON users(name COLLATE NOCASE);

-- NULL user_id = recorded before till users existed (e.g. the spreadsheet import).
ALTER TABLE sales ADD COLUMN user_id INTEGER REFERENCES users(id);
ALTER TABLE sales ADD COLUMN voided_by INTEGER REFERENCES users(id);
ALTER TABLE sales ADD COLUMN voided_at TEXT;
ALTER TABLE stock_movements ADD COLUMN user_id INTEGER REFERENCES users(id);
CREATE INDEX idx_sales_user ON sales(user_id);

INSERT OR IGNORE INTO settings (key, value) VALUES ('auto_lock_minutes', '10');
