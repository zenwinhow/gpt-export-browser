import { memo, useEffect, useMemo, useRef, useState, useTransition } from "react";
import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { Virtuoso } from "react-virtuoso";
import { BookOpen, ChevronDown, ChevronRight, CircleAlert, Code2, FileImage, FileJson2, FolderOpen, GitBranch, Image, ListFilter, LoaderCircle, Moon, PanelRight, RefreshCw, Search, Settings, Sun, Wrench, X } from "lucide-react";
import "./App.css";

const markdownPlugins = [remarkGfm];
const highlightPlugins = [rehypeHighlight];
const CACHE_LIMIT = 8;
const markerPattern = /\uE200([a-z_]+)\uE202([\s\S]*?)\uE201/g;

type Conversation = { id: string; title: string; date: string; time: string; excerpt: string };
type SummaryItem = { id: string; title: string; createdAt?: number; updatedAt?: number };
type MediaRef = { assetPointer: string; path?: string; label: string; mime?: string; width?: number; height?: number; sizeBytes?: number };
type ToolStep = { commandId: string; command: string; outputId?: string; outputPreview?: string; outputBytes: number; language?: string };
type ReaderEntry = { id: string; kind: "message" | "toolRun" | "code" | "technical"; role: string; authorName?: string; createdAt?: number; contentType: string; recipient?: string; text: string; preview: string; textBytes: number; language?: string; branchCount: number; media: MediaRef[]; toolSteps: ToolStep[] };
type TechnicalSummary = { id: string; role: string; contentType: string; recipient?: string; textPreview: string; textBytes: number };
type ConversationView = { id: string; title: string; entries: ReaderEntry[]; technicalMessages: TechnicalSummary[]; branchPoints: { id: string; childCount: number }[] };
type LibrarySummary = { root: string; conversationCount: number; sourceBytes: number; conversations: SummaryItem[]; indexStatus: string; indexDurationMs: number };
type MessagePayload = { id: string; text: string; truncated: boolean };
type RenderItem = { kind: "entry"; entry: ReaderEntry } | { kind: "technicalProcess"; id: string; entries: ReaderEntry[] };

const previewView: ConversationView = {
  id: "preview", title: "富内容渲染样本", technicalMessages: [{ id: "system", role: "system", contentType: "text", textPreview: "隐藏系统上下文", textBytes: 8 }], branchPoints: [],
  entries: [
    { id: "preview-user", kind: "message", role: "user", contentType: "text", text: "展示 ChatGPT 导出中的富内容。", preview: "展示 ChatGPT 导出中的富内容。", textBytes: 16, branchCount: 0, media: [], toolSteps: [] },
    { id: "preview-rich", kind: "message", role: "assistant", authorName: "Assistant", contentType: "text", text: "这是 **Markdown**、实体 entity[\"software\",\"Atlas\",0] 与引用 citeturn0search0turn0search1。\n\nimage_group{\"layout\":\"carousel\",\"query\":[\"minimal space mission poster\",\"spacecraft cockpit\"],\"num_per_query\":1}\n\n下方是不会自动联网的图片组查询卡。", preview: "富内容样本", textBytes: 200, branchCount: 0, media: [{ assetPointer: "sediment://missing-demo", label: "示例缺失图片.png", mime: "image/png" }], toolSteps: [] },
    { id: "preview-run", kind: "toolRun", role: "assistant", contentType: "text", recipient: "python", text: "", preview: "Python 执行步骤", textBytes: 0, branchCount: 0, media: [], toolSteps: [{ commandId: "preview-command", command: "paths = [p for p in files if 'Handbook' in p]\npaths", outputId: "preview-output", outputPreview: "['HandbookFafang.aspx', 'HandbookFafang.aspx.cs']", outputBytes: 54, language: "python" }] },
    { id: "preview-source", kind: "technical", role: "tool", contentType: "tether_quote", text: "https://example.invalid/source\n来自网页、文件或浏览结果的内容会以折叠技术卡片保留。", preview: "网页引用与工具结果", textBytes: 52, branchCount: 0, media: [], toolSteps: [] },
    { id: "preview-product", kind: "message", role: "assistant", contentType: "text", text: "product{\"product_name\":\"示例本地商品卡\",\"merchant_name\":\"导出资料\",\"price\":\"$1,890\"}\n\nunknown_featureopaque-payload", preview: "商品与未知标记", textBytes: 90, branchCount: 0, media: [], toolSteps: [] },
  ],
};

