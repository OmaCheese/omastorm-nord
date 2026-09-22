//! MET Norway's Frost API (`frost.met.no`, CC BY 4.0). Every request
//! needs a client ID (HTTP basic auth, free registration at
//! frost.met.no/auth/requestCredentials.html); without one the API answers
//! 401 "Missing authentication token". The engine reads the ID from
//! `OMASTORM_FROST_CLIENT_ID`.
//!
//! No Frost answer could be recorded without an ID, so there is no fixture
//! and no parser yet: Norway is skipped, and says why in `obs.providers`,
//! rather than parsed against a guessed format. With an ID configured it
//! is still skipped until a recorded answer lets the parser be written and
//! tested (S42 review: the human is asked for an ID).

use super::Part;

pub const ENV: &str = "OMASTORM_FROST_CLIENT_ID";

pub fn parts() -> Result<Vec<Part>, String> {
    skip_reason(std::env::var(ENV).ok().as_deref())
}

fn skip_reason(id: Option<&str>) -> Result<Vec<Part>, String> {
    match id.map(str::trim) {
        None | Some("") => Err(format!("no Frost client ID ({ENV})")),
        Some(_) => Err("Frost client ID set, but the Frost reader is not built yet".into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn norway_is_skipped_and_says_why() {
        let none = super::skip_reason(None).err().unwrap();
        assert!(none.contains("no Frost client ID"), "{none}");
        assert!(super::skip_reason(Some("  ")).is_err());
        assert!(super::skip_reason(Some("abc")).is_err());
    }
}
