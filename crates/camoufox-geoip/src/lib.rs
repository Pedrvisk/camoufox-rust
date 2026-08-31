//! # camoufox-geoip
//!
//! Public IP resolution through optional proxies and MaxMind geolocation
//! lookups backed by the GeoLite2-City database.

mod mmdb;
mod public_ip;

pub use mmdb::{download_mmdb, get_geolocation, mmdb_path, remove_mmdb, MMDB_REPO};
pub use public_ip::{public_ip, valid_ipv4, valid_ipv6, validate_ip};