const toDate = (seconds?: number) => seconds ? new Date(seconds * 1000) : undefined;
const shortDate = (seconds?: number) => toDate(seconds)?.toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" }) ?? "—";
const shortTime = (seconds?: number) => toDate(seconds)?.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" }) ?? "";
const toConversation = (item: SummaryItem): Conversation => ({ id: item.id, title: item.title, date: shortDate(item.updatedAt ?? item.createdAt), time: shortTime(item.updatedAt ?? item.createdAt), excerpt: "点击读取已导出的完整对话" });

function markerLabel(kind: string, payload: string) {
  if (kind === "cite" || kind === "filecite" || kind === "link") return payload.split("\uE202").filter(Boolean).join(" · ");
  try {
    const value = JSON.parse(payload);
    if (Array.isArray(value)) return String(value[1] ?? value[0] ?? kind);
    if (value && typeof value === "object") return String(value.product_name ?? value.title ?? value.name ?? kind);
  } catch { /* use compact raw fallback below */ }
  return payload.slice(0, 72) || kind;
}

function Marker({ kind, payload }: { kind: string; payload: string }) {
  if (kind === "image_group") {
    let details: { layout?: string; query?: string[]; num_per_query?: number } = {};
    try { details = JSON.parse(payload); } catch { return <div className="unknown-marker">未识别图片组</div>; }
    const queries = Array.isArray(details.query) ? details.query : [];
    return <section className="image-group-card"><div><Image size={16} /><strong>图片组</strong><span>{details.layout || "grid"} · 每组 {details.num_per_query ?? 1} 张</span></div><div className="query-chips">{queries.map((query) => <code key={query}>{query}</code>)}</div>{queries.length > 0 && <button onClick={() => openUrl(`https://www.google.com/search?tbm=isch&q=${encodeURIComponent(queries.join(" "))}`)}><Search size={13} />在浏览器搜索</button>}</section>;
  }
  if (["entity", "product_entity", "entity_metadata"].includes(kind)) return <span className="inline-token entity-token">{markerLabel(kind, payload)}</span>;
  if (["cite", "filecite", "link", "i"].includes(kind)) return <span className="inline-token citation-token">⌁ {markerLabel(kind, payload)}</span>;
  if (["product", "products"].includes(kind)) return <section className="product-card"><strong>{markerLabel(kind, payload)}</strong><span>导出中的商品推荐</span></section>;
  if (["video", "forecast", "finance", "navlist", "tlwm"].includes(kind)) return <section className="structured-marker"><strong>{kind}</strong><span>{markerLabel(kind, payload)}</span></section>;
  return <section className="unknown-marker"><strong>未识别导出内容：{kind}</strong><code>{payload.slice(0, 160)}</code></section>;
}

const RichMarkdown = memo(function RichMarkdown({ text }: { text: string }) {
  const chunks: Array<{ text?: string; kind?: string; payload?: string }> = [];
  let cursor = 0;
  for (const match of text.matchAll(markerPattern)) {
    if (match.index! > cursor) chunks.push({ text: text.slice(cursor, match.index) });
    chunks.push({ kind: match[1], payload: match[2] });
    cursor = match.index! + match[0].length;
  }
  if (cursor < text.length || chunks.length === 0) chunks.push({ text: text.slice(cursor) });
  return <>{chunks.map((chunk, index) => chunk.kind ? <Marker key={`${chunk.kind}-${index}`} kind={chunk.kind} payload={chunk.payload ?? ""} /> : chunk.text?.trim() ? <div className="markdown-body" key={index}><ReactMarkdown remarkPlugins={markdownPlugins} rehypePlugins={highlightPlugins}>{chunk.text}</ReactMarkdown></div> : null)}</>;
});

function MediaGallery({ media }: { media: MediaRef[] }) {
  if (!media.length) return null;
  return <div className="media-gallery">{media.map((asset) => {
    const src = asset.path && isTauri() ? convertFileSrc(asset.path) : undefined;
    if (!src) return <div className="asset-missing" key={asset.assetPointer}><FileImage size={18} /><span>{asset.label}</span><small>导出中未找到本地媒体</small></div>;
    if (asset.mime?.startsWith("video/")) return <video key={asset.assetPointer} controls preload="metadata" src={src} />;
    if (asset.mime?.startsWith("audio/")) return <audio key={asset.assetPointer} controls preload="metadata" src={src} />;
    return <figure key={asset.assetPointer}><img src={src} alt={asset.label} loading="lazy" /><figcaption>{asset.label}</figcaption></figure>;
  })}</div>;
}

