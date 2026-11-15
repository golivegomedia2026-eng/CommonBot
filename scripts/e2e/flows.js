// 主要流程的浏览器回归。每一步失败都会让脚本以非零状态退出。
const { chromium } = require("playwright");
const fs = require("fs");
const os = require("os");
const { execSync } = require("child_process");

const [out, fx] = process.argv.slice(2);
const base = "http://localhost:8600";
const lanIp = Object.values(os.networkInterfaces()).flat().find((i) => i && i.family === "IPv4" && !i.internal)?.address;
const file = (src, name) => ({ name, mimeType: "application/octet-stream", buffer: fs.readFileSync(`${fx}/${src}`) });
const problems = [];
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? " — " + detail : ""}`);
  if (!ok) problems.push(name);
};

(async () => {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, deviceScaleFactor: 2, locale: "zh-CN" });
  const p = await ctx.newPage();
  const errors = [];
  p.on("pageerror", (e) => errors.push(String(e)));
  const shot = (name) => p.screenshot({ path: `${out}/${name}.png` });
  const noOverflow = async (page, name) => {
    const [sw, cw] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
    check(`no horizontal overflow: ${name}`, sw <= cw, `${sw} vs ${cw}`);
  };
  const answered = async (page = p) => {
    await page.waitForSelector(".caret");
    await page.waitForSelector(".caret", { state: "detached", timeout: 20000 });
  };
  const ask = async (text) => {
    await p.fill(".composer textarea", text);
    await p.keyboard.press("Enter");
    await answered();
  };
  const lastSources = () => p.$$eval(".msg.bot", (m) => [...m[m.length - 1].querySelectorAll(".srcs b, .srcs .src")].map((x) => x.textContent.replace(/\s+/g, " ").trim()));
  const field = (label) => p.locator(".drawer .field", { hasText: label }).locator("input, textarea, select").first();
  const ready = (n) => p.waitForFunction((n) => document.querySelectorAll("tbody tr").length === n && [...document.querySelectorAll("tbody tr")].every((r) => r.querySelector(".tag.ok, .tag.err")), n, { timeout: 30000 });

  // ---- 模型：默认的推理服务可用；选好向量化和文字识别模型；配置联网搜索 ----
  await p.goto(base + "/settings/models");
  await p.waitForSelector(".tag.ok");
  await p.selectOption(".card.form select >> nth=0", { label: "mock-embed" });
  await p.waitForSelector(".card.form .tag.ok");
  await p.selectOption(".card.form select >> nth=1", { label: "mock-vision" });
  await p.selectOption("form.card select", "searxng");
  await p.fill("form.card input[type=text]", "http://127.0.0.1:11434");
  await p.check("form.card input[type=checkbox]");
  await p.click("form.card button[type=submit]");
  await p.waitForSelector("form.card .tag.ok");
  check("models and web search are set up", true);
  await noOverflow(p, "models");

  // ---- 知识库：文件夹、上传（含中文文件名、扫描件、不支持的类型）、检索 ----
  await p.goto(base + "/kb");
  await p.waitForSelector(".empty");
  await p.click(".page-head .btn.ghost");
  await p.fill(".inline-form input", "售后资料");
  await p.click(".inline-form button[type=submit]");
  await p.waitForSelector('.kbmain h2:has-text("售后资料")');
  await p.setInputFiles("input[type=file]", [file("text.pdf", "保修条款.pdf"), file("sample.docx", "售后手册.docx"), file("scan.pdf", "评分办法（扫描）.pdf"), { name: "压缩包.zip", mimeType: "application/zip", buffer: Buffer.from("PK") }]);
  await ready(3);
  check("unsupported file is explained", (await p.textContent(".alert.warn")).includes("压缩包.zip"));
  check("all documents are in, scan was recognised", (await p.locator("tbody .tag.ok").count()) === 3 && (await p.textContent("tbody")).includes("识别了 1 页"));
  await shot("kb");
  await p.click('.kbmain .row button:has-text("检索测试")');
  await p.fill(".drawer input", "屏幕保修多久");
  await p.keyboard.press("Enter");
  await p.waitForSelector(".drawer .hit");
  check("search finds the right passage", (await p.textContent(".drawer .hit")).includes("保修条款.pdf"));
  await p.click(".drawer .hd2 button");
  await noOverflow(p, "knowledge base");

  // ---- 智能体：手动新建，按知识库回答并带出处；对话里改范围、联网、上传文件 ----
  await p.goto(base + "/agents");
  await p.click(".page-head button.btn.ghost");
  await field("名称").fill("售后答疑");
  await field("负责的任务").fill("回答保修和退换货问题。");
  await field("示例问题").fill("屏幕坏了还能保修吗？");
  await p.locator(".drawer .picker label", { hasText: "售后资料" }).locator("input").check();
  await p.click(".drawer button[type=submit]");
  await p.waitForSelector(".quick .chip");
  await p.click(".quick .chip");
  await answered();
  const kbSources = await lastSources();
  check("answer cites the knowledge base", kbSources[0] === "知识库" && kbSources.some((s) => s.includes("保修条款.pdf")), kbSources.join(" | "));
  await p.click(".msg.bot .md .cite >> nth=0");
  await p.waitForSelector(".srcs blockquote");
  await shot("chat-sources");

  await p.click(".pw .chip");
  await p.click('.pop label:has-text("不查知识库")');
  await p.keyboard.press("Escape");
  check("scope popover closes on Escape", (await p.locator(".pop").count()) === 0);
  await p.click('.tools .chip:has-text("联网")');
  await ask("延保价格调整为多少");
  const webSources = await lastSources();
  check("with the knowledge base off and web on, sources are web pages", webSources[0] === "网页", webSources.join(" | "));

  await p.click('.tools .chip:has-text("联网")');
  await p.setInputFiles(".composer input[type=file]", [file("sample.xlsx", "历年中标.xlsx")]);
  await p.waitForSelector('.composer .att:has-text("历年中标.xlsx")');
  await ask("政务云一期中标金额是多少");
  const fileSources = await lastSources();
  check("uploaded file is used and labelled", fileSources[0] === "本次上传", fileSources.join(" | "));
  await shot("chat-files");
  // 新对话 → 切到原来的对话 → 再切回新对话
  await p.click(".page-head a.btn.primary");
  await p.waitForFunction(() => document.querySelectorAll(".msg.me").length === 0);
  await ask("这是第二个对话");
  await p.click('.hist .h:has-text("屏幕坏了还能保修吗")');
  await p.waitForFunction(() => document.querySelectorAll(".msg").length === 6);
  await p.click('.hist .h:has-text("这是第二个对话")');
  await p.waitForFunction(() => document.querySelectorAll(".msg").length === 2, null, { timeout: 5000 }).catch(() => {});
  check("can switch back to the new conversation", (await p.locator(".msg").count()) === 2 && (await p.textContent(".msg.me")).includes("这是第二个对话"));
  await p.click('.hist .h:has-text("屏幕坏了还能保修吗")');
  await p.waitForFunction(() => document.querySelectorAll(".msg").length === 6);
  await p.reload();
  await p.waitForSelector(".msg.me .att");
  check("conversation survives a reload", (await p.locator(".msg").count()) === 6);
  await noOverflow(p, "chat");

  // ---- 建造师：访谈、草稿、手动修改、试答、发布 ----
  await p.goto(base + "/builder");
  await p.click(".page-head button.btn.primary");
  await p.waitForSelector(".buildpage .msg.bot");
  await ask("给投标组用，帮他们解读招标文件里的资质、评分和时间要求");
  await p.waitForFunction(() => document.querySelector(".spec dd")?.textContent === "招标文件助手");
  await p.click(".spec button");
  await field("名称").fill("投标小助手");
  await p.click(".drawer button[type=submit]");
  await p.waitForSelector(".drawer", { state: "detached" });
  await ask("不要预测中标，也不要帮写报价");
  await p.waitForTimeout(600);
  check("manual edit survives the next interview turn", (await p.textContent(".spec dd")) === "投标小助手");
  await p.click('.buildside button:has-text("开始试答")');
  await p.waitForSelector('.buildside button:has-text("再试一次"):not([disabled])', { timeout: 40000 });
  check("tests ran with a verdict each", (await p.locator(".test .tag.ok, .test .tag.err").count()) === 3);
  await shot("builder");
  await p.click('.buildside button:has-text("发布为智能体")');
  await p.waitForSelector(".buildside .kv");
  const token = (await p.textContent(".buildside .kv >> nth=0 >> span")).trim().split("/a/")[1];
  const key = (await p.textContent(".buildside .kv >> nth=1 >> span")).trim();
  check("published with a link and a key", /^[0-9a-f]{32}$/.test(token) && key.startsWith("cb-"));
  await noOverflow(p, "builder");

  // ---- 系统：帮助页、语言、局域网开关；另一台设备凭链接使用、凭访问码进入、用密钥调接口 ----
  await p.goto(base + "/help");
  await p.waitForSelector(".guide h2");
  check("the user guide is built in", (await p.locator(".guide h2").count()) >= 10);
  await p.goto(base + "/settings/system");
  await p.waitForSelector(".switch:not([disabled])");
  for (const [label, title] of [["English", "System & appearance"], ["日本語", "システムと外観"], ["繁體中文", "系統與外觀"], ["简体中文", "系统与外观"]]) {
    await p.click(`.seg button:has-text("${label}")`);
    check(`language: ${label}`, (await p.textContent("h1")) === title);
    await noOverflow(p, `system page in ${label}`);
  }
  if (lanIp) {
    await p.click(".switch");
    await p.waitForSelector(".switch.on:not([disabled])", { timeout: 15000 });
    check("LAN switch lists an address", (await p.textContent(".cols .card >> nth=0")).includes(lanIp));
    await p.click("text=查看");
    const code = (await p.textContent(".kv span[style]")).trim();

    const phone = await (await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, locale: "zh-CN" })).newPage();
    phone.on("pageerror", (e) => errors.push("phone: " + e));
    await phone.goto(`http://${lanIp}:8600/a/${token}`);
    await phone.waitForSelector('.pubhead h1:has-text("投标小助手")');
    await phone.fill(".composer textarea", "投标截止时间是哪天？");
    await phone.click(".send");
    await answered(phone);
    check("a visitor can chat through the link without the access code", (await phone.textContent(".msg.bot .md")).includes("你是「投标小助手」"));
    await noOverflow(phone, "public page on a phone");
    await phone.screenshot({ path: `${out}/public-phone.png` });

    await phone.goto(`http://${lanIp}:8600/agents`);
    await phone.waitForSelector('input[maxlength="16"]');
    await phone.fill("input", code);
    await phone.click("button[type=submit]");
    await phone.waitForSelector(".agent");
    check("another device gets in with the access code", (await phone.locator(".agent").count()) >= 2);
    for (const path of ["/agents", "/kb", "/builder", "/settings/models", "/settings/system", "/help"]) {
      await phone.goto(`http://${lanIp}:8600${path}`);
      await phone.waitForSelector("main");
      await phone.waitForTimeout(400);
      await noOverflow(phone, `${path} on a phone`);
    }

    const reply = JSON.parse(execSync(`curl -s http://${lanIp}:8600/v1/chat/completions -H "Authorization: Bearer ${key}" -H "Content-Type: application/json" -d '{"messages":[{"role":"user","content":"hi"}]}'`).toString());
    check("the API answers with the key", reply.choices?.[0]?.message?.content?.includes("你是「投标小助手」"));
    const denied = execSync(`curl -s -o /dev/null -w "%{http_code}" http://${lanIp}:8600/v1/chat/completions -H "Content-Type: application/json" -d '{"messages":[]}'`).toString();
    check("the API refuses without a key", denied === "401");

    await p.click(".switch");
    await p.waitForSelector(".switch:not(.on):not([disabled])", { timeout: 15000 });
    const closed = execSync(`curl -s -m 2 -o /dev/null -w "%{http_code}" http://${lanIp}:8600/api/health || true`).toString();
    check("turning LAN access off closes it to other devices", closed === "000");
  } else {
    console.log("skip LAN checks: no network address on this machine");
  }

  check("no script errors in the pages", errors.length === 0, errors.join("; "));
  await browser.close();
  if (problems.length) {
    console.error(`\n${problems.length} check(s) failed`);
    process.exit(1);
  }
  console.log("\nall checks passed");
})().catch((e) => {
  console.error("FAILED:", e.message.split("\n").slice(0, 6).join("\n"));
  process.exit(1);
});
