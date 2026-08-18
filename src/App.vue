<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { getCurrentWindow, invoke } from "./platform";

const mkvLogo = "/mkv-logo.svg";

const media = ref(null);
const selectedStream = ref(null);
const editorTabs = ref([]);
const activeTabId = ref(null);
const timestampOffset = ref(500);
const timestampStart = ref({ hours: "00", minutes: "00", seconds: "00", milliseconds: "00" });
const loading = ref(false);
const saving = ref(false);
const error = ref("");
const notice = ref("");
const languageMenuOpen = ref(false);
const languagePickerElement = ref(null);
const formatMenuOpen = ref(false);
const formatPickerElement = ref(null);
const fontAttachments = ref([]);
const subsetFonts = ref(true);
const isMacOS = /Macintosh|Mac OS X/.test(navigator.userAgent);
const languageNames = typeof Intl.DisplayNames === "function"
  ? new Intl.DisplayNames(["zh-CN"], { type: "language" })
  : null;
const NEW_SUBTITLE_STREAM_INDEX = 0xffffffff;

let unlistenDrop;
let subtitleTabsElement;
let subtitleTabsScrollFrame;
let subtitleTabsScrollTarget;

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

const defaultSubtitleStreamIndex = computed(() =>
  media.value?.streams.find((stream) => stream.streamType === "subtitle" && stream.defaultStream)?.index ?? null,
);

const activeTab = computed(() => editorTabs.value.find((tab) => tab.stream.index === activeTabId.value) ?? null);
const subtitle = computed(() => activeTab.value?.subtitle ?? null);
const subtitleContent = computed({
  get: () => activeTab.value?.content ?? "",
  set: (content) => {
    if (!activeTab.value) return;
    activeTab.value.content = content;
    updateTabDirty(activeTab.value);
  },
});
const subtitleLanguage = computed({
  get: () => activeTab.value?.language ?? "",
  set: (language) => {
    if (!activeTab.value) return;
    activeTab.value.language = language;
    updateTabDirty(activeTab.value);
  },
});
const subtitleTitle = computed({
  get: () => activeTab.value?.title ?? "",
  set: (title) => {
    if (!activeTab.value) return;
    activeTab.value.title = title;
    updateTabDirty(activeTab.value);
  },
});
const subtitleFormat = computed({
  get: () => activeTab.value?.format ?? "",
  set: (format) => changeSubtitleFormat(format),
});

const supportedSubtitleLanguages = [
  { value: "chi", label: "中文" },
  { value: "eng", label: "英语" },
  { value: "jpn", label: "日文" },
];

const subtitleLanguageLabel = computed(() => {
  const language = subtitleLanguage.value;
  if (!language) return "未指定";
  const supported = supportedSubtitleLanguages.find((option) => option.value === language);
  return supported?.label ?? `${readableLanguage(language)} (${language})`;
});

function updateTabDirty(tab) {
  tab.dirty = tab.content !== tab.originalContent ||
    tab.language !== tab.originalLanguage ||
    tab.title !== tab.originalTitle ||
    tab.format !== tab.originalFormat;
}

function toggleLanguageMenu() {
  languageMenuOpen.value = !languageMenuOpen.value;
}

function selectSubtitleLanguage(language) {
  subtitleLanguage.value = language;
  languageMenuOpen.value = false;
}

const subtitleFormats = [
  { value: "ass", label: "ass" },
  { value: "srt", label: "srt" },
];

const subtitleFormatLabel = computed(() => subtitleFormats.find((option) => option.value === subtitleFormat.value)?.label ?? subtitleFormat.value);

function toggleFormatMenu() {
  formatMenuOpen.value = !formatMenuOpen.value;
}

function assTimestampToSrt(value) {
  const match = value.trim().match(/^(\d+):(\d{2}):(\d{2})[.:](\d{2})$/);
  if (!match) return value.trim();
  const [, hours, minutes, seconds, centiseconds] = match;
  return `${hours.padStart(2, "0")}:${minutes}:${seconds},${centiseconds}0`;
}

function srtTimestampToAss(value) {
  const match = value.trim().match(/^(\d+):(\d{2}):(\d{2})[,.](\d{3})$/);
  if (!match) return value.trim();
  const [, hours, minutes, seconds, milliseconds] = match;
  return `${Number(hours)}:${minutes}:${seconds}.${String(Math.floor(Number(milliseconds) / 10)).padStart(2, "0")}`;
}

function assToSrt(content) {
  const entries = [];
  for (const match of content.replace(/\r/g, "").matchAll(/^Dialogue:\s*(.*)$/gm)) {
    const fields = match[1].split(",");
    if (fields.length < 10) continue;
    const text = fields.slice(9).join(",").replace(/\{[^}]*\}/g, "").replace(/\\[Nn]/g, "\n");
    entries.push(`${assTimestampToSrt(fields[1])} --> ${assTimestampToSrt(fields[2])}\n${text}`);
  }
  return entries.map((entry, index) => `${index + 1}\n${entry}`).join("\n\n") + (entries.length ? "\n" : "");
}

function srtToAss(content) {
  const entries = [];
  const blocks = content.replace(/\r/g, "").trim().split(/\n\s*\n/);
  for (const block of blocks) {
    const lines = block.split("\n");
    const timestampIndex = lines.findIndex((line) => /-->/.test(line));
    if (timestampIndex < 0) continue;
    const match = lines[timestampIndex].match(/([^\s]+)\s*-->\s*([^\s]+)/);
    if (!match) continue;
    const text = lines.slice(timestampIndex + 1).join("\\N").replace(/\r/g, "");
    entries.push(`Dialogue: 0,${srtTimestampToAss(match[1])},${srtTimestampToAss(match[2])},Default,,0,0,0,,${text}`);
  }
  return `[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n${entries.join("\n")}\n`;
}

function convertSubtitleFormat(content, from, to) {
  if (from === to) return content;
  return from === "ass" && to === "srt" ? assToSrt(content) : srtToAss(content);
}