function DeferredPayload({ root, conversationId, entryId, preview, language }: { root: string; conversationId: string; entryId: string; preview?: string; language?: string }) {
  const [payload, setPayload] = useState<MessagePayload>();
  const [loading, setLoading] = useState(false);
  const load = async () => { if (payload || loading || !root) return; setLoading(true); try { setPayload(await invoke<MessagePayload>("read_message_payload", { root, conversationId, entryId })); } finally { setLoading(false); } };
  return <details className="payload" onToggle={(event) => { if ((event.currentTarget as HTMLDetailsElement).open) void load(); }}><summary>{loading ? "正在读取完整内容…" : preview || "展开内容"}</summary>{payload && <><pre><code className={language ? `language-${language}` : undefined}>{payload.text}</code></pre>{payload.truncated && <small>显示已达到安全上限。</small>}</>}</details>;
}

function ToolRun({ entry, root, conversationId }: { entry: ReaderEntry; root: string; conversationId: string }) {
  const tool = entry.recipient || entry.language || "tool";
  return <details className="tool-run"><summary><span><Code2 size={15} />{tool} 执行步骤</span><span>{entry.toolSteps.length} 条</span></summary>{entry.toolSteps.map((step) => <div className="tool-step" key={step.commandId}><div className="tool-step-label">{step.language || tool}</div><pre><code>{step.command}</code></pre>{step.outputId ? <DeferredPayload root={root} conversationId={conversationId} entryId={step.outputId} preview={step.outputPreview} language={step.language} /> : <small>没有记录可用输出。</small>}</div>)}</details>;
}

function TechnicalEntry({ entry, root, conversationId }: { entry: ReaderEntry; root: string; conversationId: string }) {
  return <><MediaGallery media={entry.media} /><details className="technical-message"><summary><span><Wrench size={14} /><code>{entry.contentType}</code></span><span>{entry.preview || "技术节点"}</span></summary><DeferredPayload root={root} conversationId={conversationId} entryId={entry.id} preview="读取完整内容" language={entry.language} /></details></>;
}

function TechnicalProcess({ entries, root, conversationId }: { entries: ReaderEntry[]; root: string; conversationId: string }) {
  const [open, setOpen] = useState(false);
  const thoughtCount = entries.filter((entry) => entry.kind === "technical").length;
  const runCount = entries.filter((entry) => entry.kind === "toolRun").length;
  const labels = [thoughtCount ? `思考 ${thoughtCount} 条` : "", runCount ? `执行步骤 ${runCount} 条` : ""].filter(Boolean).join(" · ");
  return <div className="reader-entry compact-entry"><details className="technical-process" onToggle={(event) => setOpen((event.currentTarget as HTMLDetailsElement).open)}><summary><span><Wrench size={15} />技术过程</span><span>{labels}<ChevronRight size={15} /></span></summary>{open ? <div className="technical-process-content">{entries.map((entry) => entry.kind === "toolRun" ? <ToolRun key={entry.id} entry={entry} root={root} conversationId={conversationId} /> : <TechnicalEntry key={entry.id} entry={entry} root={root} conversationId={conversationId} />)}</div> : null}</details></div>;
}

const EntryRenderer = memo(function EntryRenderer({ entry, root, conversationId }: { entry: ReaderEntry; root: string; conversationId: string }) {
  if (entry.kind === "toolRun") return <div className="reader-entry compact-entry"><ToolRun entry={entry} root={root} conversationId={conversationId} /></div>;
  if (entry.kind === "technical") return <div className="reader-entry compact-entry"><TechnicalEntry entry={entry} root={root} conversationId={conversationId} /></div>;
  if (entry.kind === "code") return <div className="reader-entry compact-entry"><div className="standalone-code"><span>{entry.language || entry.recipient || "code"}</span><pre><code>{entry.text}</code></pre></div></div>;
  const isUser = entry.role === "user";
  return <article className={`message ${isUser ? "user-message" : "assistant-message"}`}><div className={`avatar ${isUser ? "user-avatar" : "assistant-avatar"}`}>{isUser ? "你" : "✦"}</div><div className="message-body"><div className="message-meta"><strong>{isUser ? "你" : entry.authorName || "Assistant"}</strong><span>{shortTime(entry.createdAt)}</span>{entry.branchCount > 1 ? <span className="branch-marker"><GitBranch size={12} /> {entry.branchCount} 个分支</span> : null}</div><RichMarkdown text={entry.text} /><MediaGallery media={entry.media} /></div></article>;
});

