/** 复制文字。局域网里用 http 访问时浏览器不提供剪贴板接口，退回到选中再复制。 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* 换下面的办法 */
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  ta.remove();
  return ok;
}

export function isToday(ms: number, now = Date.now()): boolean {
  const a = new Date(ms);
  const b = new Date(now);
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** 把回答里的 [1] [2] 换成可点的出处编号；只处理确实有对应出处的编号。 */
export function linkCitations(html: string, count: number): string {
  if (!count) return html;
  // 只替换标签之外的文字，不碰属性和代码块
  return html
    .split(/(<pre[\s\S]*?<\/pre>|<code[\s\S]*?<\/code>|<[^>]+>)/)
    .map((part) =>
      part.startsWith("<")
        ? part
        : part.replace(/\[(\d{1,2})\]/g, (m, n) => (Number(n) >= 1 && Number(n) <= count ? `<button type="button" class="cite" data-cite="${n}">${n}</button>` : m)),
    )
    .join("");
}
