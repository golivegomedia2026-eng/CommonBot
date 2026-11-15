//! 发行用的小工具：生成升级签名用的密钥对、给版本策略文件签名。不随产品发布。
//!
//!   commonbot-sign keygen           生成密钥对：公钥写进 packaging/update.pub（进仓库），
//!                                   私钥打印出来，放进 CI 的 secret（COMMONBOT_SIGNING_KEY）并另行备份
//!   commonbot-sign sign <文件>      用环境变量 COMMONBOT_SIGNING_KEY 里的私钥签名，生成 <文件>.sig

use std::io::Cursor;

use base64::Engine as _;
use minisign::{KeyPair, PublicKey, SecretKey};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen"] => keygen(),
        ["sign", file] => sign(file),
        _ => Err("usage: commonbot-sign keygen | commonbot-sign sign <file>".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn keygen() -> Result<(), String> {
    let kp = KeyPair::generate_unencrypted_keypair().map_err(|e| e.to_string())?;
    // 私钥不设密码（CI 里没法输入），整段以 base64 保存；它的安全全靠 secret 的保管
    let secret = B64.encode(kp.sk.to_bytes());
    println!("Public key (commit this as packaging/update.pub):\n{}\n", kp.pk.to_base64());
    println!("Secret key (store as the CI secret COMMONBOT_SIGNING_KEY and keep a backup; never commit it):\n{secret}");
    Ok(())
}

fn sign(file: &str) -> Result<(), String> {
    let key = std::env::var("COMMONBOT_SIGNING_KEY").map_err(|_| "COMMONBOT_SIGNING_KEY is not set".to_string())?;
    let bytes = B64.decode(key.trim()).map_err(|e| format!("bad signing key: {e}"))?;
    let sk = SecretKey::from_bytes(&bytes).map_err(|e| format!("bad signing key: {e}"))?;
    let pk = PublicKey::from_secret_key(&sk).map_err(|e| e.to_string())?;
    let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let sig = minisign::sign(Some(&pk), &sk, Cursor::new(&data), None, None).map_err(|e| e.to_string())?;
    std::fs::write(format!("{file}.sig"), sig.into_string()).map_err(|e| e.to_string())?;
    println!("signed {file} (public key {})", pk.to_base64());
    Ok(())
}