function toRenderItems(entries: ReaderEntry[]): RenderItem[] {
  const items: RenderItem[] = [];
  let technicalEntries: ReaderEntry[] = [];
  const flushTechnical = () => {
    if (!technicalEntries.length) return;
    items.push({ kind: "technicalProcess", id: `technical-${technicalEntries[0].id}`, entries: technicalEntries });
    technicalEntries = [];
  };
  for (const entry of entries) {
    if (entry.kind === "technical" || entry.kind === "toolRun") technicalEntries.push(entry);
    else { flushTechnical(); items.push({ kind: "entry", entry }); }
  }
  flushTechnical();
  return items;
}

function HiddenTechnical({ item, root, conversationId }: { item: TechnicalSummary; root: string; conversationId: string }) {
  return <details className="technical-message"><summary><span><code>{item.contentType}</code>{item.recipient ? <code>{item.recipient}</code> : null}</span><span>{item.textPreview || "隐藏节点"}</span></summary><DeferredPayload root={root} conversationId={conversationId} entryId={item.id} preview="读取完整内容" /></details>;
}

function App() {
  const [dark, setDark] = useState(false); const [detailsOpen, setDetailsOpen] = useState(true); const [libraryRoot, setLibraryRoot] = useState(""); const [libraryCount, setLibraryCount] = useState(1); const [librarySize, setLibrarySize] = useState("");
  const [imported, setImported] = useState<Conversation[]>([{ id: "preview", title: "富内容渲染样本", date: "预览", time: "", excerpt: "所有已发现导出类型的安全样本" }]); const [selectedId, setSelectedId] = useState("preview"); const [view, setView] = useState<ConversationView>(previewView); const [query, setQuery] = useState(""); const [searchResults, setSearchResults] = useState<Conversation[] | null>(null); const [loading, setLoading] = useState(false); const [error, setError] = useState(""); const [isPending, startTransition] = useTransition();
  const cache = useRef(new Map<string, ConversationView>()); const searchRequest = useRef(0);
  const rows = searchResults ?? imported; const selected = rows.find((item) => item.id === selectedId) ?? imported.find((item) => item.id === selectedId);
  const renderItems = useMemo(() => toRenderItems(view.entries), [view.entries]);
  useEffect(() => { if (!libraryRoot || selectedId === "preview") return; const cached = cache.current.get(selectedId); if (cached) { startTransition(() => setView(cached)); return; } let active = true; setLoading(true); invoke<ConversationView>("read_conversation", { root: libraryRoot, conversationId: selectedId }).then((next) => { if (!active) return; cache.current.set(selectedId, next); if (cache.current.size > CACHE_LIMIT) cache.current.delete(cache.current.keys().next().value!); startTransition(() => setView(next)); }).catch((reason) => active && setError(String(reason))).finally(() => active && setLoading(false)); return () => { active = false; }; }, [libraryRoot, selectedId, startTransition]);
  useEffect(() => { if (!libraryRoot) return; const needle = query.trim(); const request = ++searchRequest.current; if (!needle) { setSearchResults(null); return; } const timer = window.setTimeout(() => { invoke<SummaryItem[]>("search_conversations", { root: libraryRoot, query: needle }).then((items) => { if (request === searchRequest.current) setSearchResults(items.map(toConversation)); }).catch((reason) => request === searchRequest.current && setError(String(reason))); }, 180); return () => window.clearTimeout(timer); }, [libraryRoot, query]);
  const load = async (force = false) => { setError(""); setLoading(true); try { const path = force ? libraryRoot : await openDialog({ directory: true, multiple: false, title: "打开 ChatGPT 导出目录" }); if (!path || Array.isArray(path)) return; const summary = await invoke<LibrarySummary>(force ? "refresh_library" : "open_library", force ? { root: path } : { root: path, forceRefresh: false }); const next = summary.conversations.map(toConversation); cache.current.clear(); setSearchResults(null); setImported(next); setLibraryRoot(summary.root); setLibraryCount(summary.conversationCount); setLibrarySize(`${(summary.sourceBytes / 1024 / 1024).toFixed(0)} MB`); setSelectedId(next[0]?.id ?? ""); } catch (reason) { setError(String(reason)); } finally { setLoading(false); } };
  const technical = view.technicalMessages.length ? <details className="technical-drawer"><summary><span><Wrench size={14} />隐藏技术详情</span><span>{view.technicalMessages.length} 条 <ChevronRight size={15} /></span></summary><div className="technical-content">{view.technicalMessages.map((item) => <HiddenTechnical key={item.id} item={item} root={libraryRoot} conversationId={view.id} />)}</div></details> : null;
  return <div className={`app-shell ${dark ? "theme-dark" : ""}`}><header className="topbar"><div className="brand-lockup"><BookOpen size={18} /><span>Atlas</span></div><button className="toolbar-button open-button" onClick={() => void load()}><FolderOpen size={15} />打开资料库</button><div className="path-chip">{libraryRoot || "本地 ChatGPT 导出"}<ChevronDown size={13} /></div><label className="top-search"><Search size={16} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索会话…" aria-label="搜索会话" /></label><div className="top-actions">{libraryRoot ? <button className="icon-button" title="刷新索引" onClick={() => void load(true)}><RefreshCw size={16} /></button> : null}<button className="icon-button" title="切换亮暗主题" onClick={() => setDark((value) => !value)}>{dark ? <Sun size={17} /> : <Moon size={17} />}</button><button className="icon-button" title="切换详情栏" onClick={() => setDetailsOpen((value) => !value)}><PanelRight size={17} /></button><button className="icon-button" title="设置"><Settings size={17} /></button></div></header><main className="workspace"><aside className="timeline-rail"><div className="rail-title">全部时间</div><div className="timeline-year active">导出资料库</div>{["最近更新", "2025", "2024", "更早"].map((label, index) => <button className={`timeline-month ${index === 0 ? "active" : ""}`} key={label}>{label}</button>)}<div className="rail-note">标题、媒体索引均保存在导出目录内</div></aside><section className="conversation-pane"><div className="pane-heading"><span>{libraryCount.toLocaleString()} 个会话</span><button className="icon-button" title="筛选"><ListFilter size={16} /></button></div><label className="list-search"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索标题…" aria-label="搜索标题" /></label><div className="conversation-list">{rows.map((item) => <button key={item.id} className={`conversation-row ${selected?.id === item.id ? "selected" : ""}`} onClick={() => setSelectedId(item.id)}><span className="row-date">{item.date}</span><span className="row-main"><strong>{item.title}</strong><span>{item.excerpt}</span></span><span className="row-meta">{item.time}</span></button>)}</div></section><section className="reader-pane"><div className="reader-toolbar"><div className="reader-title"><h1>{view.title || selected?.title}</h1><span>{view.entries.length} 条阅读条目 · {view.technicalMessages.length} 条隐藏节点</span></div>{(loading || isPending) ? <span className="loading-label"><LoaderCircle size={15} />读取中</span> : null}</div><div className="reader-content"><Virtuoso data={renderItems} style={{ height: "100%" }} increaseViewportBy={800} itemContent={(_, item) => item.kind === "technicalProcess" ? <TechnicalProcess entries={item.entries} root={libraryRoot} conversationId={view.id} /> : <EntryRenderer entry={item.entry} root={libraryRoot} conversationId={view.id} />} components={{ Footer: () => technical }} /></div></section>{detailsOpen ? <aside className="details-pane"><div className="details-section"><div className="details-heading">渲染状态</div><p className="aside-copy">会话按 JSON 字节区间读取；工具输出和本地媒体按需加载。</p></div></aside> : null}</main><footer className="statusbar"><div className="status-left"><span className="status-dot" /><span>{loading ? "正在读取本地导出" : libraryRoot ? "本地资料库已加载" : "渲染样本预览"}</span><span className="status-separator" />{librarySize || "未打开导出"}<span className="status-separator" />{libraryCount.toLocaleString()} 个会话</div><div className="status-right"><span><FileJson2 size={13} /> conversations.json</span></div></footer>{error ? <div className="error-toast"><CircleAlert size={15} /><span>{error}</span><button onClick={() => setError("")}><X size={15} /></button></div> : null}</div>;
}
export default App;
