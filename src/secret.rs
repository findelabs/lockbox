use axum::body::Bytes;
use chrono::{Duration, Utc};
use hyper::header::{CONTENT_TYPE, USER_AGENT};
use hyper::HeaderMap;
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use uuid::Uuid;

use crate::error::Error as RestError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Secret {
    pub id: String,
    pub active: bool,
    pub meta: Meta,
    pub lifecycle: Lifecycle,
    pub facts: Facts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub content_type: String,
    pub user_agent: Option<String>,
    pub x_forwarded_for: Option<String>,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lifecycle {
    pub max: LifecycleMax,
    pub current: LifecycleCurrent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleMax {
    pub reads: i64,
    pub seconds: i64,
    pub expires: bson::DateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleCurrent {
    pub reads: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Facts {
    //    owner: String,
    //    recipients: Vec<String>,
    pub pwd: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SecretPlusData {
    pub secret: Secret,
    pub key: String,
    pub value: Vec<u8>,
}

impl Secret {
    pub fn create(
        value: Bytes,
        expire_reads: Option<i64>,
        expire_seconds: Option<i64>,
        pwd: Option<&String>,
        headers: HeaderMap,
    ) -> Result<SecretPlusData, RestError> {
        let id = Uuid::new_v4().to_string();
        log::debug!("Sealing up data as {}", &id);

        // Generate random encryption key
        let key = Alphanumeric.sample_string(&mut rand::thread_rng(), 32);
        let secret_key = orion::aead::SecretKey::from_slice(key.as_bytes())?;

        // Detect binary mime-type, fallback on content-type header
        let content_type = match infer::get(&value) {
            Some(t) => {
                let mime_type = t.mime_type().to_owned();
                log::debug!("\"Detected mime type as {}\"", &mime_type);
                mime_type
            }
            None => match headers.get(CONTENT_TYPE) {
                Some(h) => h.to_str().unwrap_or("error").to_owned(),
                None => "none".to_owned(),
            },
        };

        // Encrypt data with key
        let ciphertext = match orion::aead::seal(&secret_key, &value) {
            Ok(e) => e,
            Err(e) => {
                log::error!("Error encrypting secret: {}", e);
                return Err(RestError::CryptoError(e));
            }
        };

        // Get payload size
        let bytes = ciphertext.len();

        // If neither expiration reads nor seconds is specified, then read expiration should default to one
        let expire_reads = if let Some(expire_reads) = expire_reads {
            expire_reads
        } else if expire_seconds.is_none() {
            1
        } else {
            -1
        };

        // Ensure max expire_seconds is less than a month
        let expire_seconds = match expire_seconds {
            Some(v) => {
                if v > 2592000i64 {
                    log::warn!("Incorrect expire_seconds requested, defaulting to 2,592,000");
                    2592000i64
                } else {
                    v
                }
            }
            None => {
                log::debug!("No expiration set, defaulting to one hour");
                3600
            }
        };

        // Secret expiration is now + expiration seconds
        let expires_at = Utc::now() + Duration::seconds(expire_seconds);

        // Hash password if one was provided
        let pwd = match pwd {
            Some(p) => {
                let mut hasher = DefaultHasher::new();
                p.hash(&mut hasher);
                Some(hasher.finish() as i64)
            }
            None => None,
        };

        // Get x-forwarded-for header
        let x_forwarded_for = headers
            .get("x-forwarded-for")
            .map(|s| s.to_str().unwrap_or("error").to_string());

        // Get user-agent header
        let user_agent = headers
            .get(USER_AGENT)
            .map(|s| s.to_str().unwrap_or("error").to_string());

        let secret = Secret {
            id,
            active: true,
            meta: Meta {
                content_type,
                bytes,
                x_forwarded_for,
                user_agent,
            },
            lifecycle: Lifecycle {
                max: LifecycleMax {
                    reads: expire_reads,
                    seconds: expire_seconds,
                    expires: expires_at.into(),
                },
                current: LifecycleCurrent { reads: 0i64 },
            },
            facts: Facts {
                // submitter,
                // recipients,
                pwd,
            },
        };

        Ok(SecretPlusData {
            secret,
            key,
            value: ciphertext,
        })
    }
}
