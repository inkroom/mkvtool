<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

const media = ref(null);
const selectedStream = ref(null);
const subtitle = ref(null);
const subtitleContent = ref("");
const timestampOffset = ref(500);
const timestampStart = ref("");
const loading = ref(false);
const saving = ref(false);
const error = ref("");
const notice = ref("");
let unlistenDrop;

const streamGroups = computed(() => {
  const groups = { video: [], audio: [], subtitle: [], attachment: [], other: [] };
  for (const stream of media.value?.streams ?? []) {
    (groups[stream.streamType] ?? groups.other).push(stream);
  }
  return groups;
});

const duration = computed(() => {
  const seconds = Number(media.value?.duration);
  if (!Number.isFinite(seconds)) return "时长未知";
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remaining = Math.floor(seconds % 60);
  return [hours, minutes, remaining].map((value) => String(value).padStart(2, "0")).join(":");
});

const busy = computed(() => loading.value || saving.value);
const busyMessage = computed(() => {
  if (saving.value) return "正在重新混流并写入 MKV，请稍候…";
  if (selectedStream.value?.editable) return "正在读取字幕，请稍候…";
  return "正在分析 MKV 文件，请稍候…";
});

function showError(message) {
  error.value = typeof message === "string" ? message : String(message);
  notice.value = "";
}

async function loadFile(path) {
  if (!path) return;
  loading.value = true;
  error.value = "";
  notice.value = "";
  selectedStream.value = null;
  subtitle.value = null;
  subtitleContent.value = "";
  try {
    media.value = await invoke("inspect_mkv", { path });
    notice.value = `已加载 ${media.value.streams.length} 条流。`;
  } catch (reason) {
    media.value = null;
    showError(reason);
  } finally {
    loading.value = false;
  }
}

async function chooseFile() {
  try {
   const path = await invoke("pick_mkv_file");
    await loadFile(path);
  } catch (reason) {
    showError(reason);
  }
}

async function selectStream(stream) {
  selectedStream.value = stream;
  subtitle.value = null;
  subtitleContent.value = "";
  error.value = "";
  if (!stream.editable) return;
  loading.value = true;
  try {
    subtitle.value = await invoke("read_subtitle", {
      path: media.value.path,
      streamIndex: stream.index,
    });
    subtitleContent.value = subtitle.value.content;
  } catch (reason) {
    showError(reason);
  } finally {
    loading.value = false;
  }
}

async function saveSubtitle() {
  if (!media.value || !selectedStream.value || !subtitle.value) return;
  try {
    const defaultName = media.value.name.replace(/\.mkv$/i, "") + "-edited.mkv";
    const outputPath = await invoke("pick_output_file", { suggestedName: defaultName });
    if (!outputPath) return;
    saving.value = true;
    error.value = "";
    await invoke("save_subtitle", {
      inputPath: media.value.path,
      outputPath,
      streamIndex: selectedStream.value.index,
      content: subtitleContent.value,
    });
    notice.value = `已输出：${outputPath}`;
  } catch (reason) {
    showError(reason);
  } finally {
    saving.value = false;
  }
}

function streamLabel(stream) {
  const type = { video: "视频", audio: "音频", subtitle: "字幕", attachment: "附件" }[stream.streamType] ?? "其他";
  return `${type} #${stream.index}`;
}

function timestampMilliseconds(value) {
  const match = value.trim().match(/^(\d+):(\d{2}):(\d{2})[.,:](\d{2,3})$/);
  if (!match) return null;
  const [, hours, minutes, seconds, fraction] = match;
  return (
    (Number(hours) * 3600 + Number(minutes) * 60 + Number(seconds)) * 1000 +
    Number(fraction) * (fraction.length === 2 ? 10 : 1)
  );
}

function shiftTimestamp(value, milliseconds, separator = ".") {
  const match = value.match(/^(\d+):(\d{2}):(\d{2})[.,:](\d{2,3})$/);
  const sourceMilliseconds = timestampMilliseconds(value);
  if (!match || sourceMilliseconds === null) return value;
  const [, , , , fraction] = match;
  const totalMilliseconds = Math.max(0, sourceMilliseconds + milliseconds);
  const nextHours = Math.floor(totalMilliseconds / 3600000);
  const nextMinutes = Math.floor((totalMilliseconds % 3600000) / 60000);
  const nextSeconds = Math.floor((totalMilliseconds % 60000) / 1000);
  const nextFraction = fraction.length === 2
    ? Math.floor((totalMilliseconds % 1000) / 10)
    : totalMilliseconds % 1000;
  return `${String(nextHours).padStart(2, "0")}:${String(nextMinutes).padStart(2, "0")}:${String(nextSeconds).padStart(2, "0")}${separator}${String(nextFraction).padStart(fraction.length, "0")}`;
}

