use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use std::fs;
use std::io::Read;
use alloy_primitives::hex;
use eyre::{eyre, Result};

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    iat: usize,
    iss: String,
}

pub fn generate_jwt(secret: Vec<u8>) -> Result<String, jsonwebtoken::errors::Error> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
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
pub fn parse_secret_from_file(jwt_secret_file: &str) -> Result<Vec<u8>> {
    if jwt_secret_file.is_empty() {
        return Err(eyre!("JWT secret file path is empty"));
    }

    // Read file contents
    let mut file = fs::File::open(jwt_secret_file)
        .map_err(|e| eyre!("Failed to open JWT secret file {}: {}", jwt_secret_file, e))?;
    let mut enc = String::new();
    file.read_to_string(&mut enc)?;

    // Trim whitespace and remove "0x" prefix if present
    let str_data = enc.trim();
    let clean_secret = str_data.trim_start_matches("0x");

    // Decode hex to bytes
    let secret = hex::decode(clean_secret)
        .map_err(|e| eyre!("Failed to decode JWT secret as hex: {}", e))?;

    if secret.len() < 32 {
        return Err(eyre!("JWT secret should be at least 32 bytes"));
    }

    Ok(secret)
}
