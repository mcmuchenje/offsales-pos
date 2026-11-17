# Off Sales POS

Offline point-of-sale for Windows, written in Rust (egui) with a local SQLite database.

## Features
- **Sell** – tap product tiles to build a sale, enter cash received (or quick $1–$50 buttons), see change, press **Enter** to complete.
- **Products** – add/edit products and prices, hide products from the till, receive deliveries or correct stock (every change is logged in `stock_movements`).
- **Sales** – browse sales by date, void a sale (stock is returned).
- **Reports** – takings by day and by product for any date range, plus stock on hand (low stock ≤ 5 highlighted).
- **Settings** – check for and install updates, back up the database.

## Staff and PINs
- **First launch** asks for a manager account (name and a 4–6 digit PIN). Existing sales are kept and shown as "Imported".
- **Signing in:** tap your name, then type your PIN on the keypad or keyboard. Five wrong PINs lock that person out for a minute.
- **Managers** can do everything. They add people and reset PINs in the **Staff** tab.
- **Cashiers** see Sell and My sales only. To void a sale, a manager types their PIN on that sale's row.
- **Lock** (top right) hands the till to the next person. An unfinished sale is kept if the same person signs back in, and cleared if someone else does.
- **Auto-lock:** the till locks itself after 10 idle minutes (change it in Settings; 0 = never). It never locks during a sale.
- **Who did what:** every sale, void and stock change records the person. Reports show takings **by cashier**.
- **PINs are never stored**, only Argon2id hashes.
- **Locked out:** if every manager forgets their PIN, restore a backup taken while a PIN was known.

## Several tills on the shop network
Each installation runs in one of three modes. You choose the mode at first run or in **Settings → Network**:
- **Standalone:** a single till. This is the default and works exactly as before.
- **Server:** the back-office PC. It holds the real data, takes backups and runs an encrypted service for the tills on TCP 47800, with discovery on UDP 47801.
- **Till:** connects to the server. It keeps a full local copy, so selling, signing in and reports keep working when the server or network is down. Sales and voids made offline are sent automatically when the server comes back.

### Setting up
1. On the back-office PC, go to **Settings → Network**, choose **Make this computer the server**, then **Allow through Windows Firewall** (Windows asks for admin permission once). Tick **Start with Windows** if the PC is usually on.
2. Click **Add a till**. The server shows an 8-digit code. It's valid for 10 minutes and works once.
3. On the till, choose **Extra till** on the first-run screen, or **Connect to a server** in Settings.
   - **Search the shop network** finds the server; if it doesn't, type the server's address.
   - Enter the code and a name for the till, such as "Front counter".
   - Connecting replaces the till's own data with the server's.
4. The top bar on each till shows **Online**, **Sending n** or **Offline**. Settings shows the last sync time and anything waiting to send.

### How it behaves
- **Stock:** on a till, stock is the server's count minus that till's sales not yet sent. If two tills sell the last bottle while offline, stock goes to −1 until someone corrects it with **Receive stock**.
- **Manager changes go through the server:** prices, products, stock, staff and shop settings. A till that can't reach the server says so and saves nothing. The server checks the manager's PIN itself.
- **Which till sells what:** in **Products**, edit a product and under **Sold at** choose **Every till** (the default) or tick only the tills that sell it, for example the bar's drinks at "Till 1" and the bottle store's at "Till 2". Each till's Sell screen shows only its products. The server PC's own Sell screen shows everything. Stock is shared, so it doesn't matter which till sells a product.
- **Removing a till:** do it in the server's **Settings → Network**. A removed till stops syncing until it's paired again. Products that were sold only at that till stay with the remaining ticked tills; a product left with none goes back to every till. A till paired again counts as a new till, so tick it on its products again.
- **Security:** traffic is TLS-encrypted with the server's own certificate. Each till locks onto that certificate when it pairs, and pairing proves both sides saw the same certificate. The **Security code** shown on the server and on each till should match.
- **New address:** if the server's address changes, for example a new DHCP lease, tills find it again automatically by its certificate.
- **Updates:** update the server first, then the tills. A till and server on incompatible versions say so instead of syncing.

