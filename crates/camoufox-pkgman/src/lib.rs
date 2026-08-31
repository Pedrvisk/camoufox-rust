//! # camoufox-pkgman
//!
//! Browser package management: GitHub release discovery, semantic version
//! constraints, download with progress, zip extraction and default addon
//! provisioning.

mod addons;
mod github;
mod install;
mod paths;
pub mod version;

pub use addons::{
    add_default_addons, confirm_paths, maybe_download_addons, DefaultAddon, DEFAULT_ADDONS,
};
pub use github::{github_authorization_headers, GitHubDownloader};
pub use install::{extract_zip, install, webdl, CamoufoxFetcher};
pub use paths::{
    camoufox_path, get_path, install_dir, launch_path, local_data_dir, os_arch_matrix,
    platform_arch, set_install_dir, INSTALL_DIR_ENV,
};
pub use version::{installed_ver_str, CamoufoxVersion, Constraints as CONSTRAINTS};


/// Re-exports from the domain layer for caller convenience.
pub use camoufox_core::os::OsName;
