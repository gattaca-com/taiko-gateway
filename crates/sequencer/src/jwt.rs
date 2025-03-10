use std::{fs, io::Read, path::PathBuf};

use alloy_primitives::hex;
use eyre::{bail, ensure, eyre, Result};
use jsonwebtoken::{encode, EncodingKey, Header};
use pc_common::utils::utcnow_sec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    iat: usize,
    iss: String,
}

pub fn generate_jwt(secret: Vec<u8>) -> Result<String, jsonwebtoken::errors::Error> {
    let now = utcnow_sec();
    let claims = Claims {
        sub: "soft-block".to_string(),
        exp: (now + 36000) as usize,
        iat: now as usize,
        iss: "taiko-gateway".to_string(),
    };

    let key = EncodingKey::from_secret(&secret);
    encode(&Header::default(), &claims, &key)
}

// parse_secret_from_file matches the Golang API method of the same name, ensuring the same
// validation rules.
pub fn parse_secret_from_file(jwt_secret_file: PathBuf) -> Result<Vec<u8>> {
    if jwt_secret_file.as_os_str().is_empty() {
        bail!("JWT secret file path is empty");
    }

    // Read file contents
    let mut file = fs::File::open(&jwt_secret_file).map_err(|e| {
        eyre!("Failed to open JWT secret file {}: {}", jwt_secret_file.to_string_lossy(), e)
    })?;
    let mut enc = String::new();
    file.read_to_string(&mut enc)?;

    // Decode hex to bytes
    let secret =
        hex::decode(enc.trim()).map_err(|e| eyre!("Failed to decode JWT secret as hex: {}", e))?;

    ensure!(secret.len() >= 32, "JWT secret should be at least 32 bytes");

    Ok(secret)
}
