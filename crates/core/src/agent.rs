//! 智能体：一份定义数据。系统提示词由定义生成，不单独保存，改定义即生效。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{new_id, now_ms, Error, Result};

/// 首次安装时自带的智能体；P0 阶段留下的对话也归到它名下。
pub const GENERAL_AGENT_ID: &str = "general";
/// 建造师：内置，不出现在列表里，不能改也不能删。
pub const BUILDER_AGENT_ID: &str = "builder";

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// 一句话简介，显示在列表里。
    pub description: String,
    /// 负责的任务。
    pub duty: String,
    /// 不负责的范围。
    pub out_of_scope: String,
    /// 其他要求：语气、格式等。
    pub instructions: String,
    /// 开场白：新对话里先显示的一句话，不发给模型。
    pub opening: String,
    /// 示例问题，点一下就能问。
    pub examples: Vec<String>,
    /// 固定使用的推理服务和模型；都为空表示用对话时选的。
    pub provider_id: Option<String>,
    pub model: Option<String>,
    /// 默认查哪些知识库文件夹（含子文件夹）。空表示不查知识库。
    pub folders: Vec<String>,
    /// 联网：`off` 不联网；`fallback` 知识库里找不到时联网；`always` 每次都联网。
    pub web: String,
    /// 本机模型连不上时，是否允许改用云端模型。
    pub allow_cloud: bool,
    /// 独立链接的凭据（出现在链接里）和 API 密钥；都为空表示没发布。
    pub public_token: Option<String>,
    pub api_key: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 列表里的一项，附带使用情况。
#[derive(Debug, Clone, Serialize)]
pub struct AgentSummary {
    #[serde(flatten)]
    pub agent: Agent,
    pub conversations: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentInput {
    pub name: String,
    pub description: String,
    pub duty: String,
    pub out_of_scope: String,
    pub instructions: String,
    pub opening: String,
    pub examples: Vec<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub folders: Vec<String>,
    pub web: String,
    pub allow_cloud: bool,
}

const MAX_EXAMPLES: usize = 6;

fn text(field: &'static str, value: &str, max: usize) -> Result<String> {
    let v = value.trim();
    if v.chars().count() > max {
        return Err(Error::Invalid(field.into()));
    }
    Ok(v.to_string())
}

impl AgentInput {
    pub fn validated(self) -> Result<Self> {
        let name = text("name", &self.name, 40)?;
        if name.is_empty() {
            return Err(Error::Invalid("name".into()));
        }
        let examples: Vec<String> = self
            .examples
            .iter()
            .map(|e| text("examples", e, 100))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| !e.is_empty())
            .collect();
        if examples.len() > MAX_EXAMPLES {
            return Err(Error::Invalid("examples".into()));
        }
        let provider_id = self.provider_id.filter(|p| !p.trim().is_empty());
        let model = self.model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
        // 要么都指定，要么都不指定
        if provider_id.is_some() != model.is_some() {
            return Err(Error::Invalid("model".into()));
        }
        Ok(Self {
            name,
            description: text("description", &self.description, 200)?,
            duty: text("duty", &self.duty, 4000)?,
            out_of_scope: text("out_of_scope", &self.out_of_scope, 4000)?,
            instructions: text("instructions", &self.instructions, 4000)?,
            opening: text("opening", &self.opening, 1000)?,
            examples,
            web: match self.web.as_str() {
                "" => "off".into(),
                "off" | "fallback" | "always" => self.web,
                _ => return Err(Error::Invalid("web".into())),
            },
            allow_cloud: self.allow_cloud,
            provider_id,
            model,
            folders: {
                if self.folders.len() > 50 {
                    return Err(Error::Invalid("folders".into()));
                }
                self.folders
            },
        })
    }
}

