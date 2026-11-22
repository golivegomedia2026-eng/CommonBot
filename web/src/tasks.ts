// 任务中心：后台进行的事情放在这里，切换页面不会打断。
// 服务端的任务（正在生成的回答、知识库入库、算力下载）定时去问；上传文件由这里排队逐个传。
import { computed, reactive } from "vue";

import { api, ApiError, kb, type ServerTask } from "./api";

export interface UploadTask {
  id: number;
  name: string;
  folderId: string;
  folderName: string;
  status: "waiting" | "uploading" | "done" | "failed";
  /** 失败原因的代码。 */
  code?: string;
}

export const tasks = reactive({
  server: [] as ServerTask[],
  uploads: [] as UploadTask[],
  /** 每传完一个文件加一，知识库页据此刷新。 */
  kbTick: 0,
  open: false,
});

const pendingUploads = computed(() => tasks.uploads.filter((u) => u.status === "waiting" || u.status === "uploading").length);
/** 正在进行的任务数，显示在导航的角标上。 */
export const runningCount = computed(() => tasks.server.filter((t) => t.status === "running").length + pendingUploads.value);
/** 这个对话是否有回答正在生成。 */
export const isGenerating = (conversationId: string) => tasks.server.some((t) => t.kind === "chat" && t.status === "running" && t.conversation_id === conversationId);

let timer = 0;
let started = false;
/** 马上问一次任务情况（刚发出提问、刚叫停之后调用）。 */
export async function refreshTasks() {
  window.clearTimeout(timer);
  try {
    tasks.server = (await api.tasks()).tasks;
  } catch {
    /* 问不到就等下一次 */
  }
  // 有任务在进行时问得勤一些
  if (started) timer = window.setTimeout(refreshTasks, tasks.server.some((t) => t.status === "running") ? 1500 : 8000);
}
export function startTasks() {
  if (started) return;
  started = true;
  void refreshTasks();
}

let nextId = 1;
let working = false;
const files = new Map<number, File>();

/** 把文件排进上传队列。逐个上传，一个失败不影响其余的；离开知识库页也会继续。 */
export function enqueueUploads(folderId: string, folderName: string, list: File[]) {
  for (const file of list) {
    const id = nextId++;
    files.set(id, file);
    tasks.uploads.push({ id, name: file.name, folderId, folderName, status: "waiting" });
  }
  void work();
}

async function work() {
  if (working) return;
  working = true;
  try {
    for (;;) {
      const next = tasks.uploads.find((u) => u.status === "waiting");
      if (!next) break;
      const file = files.get(next.id);
      next.status = "uploading";
      try {
        if (!file) throw new ApiError(0, "upload_failed", "");
        const r = await kb.uploadOne(next.folderId, file);
        if (r.rejected.length) Object.assign(next, { status: "failed", code: r.rejected[0].code });
        else next.status = "done";
      } catch (e) {
        Object.assign(next, { status: "failed", code: e instanceof ApiError ? e.code : "upload_failed" });
      }
      files.delete(next.id);
      tasks.kbTick++;
    }
  } finally {
    working = false;
    void refreshTasks();
  }
}

/** 清掉已经结束的任务（进行中的留着）。 */
export async function clearFinished() {
  tasks.uploads = tasks.uploads.filter((u) => u.status === "waiting" || u.status === "uploading");
  try {
    await api.clearTasks();
  } catch {
    /* 清不掉也无妨 */
  }
  await refreshTasks();
}