### Development
`cargo build --release --features dev-tools`, then set the environment variable `POS_DEV_USER=<name>` to sign that person in without a PIN, and `POS_SERVER_LOOPBACK=1` to keep server mode on 127.0.0.1. The installer never uses this feature.

## Data
- Database: `%APPDATA%\OffSales\POS\data\pos.db` (kept across updates). Run with `--db <path>` to use another file, and `--tab reports` (or sell, products, sales, settings) to open on a page, e.g. from a desktop shortcut.
- Backups: Settings → *Back up database now* writes to `Documents\OffSales POS Backups`. A backup is also taken automatically before installing an update.
- First run imports the old **Off sales.xlsx** sheet: 9 products, their opening stock and all 122 recorded sales (25–30 Oct 2026). Closing stock matches the spreadsheet.
  - The sheet had no price for **Hunters 330ml** and **Corona** – set them in *Products* before selling.
  - Times of imported sales are unknown (the sheet only had dates), so they are given placeholder times in sheet order.
- Schema changes go in a new file in `migrations/`, appended to `MIGRATIONS` in `src/db.rs`. `PRAGMA user_version` tracks what is applied, so existing shops upgrade automatically and keep their data.

To regenerate the seed from the spreadsheet: `python tools/xlsx_to_seed.py "Off sales.xlsx" migrations/seed.sql` (needs `openpyxl`). Only affects brand-new databases.

## Updates
Updates come from GitHub Releases:
1. Push this project to a GitHub repo.
2. Bump `version` in `Cargo.toml`, commit, then `git tag v0.2.0 && git push --tags`.
3. `.github/workflows/release.yml` checks the tag matches `Cargo.toml`, runs the tests, and publishes two files with that repo baked in as the update source:
   - `OffSalesPOS-Setup-<version>.exe` – the installer, for setting up a new till
   - `offsales-pos-<version>-x86_64-pc-windows-msvc.zip` – what the in-app updater downloads

The app checks quietly at startup, and on demand from Settings. When offline it just says so; the till is unaffected. The update source can also be set in Settings (`owner/repo`). If the repo is private, the app can't see releases – use a public repo for releases.

## Look and feel
The theme lives in `src/theme.rs`: bottle-ink and shelf-grey palette, square corners throughout, Bahnschrift for prices and totals and Segoe UI for everything else (both ship with Windows; egui's fonts are the fallback). Layout adapts to the window width: below 820 logical px the cart docks under the products, and on short screens only the total and the Complete button stay pinned.

## Build
```
cargo test
cargo build --release      # -> target\release\offsales-pos.exe
```
Use release builds for running the app – debug builds render very slowly.

## Installer
```
powershell -ExecutionPolicy Bypass -File installer\build.ps1   # -> dist\OffSalesPOS-Setup-<version>.exe
```
Needs NSIS 3 (`makensis.exe`); the script looks on PATH, in Program Files, and in `%LOCALAPPDATA%\tauri\NSIS`, or set `$env:MAKENSIS`.

What the installer does (`installer/offsales-pos.nsi`):
- Installs for the current Windows user into `%LOCALAPPDATA%\Programs\Off Sales POS`. No admin prompt, and the folder stays writable so in-app updates work.
- Adds a Start menu shortcut, and a desktop shortcut if ticked on the last page.
- Registers in Settings → Apps with an uninstaller. In-app updates keep the version shown there current.
- Running it again upgrades in place. If the till is open, it asks you to close it first.
- Install, upgrade and uninstall never touch the data in `%APPDATA%\OffSales\POS` or the backups.
- Silent install for setting up many tills: `OffSalesPOS-Setup-<version>.exe /S`.

Icon and installer artwork are generated by `python tools/make_icon.py` (needs Pillow) into `assets/`.
