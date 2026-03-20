use const_format::concatcp;
use rsnano_types::{NetworkType, currency_constants::WORKING_PATH_PREFIX};
use std::path::PathBuf;
use uuid::Uuid;

pub fn working_path_for(network: NetworkType) -> Option<PathBuf> {
    if let Ok(path_override) = std::env::var("NANO_APP_PATH") {
        eprintln!(
            "Application path overridden by NANO_APP_PATH environment variable: {path_override}"
        );
        return Some(path_override.into());
    }

    dirs::home_dir().and_then(|mut path| {
        let subdir = match network {
            NetworkType::Invalid => return None,
            NetworkType::NanoDevNetwork => concatcp!(WORKING_PATH_PREFIX, "Dev"),
            NetworkType::NanoBetaNetwork => concatcp!(WORKING_PATH_PREFIX, "Beta"),
            NetworkType::NanoLiveNetwork => WORKING_PATH_PREFIX,
            NetworkType::NanoTestNetwork => concatcp!(WORKING_PATH_PREFIX, "Test"),
        };
        path.push(subdir);
        Some(path)
    })
}

pub fn unique_path() -> Option<PathBuf> {
    unique_path_for(NetworkType::NanoDevNetwork)
}

fn unique_path_for(network: NetworkType) -> Option<PathBuf> {
    working_path_for(network).map(|mut path| {
        let uuid = Uuid::new_v4();
        path.push(uuid.to_string());
        std::fs::create_dir_all(&path).unwrap();
        path
    })
}