function shiftSubtitles(direction) {
  const milliseconds = Math.round(Number(timestampOffset.value) * direction);
  if (!Number.isFinite(milliseconds) || milliseconds === 0 || !subtitle.value) return;
  const startValue = timestampStart.value.trim();
  const startMilliseconds = startValue ? timestampMilliseconds(startValue) : null;
  if (startValue && startMilliseconds === null) {
    showError("起始时间格式无效，请输入 0:00:00.00 或 00:00:00,000。");
    return;
  }
  if (subtitle.value.format === "ass") {
    subtitleContent.value = subtitleContent.value.replace(/^Dialogue:\s*([^\r\n]*)$/gm, (line, fields) => {
      const values = fields.split(",");
      if (values.length < 3) return line;
      const lineStart = timestampMilliseconds(values[1]);
      if (lineStart === null || (startMilliseconds !== null && lineStart < startMilliseconds)) return line;
      values[1] = shiftTimestamp(values[1].trim(), milliseconds, ".").replace(/^0(?=\d:)/, "");
      values[2] = shiftTimestamp(values[2].trim(), milliseconds, ".").replace(/^0(?=\d:)/, "");
      return `Dialogue: ${values.join(",")}`;
    });
  } else {
    subtitleContent.value = subtitleContent.value.replace(
      /(\d{2,}:\d{2}:\d{2}[,.]\d{3})\s*-->\s*(\d{2,}:\d{2}:\d{2}[,.]\d{3})/g,
      (_, start, end) => {
        const lineStart = timestampMilliseconds(start);
        if (lineStart === null || (startMilliseconds !== null && lineStart < startMilliseconds)) return `${start} --> ${end}`;
        return `${shiftTimestamp(start, milliseconds, start.includes(",") ? "," : ".")} --> ${shiftTimestamp(end, milliseconds, end.includes(",") ? "," : ".")}`;
      },
    );
  }
  notice.value = `已将${startValue ? `${startValue} 起的` : "当前"}字幕${milliseconds > 0 ? "推迟" : "提前"} ${Math.abs(milliseconds)} ms。`;
}

onMounted(async () => {
  unlistenDrop = await getCurrentWindow().onDragDropEvent((event) => {
    if (event.payload.type === "drop") loadFile(event.payload.paths[0]);
  });
});

onBeforeUnmount(() => unlistenDrop?.());
</script>

