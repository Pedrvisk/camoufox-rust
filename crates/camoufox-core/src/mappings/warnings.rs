//! Leak-warning messages shown when a caller manually sets fingerprint-related
//! config domains, plus the warning emission logic.

/// Returns the canned warning message for a key.
pub fn warning_text(key: &str) -> Option<&'static str> {
    Some(match key {
        "navigator" => {
            "Manually setting navigator properties is not recommended. Device information is \
             automatically generated within Camoufox based on the provided `os`."
        }
        "locale" => {
            "Use the `locale` parameter in Camoufox instead of setting the config manually."
        }
        "geolocation" => {
            "Please use the `geoip` parameter in Camoufox instead of setting your geolocation \
             manually. This can lead to detection if your target geolocation does not match your \
             IP. Pass `geoip=true` or a target IP (ex: geoip='123.45.67.89') to let Camoufox \
             populate this data for you."
        }
        "header-ua" => {
            "Do not set the header.User-Agent manually. Camoufox will generate a User-Agent for you."
        }
        "viewport" => {
            "Manually setting screen & window properties is not recommended. Screen dimensions \
             are randomly generated within Camoufox based on the provided screen constraints. See \
             here: https://github.com/daijro/camoufox/tree/main/pythonlib#browserforge-integration."
        }
        "custom_fingerprint" => {
            "Passing your own fingerprint is not recommended. BrowserForge fingerprints are \
             automatically generated within Camoufox based on the provided `os` and `screen` \
             constraints."
        }
        "proxy_without_geoip" => {
            "When using a proxy, it is heavily recommended that you pass `geoip=true`."
        }
        "ff_version" => {
            "Spoofing the Firefox version will likely lead to detection. If rotating the Firefox \
             version is absolutely necessary, it would be more advisable to rotate between older \
             versions of Camoufox instead."
        }
        "no_region" => {
            "Because you did not pass in a locale region, Camoufox will generate one for you. \
             This can cause suspicion if your IP does not match your locale region."
        }
        "block_webgl" => {
            "Disabling WebGL is not recommended. Many WAFs will check if WebGL is enabled."
        }
        "block_images" => {
            "Blocking image requests has been reported to cause detection issues on major WAFs."
        }
        "custom_fonts_only" => {
            "Disabling OS-specific fonts while spoofing your OS will make your browser \
             fingerprint inconsistent. WAFs can detect this mismatch between your claimed OS and \
             available system fonts."
        }
        "disable_coop" => {
            "Disabling Cross-Origin-Opener-Policy (COOP) handling can potentially be detected by \
             sophisticated WAFs."
        }
        _ => return None,
    })
}

/// `LeakWarning.warn(key, iKnowWhatImDoing?)`.
///
/// - Skipped entirely when `i_know_what_im_doing` is `Some(true)`.
/// - When `Some(false)` is passed explicitly, the hint about
///   `i_know_what_im_doing` is appended.
pub fn warn_leak(key: &str, i_know_what_im_doing: Option<bool>) {
    if i_know_what_im_doing == Some(true) {
        return;
    }
    let Some(text) = warning_text(key) else {
        return;
    };
    if i_know_what_im_doing.is_some() {
        log::warn!("{text}\nIf this is intentional, pass `i_know_what_im_doing=true`.");
    } else {
        log::warn!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_keys_have_messages() {
        for key in [
            "navigator",
            "locale",
            "geolocation",
            "header-ua",
            "viewport",
            "custom_fingerprint",
            "proxy_without_geoip",
            "ff_version",
            "no_region",
            "block_webgl",
            "block_images",
            "custom_fonts_only",
            "disable_coop",
        ] {
            assert!(warning_text(key).is_some(), "missing warning for {key}");
        }
        assert!(warning_text("nope").is_none());
    }
}
