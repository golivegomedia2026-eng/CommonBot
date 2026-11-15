//! 升级检查。每个发行版附带一份签名的「版本策略」：最新版本是多少、最低必须是多少。
//! 程序里只有公钥；策略验签通过才采信。检查失败（没网、被篡改）时绝不强制升级。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::store::{get_setting, set_setting};
use crate::Result;

/// 版本策略文件的内容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Policy {
    /// 最新版本。
    pub version: String,
    /// 最低可用版本：低于它的必须升级。
    pub minimum: String,
    pub notes: String,
    pub pub_date: String,
    /// 下载页面。
    pub url: String,
}

/// 把 `1.2.3` 这样的版本号拆成可比较的三段。带后缀（如 `-beta`）的只看前面的数字。
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let (a, b, c) = (parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?);
    parts.next().is_none().then_some((a, b, c))
}

fn newer(a: &str, b: &str) -> bool {
    matches!((parse_version(a), parse_version(b)), (Some(x), Some(y)) if x > y)
}

/// 验签并解析策略。签名不对、内容不合理（最低版本比最新版本还高）都不采信。
pub fn verify(policy_json: &[u8], signature: &str, public_key: &str) -> Option<Policy> {
    let pk = minisign_verify::PublicKey::from_base64(public_key.trim()).ok()?;
    let sig = minisign_verify::Signature::decode(signature).ok()?;
    pk.verify(policy_json, &sig, false).ok()?;
    let policy: Policy = serde_json::from_slice(policy_json).ok()?;
    let (latest, minimum) = (parse_version(&policy.version)?, parse_version(&policy.minimum)?);
    (minimum <= latest).then_some(policy)
}

/// 界面看到的升级状态。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// `disabled`（这个构建不带升级公钥）/ `unknown`（还没查到）/ `up_to_date` / `optional` / `required`
    pub state: &'static str,
    pub current: String,
    pub latest: Option<String>,
    pub minimum: Option<String>,
    pub notes: String,
    pub url: String,
}

/// 根据已验证的策略判断当前版本的状态。
pub fn status(current: &str, policy: Option<&Policy>, enabled: bool) -> Status {
    let blank = |state| Status {
        state,
        current: current.to_string(),
        latest: None,
        minimum: None,
        notes: String::new(),
        url: String::new(),
    };
    if !enabled {
        return blank("disabled");
    }
    let Some(p) = policy else { return blank("unknown") };
    let state = if newer(&p.minimum, current) {
        "required"
    } else if newer(&p.version, current) {
        "optional"
    } else {
        "up_to_date"
    };
    Status {
        state,
        current: current.to_string(),
        latest: Some(p.version.clone()),
        minimum: Some(p.minimum.clone()),
        notes: p.notes.clone(),
        url: p.url.clone(),
    }
}

/// 上次验证通过的策略。
pub fn cached(c: &Connection) -> Result<Option<Policy>> {
    Ok(get_setting(c, "update.policy")?.and_then(|s| serde_json::from_str(&s).ok()))
}

/// 保存新验证通过的策略。版本号只进不退：旧的镜像或缓存不能把最低版本降回去。
pub fn remember(c: &Connection, fresh: Policy) -> Result<Policy> {
    let merged = match cached(c)? {
        Some(old) => Policy {
            minimum: if newer(&old.minimum, &fresh.minimum) { old.minimum.clone() } else { fresh.minimum.clone() },
            ..if newer(&old.version, &fresh.version) { old.clone() } else { fresh.clone() }
        },
        None => fresh,
    };
    set_setting(c, "update.policy", &serde_json::to_string(&merged).unwrap_or_default())?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use std::io::Cursor;

    fn signed(policy: &Policy) -> (Vec<u8>, String, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let json = serde_json::to_vec(policy).unwrap();
        let sig = minisign::sign(Some(&kp.pk), &kp.sk, Cursor::new(&json), None, None).unwrap().into_string();
        (json, sig, kp.pk.to_base64())
    }

    fn policy(version: &str, minimum: &str) -> Policy {
        Policy { version: version.into(), minimum: minimum.into(), ..Default::default() }
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(!newer("1.2.3", "1.2.3"));
        assert!(!newer("1.2", "1.0.0"), "malformed versions never count as newer");
        assert_eq!(parse_version("1.2.3-beta+5"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3.4"), None);
    }

    #[test]
    fn accepts_only_a_correctly_signed_and_sane_policy() {
        let p = policy("0.3.0", "0.2.0");
        let (json, sig, pk) = signed(&p);
        assert_eq!(verify(&json, &sig, &pk), Some(p.clone()));

        // 内容被改
        let tampered = String::from_utf8(json.clone()).unwrap().replace("0.2.0", "0.3.0");
        assert_eq!(verify(tampered.as_bytes(), &sig, &pk), None);
        // 别人的密钥签的
        let (_, _, other_pk) = signed(&p);
        assert_eq!(verify(&json, &sig, &other_pk), None);
        // 签名或公钥是乱码
        assert_eq!(verify(&json, "garbage", &pk), None);
        assert_eq!(verify(&json, &sig, "garbage"), None);
        // 最低版本比最新版本还高：不采信
        let (json, sig, pk) = signed(&policy("0.2.0", "0.3.0"));
        assert_eq!(verify(&json, &sig, &pk), None);
    }

    #[test]
    fn decides_the_state() {
        let p = policy("0.3.0", "0.2.0");
        assert_eq!(status("0.1.0", Some(&p), true).state, "required");
        assert_eq!(status("0.2.0", Some(&p), true).state, "optional");
        assert_eq!(status("0.3.0", Some(&p), true).state, "up_to_date");
        assert_eq!(status("0.4.0", Some(&p), true).state, "up_to_date");
        // 没查到（没网等）：不强制
        assert_eq!(status("0.1.0", None, true).state, "unknown");
        assert_eq!(status("0.1.0", Some(&p), false).state, "disabled");
    }

    #[tokio::test]
    async fn remembered_policy_never_goes_backwards() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            assert_eq!(cached(c)?, None);
            remember(c, policy("0.3.0", "0.2.0"))?;
            // 之后拿到一份旧的策略（例如滞后的镜像）：最新版本和最低版本都不降
            let kept = remember(c, policy("0.2.0", "0.1.0"))?;
            assert_eq!((kept.version.as_str(), kept.minimum.as_str()), ("0.3.0", "0.2.0"));
            // 更新的策略照常生效
            let up = remember(c, policy("0.4.0", "0.4.0"))?;
            assert_eq!((up.version.as_str(), up.minimum.as_str()), ("0.4.0", "0.4.0"));
            assert_eq!(cached(c)?, Some(up));
            Ok(())
        })
        .await
        .unwrap();
    }
}