<template>
  <main class="app-shell">
    <header class="topbar">
      <div>
        <p class="eyebrow">MATROSKA TOOL</p>
        <h1>MKV 字幕工作台</h1>
      </div>
      <button class="primary" :disabled="busy" @click="chooseFile">
        {{ loading ? "正在读取…" : "选择 MKV 文件" }}
      </button>
    </header>

    <div v-if="busy" class="loading-overlay" role="status" aria-live="polite" aria-label="处理中">
      <div class="loading-card">
        <span class="loading-spinner" aria-hidden="true"></span>
        <strong>{{ busyMessage }}</strong>
        <small>请不要关闭窗口或修改当前内容。</small>
      </div>
    </div>

    <section v-if="!media" class="drop-zone" @click="chooseFile">
      <div class="drop-icon">↓</div>
      <h2>拖放 MKV 文件到这里</h2>
      <p>或点击此处打开文件选择框。文件会先由 FFprobe 分析，所有流（包括附件）均会显示。</p>
    </section>

    <p v-if="error" class="message error">{{ error }}</p>
    <p v-if="notice" class="message notice">{{ notice }}</p>

    <section v-if="media" class="workspace">
      <aside class="sidebar">
        <div class="file-summary">
          <span class="file-badge">MKV</span>
          <div>
            <strong>{{ media.name }}</strong>
            <small>{{ duration }} · {{ media.streams.length }} 条流</small>
          </div>
        </div>

        <template v-for="(streams, type) in streamGroups" :key="type">
          <div v-if="streams.length" class="stream-group">
            <h2>{{ { video: "视频", audio: "音频", subtitle: "字幕", attachment: "附件", other: "其他" }[type] }}</h2>
            <button
              v-for="stream in streams"
              :key="stream.index"
              class="stream-row"
              :class="{ selected: selectedStream?.index === stream.index }"
              @click="selectStream(stream)"
            >
              <span class="stream-index">{{ streamLabel(stream) }}</span>
              <strong>{{ stream.codecName ?? "未知编码" }}</strong>
              <small>{{ stream.title || stream.language || stream.codecDescription || "无附加信息" }}</small>
              <span v-if="stream.editable" class="editable">可编辑</span>
              <span v-else-if="stream.defaultStream || stream.forced" class="flags">{{ stream.defaultStream ? "默认" : "" }} {{ stream.forced ? "强制" : "" }}</span>
            </button>
          </div>
        </template>
      </aside>

      <section class="editor-panel">
        <template v-if="subtitle">
          <div class="editor-header">
            <div>
              <p class="eyebrow">内存字幕编辑器</p>
              <h2>{{ streamLabel(selectedStream) }} · {{ subtitle.codecName }}</h2>
              <p>保存时仅重新编码当前字幕流；其余音频、视频、附件及流均以 copy 模式重新混流。</p>
            </div>
            <button class="primary" :disabled="busy" @click="saveSubtitle">
              {{ saving ? "正在重新混流…" : "导出 MKV" }}
            </button>
          </div>
          <div class="timestamp-toolbar">
            <span>时间戳偏移</span>
            <button @click="shiftSubtitles(-1)">提前</button>
            <input v-model.number="timestampOffset" type="number" min="1" step="100" aria-label="时间戳偏移毫秒" />
            <span>ms</span>
            <button @click="shiftSubtitles(1)">推迟</button>
          </div>
          <div class="timestamp-start-row">
            <label for="timestamp-start">起始时间</label>
            <input id="timestamp-start" v-model="timestampStart" class="timestamp-start" placeholder="留空为全部字幕" aria-label="时间轴偏移起始时间" />
            <small>格式：ASS 使用 <code>0:00:00.00</code>；SRT/WebVTT 使用 <code>00:00:00,000</code>。</small>
          </div>
          <textarea v-model="subtitleContent" spellcheck="false" aria-label="字幕内容" />
        </template>
        <div v-else class="empty-editor">
          <template v-if="selectedStream?.streamType === 'subtitle'">
            <h2>此字幕流暂不可编辑</h2>
            <p>当前仅支持文本字幕：SRT、ASS/SSA、WebVTT。该流会在导出其他字幕时完整保留。</p>
          </template>
          <template v-else-if="selectedStream">
            <h2>{{ streamLabel(selectedStream) }}</h2>
            <p>{{ selectedStream.codecDescription || "此流没有可显示的详细信息。" }}</p>
          </template>
          <template v-else>
            <h2>选择一个字幕流开始编辑</h2>
            <p>选择左侧标有“可编辑”的字幕流。编辑内容始终保存在内存中，直到你导出文件。</p>
          </template>
        </div>
      </section>
    </section>
  </main>
</template>

