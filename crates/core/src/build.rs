//! 建造师：通过对话把一个新智能体定义清楚。
//! 一次建造 = 一段访谈对话 + 由访谈整理出的定义草稿 + 试答结果；确认后发布成正式的智能体。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{AgentInput, BUILDER_AGENT_ID};
use crate::store::{self, Message};
use crate::{new_id, now_ms, Error, Result};

/// 定义草稿。`manual` 记着用户亲手改过的字段：之后的访谈整理不再覆盖它们。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Draft {
    #[serde(flatten)]
    pub input: AgentInput,
    pub manual: Vec<String>,
}

/// 一道试答题。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TestCase {
    pub question: String,
    /// 期望的要点。
    pub expect: String,
    pub answer: Option<String>,
    /// 是否通过；`None` 表示还没答或没能评判。
    pub pass: Option<bool>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub id: String,
    pub conversation_id: String,
    pub draft: Draft,
    pub tests: Vec<TestCase>,
    /// 发布后生成的智能体。
    pub agent_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Build> {
    Ok(Build {
        id: r.get("id")?,
        conversation_id: r.get("conversation_id")?,
        draft: serde_json::from_str(&r.get::<_, String>("draft")?).unwrap_or_default(),
        tests: serde_json::from_str(&r.get::<_, String>("tests")?).unwrap_or_default(),
        agent_id: r.get("agent_id")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

/// 开始一次建造。`opening` 是建造师的第一句话（由界面按当前语言给出）。
pub fn create(c: &mut Connection, opening: &str) -> Result<Build> {
    let opening = opening.trim();
    if opening.is_empty() || opening.chars().count() > 500 {
        return Err(Error::Invalid("opening".into()));
    }
    let tx = c.transaction()?;
    let conv = store::create_conversation_from(&tx, BUILDER_AGENT_ID, "builder")?;
    let reply = store::Reply { content: opening, status: "ok", ..Default::default() };
    store::add_assistant_message(&tx, &conv.id, &reply)?;
    let (id, now) = (new_id(), now_ms());
    tx.execute(
        "INSERT INTO builds(id, conversation_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, conv.id, now],
    )?;
    tx.commit()?;
    get(c, &id)
}

pub fn get(c: &Connection, id: &str) -> Result<Build> {
    c.query_row("SELECT * FROM builds WHERE id = ?1", [id], from_row).optional()?.ok_or(Error::NotFound("build"))
}

pub fn list(c: &Connection) -> Result<Vec<Build>> {
    let mut stmt = c.prepare("SELECT * FROM builds ORDER BY updated_at DESC LIMIT 100")?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// 删除一次建造和它的访谈记录。已经发布出去的智能体不受影响。
pub fn delete(c: &Connection, id: &str) -> Result<()> {
    let b = get(c, id)?;
    c.execute("DELETE FROM conversations WHERE id = ?1", [&b.conversation_id])?; // 建造记录随对话一起删除
    Ok(())
}

pub fn save_draft(c: &Connection, id: &str, draft: &Draft) -> Result<()> {
    let n = c.execute(
        "UPDATE builds SET draft = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, serde_json::to_string(draft).unwrap_or_else(|_| "{}".into()), now_ms()],
    )?;
    if n == 0 {
        return Err(Error::NotFound("build"));
    }
    Ok(())
}

pub fn save_tests(c: &Connection, id: &str, tests: &[TestCase]) -> Result<()> {
    c.execute(
        "UPDATE builds SET tests = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, serde_json::to_string(tests).unwrap_or_else(|_| "[]".into()), now_ms()],
    )?;
    Ok(())
}

pub fn set_agent(c: &Connection, id: &str, agent_id: &str) -> Result<()> {
    c.execute("UPDATE builds SET agent_id = ?2, updated_at = ?3 WHERE id = ?1", params![id, agent_id, now_ms()])?;
    Ok(())
}

const TEXT_FIELDS: [&str; 6] = ["name", "description", "duty", "out_of_scope", "instructions", "opening"];

fn field<'a>(input: &'a mut AgentInput, name: &str) -> &'a mut String {
    match name {
        "name" => &mut input.name,
        "description" => &mut input.description,
        "duty" => &mut input.duty,
        "out_of_scope" => &mut input.out_of_scope,
        "instructions" => &mut input.instructions,
        _ => &mut input.opening,
    }
}

impl Draft {
    /// 把模型从访谈里整理出的内容并入草稿：只填它确实给出的内容，不动用户亲手改过的字段。
    pub fn merge_extracted(&mut self, v: &Value) {
        for name in TEXT_FIELDS {
            if self.manual.iter().any(|m| m == name) {
                continue;
            }
            if let Some(text) = v[name].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                *field(&mut self.input, name) = text.chars().take(4000).collect();
            }
        }
        if !self.manual.iter().any(|m| m == "examples") {
            if let Some(list) = v["examples"].as_array() {
                let examples: Vec<String> = list
                    .iter()
                    .filter_map(|e| e.as_str())
                    .map(|e| e.trim().chars().take(100).collect::<String>())
                    .filter(|e| !e.is_empty())
                    .take(6)
                    .collect();
                if !examples.is_empty() {
                    self.input.examples = examples;
                }
            }
        }
        if !self.manual.iter().any(|m| m == "web") {
            if let Some(w) = v["web"].as_str().filter(|w| matches!(*w, "off" | "fallback" | "always")) {
                self.input.web = w.to_string();
            }
        }
        // 名称是必填项且有长度上限
        self.input.name = self.input.name.chars().take(40).collect();
    }

    /// 用户手动改了草稿：记下改过的字段。
    pub fn apply_manual(&mut self, new: AgentInput) {
        let mut mark = |name: &str, changed: bool| {
            if changed && !self.manual.iter().any(|m| m == name) {
                self.manual.push(name.to_string());
            }
        };
        mark("name", new.name != self.input.name);
        mark("description", new.description != self.input.description);
        mark("duty", new.duty != self.input.duty);
        mark("out_of_scope", new.out_of_scope != self.input.out_of_scope);
        mark("instructions", new.instructions != self.input.instructions);
        mark("opening", new.opening != self.input.opening);
        mark("examples", new.examples != self.input.examples);
        mark("web", new.web != self.input.web);
        self.input = new;
    }

    /// 还缺哪些必要内容，试答和发布之前要补上。
    pub fn missing(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if self.input.name.trim().is_empty() {
            m.push("name");
        }
        if self.input.duty.trim().is_empty() {
            m.push("duty");
        }
        m
    }
}

/// 把访谈记录写成文字。
pub fn transcript(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|m| !m.content.trim().is_empty())
        .map(|m| format!("{}：{}", if m.role == "user" { "用户" } else { "建造师" }, m.content.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 建造师访谈时的要求（作为「其他要求」放进它的系统提示词）。
pub fn interviewer_instructions(draft: &Draft) -> String {
    let current = serde_json::to_string(&draft.input).unwrap_or_default();
    format!(
        "你的任务是通过提问，帮用户把一个新的智能体定义清楚。\n\
         - 每次只问一个问题，问题要具体、简短，不要一次列出一串问题。\n\
         - 需要弄清楚：给谁用、解决什么问题；典型的问法；要依据哪些资料、是否需要联网；哪些事不该做；回答的语气和格式；开场白。\n\
         - 已经清楚的不要重复问。用户上传了资料时，可以结合资料内容来问。\n\
         - 用户说「可以了」或信息已经足够时，用几句话复述你理解的定义，并请用户看旁边的草稿，点「试答」检验。\n\
         - 不要替用户编造他没说过的要求，也不要直接替那个智能体回答业务问题。\n\
         目前整理出的草稿（JSON）：{current}"
    )
}

/// 让模型把访谈整理成定义。
pub fn extract_prompt(transcript: &str) -> String {
    format!(
        "根据下面的访谈记录，整理出要新建的智能体的定义。只输出一个 JSON 对象，不要输出任何别的文字。\n\
         字段：\n\
         - name：名称，不超过 20 个字\n\
         - description：一句话简介\n\
         - duty：负责的任务\n\
         - out_of_scope：不负责的范围\n\
         - instructions：语气、格式等其他要求\n\
         - opening：开场白\n\
         - examples：示例问题，字符串数组，最多 3 个\n\
         - web：是否联网，取 off（不联网）、fallback（资料里没有时联网）、always（每次都联网）之一\n\
         访谈里没有提到的字段填空字符串（examples 填空数组），不要编造。\n\n\
         访谈记录：\n{transcript}"
    )
}

/// 让模型出试答题。
pub fn tests_prompt(draft: &Draft) -> String {
    format!(
        "下面是一个智能体的定义。请为它生成测试问题，用来检验它是否按定义回答。只输出一个 JSON 数组，不要输出任何别的文字。\n\
         数组里每一项是 {{\"question\": 问题, \"expect\": 期望回答的要点}}。一共 4 道：\n\
         - 3 道在它负责范围内的典型问题\n\
         - 1 道它不该回答的问题（定义里写了不负责的范围时，从那里出；否则出一道与它的任务无关的问题），期望是它说明这不归它负责\n\n\
         名称：{}\n负责的任务：{}\n不负责的范围：{}\n其他要求：{}",
        draft.input.name, draft.input.duty, draft.input.out_of_scope, draft.input.instructions
    )
}

/// 让模型评判一次试答。
pub fn judge_prompt(draft: &Draft, case: &TestCase, answer: &str) -> String {
    format!(
        "请判断这个回答是否符合要求。只输出一个 JSON 对象：{{\"pass\": true 或 false, \"reason\": 一句话原因}}，不要输出任何别的文字。\n\n\
         智能体负责的任务：{}\n不负责的范围：{}\n\n问题：{}\n期望的要点：{}\n\n回答：{}",
        draft.input.duty, draft.input.out_of_scope, case.question, case.expect, answer
    )
}

/// 解析模型出的题。
pub fn parse_tests(v: &Value) -> Vec<TestCase> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    let question = t["question"].as_str()?.trim().to_string();
                    (!question.is_empty()).then(|| TestCase {
                        question,
                        expect: t["expect"].as_str().unwrap_or("").trim().to_string(),
                        ..Default::default()
                    })
                })
                .take(6)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use serde_json::json;

    #[tokio::test]
    async fn build_lifecycle() {
        let db = Db::open_in_memory().unwrap();
        db.run(|c| {
            assert!(create(c, "  ").is_err());
            let b = create(c, "这个智能体给谁用？")?;
            let msgs = store::list_messages(c, &b.conversation_id)?;
            assert_eq!((msgs.len(), msgs[0].role.as_str()), (1, "assistant"));
            // 访谈对话不出现在任何智能体的对话列表里
            assert!(store::list_conversations(c, BUILDER_AGENT_ID)?.is_empty());
            assert_eq!(b.draft, Draft::default());

            let mut d = b.draft.clone();
            d.merge_extracted(&json!({ "name": "招标助手", "duty": "解读招标文件" }));
            save_draft(c, &b.id, &d)?;
            save_tests(c, &b.id, &[TestCase { question: "q".into(), pass: Some(true), ..Default::default() }])?;
            let got = get(c, &b.id)?;
            assert_eq!(got.draft.input.name, "招标助手");
            assert_eq!(got.tests[0].pass, Some(true));
            assert_eq!(list(c)?.len(), 1);

            delete(c, &b.id)?;
            assert!(get(c, &b.id).is_err());
            assert!(store::get_conversation(c, &b.conversation_id).is_err());
            assert!(save_draft(c, &b.id, &d).is_err());
            Ok(())
        })
        .await
        .unwrap();
    }

    #[test]
    fn extraction_fills_gaps_but_respects_manual_edits() {
        let mut d = Draft::default();
        d.merge_extracted(&json!({ "name": " 招标文件助手 ", "duty": "解读招标文件", "out_of_scope": "", "examples": ["截止时间？", " "], "web": "fallback", "opening": 3 }));
        assert_eq!(
            (d.input.name.as_str(), d.input.duty.as_str(), d.input.web.as_str()),
            ("招标文件助手", "解读招标文件", "fallback")
        );
        assert_eq!(d.input.examples, vec!["截止时间？"]);
        assert_eq!(d.missing(), Vec::<&str>::new());

        // 用户把名称改了：之后的整理不再动名称，其余照常更新；空值和非法值不覆盖已有内容
        let mut edited = d.input.clone();
        edited.name = "投标小助手".into();
        d.apply_manual(edited);
        assert_eq!(d.manual, vec!["name"]);
        d.merge_extracted(&json!({ "name": "别的名字", "duty": "", "out_of_scope": "不预测中标", "web": "sometimes", "examples": [] }));
        assert_eq!(d.input.name, "投标小助手");
        assert_eq!(
            (d.input.duty.as_str(), d.input.out_of_scope.as_str(), d.input.web.as_str()),
            ("解读招标文件", "不预测中标", "fallback")
        );
        assert_eq!(d.input.examples, vec!["截止时间？"]);
        assert_eq!(Draft::default().missing(), vec!["name", "duty"]);
    }

    #[test]
    fn parses_generated_tests_tolerantly() {
        let t = parse_tests(
            &json!([{ "question": " 截止时间？ ", "expect": "给出日期" }, { "question": "" }, { "expect": "x" }, { "question": "能中标吗" }]),
        );
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].question.as_str(), t[0].expect.as_str(), t[0].pass), ("截止时间？", "给出日期", None));
        assert!(parse_tests(&json!({ "not": "a list" })).is_empty());
    }
}
