//! PIN hashing and sign-in throttling.
//!
//! PINs are short, so the hash alone can't make them strong. Argon2id makes each
//! guess against a stolen database expensive, and the in-app throttle stops
//! someone at the counter from trying PINs one after another.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand_core::OsRng;
use argon2::{Algorithm, Argon2, Params, Version};

pub const PIN_MIN: usize = 4;
pub const PIN_MAX: usize = 6;

/// Wrong PINs allowed per person before a cool-down.
const MAX_TRIES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

fn hasher() -> Argon2<'static> {
    // 19 MiB, 2 passes: the OWASP baseline for Argon2id; ~30 ms on a laptop.
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(19 * 1024, 2, 1, None).expect("valid params"))
}

pub fn validate_pin(pin: &str) -> Result<()> {
    if !(PIN_MIN..=PIN_MAX).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        bail!("A PIN is {PIN_MIN} to {PIN_MAX} digits.");
    }
    Ok(())
}

pub fn hash_pin(pin: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(hasher().hash_password(pin.as_bytes(), &salt).map_err(|e| anyhow!("Could not secure the PIN: {e}"))?.to_string())
}

pub fn verify_pin(pin: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|h| hasher().verify_password(pin.as_bytes(), &h).is_ok())
}

/// Counts wrong PINs per user and enforces a short cool-down.
#[derive(Default)]
pub struct Throttle {
    fails: HashMap<i64, (u32, Option<Instant>)>,
}

impl Throttle {
    /// Seconds left before this user may try again, if locked.
    pub fn locked_for(&self, user_id: i64) -> Option<u64> {
        let (_, until) = self.fails.get(&user_id)?;
        let left = until.as_ref()?.checked_duration_since(Instant::now())?;
        Some(left.as_secs() + 1)
    }

    /// Records a wrong PIN. Returns tries left before lockout (0 = now locked).
    pub fn fail(&mut self, user_id: i64) -> u32 {
        let e = self.fails.entry(user_id).or_insert((0, None));
        if e.1.is_some_and(|t| t <= Instant::now()) {
            *e = (0, None); // previous lockout expired
        }
        e.0 += 1;
        if e.0 >= MAX_TRIES {
            e.1 = Some(Instant::now() + LOCKOUT);
            e.0 = 0;
            0
        } else {
            MAX_TRIES - e.0
        }
    }

    pub fn success(&mut self, user_id: i64) {
        self.fails.remove(&user_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_rules() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("123456").is_ok());
        assert!(validate_pin("123").is_err());
        assert!(validate_pin("1234567").is_err());
        assert!(validate_pin("12a4").is_err());
    }

    #[test]
    fn hash_roundtrip_and_salted() {
        let a = hash_pin("4821").unwrap();
        let b = hash_pin("4821").unwrap();
        assert_ne!(a, b, "each hash has its own salt");
        assert!(!a.contains("4821"));
        assert!(verify_pin("4821", &a));
        assert!(!verify_pin("4822", &a));
        assert!(!verify_pin("4821", "not a hash"));
    }

    #[test]
    fn throttle_locks_after_five() {
        let mut t = Throttle::default();
        for left in (1..=4).rev() {
            assert_eq!(t.fail(7), left);
        }
        assert_eq!(t.fail(7), 0);
        assert!(t.locked_for(7).is_some());
        assert!(t.locked_for(8).is_none());
        t.success(7);
        assert!(t.locked_for(7).is_none());
    }
}
