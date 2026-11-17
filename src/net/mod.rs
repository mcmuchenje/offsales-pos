//! LAN server/till networking.
//!
//! Server mode: `server::start` runs an HTTPS API (and UDP discovery) next to the
//! normal app. Till mode: `worker::SyncWorker` keeps a local copy in step with
//! the server and sends sales and voids made while offline.
pub mod client;
pub mod protocol;
pub mod server;
pub mod tls;
pub mod worker;

#[cfg(test)]
mod e2e {
    use std::net::IpAddr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use super::client::{discover, pair, Remote};
    use super::protocol::{ProductChange, UserChange};
    use super::server;
    use super::worker::{pairing, sync_once};
    use crate::db::{CartLine, Db, Role};

    fn temp_path(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("pos-e2e-{tag}-{}-{n}", std::process::id())).join("pos.db")
    }

    fn castle(db: &Db) -> (CartLine, i64) {
        let p = db.products(true).unwrap().into_iter().find(|p| p.name == "Castle 750ml").unwrap();
        (CartLine { product_id: p.id, name: p.name, qty: 1, unit_price_cents: p.price_cents }, p.stock_qty)
    }

    /// Pairs a fresh till database with the server, as the setup screen does.
    fn new_till(srv: &server::ServerHandle, tag: &str) -> (Db, Remote) {
        let code = srv.new_pairing_code();
        let addr = format!("127.0.0.1:{}", srv.addr.port());
        let p = pair(&addr, &code, tag).unwrap();
        assert_eq!(p.fingerprint, srv.fingerprint());
        let mut db = Db::open(&temp_path(tag)).unwrap();
        db.wipe_for_till().unwrap();
        db.set_setting("mode", "till").unwrap();
        db.set_setting("server_addr", &p.addr).unwrap();
        db.set_setting("server_fingerprint", &p.fingerprint).unwrap();
        db.set_setting("device_token", &p.token).unwrap();
        let (a, f, t) = pairing(&db).unwrap();
        let remote = Remote::connect(&a, &f, &t).unwrap();
        sync_once(&mut db, &remote).unwrap();
        (db, remote)
    }

    #[test]
    fn server_and_tills_over_tls() {
        let server_path = temp_path("server");
        let (boss, rudo) = {
            let db = Db::open(&server_path).unwrap();
            (db.create_user("Tendai", Role::Manager, "4821").unwrap(), db.create_user("Rudo", Role::Cashier, "1357").unwrap())
        };
        let srv = server::start(&server_path, true, 0, Some(0), Arc::new(|| {})).unwrap();
        let addr = format!("127.0.0.1:{}", srv.addr.port());

        // Discovery finds it.
        let found = discover(srv.discovery_port.unwrap(), Duration::from_millis(800), &[IpAddr::from([127, 0, 0, 1])]);
        assert!(found.iter().any(|f| f.addr == addr), "discovered {found:?}");

        // Pairing: no code, wrong code, then the right one; codes are single use.
        assert!(pair(&addr, "12345678", "Till X").is_err(), "no active code");
        let code = srv.new_pairing_code();
        let wrong = if code == "00000000" { "11111111" } else { "00000000" };
        let err = pair(&addr, wrong, "Till X").unwrap_err().to_string();
        assert!(err.contains("pairing code didn't work"), "{err}");
        assert!(pair(&addr, &code, "Till X").is_ok());
        assert!(pair(&addr, &code, "Till Y").is_err(), "code was used up");

        let (mut a, ra) = new_till(&srv, "Till A");
        let (mut b, rb) = new_till(&srv, "Till B");
        assert!(a.verify_pin(rudo, "1357").unwrap().is_some(), "staff and PIN hashes reached the till");

        // A till can't be fooled by a different server certificate.
        let (_, _, token) = pairing(&a).unwrap();
        let err = Remote::connect(&addr, &"0".repeat(64), &token).unwrap().hello().unwrap_err().to_string();
        assert!(err.contains("isn't the server this till was paired with"), "{err}");

        // Both tills sell; everyone converges.
        let (line, start) = castle(&a);
        a.record_sale_as(&[line.clone()], 500, "cash", rudo, true).unwrap();
        b.record_sale_as(&[line.clone()], 500, "cash", rudo, true).unwrap();
        sync_once(&mut a, &ra).unwrap();
        sync_once(&mut b, &rb).unwrap();
        sync_once(&mut a, &ra).unwrap();
        let server_db = Db::open(&server_path).unwrap();
        for db in [&server_db, &a, &b] {
            assert_eq!(castle(db).1, start - 2);
        }
        let tills: Vec<_> = server_db.snapshot(0, None).unwrap().sales.into_iter().filter_map(|s| s.sale.till).collect();
        assert!(tills.contains(&"Till A".to_string()) && tills.contains(&"Till B".to_string()));

        // Server goes away: till A keeps selling, then catches up on a new server address.
        srv.stop();
        drop(srv);
        std::thread::sleep(Duration::from_millis(300));
        a.record_sale_as(&[line.clone()], 500, "cash", rudo, true).unwrap();
        assert!(sync_once(&mut a, &ra).is_err(), "offline");
        assert_eq!(castle(&a).1, start - 3);
        assert_eq!(a.unsent_counts().unwrap(), (1, 0));
        let srv = server::start(&server_path, true, 0, None, Arc::new(|| {})).unwrap();
        let (_, fp, token) = pairing(&a).unwrap();
        let ra = Remote::connect(&format!("127.0.0.1:{}", srv.addr.port()), &fp, &token).unwrap();
        sync_once(&mut a, &ra).unwrap();
        assert_eq!(a.unsent_counts().unwrap(), (0, 0));
        assert_eq!(castle(&Db::open(&server_path).unwrap()).1, start - 3);

        // Manager changes go through the server with a manager session.
        assert!(ra.session(rudo, "1357").is_err(), "cashiers can't manage");
        assert!(ra.session(boss, "0000").is_err(), "wrong PIN");
        let s = ra.session(boss, "4821").unwrap();
        let (line, _) = castle(&a);
        ra.change_product(&s, &ProductChange { id: Some(line.product_id), name: line.name.clone(), price_cents: 250, opening_stock: 0, active: true, tills: vec![] })
            .unwrap();
        ra.change_user(&s, &UserChange { id: None, name: "Farai".into(), role: "cashier".into(), pin: Some("2468".into()), active: true }).unwrap();
        assert!(ra.change_product("not-a-session", &ProductChange { id: None, name: "X".into(), price_cents: 1, opening_stock: 0, active: true, tills: vec![] }).is_err());
        sync_once(&mut a, &ra).unwrap();
        assert_eq!(a.products(true).unwrap().into_iter().find(|p| p.id == line.product_id).unwrap().price_cents, 250);
        assert!(a.users(false).unwrap().iter().any(|u| u.name == "Farai"));

        // Removing a till on the server cuts it off.
        let server_db = Db::open(&server_path).unwrap();
        let id = server_db.devices().unwrap().into_iter().find(|d| d.1 == "Till A").unwrap().0;
        server_db.revoke_device(id).unwrap();
        let err = sync_once(&mut a, &ra).unwrap_err().to_string();
        assert!(err.contains("removed from the server"), "{err}");
        drop(rb);
    }
}
