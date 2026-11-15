//! 把文档切成检索用的片段，并生成全文索引用的词。

use super::parse::{Locator, Section};

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub locator: Locator,
    pub text: String,
}

/// 一个片段的目标长度（字符）。中文约 600 字，正好是一两段话。
pub const TARGET: usize = 600;
/// 相邻片段重叠的长度，避免一句话被切在两个片段中间而两边都搜不到。
pub const OVERLAP: usize = 80;

fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';' | '\n')
}

/// 把过长的一段按句子切开，实在没有句号就硬切。
fn split_long(para: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut len = 0;
    for sentence in para.split_inclusive(is_sentence_end) {
        let n = sentence.chars().count();
        if len + n > max && len > 0 {
            out.push(std::mem::take(&mut cur));
            len = 0;
        }
        if n > max {
            let chars: Vec<char> = sentence.chars().collect();
            out.extend(chars.chunks(max).map(|c| c.iter().collect::<String>()));
        } else {
            cur.push_str(sentence);
            len += n;
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(n);
    // 尽量从一句话的开头开始重叠
    let begin = (start..chars.len()).find(|&i| i > 0 && is_sentence_end(chars[i - 1])).unwrap_or(start);
    chars[begin..].iter().collect::<String>().trim().to_string()
}

/// 不同位置的内容能否放进同一个片段。页、幻灯片、工作表之间不合并，出处才能标准确；
/// 标题下的小节很短时可以和下一节合并，免得产生一堆只有一两句话的片段。
fn can_join(cur: &Locator, next: &Locator, cur_len: usize) -> bool {
    if cur == next {
        return true;
    }
    let loose = |l: &Locator| matches!(l, Locator::None | Locator::Heading(_));
    loose(cur) && loose(next) && cur_len < TARGET / 3
}

pub fn chunk(sections: &[Section]) -> Vec<Chunk> {
    // 先摊平成（位置，段落）
    let mut paras: Vec<(&Locator, String)> = Vec::new();
    for s in sections {
        for line in s.text.lines() {
            let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if line.is_empty() {
                continue;
            }
            if line.chars().count() > TARGET {
                paras.extend(split_long(&line, TARGET).into_iter().map(|p| (&s.locator, p)));
            } else {
                paras.push((&s.locator, line));
            }
        }
    }

    let mut out: Vec<Chunk> = Vec::new();
    let mut cur: Option<(Locator, String, usize)> = None;
    for (loc, para) in paras {
        let n = para.chars().count();
        match &mut cur {
            Some((l, text, len)) if *len + n <= TARGET && can_join(l, loc, *len) => {
                text.push('\n');
                text.push_str(&para);
                *len += n + 1;
            }
            _ => {
                let overlap = match cur.take() {
                    Some((l, text, _)) => {
                        // 只在同一位置内重叠；跨页重叠会让出处标错页
                        let t = if &l == loc { tail(&text, OVERLAP) } else { String::new() };
                        out.push(Chunk { locator: l, text });
                        t
                    }
                    None => String::new(),
                };
                let text = if overlap.is_empty() { para } else { format!("{overlap}\n{para}") };
                cur = Some((loc.clone(), text, n));
            }
        }
    }
    if let Some((l, text, _)) = cur {
        out.push(Chunk { locator: l, text });
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF | 0x20000..=0x2FFFF)
}

/// 全文索引用的词：英文和数字按词，中日韩文字按相邻两字一组（不依赖分词词典，离线可用）。
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_word = |w: &mut String, out: &mut Vec<String>| {
        if !w.is_empty() {
            out.push(std::mem::take(w));
        }
    };
    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        match run.len() {
            0 => {}
            1 => out.push(run[0].to_string()),
            _ => out.extend(run.windows(2).map(|w| w.iter().collect::<String>())),
        }
        run.clear();
    };
    for c in text.chars() {
        if is_cjk(c) {
            flush_word(&mut word, &mut out);
            cjk.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            word.extend(c.to_lowercase());
        } else {
            flush_word(&mut word, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

/// 由提问生成全文检索条件：任一词命中即可，命中越多排得越前。
pub fn fts_query(text: &str) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let terms: Vec<String> = tokens(text)
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .take(64)
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(loc: Locator, text: &str) -> Section {
        Section { locator: loc, text: text.into() }
    }

    #[test]
    fn tokens_mix_words_and_cjk_pairs() {
        assert_eq!(tokens("保修条款 v2.0，GPU加速"), vec!["保修", "修条", "条款", "v2", "0", "gpu", "加速"]);
        assert_eq!(tokens("页"), vec!["页"]);
        assert_eq!(fts_query("保修 保修").unwrap(), "\"保修\"");
        assert!(fts_query("，。！").is_none());
    }

    #[test]
    fn packs_short_paragraphs_but_never_across_pages() {
        let chunks = chunk(&[sec(Locator::Page(1), "第一段。\n\n第二段。"), sec(Locator::Page(2), "第三段。")]);
        let got: Vec<_> = chunks.iter().map(|c| (c.locator.clone(), c.text.as_str())).collect();
        assert_eq!(got, vec![(Locator::Page(1), "第一段。\n第二段。"), (Locator::Page(2), "第三段。")]);
    }

    #[test]
    fn short_heading_sections_merge_but_long_ones_do_not() {
        let h = |t: &str| Locator::Heading(t.into());
        let short = chunk(&[sec(Locator::None, "手册"), sec(h("一"), "一\n很短。"), sec(h("二"), "二\n也很短。")]);
        assert_eq!(short.len(), 1);
        let long = "这一节写得比较长。".repeat(30);
        let split = chunk(&[sec(h("一"), &long), sec(h("二"), "二\n下一节。")]);
        assert_eq!(split.len(), 2);
        assert_eq!(split[1].locator, h("二"));
    }

    #[test]
    fn splits_long_text_with_overlap_and_bounded_size() {
        let sentence = "这是一句用来测试切分的话，长度适中。";
        let long = sentence.repeat(100);
        let chunks = chunk(&[sec(Locator::Page(7), &long)]);
        assert!(chunks.len() > 2);
        for c in &chunks {
            assert!(c.text.chars().count() <= TARGET + OVERLAP + 1, "{}", c.text.chars().count());
            assert_eq!(c.locator, Locator::Page(7));
        }
        // 相邻片段有重叠，且重叠从句子开头开始
        let second_start: String = chunks[1].text.chars().take(sentence.chars().count()).collect();
        assert_eq!(second_start, sentence);
        // 不丢内容：去掉重叠后拼回去包含原文的每一句
        let total: usize = chunks.iter().map(|c| c.text.matches(sentence).count()).sum();
        assert!(total >= 100, "{total}");
    }

    #[test]
    fn hard_cuts_text_without_punctuation() {
        let chunks = chunk(&[sec(Locator::None, &"字".repeat(2000))]);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.text.chars().count() <= TARGET + OVERLAP + 1));
    }
}