function changeSubtitleFormat(format) {
  if (!activeTab.value || !subtitleFormats.some((option) => option.value === format)) return;
  const tab = activeTab.value;
  if (tab.format !== format) {
    tab.content = format === tab.originalFormat
      ? tab.originalContent
      : convertSubtitleFormat(tab.content, tab.format, format);
    tab.format = format;
    updateTabDirty(tab);
  }
  formatMenuOpen.value = false;
}

function closeLanguageMenu(event) {
  if (event.key === "Escape" || !languagePickerElement.value?.contains(event.target)) {
    languageMenuOpen.value = false;
  }
  if (event.key === "Escape" || !formatPickerElement.value?.contains(event.target)) {
    formatMenuOpen.value = false;
  }
}

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
  editorTabs.value = [];
  activeTabId.value = null;
  fontAttachments.value = [];
  subsetFonts.value = true;
  try {
    media.value = await invoke("inspect_mkv", { path });
    let foundDefaultSubtitle = false;
    for (const stream of media.value.streams) {
      stream.selected = true;
      if (stream.streamType === "subtitle" && stream.defaultStream) {
        if (foundDefaultSubtitle) stream.defaultStream = false;
        foundDefaultSubtitle = true;
      }
    }
  } catch (reason) {
    media.value = null;
    showError(reason);
  } finally {
    loading.value = false;
  }
}

function setDefaultSubtitle(stream, checked) {
  if (stream.streamType !== "subtitle") return;
  if (checked) {
    for (const candidate of media.value?.streams ?? []) {
      if (candidate.streamType === "subtitle") candidate.defaultStream = candidate.index === stream.index;
    }
  } else {
    stream.defaultStream = false;
  }
}

function setStreamSelected(stream, checked) {
  stream.selected = checked;
  if (!checked && stream.streamType === "subtitle") stream.defaultStream = false;
}

function createSubtitle() {
  if (!media.value || media.value.streams.some((stream) => stream.newStream)) return;
  const stream = {
    index: NEW_SUBTITLE_STREAM_INDEX,
    streamType: "subtitle",
    codecName: "srt",
    codecDescription: "SubRip subtitle",
    title: "",
    filename: null,
    language: "chi",
    defaultStream: false,
    forced: false,
    editable: true,
    selected: true,
    newStream: true,
    subtitle: { content: "", format: "srt", codecName: "srt" },
  };
  const firstSubtitle = media.value.streams.findIndex((candidate) => candidate.streamType === "subtitle");
  media.value.streams.splice(firstSubtitle < 0 ? media.value.streams.length : firstSubtitle, 0, stream);
  const tab = {
    stream,
    subtitle: stream.subtitle,
    originalContent: "",
    content: "",
    originalFormat: "srt",
    format: "srt",
    originalLanguage: "",
    language: "chi",
    originalTitle: "",
    title: "",
    newStream: true,
    dirty: true,
  };
  editorTabs.value.unshift(tab);
  selectedStream.value = stream;
  activeTabId.value = stream.index;
  error.value = "";
}

async function chooseFile() {
  try {
   const path = await invoke("pick_mkv_file");
    await loadFile(path);
  } catch (reason) {
    showError(reason);
  }
}

async function chooseFontAttachment() {
  try {
    const path = await invoke("pick_font_file");
    if (!path || fontAttachments.value.some((font) => font.path === path)) return;
    const name = path.split(/[\\/]/).pop() || path;
    const fontName = await invoke("read_font_name", { path });
    fontAttachments.value.push({
      path,
      name,
      fontName,
    });
  } catch (reason) {
    showError(reason);
  }
}

function removeFontAttachment(path) {
  fontAttachments.value = fontAttachments.value.filter((font) => font.path !== path);
}

async function minimizeWindow() {
  await getCurrentWindow().minimize();
}

async function closeWindow() {
  await getCurrentWindow().close();
}

async function selectStream(stream) {
  selectedStream.value = stream;
  error.value = "";
  if (!stream.editable) return;
  const existingTab = editorTabs.value.find((tab) => tab.stream.index === stream.index);
  if (existingTab) {
    activeTabId.value = existingTab.stream.index;
    return;
  }

  loading.value = true;
  try {
    if (!stream.subtitle) {
      stream.subtitle = await invoke("read_subtitle", {
        path: media.value.path,
        streamIndex: stream.index,
      });
    }
    const tab = {
      stream,
      subtitle: stream.subtitle,
      originalContent: stream.subtitle.content,
      content: stream.subtitle.content,
      originalFormat: stream.subtitle.format,
      format: stream.subtitle.format,
      originalLanguage: stream.language ?? "",
      language: stream.language ?? "",
      originalTitle: stream.title ?? "",
      title: stream.title ?? "",
      newStream: false,
      dirty: false,
    };
    const replaceIndex = editorTabs.value.findIndex((candidate) => !candidate.dirty);
    if (replaceIndex >= 0) {
      editorTabs.value.splice(replaceIndex, 1, tab);
    } else {
      editorTabs.value.push(tab);
    }
    activeTabId.value = stream.index;
  } catch (reason) {
    showError(reason);
  } finally {
    loading.value = false;
  }
}

function activateTab(tab) {
  selectedStream.value = tab.stream;
  activeTabId.value = tab.stream.index;
  error.value = "";
}

function scrollSubtitleTabs(event) {
  subtitleTabsElement = event.currentTarget;
  const delta = event.deltaX || event.deltaY;
  const multiplier = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? subtitleTabsElement.clientWidth : 1;
  const maximum = subtitleTabsElement.scrollWidth - subtitleTabsElement.clientWidth;
  subtitleTabsScrollTarget = Math.min(
    maximum,
    Math.max(0, (subtitleTabsScrollTarget ?? subtitleTabsElement.scrollLeft) + delta * multiplier),
  );
  if (subtitleTabsScrollFrame) return;

  const animate = () => {
    const distance = subtitleTabsScrollTarget - subtitleTabsElement.scrollLeft;
    if (Math.abs(distance) < 0.5) {
      subtitleTabsElement.scrollLeft = subtitleTabsScrollTarget;
      subtitleTabsScrollFrame = undefined;
      subtitleTabsScrollTarget = undefined;
      return;
    }
    subtitleTabsElement.scrollLeft += distance * 0.22;
    subtitleTabsScrollFrame = requestAnimationFrame(animate);
  };
  subtitleTabsScrollFrame = requestAnimationFrame(animate);
}

