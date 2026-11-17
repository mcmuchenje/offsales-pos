//! Messages exchanged between the server and tills (JSON over HTTPS).

use serde::{Deserialize, Serialize};

/// Bump when the API changes incompatibly; tills and server refuse to sync on mismatch.
pub const API_VERSION: u32 = 1;
pub const DEFAULT_PORT: u16 = 47800;
pub const DISCOVERY_PORT: u16 = 47801;
pub const DISCOVERY_PROBE: &[u8] = b"OFFSALES-POS-DISCOVER-1";

/// Settings the server shares with every till.
pub const SHARED_SETTINGS: &[&str] = &["shop_name", "auto_lock_minutes", "update_repo"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SaleLine {
    pub product_id: i64,
    pub qty: i64,
    pub unit_price_cents: i64,
}

/// A sale as a till sends it, and as the server returns it in snapshots.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SaleRecord {
    pub uuid: String,
    pub created_at: String,
    pub user_id: Option<i64>,
    pub till: Option<String>,
    pub total_cents: i64,
    pub tendered_cents: i64,
    pub change_cents: i64,
    pub payment_method: String,
    pub lines: Vec<SaleLine>,
    pub voided: bool,
    pub voided_by: Option<i64>,
    pub voided_at: Option<String>,
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncedSale {
    pub id: i64,
    pub rev: i64,
    pub sale: SaleRecord,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VoidRecord {
    pub uuid: String,
    pub by_user: i64,
    pub at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProductRecord {
    pub id: i64,
    pub name: String,
    pub price_cents: i64,
    pub stock_qty: i64,
    pub active: bool,
    /// Tills that sell it; empty = every till.
    #[serde(default)]
    pub tills: Vec<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserRecord {
    pub id: i64,
    pub name: String,
    pub role: String,
    /// Argon2 hash, so tills can check PINs while the server is unreachable.
    pub pin_hash: String,
    pub active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub api: u32,
    pub products: Vec<ProductRecord>,
    pub users: Vec<UserRecord>,
    pub settings: Vec<(String, String)>,
    /// Sales changed since the `since` revision the till asked for.
    pub sales: Vec<SyncedSale>,
    /// Highest revision included; the till asks for changes after this next time.
    pub rev: i64,
    /// Every paired till (id, name), for assigning products from any till.
    #[serde(default)]
    pub tills: Vec<(i64, String)>,
    /// The id of the till that asked.
    #[serde(default)]
    pub this_till: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Hello {
    pub api: u32,
    pub app_version: String,
    pub shop_name: String,
}

/// Discovery reply (UDP). Informational only: trust comes from pairing.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Announce {
    pub api: u32,
    pub shop_name: String,
    pub port: u16,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PairRequest {
    pub till_name: String,
    pub nonce: String,
    /// HMAC(code, "till" | server cert fingerprint as seen by the till | nonce)
    pub proof: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PairResponse {
    pub token: String,
    /// HMAC(code, "server" | fingerprint | nonce | token): proves the server knew the code.
    pub proof: String,
    pub shop_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionRequest {
    pub user_id: i64,
    pub pin: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionResponse {
    pub session: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProductChange {
    /// None = add a new product.
    pub id: Option<i64>,
    pub name: String,
    pub price_cents: i64,
    pub opening_stock: i64,
    pub active: bool,
    /// Tills that sell it; empty = every till.
    #[serde(default)]
    pub tills: Vec<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StockChange {
    pub product_id: i64,
    pub qty: i64,
    pub note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserChange {
    /// None = add a new person.
    pub id: Option<i64>,
    pub name: String,
    pub role: String,
    /// Required when adding; optional (None = keep) when editing.
    pub pin: Option<String>,
    pub active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SettingsChange {
    pub values: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ApiError {
    pub error: String,
}
