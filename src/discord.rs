//! Discord HTTP interactions. No gateway connection or stored Bot Token.
use crate::{config::Config, operations, util};
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io::Read};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub user: String,
    pub kind: String,
    pub phase: String,
    pub created: u64,
    pub boot: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub id: String,
    pub phase: String,
    pub boot: String,
    pub message: String,
}
fn job_path(c: &Config) -> std::path::PathBuf {
    c.paths.discord_state.join("job.json")
}
fn outcome_path(c: &Config) -> std::path::PathBuf {
    c.state().join("discord-outcome.json")
}
fn save(c: &Config, j: &Job) -> Result<()> {
    util::json(&job_path(c), j, 0o600)
}
fn message(text: &str) -> Value {
    json!({"type":4,"data":{"content":text,"flags":64,"allowed_mentions":{"parse":[]}}})
}
pub fn authorized(c: &Config, v: &Value) -> bool {
    v["application_id"].as_str() == Some(&c.discord.application_id)
        && v["guild_id"].as_str() == Some(&c.discord.guild_id)
        && (v["member"]["user"]["id"]
            .as_str()
            .is_some_and(|id| c.discord.allowed_users.iter().any(|x| x == id))
            || v["member"]["roles"].as_array().is_some_and(|roles| {
                roles.iter().any(|r| {
                    r.as_str()
                        .is_some_and(|r| c.discord.allowed_roles.iter().any(|x| x == r))
                })
            }))
}
pub fn signed(key: &str, timestamp: &str, signature: &str, body: &[u8], now: u64) -> bool {
    let Ok(time) = timestamp.parse::<u64>() else {
        return false;
    };
    if now.abs_diff(time) > 300 {
        return false;
    }
    let Ok(key) = hex::decode(key) else {
        return false;
    };
    let Ok(bytes) = <[u8; 32]>::try_from(key) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&bytes) else {
        return false;
    };
    let Ok(sig) = hex::decode(signature) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&sig) else {
        return false;
    };
    let mut content = timestamp.as_bytes().to_vec();
    content.extend_from_slice(body);
    key.verify_strict(&content, &sig).is_ok()
}
fn pending(c: &Config) -> Option<Job> {
    util::read_json(&job_path(c)).ok()
}
fn busy(c: &Config, j: &Job) -> bool {
    if j.phase == "confirm" {
        return util::now().saturating_sub(j.created) < 60;
    }
    if j.phase != "dispatched" {
        return false;
    }
    if let Ok(o) = util::read_json::<Outcome>(&outcome_path(c))
        && o.id == j.id
        && matches!(o.phase.as_str(), "done" | "failed")
    {
        return false;
    }
    // A reboot that changed the boot ID is complete. Never submit an uncertain job again.
    if j.kind == "reboot" && j.boot != util::boot() {
        return false;
    }
    true
}
pub fn handle(c: &Config, v: &Value) -> Result<(Value, Option<Job>)> {
    if v["type"] == 1 {
        return Ok((json!({"type":1}), None));
    }
    if !authorized(c, v) {
        return Ok((message("この操作を実行する権限がありません。"), None));
    }
    if crate::admin::in_maintenance(c) {
        return Ok((
            message("管理者が保守作業中です。終了まで再起動できません。"),
            None,
        ));
    }
    let user = v["member"]["user"]["id"]
        .as_str()
        .context("ユーザーIDがありません")?;
    if v["type"] == 2 {
        let kind = v["data"]["name"].as_str().unwrap_or("");
        if !matches!(kind, "restart" | "reboot") {
            return Ok((message("未対応の操作です。"), None));
        }
        if let Some(j) = pending(c) {
            if busy(c, &j) {
                return Ok((
                    message(
                        "別の確認または操作が進行中です。結果不明の操作は管理者が状態を確認してください。",
                    ),
                    None,
                ));
            }
            if j.phase == "dispatched"
                && util::now().saturating_sub(j.created) < c.discord.cooldown_seconds
            {
                return Ok((
                    message("連続操作を防ぐため、しばらく待ってから実行してください。"),
                    None,
                ));
            }
        }
        let j = Job {
            id: util::nonce()?,
            user: user.into(),
            kind: kind.into(),
            phase: "confirm".into(),
            created: util::now(),
            boot: util::boot(),
        };
        save(c, &j)?;
        let label = if kind == "restart" {
            "Minecraftを再起動"
        } else {
            "OSを再起動"
        };
        let data = json!({"content":format!("{label}しますか？ 確認は60秒間有効です。"),"flags":64,"allowed_mentions":{"parse":[]},"components":[{"type":1,"components":[{"type":2,"style":4,"label":"実行","custom_id":format!("bds:{}:yes",j.id)},{"type":2,"style":2,"label":"キャンセル","custom_id":format!("bds:{}:no",j.id)}]}]});
        return Ok((json!({"type":4,"data":data}), None));
    }
    if v["type"] == 3 {
        let Some(mut j) = pending(c) else {
            return Ok((message("確認が見つかりません。"), None));
        };
        let custom = v["data"]["custom_id"].as_str().unwrap_or("");
        let parts: Vec<_> = custom.split(':').collect();
        if parts.len() != 3
            || parts[0] != "bds"
            || parts[1] != j.id
            || j.user != user
            || j.phase != "confirm"
            || util::now().saturating_sub(j.created) >= 60
            || j.boot != util::boot()
        {
            return Ok((
                message("確認の有効期限が切れているか、実行者が異なります。"),
                None,
            ));
        }
        if !matches!(parts[2], "yes" | "no") {
            return Ok((message("確認内容が不正です。"), None));
        }
        j.phase = if parts[2] == "yes" {
            "dispatched"
        } else {
            "cancelled"
        }
        .into();
        j.created = util::now();
        save(c, &j)?;
        let launch = (j.phase == "dispatched").then_some(j);
        return Ok((
            json!({"type":7,"data":{"content":if launch.is_some(){"操作を受け付けました。ゲーム内で予告してから実行します。結果は通知チャンネルを確認してください。"}else{"キャンセルしました。"},"components":[],"allowed_mentions":{"parse":[]}}}),
            launch,
        ));
    }
    Ok((message("未対応の操作です。"), None))
}
pub fn serve(c: &Config) -> Result<()> {
    ensure!(c.discord.enabled, "Discord受付が無効です");
    fs::create_dir_all(&c.paths.discord_state)?;
    let _lock = util::Lock::acquire(&c.paths.discord_state.join("receiver.lock"), 0)?;
    let listener = std::net::TcpListener::bind(&c.discord.listen)?;
    supervisor_notify()?;
    let config = std::sync::Arc::new(c.clone());
    let jobs = std::sync::Arc::new(std::sync::Mutex::new(()));
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        use std::sync::atomic::Ordering;
        if connections.load(Ordering::Acquire) >= 4 {
            crate::http::reject(&mut stream, 503);
            continue;
        }
        connections.fetch_add(1, Ordering::AcqRel);
        let permit = ConnectionPermit(connections.clone());
        let config = config.clone();
        let jobs = jobs.clone();
        let accepted = std::time::Instant::now();
        if std::thread::Builder::new()
            .name("interaction".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let _permit = permit;
                if let Err(code) = receive(&config, &jobs, &mut stream, accepted) {
                    crate::http::reject(&mut stream, code);
                }
            })
            .is_err()
        {
            eprintln!("Discord受付の処理を開始できませんでした");
        }
    }
    Ok(())
}
struct ConnectionPermit(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
fn receive(
    c: &Config,
    jobs: &std::sync::Mutex<()>,
    stream: &mut std::net::TcpStream,
    accepted: std::time::Instant,
) -> std::result::Result<(), u16> {
    let request = crate::http::read(stream, accepted + std::time::Duration::from_secs(2))?;
    if !signed(
        &c.discord.public_key,
        &request.timestamp,
        &request.signature,
        &request.body,
        util::now(),
    ) {
        return Err(401);
    }
    let value = serde_json::from_slice::<Value>(&request.body).map_err(|_| 400u16)?;
    // Network reads are parallel, but confirmation/job changes remain serial.
    let (response, launch) = {
        let _guard = jobs.lock().map_err(|_| 500u16)?;
        handle(c, &value).map_err(|_| 500u16)?
    };
    // Dispatch an accepted operation even if its acknowledgement cannot be delivered.
    let _ = crate::http::respond(stream, 200, response.to_string().as_bytes());
    if let Some(j) = launch {
        let unit = format!("bds-discord-{}.service", j.kind);
        if util::systemctl(&["--no-block", "start", &unit]).is_err() {
            let _ = util::webhook(
                &c.paths.webhook_file,
                "Discord操作を開始できませんでした。権限設定とサービス状態を確認してください。",
            );
        }
    }
    Ok(())
}

fn supervisor_notify() -> Result<()> {
    crate::supervisor::notify("READY=1\nSTATUS=Discord署名検証付き受付を開始しました")
}
pub fn execute(c: &Config, kind: &str) -> Result<()> {
    util::root()?;
    ensure!(matches!(kind, "restart" | "reboot"), "操作が不正です");
    let _lock = util::Lock::acquire(&c.state().join("discord-job.lock"), 0)?;
    let j: Job = util::read_json(&job_path(c))?;
    ensure!(
        j.phase == "dispatched"
            && j.kind == kind
            && j.boot == util::boot()
            && util::now().saturating_sub(j.created) <= 120,
        "実行待ちの操作がありません"
    );
    let previous = outcome_path(c);
    if previous.exists() {
        let o: Outcome =
            util::read_json(&previous).context("操作結果の記録が不正です。再実行しません")?;
        ensure!(o.id != j.id, "この操作は既に受け付けています");
    }
    let mut o = Outcome {
        id: j.id,
        phase: "running".into(),
        boot: util::boot(),
        message: String::new(),
    };
    util::json(&outcome_path(c), &o, 0o644)?;
    let result = if kind == "restart" {
        operations::restart(c)
    } else {
        operations::reboot(c)
    };
    o.phase = if result.is_ok() {
        if kind == "reboot" {
            "reboot-requested"
        } else {
            "done"
        }
    } else {
        "failed"
    }
    .into();
    o.message = if result.is_ok() {
        if kind == "reboot" {
            "OS再起動を要求しました。"
        } else {
            "Minecraftの再起動が完了しました。"
        }
    } else {
        "操作に失敗しました。サーバーの状態を確認してください。"
    }
    .into();
    util::json(&outcome_path(c), &o, 0o644)?;
    let _ = util::webhook(&c.paths.webhook_file, &o.message);
    result
}
pub fn register(c: &Config) -> Result<()> {
    ensure!(c.discord.enabled, "Discord設定が必要です");
    let mut token = String::new();
    std::io::stdin().take(1024).read_to_string(&mut token)?;
    let token = token.trim();
    ensure!(
        !token.is_empty() && !token.contains(['\r', '\n']),
        "Bot Tokenを標準入力に渡してください"
    );
    let url = format!(
        "https://discord.com/api/v10/applications/{}/guilds/{}/commands",
        c.discord.application_id, c.discord.guild_id
    );
    let commands = json!([{"name":"restart","description":"Minecraftサーバーを再起動します","type":1,"default_member_permissions":null,"dm_permission":false},{"name":"reboot","description":"OSを再起動します","type":1,"default_member_permissions":null,"dm_permission":false}]);
    ensure!(
        util::https_agent(15)
            .put(&url)
            .header("Authorization", format!("Bot {token}"))
            .send_json(commands)
            .is_ok(),
        "コマンド登録に失敗しました。Token・アプリ・サーバーIDを確認してください"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn signatures() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let body = b"{\"type\":1}";
        let t = "1000";
        let mut m = t.as_bytes().to_vec();
        m.extend_from_slice(body);
        let signature = hex::encode(key.sign(&m).to_bytes());
        let public = hex::encode(key.verifying_key().to_bytes());
        assert!(signed(&public, t, &signature, body, 1000));
        assert!(!signed(&public, t, &signature, b"changed", 1000));
        assert!(!signed(&public, t, &signature, body, 1301));
    }
    #[test]
    fn authorization() {
        let mut c = Config::default();
        c.discord.application_id = "1".into();
        c.discord.guild_id = "2".into();
        c.discord.allowed_roles = vec!["3".into()];
        let mut v =
            json!({"application_id":"1","guild_id":"2","member":{"user":{"id":"4"},"roles":["3"]}});
        assert!(authorized(&c, &v));
        v["guild_id"] = json!("9");
        assert!(!authorized(&c, &v));
    }
    #[test]
    fn signed_ping_is_served_over_http() {
        use std::{
            io::Write,
            net::{TcpListener, TcpStream},
            sync::Mutex,
            time::Instant,
        };
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut c = Config::default();
        c.discord.public_key = hex::encode(key.verifying_key().to_bytes());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap().0;
            receive(&c, &Mutex::new(()), &mut stream, Instant::now()).unwrap();
        });
        let body = b"{\"type\":1}";
        let timestamp = util::now().to_string();
        let mut content = timestamp.as_bytes().to_vec();
        content.extend_from_slice(body);
        let signature = hex::encode(key.sign(&content).to_bytes());
        write!(client, "POST /interactions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nX-Signature-Timestamp: {timestamp}\r\nX-Signature-Ed25519: {signature}\r\n\r\n", body.len()).unwrap();
        client.write_all(body).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "{\"type\":1}");
    }
}
