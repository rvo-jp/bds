//! The server identity belongs to durable data, never to a disposable engine copy.
use crate::util;
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{rand::SystemRandom, signature};
use std::{fs, path::Path};

pub(crate) const KEY: &str = "server_identity_key.pem";

/// Move a previously saved engine-local identity before linking the canonical directory.
pub(crate) fn adopt(data: &Path, work: &Path) -> Result<()> {
    let source = work.join("keys");
    match fs::symlink_metadata(&source) {
        Ok(m) if m.is_dir() => {
            util::files(&source)?;
            if fs::read_dir(&source)?.next().is_none() {
                fs::remove_dir(&source)?;
                return Ok(());
            }
            let target = data.join("keys");
            ensure!(
                !target.try_exists()? || fs::read_dir(&target)?.next().is_none(),
                "識別鍵の保存先が競合しています。data/keysと実行領域のkeysを確認してください"
            );
            util::sync_tree(&source)?;
            fs::rename(&source, &target)?;
            fs::File::open(data)?.sync_all()?;
            fs::File::open(work)?.sync_all()?;
        }
        Ok(_) => {} // The managed link is checked by releases::prepare.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Called with BDS stopped and the operation lock held. Existing keys are not rotated.
pub(crate) fn prepare(data: &Path) -> Result<()> {
    let keys = data.join("keys");
    util::chmod(&keys, 0o700)?;
    let key = keys.join(KEY);
    match fs::symlink_metadata(&key) {
        Ok(m) => {
            ensure!(m.is_file() && m.len() > 0, "サーバー識別鍵が不正です");
            util::chmod(&key, 0o600)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // ring and base64 are also used by HTTPS; no extra process or crypto provider.
            let der = signature::EcdsaKeyPair::generate_pkcs8(
                &signature::ECDSA_P384_SHA384_ASN1_SIGNING,
                &SystemRandom::new(),
            )
            .map_err(|_| anyhow::anyhow!("サーバー識別鍵を生成できません"))?;
            let encoded = STANDARD.encode(der.as_ref());
            let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
            for line in encoded.as_bytes().chunks(64) {
                pem.push_str(std::str::from_utf8(line)?);
                pem.push('\n');
            }
            pem.push_str("-----END PRIVATE KEY-----\n");
            util::atomic(&key, pem.as_bytes(), 0o600)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
