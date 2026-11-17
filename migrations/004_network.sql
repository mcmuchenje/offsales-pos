-- Server/till sync on the LAN.
--
-- Every sale gets a globally unique uuid so a till can resend it safely.
-- On the server, `rev` increases whenever a sale is added or voided; tills
-- pull "everything with rev greater than what I last saw".
-- On a till, `pending` = not yet accepted by the server (these use negative
-- ids until the server's copy replaces them); `void_pending` = voided on this
-- till but the server hasn't been told yet.
ALTER TABLE sales ADD COLUMN uuid TEXT;
ALTER TABLE sales ADD COLUMN till TEXT;
ALTER TABLE sales ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sales ADD COLUMN pending INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sales ADD COLUMN void_pending INTEGER NOT NULL DEFAULT 0;
UPDATE sales SET uuid = lower(hex(randomblob(16))) WHERE uuid IS NULL;
UPDATE sales SET rev = id;
CREATE UNIQUE INDEX idx_sales_uuid ON sales(uuid);
CREATE INDEX idx_sales_rev ON sales(rev);
CREATE INDEX idx_sales_pending ON sales(pending);

-- Tills paired with this server. The token itself is only ever on the till.
CREATE TABLE devices (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    token_hash TEXT    NOT NULL UNIQUE,
    created_at TEXT    NOT NULL DEFAULT (datetime('now', 'localtime')),
    last_seen  TEXT,
    revoked    INTEGER NOT NULL DEFAULT 0
);

INSERT OR IGNORE INTO settings (key, value) VALUES ('mode', 'standalone');
