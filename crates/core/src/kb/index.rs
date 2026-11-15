//! 向量索引：所有片段的向量放在内存里逐个比对（精确检索）。
//! 一百万个片段以内够用（1024 维约占 4 GB 内存、单次检索约半秒）；再大需要换近似索引。

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use rusqlite::Connection;

use super::blob_to_vector;
use crate::Result;

#[derive(Default)]
struct Inner {
    loaded: bool,
    model: String,
    dim: usize,
    chunk_ids: Vec<i64>,
    /// 每个片段所属文档在 `docs` 里的下标。
    doc_of: Vec<u32>,
    docs: Vec<String>,
    doc_lookup: HashMap<String, u32>,
    /// 连续存放的向量，长度 = 片段数 × 维度。
    data: Vec<f32>,
}

impl Inner {
    fn push(&mut self, doc_id: &str, chunk_id: i64, v: &[f32]) {
        if self.dim == 0 {
            self.dim = v.len();
        }
        if v.len() != self.dim || v.is_empty() {
            return; // 维度对不上的向量来自别的模型，不参与检索
        }
        let doc = match self.doc_lookup.get(doc_id) {
            Some(i) => *i,
            None => {
                let i = self.docs.len() as u32;
                self.docs.push(doc_id.to_string());
                self.doc_lookup.insert(doc_id.to_string(), i);
                i
            }
        };
        self.chunk_ids.push(chunk_id);
        self.doc_of.push(doc);
        self.data.extend_from_slice(v);
    }
}

#[derive(Default)]
pub struct VectorIndex {
    inner: RwLock<Inner>,
}

impl VectorIndex {
    /// 内容有删除或模型有变化时调用；下次检索时重新从数据库读入。
    pub fn invalidate(&self) {
        *self.inner.write().unwrap_or_else(|p| p.into_inner()) = Inner::default();
    }

    /// 确保内存里是 `model` 这个向量化模型的全部向量。
    pub fn ensure_loaded(&self, c: &Connection, model: &str) -> Result<()> {
        {
            let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
            if inner.loaded && inner.model == model {
                return Ok(());
            }
        }
        let mut fresh = Inner { loaded: true, model: model.to_string(), ..Default::default() };
        let mut stmt = c.prepare(
            "SELECT c.id, c.document_id, c.vector FROM chunks c JOIN documents d ON d.id = c.document_id
             WHERE d.status = 'ready' AND d.embed_model = ?1 AND c.vector IS NOT NULL ORDER BY c.id",
        )?;
        let mut rows = stmt.query([model])?;
        while let Some(r) = rows.next()? {
            let (id, doc, blob): (i64, String, Vec<u8>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            fresh.push(&doc, id, &blob_to_vector(&blob));
        }
        *self.inner.write().unwrap_or_else(|p| p.into_inner()) = fresh;
        Ok(())
    }

    /// 一份文档刚完成向量化：已载入时直接追加，不用整体重读。
    pub fn append(&self, model: &str, doc_id: &str, vectors: &[(i64, Vec<f32>)]) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if !inner.loaded || inner.model != model {
            return;
        }
        for (id, v) in vectors {
            inner.push(doc_id, *id, v);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|p| p.into_inner()).chunk_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 在允许的文档范围内找与 `query` 最接近的 `k` 个片段，返回（片段编号，相似度）。
    pub fn search(&self, query: &[f32], allowed_docs: &HashSet<String>, k: usize) -> Vec<(i64, f32)> {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        if inner.dim == 0 || query.len() != inner.dim || k == 0 {
            return Vec::new();
        }
        let allowed: Vec<bool> = inner.docs.iter().map(|d| allowed_docs.contains(d)).collect();
        let mut scored: Vec<(f32, i64)> = inner
            .data
            .chunks_exact(inner.dim)
            .zip(inner.chunk_ids.iter().zip(&inner.doc_of))
            .filter(|(_, (_, doc))| allowed[**doc as usize])
            .map(|(v, (id, _))| (v.iter().zip(query).map(|(a, b)| a * b).sum::<f32>(), *id))
            .collect();
        let by_score = |a: &(f32, i64), b: &(f32, i64)| b.0.total_cmp(&a.0);
        if scored.len() > k {
            scored.select_nth_unstable_by(k - 1, by_score);
            scored.truncate(k);
        }
        scored.sort_by(by_score);
        scored.into_iter().map(|(s, id)| (id, s)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nearest_within_allowed_documents() {
        let idx = VectorIndex::default();
        {
            let mut inner = idx.inner.write().unwrap();
            inner.loaded = true;
            inner.model = "m".into();
        }
        idx.append("m", "a", &[(1, vec![1.0, 0.0]), (2, vec![0.0, 1.0])]);
        idx.append("m", "b", &[(3, vec![0.9, 0.1]), (4, vec![1.0, 0.0, 0.0])]); // 4 维度不对，被忽略
        idx.append("other-model", "c", &[(5, vec![1.0, 0.0])]); // 模型不对，被忽略
        assert_eq!(idx.len(), 3);

        let all: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let ids: Vec<i64> = idx.search(&[1.0, 0.0], &all, 2).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![1, 3]);
        let only_b: HashSet<String> = ["b".to_string()].into();
        assert_eq!(idx.search(&[1.0, 0.0], &only_b, 5).len(), 1);
        assert!(idx.search(&[1.0, 0.0, 0.0], &all, 5).is_empty(), "query of another dimension");
        idx.invalidate();
        assert!(idx.is_empty());
    }
}