<style>
:root { font-family: Inter, ui-sans-serif, system-ui, sans-serif; color: #e8ecf6; background: #0d1220; font-synthesis: none; }
* { box-sizing: border-box; }
body { margin: 0; min-width: 760px; min-height: 100vh; background: radial-gradient(circle at 20% 0%, #202d4d, transparent 42%), #0d1220; }
button, textarea { font: inherit; }
button { cursor: pointer; }
button:disabled { cursor: wait; opacity: .65; }
.loading-overlay { position: fixed; z-index: 10; inset: 0; display: grid; place-items: center; padding: 24px; background: #080d18bd; backdrop-filter: blur(3px); }
.loading-card { display: grid; justify-items: center; gap: 13px; min-width: 300px; padding: 28px 32px; border: 1px solid #405c91; border-radius: 14px; color: #edf3ff; background: #14213ad9; box-shadow: 0 20px 60px #0008; text-align: center; }.loading-card small { color: #aebcd5; }.loading-spinner { width: 34px; height: 34px; border: 4px solid #7fe2b744; border-top-color: #7fe2b7; border-radius: 50%; animation: spin .8s linear infinite; } @keyframes spin { to { transform: rotate(360deg); } }
.app-shell { max-width: 1400px; min-height: 100vh; margin: auto; padding: 36px; }
.topbar, .editor-header, .file-summary { display: flex; align-items: center; justify-content: space-between; gap: 24px; }
.topbar { margin-bottom: 28px; }
.eyebrow { color: #75a7ff; font-size: .71rem; letter-spacing: .16em; font-weight: 800; margin: 0 0 6px; }
h1, h2, p { margin-top: 0; } h1 { margin-bottom: 0; font-size: 1.7rem; } h2 { font-size: 1.1rem; }
.primary { border: 0; border-radius: 9px; padding: 11px 16px; color: #061323; background: #7fe2b7; font-weight: 800; box-shadow: 0 7px 20px #0004; }
.drop-zone { min-height: 380px; display: grid; place-content: center; text-align: center; padding: 48px; border: 1px dashed #6d86b7; border-radius: 16px; background: #121b30a8; color: #aebad1; transition: .2s; }
.drop-zone:hover { border-color: #7fe2b7; background: #15233c; } .drop-zone h2 { color: #edf2ff; margin-bottom: 8px; } .drop-zone p { max-width: 520px; margin: 0; line-height: 1.6; }
.drop-icon { width: 46px; height: 46px; display: grid; place-items: center; margin: 0 auto 18px; border-radius: 50%; font-size: 1.8rem; background: #24375c; color: #7fe2b7; }
.message { margin: 0 0 18px; padding: 12px 15px; border-radius: 8px; white-space: pre-wrap; }.error { color: #ffc6c6; background: #4b202b; }.notice { color: #b7f6d7; background: #173d35; }
.workspace { display: grid; grid-template-columns: 340px minmax(420px, 1fr); min-height: 620px; overflow: hidden; border: 1px solid #2b3855; border-radius: 14px; background: #11192a; box-shadow: 0 20px 60px #0003; }
.sidebar { overflow: auto; padding: 16px 12px; border-right: 1px solid #2b3855; background: #101827; }.file-summary { justify-content: flex-start; padding: 8px 8px 20px; }.file-summary strong, .file-summary small { display: block; max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.file-summary small { color: #8e9bb7; margin-top: 4px; }.file-badge { padding: 7px 5px; border-radius: 5px; color: #0c1b2d; background: #7fe2b7; font-size: .68rem; font-weight: 900; }
.stream-group h2 { margin: 17px 8px 7px; color: #91a1c2; font-size: .76rem; letter-spacing: .1em; text-transform: uppercase; }.stream-row { position: relative; width: 100%; display: block; padding: 10px 9px; text-align: left; border: 1px solid transparent; border-radius: 8px; color: #e8ecf6; background: transparent; }.stream-row:hover { background: #1a2740; }.stream-row.selected { border-color: #4e75bc; background: #21365b; }.stream-row strong, .stream-row small, .stream-index { display: block; }.stream-index { color: #91a1c2; font-size: .7rem; }.stream-row strong { margin: 2px 0; font-size: .87rem; }.stream-row small { max-width: 220px; overflow: hidden; color: #9eadc9; font-size: .75rem; text-overflow: ellipsis; white-space: nowrap; }.editable, .flags { position: absolute; right: 8px; top: 9px; border-radius: 4px; padding: 2px 4px; font-size: .62rem; }.editable { color: #13261f; background: #7fe2b7; }.flags { color: #bfcae1; background: #31425f; }
.editor-panel { min-width: 0; display: flex; flex-direction: column; }.editor-header { padding: 25px 28px 17px; border-bottom: 1px solid #2b3855; }.editor-header h2 { margin: 0 0 6px; }.editor-header p:not(.eyebrow) { max-width: 570px; margin: 0; color: #98a8c6; font-size: .82rem; line-height: 1.45; }.timestamp-toolbar, .timestamp-start-row { display: flex; align-items: center; gap: 8px; padding: 9px 28px; color: #a9b6cf; font-size: .8rem; }.timestamp-toolbar { border-bottom: 1px solid #2b3855; }.timestamp-start-row { padding-top: 7px; padding-bottom: 11px; border-bottom: 1px solid #2b3855; }.timestamp-toolbar button { border: 1px solid #425a87; border-radius: 5px; padding: 4px 9px; color: #dbe7ff; background: #1d2c48; }.timestamp-toolbar input, .timestamp-start { width: 84px; border: 1px solid #425a87; border-radius: 5px; padding: 4px 7px; color: #e5edff; background: #101827; }.timestamp-start-row .timestamp-start { width: 132px; }.timestamp-start-row small { color: #8798ba; }.timestamp-start-row code { color: #c8d8f6; } textarea { flex: 1; min-height: 500px; resize: none; padding: 24px 28px; border: 0; outline: 0; color: #dfe8fb; background: #101827; font-family: "SFMono-Regular", Consolas, monospace; font-size: .86rem; line-height: 1.6; }.empty-editor { display: grid; flex: 1; place-content: center; padding: 40px; text-align: center; color: #99a8c4; }.empty-editor h2 { color: #e7edf9; }.empty-editor p { max-width: 430px; margin: 0; line-height: 1.6; }
</style>