async function saveSubtitle() {
  if (!media.value) return;
  const selectedStreamIndices = media.value.streams
    .filter((stream) => stream.selected)
    .map((stream) => stream.index);
  if (!selectedStreamIndices.length) {
    showError("请至少选择一条要导出的流。");
    return;
  }
  for (const tab of editorTabs.value.filter((tab) => tab.newStream && tab.stream.selected)) {
    if (!tab.content.trim()) {
      showError("请填写新字幕文本。");
      return;
    }
    if (!tab.title.trim()) {
      showError("请填写新字幕标题。");
      return;
    }
    if (!tab.language) {
      showError("请选择新字幕语言。");
      return;
    }
  }
  const edits = editorTabs.value
    .filter((tab) => tab.dirty && tab.stream.selected)
    .map((tab) => ({
      streamIndex: tab.stream.index,
      content: tab.content,
      ...(tab.newStream ? { newStream: true, format: tab.format, language: tab.language, title: tab.title } : {}),
      ...(tab.language !== tab.originalLanguage ? { language: tab.language } : {}),
      ...(tab.title !== tab.originalTitle ? { title: tab.title } : {}),
      ...(tab.format !== tab.originalFormat ? { format: tab.format } : {}),
    }));
  const editableSubtitleStreams = media.value.streams.filter((stream) => stream.editable);
  const subtitleText = editableSubtitleStreams.every((stream) => stream.subtitle)
    ? editableSubtitleStreams.map((stream) =>
      editorTabs.value.find((tab) => tab.stream.index === stream.index)?.content ?? stream.subtitle.content,
    ).join("\n")
    : null;
  try {
    const defaultName = media.value.name.replace(/\.mkv$/i, "") + "-edited.mkv";
    const outputPath = await invoke("pick_output_file", { suggestedName: defaultName });
    if (!outputPath) return;
    saving.value = true;
    error.value = "";
    await invoke("save_subtitles", {
      inputPath: media.value.path,
      outputPath,
      edits,
      selectedStreamIndices,
      defaultSubtitleStreamIndex: defaultSubtitleStreamIndex.value,
      fontAttachments: fontAttachments.value.map((font) => ({ path: font.path })),
      subsetFonts: subsetFonts.value,
      subtitleText,
    });
    for (const tab of editorTabs.value) {
      if (tab.dirty && tab.stream.selected) {
        tab.originalContent = tab.content;
        tab.originalLanguage = tab.language;
        tab.originalTitle = tab.title;
        tab.originalFormat = tab.format;
        tab.stream.title = tab.title;
        tab.subtitle.format = tab.format;
        tab.dirty = false;
      }
    }
    notice.value = `已输出：${outputPath}`;
  } catch (reason) {
    showError(reason);
  } finally {
    saving.value = false;
  }
}

function streamLabel(stream) {
  const type = { video: "视频", audio: "音频", subtitle: "字幕", attachment: "附件" }[stream.streamType] ?? "其他";
  return stream.newStream ? "新建字幕" : `${type} #${stream.index}`;
}

