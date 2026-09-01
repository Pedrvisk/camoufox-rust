//! MaxMind GeoLite2-City database management and geolocation lookup.

use std::path::PathBuf;

use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::locale::{from_region, Geolocation};

use crate::public_ip::validate_ip;

/// The GeoLite2 database mirror repository.
pub const MMDB_REPO: &str = "P3TERX/GeoLite.mmdb";

/// Path of the GeoLite2-City database inside the install directory.
pub fn mmdb_path() -> PathBuf {
    camoufox_pkgman::install_dir().join("GeoLite2-City.mmdb")
}

/// Downloads the GeoLite2-City database.
///
/// The download is staged in a temp file and moved into place only once it
/// completes, so a failed download never leaves a truncated database behind.
pub async fn download_mmdb() -> Result<()> {
    if camoufox_core::env_utils::skip_browser_download() {
        log::info!("Skipping GeoIP database download due to PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD set!");
        return Ok(());
    }

    let downloader = camoufox_pkgman::GitHubDownloader::new(MMDB_REPO);
    let asset = downloader
        .get_asset(
            |asset| {
                if asset.name.ends_with("-City.mmdb") {
                    Some((
                        camoufox_pkgman::CamoufoxVersion::new("0", None),
                        asset.browser_download_url.clone(),
                    ))
                } else {
                    None
                }
            },
            5,
        )
        .await
        .map_err(|_| {
            CamoufoxError::MissingRelease("Failed to find GeoIP database release asset".into())
        })?;

    let target = mmdb_path();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_file = target.with_extension(format!("mmdb.{}.download", std::process::id()));

    let result: Result<()> = async {
        let mut file = std::fs::File::create(&temp_file)?;
        camoufox_pkgman::webdl(
            &asset.url,
            "Downloading GeoIP database",
            true,
            Some(&mut file),
            5,
        )
        .await?;
        drop(file);
        std::fs::rename(&temp_file, &target)?;
        Ok(())
    }
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&temp_file);
            Err(e)
        }
    }
}

/// Removes the GeoLite2-City database.
pub fn remove_mmdb() {
    let path = mmdb_path();
    if !path.exists() {
        log::info!("GeoIP database not found.");
        return;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => log::info!("GeoIP database removed."),
        Err(e) => log::warn!("Failed to remove GeoIP database: {e}"),
    }
}

/// Looks up the geolocation for an IP, downloading the database when missing.
pub async fn get_geolocation(ip: &str) -> Result<Geolocation> {
    if !mmdb_path().exists() {
        download_mmdb().await?;
    }

    validate_ip(ip)?;

    let reader = maxminddb::Reader::open_readfile(mmdb_path())
        .map_err(|e| CamoufoxError::MaxMind(format!("could not open GeoIP database: {e}")))?;

    let response: maxminddb::geoip2::City = reader
        .lookup(
            ip.parse()
                .map_err(|_| CamoufoxError::InvalidIp(ip.to_string()))?,
        )
        .map_err(|e| CamoufoxError::MaxMind(format!("lookup failed: {e}")))?;

    let iso_code = response
        .country
        .as_ref()
        .and_then(|c| c.iso_code)
        .map(str::to_uppercase);
    let Some(location) = response.location else {
        return Err(CamoufoxError::UnknownIpLocation(format!(
            "Unknown IP location: {ip}"
        )));
    };

    let (Some(longitude), Some(latitude), Some(time_zone), Some(iso_code)) = (
        location.longitude,
        location.latitude,
        location.time_zone,
        iso_code,
    ) else {
        return Err(CamoufoxError::UnknownIpLocation(format!(
            "Unknown IP location: {ip}"
        )));
    };

    let locale = from_region(&iso_code)?;

    Ok(Geolocation {
        locale,
        longitude,
        latitude,
        timezone: time_zone.to_string(),
        accuracy: location.accuracy_radius.map(u32::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmdb_path_is_in_install_dir() {
        let path = mmdb_path();
        assert!(path.to_string_lossy().contains("GeoLite2-City.mmdb"));
    }
}