/// 由定义生成系统提示词。空的部分不写进去。
pub fn system_prompt(name: &str, description: &str, duty: &str, out_of_scope: &str, instructions: &str) -> String {
    let mut p = format!("你是「{name}」。");
    if !description.is_empty() {
        p.push_str(description);
    }
    if !duty.is_empty() {
        p.push_str("\n\n## 你负责的任务\n");
        p.push_str(duty);
    }
    if !out_of_scope.is_empty() {
        p.push_str("\n\n## 不在你负责范围内\n");
        p.push_str(out_of_scope);
        p.push_str("\n\n遇到上面这类问题时，明确告诉对方这不归你负责，不要尝试回答；可以建议对方找合适的人或渠道。");
    }
    if !duty.is_empty() {
        p.push_str("\n\n与你负责的任务无关的问题，同样说明不归你负责。");
    }
    if !instructions.is_empty() {
        p.push_str("\n\n## 其他要求\n");
        p.push_str(instructions);
    }
    p.push_str("\n\n用提问者使用的语言回答。不确定的内容直接说不确定，不要编造。");
    p
}

impl Agent {
    /// 用一份还没保存的定义临时构造一个智能体（试答、建造师用）。
    pub fn draft(id: &str, input: AgentInput) -> Self {
        Agent {
            id: id.to_string(),
            name: input.name,
            description: input.description,
            duty: input.duty,
            out_of_scope: input.out_of_scope,
            instructions: input.instructions,
            opening: input.opening,
            examples: input.examples,
            provider_id: input.provider_id,
            model: input.model,
            folders: input.folders,
            web: if input.web.is_empty() { "off".into() } else { input.web },
            allow_cloud: input.allow_cloud,
            public_token: None,
            api_key: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    pub fn system_prompt(&self) -> String {
        system_prompt(&self.name, &self.description, &self.duty, &self.out_of_scope, &self.instructions)
    }
}

impl AgentInput {
    pub fn system_prompt(&self) -> String {
        system_prompt(
            self.name.trim(),
            self.description.trim(),
            self.duty.trim(),
            self.out_of_scope.trim(),
            self.instructions.trim(),
        )
    }
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Agent> {
    let examples: String = r.get("examples")?;
    Ok(Agent {
        id: r.get("id")?,
        name: r.get("name")?,
        description: r.get("description")?,
        duty: r.get("duty")?,
        out_of_scope: r.get("out_of_scope")?,
        instructions: r.get("instructions")?,
        opening: r.get("opening")?,
        examples: serde_json::from_str(&examples).unwrap_or_default(),
        provider_id: r.get("provider_id")?,
        model: r.get("model")?,
        folders: serde_json::from_str(&r.get::<_, String>("folders")?).unwrap_or_default(),
        web: r.get("web")?,
        allow_cloud: r.get("allow_cloud")?,
        public_token: r.get("public_token")?,
        api_key: r.get("api_key")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

pub fn list(c: &Connection) -> Result<Vec<AgentSummary>> {
    let mut stmt = c.prepare(
        "SELECT a.*, count(v.id) AS conversations, max(v.updated_at) AS last_used_at
         FROM agents a LEFT JOIN conversations v ON v.agent_id = a.id AND v.title <> '' AND v.origin = 'owner'
         WHERE a.id <> 'builder'
         GROUP BY a.id ORDER BY coalesce(max(v.updated_at), a.updated_at) DESC, a.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(AgentSummary {
            agent: from_row(r)?,
            conversations: r.get("conversations")?,
            last_used_at: r.get("last_used_at")?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id: &str) -> Result<Agent> {
    c.query_row("SELECT * FROM agents WHERE id = ?1", [id], from_row).optional()?.ok_or(Error::NotFound("agent"))
}

fn json(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

pub fn create(c: &Connection, input: AgentInput) -> Result<Agent> {
    let i = input.validated()?;
    let (id, now) = (new_id(), now_ms());
    c.execute(
        "INSERT INTO agents(id, name, description, duty, out_of_scope, instructions, opening, examples, provider_id, model, folders, web, allow_cloud, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
        params![id, i.name, i.description, i.duty, i.out_of_scope, i.instructions, i.opening, json(&i.examples), i.provider_id, i.model, json(&i.folders), i.web, i.allow_cloud, now],
    )?;
    get(c, &id)
}

pub fn update(c: &Connection, id: &str, input: AgentInput) -> Result<Agent> {
    if id == BUILDER_AGENT_ID {
        return Err(Error::NotFound("agent"));
    }
    let i = input.validated()?;
    let changed = c.execute(
        "UPDATE agents SET name = ?2, description = ?3, duty = ?4, out_of_scope = ?5, instructions = ?6, opening = ?7, examples = ?8, provider_id = ?9, model = ?10, folders = ?11, web = ?12, allow_cloud = ?13, updated_at = ?14 WHERE id = ?1",
        params![id, i.name, i.description, i.duty, i.out_of_scope, i.instructions, i.opening, json(&i.examples), i.provider_id, i.model, json(&i.folders), i.web, i.allow_cloud, now_ms()],
    )?;
    if changed == 0 {
        return Err(Error::NotFound("agent"));
    }
    get(c, id)
}

/// 删除智能体，连同它名下的对话。
pub fn delete(c: &mut Connection, id: &str) -> Result<()> {
    if id == BUILDER_AGENT_ID {
        return Err(Error::NotFound("agent"));
    }
    let tx = c.transaction()?;
    tx.execute("DELETE FROM conversations WHERE agent_id = ?1", [id])?;
    let n = tx.execute("DELETE FROM agents WHERE id = ?1", [id])?;
    if n == 0 {
        return Err(Error::NotFound("agent"));
    }
    tx.commit()?;
    Ok(())
}

fn random_hex(len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::rngs::OsRng;
    (0..len).map(|_| char::from_digit(rng.gen_range(0..16), 16).unwrap_or('0')).collect()
}

/// 发布：生成独立链接的凭据和 API 密钥。发布出去的智能体没有人替它选模型，所以必须先固定模型。
/// 已经发布过的保持原来的链接和密钥不变。
pub fn publish(c: &Connection, id: &str) -> Result<Agent> {
    let a = get(c, id)?;
    if id == BUILDER_AGENT_ID {
        return Err(Error::NotFound("agent"));
    }
    if a.model.is_none() {
        return Err(Error::Invalid("publish_needs_model".into()));
    }
    if a.public_token.is_none() {
        c.execute(
            "UPDATE agents SET public_token = ?2, api_key = ?3 WHERE id = ?1",
            params![id, random_hex(32), format!("cb-{}", random_hex(40))],
        )?;
    }
    get(c, id)
}

/// 取消发布：链接和密钥立即失效。
pub fn unpublish(c: &Connection, id: &str) -> Result<Agent> {
    get(c, id)?;
    c.execute("UPDATE agents SET public_token = NULL, api_key = NULL WHERE id = ?1", [id])?;
    get(c, id)
}

/// 换一个 API 密钥（旧的立即失效），链接不变。
pub fn rotate_key(c: &Connection, id: &str) -> Result<Agent> {
    let a = get(c, id)?;
    if a.public_token.is_none() {
        return Err(Error::Invalid("not_published".into()));
    }
    c.execute("UPDATE agents SET api_key = ?2 WHERE id = ?1", params![id, format!("cb-{}", random_hex(40))])?;
    get(c, id)
}

pub fn by_token(c: &Connection, token: &str) -> Result<Agent> {
    c.query_row("SELECT * FROM agents WHERE public_token = ?1", [token], from_row)
        .optional()?
        .ok_or(Error::NotFound("agent"))
}

pub fn by_api_key(c: &Connection, key: &str) -> Result<Agent> {
    c.query_row("SELECT * FROM agents WHERE api_key = ?1", [key], from_row).optional()?.ok_or(Error::NotFound("agent"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store, Db};

    fn input(name: &str) -> AgentInput {
        AgentInput { name: name.into(), ..Default::default() }
    }

    #[test]
    fn prompt_includes_only_filled_parts() {
        let bare = system_prompt("通用助手", "", "", "", "");
        assert!(bare.starts_with("你是「通用助手」。"));
        assert!(!bare.contains("##"), "{bare}");

        let full = system_prompt("售后答疑", "回答售后问题。", "保修与退换货", "不报价", "语气礼貌");
        for part in [
            "回答售后问题。",
            "## 你负责的任务\n保修与退换货",
            "## 不在你负责范围内\n不报价",
            "不归你负责",
            "## 其他要求\n语气礼貌",
        ] {
            assert!(full.contains(part), "missing {part:?} in {full}");
        }
    }

    #[test]
    fn validates_input() {
        assert!(matches!(input("  ").validated(), Err(Error::Invalid(f)) if f == "name"));
        assert!(input(&"长".repeat(41)).validated().is_err());
        let mut many = input("a");
        many.examples = (0..7).map(|i| format!("q{i}")).collect();
        assert!(many.validated().is_err());
        let mut half = input("a");
        half.model = Some("m".into());
        assert!(matches!(half.validated(), Err(Error::Invalid(f)) if f == "model"));
        let mut ok = input(" 名字 ");
        ok.examples = vec![" 问题一 ".into(), "  ".into()];
        let ok = ok.validated().unwrap();
        assert_eq!((ok.name.as_str(), ok.examples), ("名字", vec!["问题一".to_string()]));
    }

    #[tokio::test]
    async fn crud_usage_counts_and_cascade() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            // 自带一个通用助手；内置的建造师不出现在列表里，也不能改、不能删
            assert_eq!(list(c)?.len(), 1);
            assert!(update(c, BUILDER_AGENT_ID, input("x")).is_err());
            assert!(delete(c, BUILDER_AGENT_ID).is_err());
            assert!(get(c, BUILDER_AGENT_ID).is_ok());
            assert_eq!(get(c, GENERAL_AGENT_ID)?.name, "通用助手");

            let a = create(c, input("售后答疑"))?;
            let conv = store::create_conversation(c, &a.id)?;
            // 还没说过话的空对话不计数
            assert_eq!(list(c)?.iter().find(|s| s.agent.id == a.id).unwrap().conversations, 0);
            store::add_user_message(c, &conv.id, "你好", "p", "m")?;
            let listed = list(c)?;
            assert_eq!(listed[0].agent.id, a.id, "most recently used first");
            assert_eq!(listed[0].conversations, 1);

            let mut edit = input("售后答疑二");
            edit.duty = "保修".into();
            assert_eq!(update(c, &a.id, edit)?.duty, "保修");
            assert!(matches!(update(c, "nope", input("x")), Err(Error::NotFound(_))));

            delete(c, &a.id)?;
            assert!(matches!(store::get_conversation(c, &conv.id), Err(Error::NotFound(_))));
            let left: i64 = c.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?;
            assert_eq!(left, 0);
            assert!(matches!(store::create_conversation(c, &a.id), Err(Error::NotFound(_))));
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn publishing_needs_a_fixed_model_and_can_be_revoked() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            let a = create(c, input("售后答疑"))?;
            assert!(matches!(publish(c, &a.id), Err(Error::Invalid(m)) if m == "publish_needs_model"));
            let mut with_model = input("售后答疑");
            with_model.provider_id = Some("p".into());
            with_model.model = Some("m".into());
            update(c, &a.id, with_model)?;

            let p = publish(c, &a.id)?;
            let (token, key) = (p.public_token.clone().unwrap(), p.api_key.clone().unwrap());
            assert_eq!(token.len(), 32);
            assert!(key.starts_with("cb-") && key.len() == 43);
            // 再发布一次不换链接和密钥
            assert_eq!(publish(c, &a.id)?.public_token.unwrap(), token);
            assert_eq!(by_token(c, &token)?.id, a.id);
            assert_eq!(by_api_key(c, &key)?.id, a.id);

            let rotated = rotate_key(c, &a.id)?;
            assert_ne!(rotated.api_key.clone().unwrap(), key);
            assert_eq!(rotated.public_token.unwrap(), token);
            assert!(by_api_key(c, &key).is_err(), "old key stops working");

            unpublish(c, &a.id)?;
            assert!(by_token(c, &token).is_err());
            assert!(matches!(rotate_key(c, &a.id), Err(Error::Invalid(_))));
            assert!(by_token(c, "").is_err() && by_api_key(c, "").is_err());
            Ok(())
        })
        .await
        .unwrap();
    }
}
