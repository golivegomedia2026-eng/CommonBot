import { describe, expect, it } from "vitest";

import { parseEvents } from "./api";
import en from "./i18n/en";
import ja from "./i18n/ja";
import zhCN from "./i18n/zh-CN";
import zhTW from "./i18n/zh-TW";
import { renderMarkdown } from "./md";
import { isToday } from "./util";

describe("parseEvents", () => {
  const stream =
    ': keep-alive\n\nevent: delta\ndata: {"text":"你好"}\n\nevent: delta\ndata: {"text":"，世界"}\n\nevent: done\ndata: {"message":{"id":"m1"}}\n\n';

  it("gives the same events wherever the text is cut", () => {
    for (let cut = 0; cut <= stream.length; cut++) {
      const first = parseEvents(stream.slice(0, cut));
      const second = parseEvents(first.rest + stream.slice(cut));
      const all = [...first.events, ...second.events];
      expect(all.map((e) => e.type)).toEqual(["delta", "delta", "done"]);
      expect(all[1]).toEqual({ type: "delta", text: "，世界" });
      expect(second.rest).toBe("");
    }
  });

  it("skips broken events and handles CRLF", () => {
    const { events } = parseEvents('event: delta\r\ndata: {"text":"a"}\r\n\r\nevent: delta\ndata: {oops\n\n');
    expect(events).toEqual([{ type: "delta", text: "a" }]);
  });
});

describe("renderMarkdown", () => {
  it("shows HTML from the model as text instead of running it", () => {
    const html = renderMarkdown('<img src=x onerror="alert(1)"> **粗体** <script>alert(1)</script>');
    expect(html).not.toMatch(/<img|<script/);
    expect(html).toContain("<strong>粗体</strong>");
  });
  it("refuses javascript: links and opens real links in a new tab", () => {
    expect(renderMarkdown("[x](javascript:alert(1))")).not.toContain("href");
    expect(renderMarkdown("[x](https://example.com)")).toContain('target="_blank" rel="noopener noreferrer"');
  });
});

describe("translations", () => {
  const keys = (o: object, prefix = ""): string[] =>
    Object.entries(o).flatMap(([k, v]) => (typeof v === "object" ? keys(v, `${prefix}${k}.`) : [`${prefix}${k}`]));
  const placeholders = (s: string) => (s.match(/\{\w+\}/g) ?? []).sort().join();
  const flat = (o: object, prefix = ""): Record<string, string> =>
    Object.fromEntries(Object.entries(o).flatMap(([k, v]) => (typeof v === "object" ? Object.entries(flat(v, `${prefix}${k}.`)) : [[`${prefix}${k}`, v]])));

  it("every language has the same keys and placeholders, with no empty text", () => {
    const base = flat(zhCN);
    for (const [name, lang] of Object.entries({ zhTW, en, ja })) {
      expect(keys(lang).sort(), name).toEqual(keys(zhCN).sort());
      for (const [k, v] of Object.entries(flat(lang))) {
        expect(v.trim(), `${name}.${k}`).not.toBe("");
        expect(placeholders(v), `${name}.${k}`).toBe(placeholders(base[k]));
      }
    }
  });
});

describe("isToday", () => {
  it("compares calendar days", () => {
    const now = new Date(2026, 10, 15, 12).getTime();
    expect(isToday(new Date(2026, 10, 15, 0, 1).getTime(), now)).toBe(true);
    expect(isToday(new Date(2026, 10, 14, 23, 59).getTime(), now)).toBe(false);
  });
});

import { linkCitations } from "./util";

describe("linkCitations", () => {
  it("links only numbers that have a source, and leaves code and attributes alone", () => {
    const html = '<p>保修 24 个月 [1]，见 [3] 和 [2]。</p><pre><code>a[1]</code></pre><a href="x[1]">链接 [2]</a>';
    const out = linkCitations(html, 2);
    expect(out).toContain('个月 <button type="button" class="cite" data-cite="1">1</button>');
    expect(out).toContain("见 [3] 和");
    expect(out).toContain("<code>a[1]</code>");
    expect(out).toContain('href="x[1]"');
    expect(out.match(/data-cite/g)?.length).toBe(3);
    expect(linkCitations(html, 0)).toBe(html);
  });
});
