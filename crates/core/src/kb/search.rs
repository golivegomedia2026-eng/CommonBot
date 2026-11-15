//! 检索：关键词 + 向量，两路结果按名次融合。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;

use super::index::VectorIndex;
use super::{keyword_search, load_hits, models, subtree, Hit};
use crate::llm::{Endpoint, LlmClient};
use crate::{Db, Result};

/// 每一路各取多少个候选。
const CANDIDATES: usize = 30;

#[derive(Debug, Clone, Serialize, Default)]
pub struct Retrieved {
    pub hits: Vec<Hit>,
    /// 这次是否用上了向量检索。false 时只按关键词查（没配置向量化模型，或它暂时连不上）。
    pub semantic: bool,
    /// 向量检索没用上的原因代码，供界面提示。
    pub note: Option<&'static str>,
}

#[derive(Clone)]
pub struct Retriever {
    pub db: Db,
    pub llm: LlmClient,
    pub index: Arc<VectorIndex>,
}

/// 按名次融合两路结果（Reciprocal Rank Fusion）：两路都靠前的排在最前。
pub fn fuse(lists: &[Vec<i64>], k: usize) -> Vec<i64> {
    let mut score: HashMap<i64, f32> = HashMap::new();
    let mut first_seen: Vec<i64> = Vec::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            if !score.contains_key(id) {
                first_seen.push(*id);
            }
            *score.entry(*id).or_default() += 1.0 / (60.0 + rank as f32);
        }
    }
    first_seen.sort_by(|a, b| score[b].total_cmp(&score[a]));
    first_seen.truncate(k);
    first_seen
}

impl Retriever {
    /// 在给定文件夹（含子文件夹）里找与提问最相关的片段。
    pub async fn retrieve(&self, query: &str, folder_ids: &[String], k: usize) -> Result<Retrieved> {
        if folder_ids.is_empty() || query.trim().is_empty() {
            return Ok(Retrieved::default());
        }
        let (q, ids) = (query.to_string(), folder_ids.to_vec());
        let (folders, keyword, kb_models) = self
            .db
            .run(move |c| {
                let folders = subtree(c, &ids)?;
                let keyword = keyword_search(c, &q, &folders, CANDIDATES)?;
                Ok((folders, keyword, models(c)?))
            })
            .await?;

        let mut lists = vec![keyword];
        let (mut semantic, mut note) = (false, None);
        match kb_models.embed {
            None => note = Some("embed_not_configured"),
            Some((provider, model)) => {
                let ep = Endpoint { base_url: provider.base_url.clone(), api_key: provider.api_key.clone() };
                match self.llm.embed(&ep, &model, &[query.to_string()]).await {
                    Err(e) => {
                        // 向量化服务暂时不可用时不让整个问答失败，退回到关键词
                        note = Some(e.code());
                    }
                    Ok(mut vectors) => {
                        let qv = vectors.pop().unwrap_or_default();
                        let index = self.index.clone();
                        let vector_hits = self
                            .db
                            .run(move |c| {
                                index.ensure_loaded(c, &model)?;
                                let mut stmt = c.prepare(
                                    "SELECT id FROM documents WHERE status = 'ready' AND folder_id IN (SELECT value FROM json_each(?1))",
                                )?;
                                let allowed: HashSet<String> = stmt
                                    .query_map([serde_json::to_string(&folders).unwrap_or_default()], |r| r.get(0))?
                                    .collect::<rusqlite::Result<_>>()?;
                                Ok(index.search(&qv, &allowed, CANDIDATES))
                            })
                            .await?;
                        semantic = true;
                        lists.push(vector_hits.into_iter().map(|(id, _)| id).collect());
                    }
                }
            }
        }

        let top = fuse(&lists, k);
        let hits = self.db.run(move |c| load_hits(c, &top)).await?;
        Ok(Retrieved { hits, semantic, note })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_prefers_items_both_lists_agree_on() {
        // 3 在两路里都出现；1 和 7 各只在一路排第一
        assert_eq!(fuse(&[vec![1, 3, 5], vec![7, 3, 9]], 3)[0], 3);
        assert_eq!(fuse(&[vec![1, 2], vec![]], 5), vec![1, 2]);
        assert!(fuse(&[], 5).is_empty());
        assert_eq!(fuse(&[vec![1, 2, 3, 4]], 2), vec![1, 2]);
    }
}