function readableLanguage(language) {
  if (!language) return "";
  const normalized = language.trim().replace(/_/g, "-").toLowerCase();
  const commonLanguages = {
    und: "未指定",
    mul: "多语言",
    zho: "中文",
    chi: "中文",
    zh: "中文",
    "zh-cn": "中文",
    "zh-sg": "中文",
    chs: "中文",
    "zh-tw": "繁体中文",
    "zh-hk": "繁体中文",
    cht: "繁体中文",
    yue: "粤语",
    eng: "英语",
    jpn: "日语",
    kor: "韩语",
    fra: "法语",
    fre: "法语",
    deu: "德语",
    ger: "德语",
    spa: "西班牙语",
    ita: "意大利语",
    por: "葡萄牙语",
    rus: "俄语",
  };
  if (commonLanguages[normalized]) return commonLanguages[normalized];
  try {
    return languageNames?.of(normalized) ?? language;
  } catch {
    return language;
  }
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

function startTimestampMilliseconds({ hours, minutes, seconds, milliseconds }) {
  if (
    !/^\d{1,}$/.test(hours) ||
    !/^\d{1,2}$/.test(minutes) ||
    !/^\d{1,2}$/.test(seconds) ||
    !/^\d{1,3}$/.test(milliseconds) ||
    Number(minutes) > 59 ||
    Number(seconds) > 59
  ) return null;
  return (Number(hours) * 3600 + Number(minutes) * 60 + Number(seconds)) * 1000 + Number(milliseconds);
}

function normalizeTimestampStartPart(part, maxLength) {
  const value = timestampStart.value[part].replace(/\D/g, "").slice(0, maxLength);
  timestampStart.value[part] = value || "00";
}

function formatTimestampStart() {
  const { hours, minutes, seconds, milliseconds } = timestampStart.value;
  return `${hours.padStart(2, "0")}:${minutes.padStart(2, "0")}:${seconds.padStart(2, "0")}.${milliseconds.padStart(3, "0")}`;
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
  const startMilliseconds = startTimestampMilliseconds(timestampStart.value);
  if (startMilliseconds === null) {
    showError("起始时间无效，请检查分、秒和毫秒。");
    return;
  }
  if (activeTab.value?.format === "ass") {
    subtitleContent.value = subtitleContent.value.replace(/^Dialogue:\s*([^\r\n]*)$/gm, (line, fields) => {
      const values = fields.split(",");
      if (values.length < 3) return line;
      const lineStart = timestampMilliseconds(values[1]);
      if (lineStart === null || lineStart < startMilliseconds) return line;
      values[1] = shiftTimestamp(values[1].trim(), milliseconds, ".").replace(/^0(?=\d:)/, "");
      values[2] = shiftTimestamp(values[2].trim(), milliseconds, ".").replace(/^0(?=\d:)/, "");
      return `Dialogue: ${values.join(",")}`;
    });
  } else {
    subtitleContent.value = subtitleContent.value.replace(
      /(\d{2,}:\d{2}:\d{2}[,.]\d{3})\s*-->\s*(\d{2,}:\d{2}:\d{2}[,.]\d{3})/g,
      (_, start, end) => {
        const lineStart = timestampMilliseconds(start);
        if (lineStart === null || lineStart < startMilliseconds) return `${start} --> ${end}`;
        return `${shiftTimestamp(start, milliseconds, start.includes(",") ? "," : ".")} --> ${shiftTimestamp(end, milliseconds, end.includes(",") ? "," : ".")}`;
      },
    );
  }
  notice.value = `已将 ${formatTimestampStart()} 起的字幕${milliseconds > 0 ? "推迟" : "提前"} ${Math.abs(milliseconds)} ms。`;
}

onMounted(async () => {
  document.addEventListener("pointerdown", closeLanguageMenu);
  document.addEventListener("keydown", closeLanguageMenu);
  unlistenDrop = await getCurrentWindow().onDragDropEvent((event) => {
    if (event.payload.type === "drop") loadFile(event.payload.paths[0]);
  });
});

onBeforeUnmount(() => {
  unlistenDrop?.();
  document.removeEventListener("pointerdown", closeLanguageMenu);
  document.removeEventListener("keydown", closeLanguageMenu);
  if (subtitleTabsScrollFrame) cancelAnimationFrame(subtitleTabsScrollFrame);
});
</script>

<template>
  <main class="app-shell">
    <div class="window-bar" :class="{ 'macos-window-bar': isMacOS }" @selectstart.prevent @dragstart.prevent>
      <div class="window-drag-area" data-tauri-drag-region aria-hidden="true"></div>
      <img v-if="!isMacOS" class="window-logo" :src="mkvLogo" alt="" aria-hidden="true" />
      <span class="window-title">MKV 字幕工作台</span>
      <div class="window-controls" @mousedown.stop>
        <template v-if="isMacOS">
          <button class="window-control close-window" type="button" aria-label="关闭窗口" title="关闭" @click="closeWindow"><span aria-hidden="true">×</span></button>
          <button class="window-control minimize-window" type="button" aria-label="最小化窗口" title="最小化" @click="minimizeWindow"><span aria-hidden="true">−</span></button>
        </template>
        <template v-else>
          <button class="window-control minimize-window" type="button" aria-label="最小化窗口" title="最小化" @click="minimizeWindow"><span aria-hidden="true">−</span></button>
          <button class="window-control close-window" type="button" aria-label="关闭窗口" title="关闭" @click="closeWindow"><span aria-hidden="true">×</span></button>
        </template>
      </div>
    </div>

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
      <p>或点击此处打开文件选择框</p>
    </section>

    <p v-if="error" class="message error">{{ error }}</p>
    <div v-if="notice" class="notice" role="status" aria-live="polite">
      <span>{{ notice }}</span>
      <button class="notice-close" type="button" aria-label="关闭提示" title="关闭提示" @click="notice = ''">×</button>
    </div>

    <section v-if="media" class="workspace">
      <aside class="sidebar">
        <div class="file-summary">
          <img class="file-logo" :src="mkvLogo" alt="MKV" />
          <div>
            <strong :title="media.name" :aria-label="media.name">{{ media.name }}</strong>
            <div class="file-details">
              <small>{{ duration }} · {{ media.streams.length }} 条流</small>
              <button class="choose-file" :disabled="busy" @click="chooseFile">
                {{ loading ? "正在读取…" : "选择 MKV" }}
              </button>
            </div>
          </div>
        </div>

        <template v-for="(streams, type) in streamGroups" :key="type">
          <div v-if="streams.length || type === 'attachment' || type === 'subtitle'" class="stream-group">
            <div class="stream-group-heading">
              <h2>{{ { video: "视频", audio: "音频", subtitle: "字幕", attachment: "附件", other: "其他" }[type] }}</h2>
              <button
                v-if="type === 'subtitle'"
                class="new-subtitle-button"
                type="button"
                :disabled="busy || streams.some((stream) => stream.newStream)"
                @click="createSubtitle"
              >新建字幕</button>
            </div>
            <div
              v-for="stream in streams"
              :key="stream.index"
              class="stream-row"
              :class="{ selected: selectedStream?.index === stream.index }"
              role="button"
              tabindex="0"
              @click="selectStream(stream)"
              @keydown.enter.prevent="selectStream(stream)"
              @keydown.space.prevent="selectStream(stream)"
            >
              <label class="stream-selection" :title="stream.selected ? '导出此流' : '剔除此流'" @click.stop>
                <input
                  :checked="stream.selected"
                  type="checkbox"
                  :disabled="busy"
                  :aria-label="`${streamLabel(stream)} 导出`"
                  @change="setStreamSelected(stream, $event.target.checked)"
                />
              </label>
              <span class="stream-index">{{ streamLabel(stream) }}</span>
              <strong>{{ stream.codecName ?? "未知编码" }}</strong>
              <small v-if="stream.streamType === 'subtitle'" class="stream-details">
                <span v-if="stream.title">文件名：{{ stream.title }}</span>
                <span class="subtitle-language-row">
                  <span v-if="stream.language">语言：{{ readableLanguage(stream.language) }}({{stream.language}})</span>
                  <span v-else-if="!stream.title">{{ stream.codecDescription || "无附加信息" }}</span>
                  <span v-else></span>
                  <label class="subtitle-default-selection" title="设为默认字幕" @click.stop>
                    <input
                      :checked="stream.defaultStream"
                      type="checkbox"
                      :disabled="busy"
                      :aria-label="`${streamLabel(stream)} 默认字幕`"
                      @change="setDefaultSubtitle(stream, $event.target.checked)"
                    />
                    <span>默认字幕</span>
                  </label>
                </span>
              </small>
              <small v-else-if="stream.streamType === 'attachment'">
                {{ stream.filename ? `文件名：${stream.filename}` : (stream.title || stream.codecDescription || "无附加信息") }}
              </small>
              <small v-else>{{ stream.title || readableLanguage(stream.language) || stream.codecDescription || "无附加信息" }}</small>
              <span v-if="stream.editable || stream.defaultStream || stream.forced" class="stream-tags">
                <span v-if="stream.editable" class="editable">可编辑</span>
                <span v-if="stream.defaultStream || stream.forced" class="flags">{{ stream.defaultStream ? "默认" : "" }} {{ stream.forced ? "强制" : "" }}</span>
              </span>
            </div>
            <div v-if="type === 'attachment'" class="font-attachments">
              <button class="add-font-attachment" type="button" :disabled="busy" @click="chooseFontAttachment">选择字体文件</button>
              <label v-if="fontAttachments.length" class="font-subset-option">
                <input v-model="subsetFonts" type="checkbox" :disabled="busy" />
                <span>字体子集化</span>
              </label>
              <div v-for="font in fontAttachments" :key="font.path" class="font-attachment">
                <span class="font-attachment-details" :title="font.path">
                  <span v-if="font.fontName" class="font-attachment-name">{{ font.fontName }}</span>
                  <span class="font-attachment-filename">{{ font.name }}</span>
                </span>
                <button type="button" :disabled="busy" :aria-label="`移除 ${font.name}`" title="移除字体" @click="removeFontAttachment(font.path)">×</button>
              </div>
            </div>
          </div>
        </template>
      </aside>

      <section class="editor-panel">
        <template v-if="subtitle">
          <div class="editor-header">
            <div class="editor-heading">
              <h2>{{ streamLabel(selectedStream) }} · {{ subtitle.codecName }}</h2>
            </div>
            <div class="editor-actions">
              <label class="subtitle-title-field">
                <input v-model="subtitleTitle" :disabled="busy" type="text" aria-label="字幕标题" placeholder="未命名" />
              </label>
              <button class="primary" :disabled="busy" @click="saveSubtitle">
                {{ saving ? "正在重新混流…" : "导出 MKV" }}
              </button>
            </div>
          </div>
          <div class="timestamp-toolbar">
            <span>时间戳偏移</span>
            <button @click="shiftSubtitles(-1)">提前</button>
            <input v-model.number="timestampOffset" type="input" aria-label="时间戳偏移毫秒" />
            <span>ms</span>
            <button @click="shiftSubtitles(1)">推迟</button>
            <div ref="languagePickerElement" class="subtitle-language" :class="{ open: languageMenuOpen }">
              <span>语言</span>
              <button
                class="language-picker-trigger"
                type="button"
                role="combobox"
                aria-haspopup="listbox"
                :aria-expanded="languageMenuOpen"
                aria-controls="subtitle-language-options"
                @click="toggleLanguageMenu"
              >
                <span class="language-picker-label">{{ subtitleLanguageLabel }}</span>
                <span class="language-picker-arrow" aria-hidden="true"></span>
              </button>
              <div v-if="languageMenuOpen" id="subtitle-language-options" class="language-picker-menu" role="listbox" aria-label="字幕语言">
                <button
                  v-for="option in supportedSubtitleLanguages"
                  :key="option.value"
                  class="language-picker-option"
                  :class="{ selected: subtitleLanguage === option.value }"
                  type="button"
                  role="option"
                  :aria-selected="subtitleLanguage === option.value"
                  @click="selectSubtitleLanguage(option.value)"
                >
                  {{ option.label }}
                </button>
              </div>
            </div>
            <div ref="formatPickerElement" class="subtitle-language subtitle-format" :class="{ open: formatMenuOpen }">
              <span>格式</span>
              <button
                class="language-picker-trigger"
                type="button"
                role="combobox"
                aria-haspopup="listbox"
                :aria-expanded="formatMenuOpen"
                aria-controls="subtitle-format-options"
                @click="toggleFormatMenu"
              >
                <span class="language-picker-label">{{ subtitleFormatLabel }}</span>
                <span class="language-picker-arrow" aria-hidden="true"></span>
              </button>
              <div v-if="formatMenuOpen" id="subtitle-format-options" class="language-picker-menu" role="listbox" aria-label="字幕格式">
                <button
                  v-for="option in subtitleFormats"
                  :key="option.value"
                  class="language-picker-option"
                  :class="{ selected: subtitleFormat === option.value }"
                  type="button"
                  role="option"
                  :aria-selected="subtitleFormat === option.value"
                  @click="changeSubtitleFormat(option.value)"
                >
                  {{ option.label }}
                </button>
              </div>
            </div>
          </div>
          <div class="timestamp-start-row">
            <div class="timestamp-start-controls">
              <span class="timestamp-start-label">起始时间</span>
              <div class="timestamp-start-inputs" aria-label="时间轴偏移起始时间">
                <input v-model="timestampStart.hours" class="timestamp-start" inputmode="numeric" maxlength="2" aria-label="起始时间小时" @input="normalizeTimestampStartPart('hours', 2)" />
                <span>:</span>
                <input v-model="timestampStart.minutes" class="timestamp-start" inputmode="numeric" maxlength="2" aria-label="起始时间分钟" @input="normalizeTimestampStartPart('minutes', 2)" />
                <span>:</span>
                <input v-model="timestampStart.seconds" class="timestamp-start" inputmode="numeric" maxlength="2" aria-label="起始时间秒" @input="normalizeTimestampStartPart('seconds', 2)" />
                <span>.</span>
                <input v-model="timestampStart.milliseconds" class="timestamp-start milliseconds" inputmode="numeric" maxlength="3" aria-label="起始时间毫秒" @input="normalizeTimestampStartPart('milliseconds', 3)" />
              </div>
            </div>
            <div class="subtitle-tabs" role="tablist" aria-label="已打开的字幕" @wheel.prevent="scrollSubtitleTabs">
              <button
                v-for="tab in editorTabs"
                :key="tab.stream.index"
                class="subtitle-tab"
                :class="{ active: activeTabId === tab.stream.index, dirty: tab.dirty }"
                type="button"
                role="tab"
                :aria-selected="activeTabId === tab.stream.index"
                @click="activateTab(tab)"
              >
                {{ streamLabel(tab.stream) }}<span v-if="tab.dirty" aria-label="已编辑">*</span>
              </button>
            </div>
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
            <p>选择左侧标有“可编辑”的字幕流。</p>
          </template>
        </div>
      </section>
    </section>
  </main>
</template>

<style>
:root { font-family: Inter, ui-sans-serif, system-ui, sans-serif; color: #e8ecf6; background: #0d1220; font-synthesis: none; }
* { box-sizing: border-box; }
body { margin: 0; min-width: 760px; height: 100vh; overflow: hidden; background: radial-gradient(circle at 20% 0%, #202d4d, transparent 42%), #0d1220; }
button, textarea { font: inherit; }
button { cursor: pointer; }
button:disabled { cursor: wait; opacity: .65; }
.sidebar, textarea { scrollbar-width: thin; scrollbar-color: #536987 #101827; }
.sidebar::-webkit-scrollbar, textarea::-webkit-scrollbar { width: 10px; height: 10px; }
.sidebar::-webkit-scrollbar-track, textarea::-webkit-scrollbar-track { background: #101827; }
.sidebar::-webkit-scrollbar-thumb, textarea::-webkit-scrollbar-thumb { min-height: 36px; border: 2px solid #101827; border-radius: 999px; background: #536987; }
.sidebar::-webkit-scrollbar-thumb:hover, textarea::-webkit-scrollbar-thumb:hover { background: #7188aa; }
.sidebar::-webkit-scrollbar-corner, textarea::-webkit-scrollbar-corner { background: #101827; }
.sidebar::-webkit-scrollbar-button, textarea::-webkit-scrollbar-button { width: 0; height: 0; }
.loading-overlay { position: fixed; z-index: 10; inset: 0; display: grid; place-items: center; padding: 24px; background: #080d18bd; backdrop-filter: blur(3px); }
.loading-card { display: grid; justify-items: center; gap: 13px; min-width: 300px; padding: 28px 32px; border: 1px solid #405c91; border-radius: 14px; color: #edf3ff; background: #14213ad9; box-shadow: 0 20px 60px #0008; text-align: center; }.loading-card small { color: #aebcd5; }.loading-spinner { width: 34px; height: 34px; border: 4px solid #7fe2b744; border-top-color: #7fe2b7; border-radius: 50%; animation: spin .8s linear infinite; } @keyframes spin { to { transform: rotate(360deg); } }
.app-shell { width: 100%; height: 100vh; min-height: 0; display: flex; flex-direction: column; }
.window-bar { position: relative; z-index: 11; height: 34px; flex: 0 0 34px; display: flex; align-items: center; border-bottom: 1px solid #283650; color: #aebbd4; background: #101827; user-select: none; -webkit-user-select: none; }.window-drag-area { position: absolute; inset: 0; }.window-logo { position: relative; z-index: 1; width: 20px; height: 20px; margin-left: 9px; object-fit: contain; pointer-events: none; }.window-title { position: absolute; z-index: 1; inset: 0; display: grid; place-items: center; pointer-events: none; font-size: .72rem; font-weight: 700; }.window-controls { z-index: 2; align-self: stretch; display: flex; margin-left: auto; }.window-control { width: 42px; height: 34px; display: grid; place-items: center; padding: 0; border: 0; color: #cbd7ec; background: transparent; font-size: 1.05rem; line-height: 1; }.window-control:hover { background: #25344f; }.close-window:hover { color: #fff; background: #bf3944; }.macos-window-bar .window-controls { align-items: center; gap: 8px; margin-right: auto; margin-left: 0; padding: 0 12px; }.macos-window-bar .window-control { width: 12px; height: 12px; display: flex; align-items: center; justify-content: center; border-radius: 50%; color: transparent; font-size: .68rem; font-weight: 800; line-height: 12px; }.macos-window-bar .window-control span { display: block; height: 12px; line-height: 11px; transform: translateY(-.5px); }.macos-window-bar .close-window { background: #ff5f57; }.macos-window-bar .minimize-window { background: #febc2e; }.macos-window-bar .window-control:hover { color: #4d3220; }.macos-window-bar .close-window:hover { background: #ff5f57; }.macos-window-bar .minimize-window:hover { background: #febc2e; }
.editor-header, .file-summary { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.eyebrow { color: #75a7ff; font-size: .71rem; letter-spacing: .16em; font-weight: 800; margin: 0 0 4px; }
h2, p { margin-top: 0; } h2 { font-size: 1.1rem; }
.primary { border: 0; border-radius: 9px; padding: 8px 12px; color: #061323; background: #7fe2b7; font-weight: 800; box-shadow: 0 7px 20px #0004; }
.drop-zone { flex: 1; min-height: 0; display: grid; place-content: center; text-align: center; padding: 48px; border: 1px dashed #6d86b7; background: #121b30a8; color: #aebad1; transition: .2s; }
.drop-zone:hover { border-color: #7fe2b7; background: #15233c; } .drop-zone h2 { color: #edf2ff; margin-bottom: 8px; } .drop-zone p { max-width: 520px; margin: 0; line-height: 1.6; }
.drop-icon { width: 46px; height: 46px; display: grid; place-items: center; margin: 0 auto 18px; border-radius: 50%; font-size: 1.8rem; background: #24375c; color: #7fe2b7; }
.message { margin: 0; padding: 10px 13px; border-radius: 0; white-space: pre-wrap; }.error { color: #ffc6c6; background: #4b202b; }.notice { position: fixed; z-index: 9; top: 46px; right: 18px; display: flex; align-items: center; gap: 12px; max-width: min(440px, calc(100vw - 36px)); padding: 10px 10px 10px 14px; border: 1px solid #368568; border-radius: 8px; color: #b7f6d7; background: #173d35; box-shadow: 0 12px 32px #0006; }.notice-close { width: 24px; height: 24px; flex: 0 0 24px; padding: 0; border: 0; border-radius: 5px; color: #d5ffe8; background: transparent; font-size: 1.2rem; line-height: 1; }.notice-close:hover { background: #28604f; }
.workspace { flex: 1; min-height: 0; display: grid; grid-template-columns: 340px minmax(420px, 1fr); overflow: hidden; background: #11192a; }
.sidebar { min-height: 0; overflow: auto; padding: 16px 12px; border-right: 1px solid #2b3855; background: #101827; }.file-summary { justify-content: flex-start; padding: 8px 8px 20px; }.file-logo { width: 34px; height: 34px; flex: 0 0 34px; object-fit: contain; }.file-summary strong, .file-summary small { display: block; max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.file-summary small { color: #8e9bb7; }.file-details { display: flex; align-items: center; gap: 8px; margin-top: 4px; }.choose-file { flex: 0 0 auto; padding: 3px 6px; border: 1px solid #425a87; border-radius: 5px; color: #dbe7ff; background: #1d2c48; font-size: .7rem; }.choose-file:hover { border-color: #7fe2b7; color: #dfffee; }
.stream-group-heading { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin: 17px 8px 7px; }.stream-group-heading h2 { margin: 0; color: #91a1c2; font-size: .76rem; letter-spacing: .1em; text-transform: uppercase; }.new-subtitle-button { border: 1px solid #465e8c; border-radius: 5px; padding: 4px 8px; color: #dbe8ff; background: #1c2b47; font-size: .72rem; white-space: nowrap; }.new-subtitle-button:hover:not(:disabled) { border-color: #7fe2b7; color: #effff7; background: #23445a; }.new-subtitle-button:disabled { opacity: .5; cursor: not-allowed; }.stream-row { position: relative; width: 100%; display: block; padding: 10px 9px 10px 34px; text-align: left; border: 1px solid transparent; border-radius: 8px; color: #e8ecf6; background: transparent; cursor: pointer; }.stream-row:hover { background: #1a2740; }.stream-row:focus-visible { outline: 2px solid #7fe2b7; outline-offset: -2px; }.stream-row.selected { border-color: #4e75bc; background: #21365b; }.stream-selection { position: absolute; top: 12px; left: 10px; display: grid; place-items: center; cursor: pointer; }.stream-selection input { width: 15px; height: 15px; margin: 0; accent-color: #7fe2b7; cursor: inherit; }.stream-selection input:disabled { cursor: wait; }.stream-row strong, .stream-row small, .stream-index { display: block; }.stream-index { color: #91a1c2; font-size: .7rem; }.stream-row strong { margin: 2px 0; font-size: .87rem; }.stream-row small { max-width: 220px; overflow: hidden; color: #9eadc9; font-size: .75rem; text-overflow: ellipsis; white-space: nowrap; }.stream-row .stream-details { overflow: visible; text-overflow: clip; white-space: normal; line-height: 1.35; }.stream-details span { display: block; overflow-wrap: anywhere; }.stream-tags { position: absolute; top: 9px; right: 8px; display: flex; gap: 4px; }.editable, .flags { border-radius: 4px; padding: 2px 4px; font-size: .62rem; white-space: nowrap; }.editable { color: #13261f; background: #7fe2b7; }.flags { color: #bfcae1; background: #31425f; }
.font-attachments { display: grid; gap: 5px; margin: 5px 0 4px; padding: 0 8px; }.add-font-attachment { width: 100%; border: 1px dashed #536987; border-radius: 5px; padding: 6px 8px; color: #bcd0ee; background: #17243b; font-size: .73rem; text-align: left; }.add-font-attachment:hover { border-color: #7fe2b7; color: #e5fff3; background: #1b3245; }.font-attachment { min-width: 0; display: flex; align-items: center; gap: 5px; padding: 4px 5px 4px 7px; border: 1px solid #354969; border-radius: 5px; color: #aebcd5; background: #151f32; font-size: .7rem; }.font-attachment-details { min-width: 0; flex: 1; display: grid; gap: 1px; }.font-attachment-name, .font-attachment-filename { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.font-attachment-name { color: #d7e5fa; }.font-attachment-filename { color: #7f91b0; font-size: .66rem; }.font-attachment button { width: 19px; height: 19px; flex: 0 0 19px; padding: 0; border: 0; border-radius: 3px; color: #b8c7e1; background: transparent; font-size: 1rem; line-height: 1; }.font-attachment button:hover { color: #fff; background: #6a3441; }
.font-subset-option { display: flex; align-items: center; gap: 6px; color: #bcd0ee; font-size: .73rem; cursor: pointer; }.font-subset-option input { width: 14px; height: 14px; margin: 0; accent-color: #7fe2b7; cursor: inherit; }.font-subset-option input:disabled { cursor: wait; }
.editor-panel { min-width: 0; min-height: 0; display: flex; flex-direction: column; }.editor-header { padding: 18px 24px 13px; border-bottom: 1px solid #2b3855; }.editor-heading { min-width: 0; flex: 1; }.editor-header h2 { margin: 0 0 4px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.editor-header p:not(.eyebrow) { max-width: 570px; margin: 0; color: #98a8c6; font-size: .82rem; line-height: 1.45; }.editor-actions { display: flex; flex: 0 1 auto; align-items: flex-end; gap: 10px; }.subtitle-title-field { min-width: 0; display: grid; gap: 3px; color: #a9b6cf; font-size: .72rem; }.subtitle-title-field input { width: min(240px, 28vw); min-width: 120px; padding: 5px 7px; border: 1px solid #425a87; border-radius: 5px; color: #e5edff; background: #101827; }.subtitle-title-field input:focus { outline: 2px solid #7fe2b7; outline-offset: 1px; }.editor-header .primary { flex: 0 0 auto; padding: 6px 10px; font-size: .8rem; }.timestamp-toolbar, .timestamp-start-row { display: flex; align-items: center; gap: 8px; padding: 8px 24px; color: #a9b6cf; font-size: .8rem; }.timestamp-toolbar { border-bottom: 1px solid #2b3855; }.timestamp-start-row { min-width: 0; align-items: flex-end; padding-top: 6px; padding-bottom: 0; border-bottom: 1px solid #2b3855; }.timestamp-start-label, .timestamp-start-inputs { margin-bottom: 9px; }.timestamp-start-label { white-space: nowrap; }.timestamp-start-inputs { display: flex; flex: 0 0 auto; align-items: center; gap: 3px; }.subtitle-tabs { align-self: stretch; min-width: 0; display: flex; flex: 1; align-items: flex-end; gap: 0; overflow-x: auto; scrollbar-width: none; }.subtitle-tabs::-webkit-scrollbar { display: none; }.subtitle-tab { position: relative; z-index: 0; flex: 0 0 auto; max-width: 132px; margin: 0 0 -1px -1px; overflow: hidden; border: 1px solid #33486e; border-bottom-color: #2b3855; border-radius: 5px 5px 0 0; padding: 7px 9px 8px; color: #9eafcf; background: #18243a; font-size: .72rem; text-overflow: ellipsis; white-space: nowrap; }.subtitle-tab:first-child { margin-left: 0; }.subtitle-tab:hover { z-index: 1; border-color: #536987; color: #dbe7ff; background: #223451; }.subtitle-tab.active { z-index: 2; border-color: #6d92d6; border-bottom: 0; padding-bottom: 9px; color: #edf3ff; background: #101827; }.subtitle-tab.dirty { color: #d8f1a6; }.subtitle-tab span { margin-left: 3px; color: #7fe2b7; }.timestamp-toolbar button { border: 1px solid #425a87; border-radius: 5px; padding: 4px 9px; color: #dbe7ff; background: #1d2c48; }.timestamp-toolbar input, .timestamp-start { width: 84px; border: 1px solid #425a87; border-radius: 5px; padding: 4px 7px; color: #e5edff; background: #101827; }.timestamp-start-inputs .timestamp-start { width: 36px; padding: 4px 3px; text-align: center; }.timestamp-start-inputs .milliseconds { width: 44px; } textarea { flex: 1; min-height: 0; resize: none; padding: 20px 24px; border: 0; outline: 0; color: #dfe8fb; background: #101827; font-family: "SFMono-Regular", Consolas, monospace; font-size: .86rem; line-height: 1.6; }.empty-editor { display: grid; flex: 1; min-height: 0; place-content: center; padding: 32px; text-align: center; color: #99a8c4; }.empty-editor h2 { color: #e7edf9; }.empty-editor p { max-width: 430px; margin: 0; line-height: 1.6; }
.timestamp-start-row { border-bottom: 0; }
.timestamp-start-row { position: relative; gap: 8px; }
.timestamp-start-controls { position: relative; align-self: stretch; display: flex; flex: 0 0 auto; align-items: center; gap: 8px; }
.timestamp-start-controls::after { position: absolute; right: -8px; bottom: 0; left: -24px; content: ""; border-bottom: 1px solid #2b3855; }
.timestamp-start-label, .timestamp-start-inputs { margin-bottom: 0; }
.subtitle-tabs { overflow-y: hidden; }
.subtitle-tabs::after { content: ""; flex: 1 0 0; align-self: flex-end; border-bottom: 1px solid #2b3855; }
.subtitle-tab { margin-bottom: 0; }
.subtitle-language-row { display: flex !important; align-items: center; justify-content: space-between; gap: 8px; }
.subtitle-language-row > :first-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.stream-row .stream-details { width: 100%; max-width: none; }
.subtitle-default-selection { flex: 0 0 auto; display: flex; align-items: center; gap: 4px; color: #9eadc9; font-size: .65rem; cursor: pointer; }
.subtitle-default-selection input { width: 14px; height: 14px; margin: 0; accent-color: #7fe2b7; cursor: inherit; }
.subtitle-default-selection input:disabled { cursor: wait; }
.subtitle-language { position: relative; display: flex; align-items: center; gap: 6px; margin-left: 6px; }
.language-picker-trigger { width: 62px; display: flex; align-items: center; justify-content: space-between; gap: 5px; border: 1px solid #425a87; border-radius: 5px; padding: 4px 6px; color: #e5edff; background: #101827; text-align: left; }
.language-picker-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.language-picker-trigger:hover, .subtitle-language.open .language-picker-trigger { border-color: #6d92d6; background: #18243a; }
.language-picker-trigger:focus-visible, .language-picker-option:focus-visible { outline: 2px solid #7fe2b7; outline-offset: 1px; }
.language-picker-arrow { width: 6px; height: 6px; flex: 0 0 6px; border-right: 1px solid #a9b6cf; border-bottom: 1px solid #a9b6cf; transform: rotate(45deg) translateY(-2px); transition: transform .15s ease; }
.subtitle-language.open .language-picker-arrow { transform: rotate(225deg) translate(-2px, -1px); }
.language-picker-menu { position: absolute; z-index: 4; top: calc(100% + 5px); right: 0; width: 62px; overflow: hidden; border: 1px solid #536987; border-radius: 5px; padding: 3px; background: #101827; box-shadow: 0 10px 22px #0008; }
.subtitle-format .language-picker-trigger, .subtitle-format .language-picker-menu { width: 55px; }
.language-picker-option { width: 100%; border: 0; border-radius: 3px; padding: 6px 7px; color: #cbd7ec; background: transparent; text-align: left; }
.language-picker-option:hover, .language-picker-option:focus-visible { color: #edf3ff; background: #253b60; }
.language-picker-option.selected { color: #d8f1a6; background: #1d3840; }
</style>
