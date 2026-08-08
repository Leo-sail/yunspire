import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const creationRoot = resolve(projectRoot, "resources/creation");
const themeDirectory = resolve(creationRoot, "themes");
const componentDirectory = resolve(creationRoot, "components");
const templateDirectory = resolve(creationRoot, "templates");
const catalogPath = resolve(creationRoot, "catalog/creation-catalog.json");
const runtimeBundlePath = resolve(creationRoot, "catalog/runtime-bundle.json");
const writingResourcesPath = resolve(creationRoot, "catalog/writing-resources.json");

const CATALOG_VERSION = "0.4.0";
const RUNTIME_VERSION = "0.3.0";
const MIN_RUNTIME_VERSION = "0.1.2";
const REPOSITORY = "https://github.com/Leo-sail/yunspire";

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function unique(values) {
  return [...new Set(values)];
}

function resourceName(value, suffix) {
  return [...String(value)].length >= 3 ? String(value) : String(value) + suffix;
}

function json(value) {
  return JSON.stringify(value, null, 2) + "\n";
}

function hash(content) {
  return "sha256:" + createHash("sha256").update(content).digest("hex");
}

function normalizedText(content) {
  return String(content || "").replace(/\s+/gu, " ").trim();
}

function hexToRgb(color) {
  return [
    Number.parseInt(color.slice(1, 3), 16),
    Number.parseInt(color.slice(3, 5), 16),
    Number.parseInt(color.slice(5, 7), 16),
  ];
}

function rgbToHex(rgb) {
  return "#" + rgb.map((channel) => Math.max(0, Math.min(255, Math.round(channel))).toString(16).padStart(2, "0")).join("");
}

function mixHex(first, second, secondWeight) {
  const a = hexToRgb(first);
  const b = hexToRgb(second);
  return rgbToHex(a.map((channel, index) => channel * (1 - secondWeight) + b[index] * secondWeight));
}

function tuneHex(color, amount) {
  return mixHex(color, amount >= 0 ? "#ffffff" : "#000000", Math.abs(amount));
}

function sourceFor(kind) {
  return {
    policy: "yunspire_first_party",
    authoredBy: "Yunspire",
    repository: REPOSITORY,
    upstreamCodeCopied: false,
    researchBoundary: "This " + kind + " is an original Yunspire project asset. External repositories informed capability research only; no upstream code, prompt, template, wording, or visual asset is included.",
  };
}

function licenseFor(kind) {
  return {
    scope: "yunspire_first_party_project_asset",
    notice: "Original Yunspire " + kind + "; use is governed by the repository LICENSE.",
    thirdPartyAssets: [],
  };
}

const LEGACY_THEMES = [
  {
    id: "ink",
    displayName: "云墨",
    description: "为深度长文保留清晰层级与克制蓝灰强调色的兼容主题。",
    category: "longform",
    tags: ["深度长文", "克制", "蓝灰"],
    palette: {
      accent: "#31536f",
      accentSoft: "#edf3f6",
      text: "#202b33",
      muted: "#66727a",
      border: "#dbe2e7",
      quote: "#f4f7f9",
      heading: "#17232c",
      background: "#ffffff",
    },
  },
  {
    id: "jade",
    displayName: "青序",
    description: "为教程与清单提供清爽青绿色层级和易扫描正文节奏的兼容主题。",
    category: "tutorial",
    tags: ["教程", "清单", "青绿"],
    palette: {
      accent: "#0f766e",
      accentSoft: "#ecf7f5",
      text: "#23312f",
      muted: "#61706d",
      border: "#d6e5e1",
      quote: "#f1f8f6",
      heading: "#153f3a",
      background: "#ffffff",
    },
  },
  {
    id: "vermilion",
    displayName: "朱简",
    description: "为观点评论提供明确朱红强调和稳重正文对比的兼容主题。",
    category: "commentary",
    tags: ["观点", "评论", "朱红"],
    palette: {
      accent: "#b42318",
      accentSoft: "#fff1ee",
      text: "#352724",
      muted: "#786a66",
      border: "#eadbd7",
      quote: "#fff7f5",
      heading: "#631d17",
      background: "#ffffff",
    },
  },
  {
    id: "graphite",
    displayName: "素刊",
    description: "为专业报告提供中性石墨色和低干扰信息层级的兼容主题。",
    category: "report",
    tags: ["报告", "专业", "中性"],
    palette: {
      accent: "#52525b",
      accentSoft: "#f1f1f3",
      text: "#27272a",
      muted: "#71717a",
      border: "#e1e1e5",
      quote: "#f7f7f8",
      heading: "#18181b",
      background: "#ffffff",
    },
  },
];

const THEME_FAMILIES = [
  { id: "harbor", name: "海岬", tone: "清澈海蓝", accent: "#2563a6", text: "#203040", heading: "#10253a" },
  { id: "forest", name: "森序", tone: "沉静松绿", accent: "#18705f", text: "#21332f", heading: "#123d34" },
  { id: "coral", name: "珊简", tone: "温和珊红", accent: "#c44d4a", text: "#3b2928", heading: "#6f2524" },
  { id: "orchid", name: "兰笺", tone: "雅致兰紫", accent: "#76529b", text: "#312a38", heading: "#402858" },
  { id: "amber", name: "琥章", tone: "明净琥珀", accent: "#a85d12", text: "#382b20", heading: "#633507" },
  { id: "tide", name: "潮汐", tone: "通透青蓝", accent: "#087f8c", text: "#203438", heading: "#07505a" },
  { id: "rose", name: "绯页", tone: "柔韧玫红", accent: "#b63b69", text: "#392832", heading: "#68203c" },
  { id: "frost", name: "霜格", tone: "冷静钢青", accent: "#4b6578", text: "#29333b", heading: "#223b4d" },
  { id: "moss", name: "苔径", tone: "自然苔绿", accent: "#5c742f", text: "#303628", heading: "#384817" },
];

const THEME_VARIANTS = [
  {
    id: "brief",
    name: "简报",
    use: "管理简报和结论先行的业务摘要",
    category: "report",
    family: "sans",
    baseSize: 15,
    lineHeight: 1.65,
    paragraph: 12,
    section: 24,
    pageX: 18,
    pageY: 20,
    accentShift: -0.08,
    features: ["autoNumbering", "tableOfContents", "cjkSpacing", "externalLinkFootnotes"],
  },
  {
    id: "narrative",
    name: "叙事",
    use: "人物故事和有情节推进的深度长文",
    category: "longform",
    family: "serif",
    baseSize: 17,
    lineHeight: 1.95,
    paragraph: 20,
    section: 40,
    pageX: 22,
    pageY: 30,
    accentShift: 0.04,
    features: ["introduction", "signature", "spanLeaf", "cjkSpacing"],
  },
  {
    id: "tutorial",
    name: "教程",
    use: "步骤教学、操作清单和知识拆解",
    category: "tutorial",
    family: "sans",
    baseSize: 16,
    lineHeight: 1.82,
    paragraph: 16,
    section: 32,
    pageX: 20,
    pageY: 24,
    accentShift: -0.02,
    features: ["autoNumbering", "keywordUnderline", "tableOfContents", "spanLeaf", "cjkSpacing"],
  },
  {
    id: "commentary",
    name: "评论",
    use: "观点表达、趋势判断和公共议题评论",
    category: "commentary",
    family: "serif",
    baseSize: 17,
    lineHeight: 1.86,
    paragraph: 18,
    section: 34,
    pageX: 20,
    pageY: 26,
    accentShift: -0.13,
    features: ["introduction", "keywordUnderline", "signature", "spanLeaf", "externalLinkFootnotes"],
  },
  {
    id: "notebook",
    name: "手记",
    use: "生活经验、观察随笔和个人成长记录",
    category: "lifestyle",
    family: "kaiti",
    baseSize: 17,
    lineHeight: 2,
    paragraph: 20,
    section: 38,
    pageX: 22,
    pageY: 30,
    accentShift: 0.12,
    features: ["introduction", "signature", "spanLeaf", "cjkSpacing"],
  },
  {
    id: "campaign",
    name: "品牌",
    use: "品牌故事、活动发布和产品价值表达",
    category: "brand",
    family: "sans",
    baseSize: 16,
    lineHeight: 1.72,
    paragraph: 15,
    section: 34,
    pageX: 20,
    pageY: 26,
    accentShift: -0.04,
    features: ["keywordUnderline", "introduction", "signature", "spanLeaf"],
  },
  {
    id: "gallery",
    name: "图集",
    use: "图片主导的视觉故事和作品展示",
    category: "visual",
    family: "sans",
    baseSize: 15,
    lineHeight: 1.7,
    paragraph: 14,
    section: 44,
    pageX: 14,
    pageY: 22,
    accentShift: 0.18,
    features: ["introduction", "spanLeaf", "cjkSpacing"],
  },
  {
    id: "research",
    name: "研究",
    use: "证据综述、研究报告和数据解释",
    category: "report",
    family: "serif",
    baseSize: 16,
    lineHeight: 1.88,
    paragraph: 17,
    section: 36,
    pageX: 22,
    pageY: 28,
    accentShift: -0.17,
    features: ["autoNumbering", "tableOfContents", "introduction", "spanLeaf", "cjkSpacing", "externalLinkFootnotes"],
  },
  {
    id: "letter",
    name: "通讯",
    use: "周期通讯、社群来信和编辑部精选",
    category: "longform",
    family: "sans",
    baseSize: 16,
    lineHeight: 1.84,
    paragraph: 18,
    section: 34,
    pageX: 20,
    pageY: 26,
    accentShift: 0.08,
    features: ["introduction", "signature", "spanLeaf", "cjkSpacing", "externalLinkFootnotes"],
  },
];

function featureFlags(enabled) {
  const keys = ["autoNumbering", "keywordUnderline", "tableOfContents", "introduction", "signature", "spanLeaf", "cjkSpacing", "externalLinkFootnotes"];
  return Object.fromEntries(keys.map((key) => [key, enabled.includes(key)]));
}

function legacyTheme(seed, componentIds) {
  return {
    schemaVersion: "1.0",
    manifestType: "theme",
    catalogVersion: CATALOG_VERSION,
    id: seed.id,
    version: "1.1.0",
    displayName: resourceName(seed.displayName, "主题"),
    description: seed.description,
    status: "active",
    category: seed.category,
    tags: seed.tags,
    legacyIds: [],
    palette: seed.palette,
    typography: {
      defaultFamily: "sans",
      fallbackStack: "-apple-system,BlinkMacSystemFont,\"PingFang SC\",\"Microsoft YaHei\",sans-serif",
      baseSize: 16,
      lineHeight: 1.8,
      headingWeight: 700,
      bodyWeight: 400,
    },
    spacing: { paragraph: 16, section: 32, pageX: 18, pageY: 24 },
    features: featureFlags(["spanLeaf"]),
    supportedComponentIds: componentIds,
    renderers: {
      markdown: "creation.theme." + seed.id + ".markdown.v1",
      html: "creation.theme." + seed.id + ".html.v1",
      wechatRichText: "creation.theme." + seed.id + ".wechat.v1",
    },
    compatibility: {
      targets: ["markdown", "html", "wechatRichText"],
      wechatCertification: "legacyCompatible",
      minRuntimeVersion: MIN_RUNTIME_VERSION,
    },
    source: {
      policy: "yunspire_first_party",
      authoredBy: "Yunspire",
      repository: REPOSITORY,
      upstreamCodeCopied: false,
      researchBoundary: "Migrated from the existing Yunspire creation renderer and expanded only with original first-party metadata. External repositories informed capability research only.",
    },
    license: licenseFor("theme manifest and rendering tokens"),
  };
}

function generatedTheme(family, variant, componentIds) {
  const accent = tuneHex(family.accent, variant.accentShift);
  const fontStacks = {
    sans: "-apple-system,BlinkMacSystemFont,\"PingFang SC\",\"Microsoft YaHei\",sans-serif",
    serif: "\"Songti SC\",\"STSong\",\"Noto Serif CJK SC\",serif",
    kaiti: "\"Kaiti SC\",\"STKaiti\",\"KaiTi\",serif",
  };
  return {
    schemaVersion: "1.0",
    manifestType: "theme",
    catalogVersion: CATALOG_VERSION,
    id: family.id + "-" + variant.id,
    version: "1.0.0",
    displayName: family.name + "·" + variant.name,
    description: "面向" + variant.use + "，以" + family.tone + "建立清晰层级、稳定阅读节奏和可复用的原创排版规则。",
    status: "active",
    category: variant.category,
    tags: [variant.name, family.tone, "多端候选"],
    legacyIds: [],
    palette: {
      accent,
      accentSoft: mixHex(accent, "#ffffff", 0.88),
      text: family.text,
      muted: mixHex(family.text, "#ffffff", 0.42),
      border: mixHex(accent, "#ffffff", 0.8),
      quote: mixHex(accent, "#ffffff", 0.93),
      heading: family.heading,
      background: mixHex(accent, "#ffffff", 0.975),
    },
    typography: {
      defaultFamily: variant.family,
      fallbackStack: fontStacks[variant.family],
      baseSize: variant.baseSize,
      lineHeight: variant.lineHeight,
      headingWeight: variant.category === "visual" ? 600 : 700,
      bodyWeight: 400,
    },
    spacing: {
      paragraph: variant.paragraph,
      section: variant.section,
      pageX: variant.pageX,
      pageY: variant.pageY,
    },
    features: featureFlags(variant.features),
    supportedComponentIds: componentIds,
    renderers: {
      markdown: "creation.theme." + family.id + "-" + variant.id + ".markdown.v1",
      html: "creation.theme." + family.id + "-" + variant.id + ".html.v1",
      wechatRichText: "creation.theme." + family.id + "-" + variant.id + ".wechat.v1",
    },
    compatibility: {
      targets: ["markdown", "html", "wechatRichText"],
      wechatCertification: "candidate",
      minRuntimeVersion: MIN_RUNTIME_VERSION,
    },
    source: sourceFor("theme"),
    license: licenseFor("theme manifest and rendering tokens"),
  };
}

function slot(id, kind, required = true, maxLength = 2000) {
  return { id, kind, required, maxLength };
}

function componentSeed(id, displayName, description, category, blockKind, role, markdownFallback, fields, minItems = 1, maxItems = 1) {
  const slotKind = blockKind === "collection" ? "list" : blockKind === "media" ? "image" : "richText";
  return {
    id,
    displayName,
    description,
    category,
    blockKind,
    role,
    markdownFallback,
    fields,
    minItems,
    maxItems,
    slots: blockKind === "divider" ? [] : [slot(blockKind === "collection" ? "items" : "body", slotKind, true, blockKind === "collection" ? 10000 : 4000)],
  };
}

const COMPONENT_SEEDS = [
  componentSeed("lead", "导读", "在正文开始处交代内容解决的问题和读者收益。", "structure", "container", "note", "callout", ["问题背景", "读者收益", "阅读路径"]),
  componentSeed("quote", "引用", "突出有出处的原话或需要读者停留的关键句。", "emphasis", "container", "blockquote", "blockquote", ["引用原文", "作者或来源", "引用位置"]),
  componentSeed("notice", "提示", "对读者可能忽略的限制、风险或使用条件做醒目说明。", "information", "container", "note", "callout", ["适用条件", "注意事项", "核验动作"]),
  componentSeed("steps", "步骤", "把操作过程拆成可核验、可逐项完成的步骤。", "sequence", "collection", "list", "list", ["准备输入", "执行动作", "复核结果"], 1, 20),
  componentSeed("metrics", "数据", "用最多三个指标单元强调关键数字及其含义。", "information", "collection", "group", "table", ["指标名称", "当前数值", "口径解释"], 1, 3),
  componentSeed("compare", "对比", "并列两种方案、状态或选择，明确差异与取舍。", "comparison", "collection", "group", "table", ["比较维度", "方案 A", "方案 B"], 2, 2),
  componentSeed("dialogue", "对话", "用成对问答或多轮对话推进解释。", "conversation", "collection", "dialog", "paragraphs", ["读者问题", "基于来源的回答", "需要追问的边界"], 2, 20),
  componentSeed("timeline", "时间线", "按时间顺序呈现事件、版本或研究进展。", "sequence", "collection", "timeline", "list", ["起点事件", "关键转折", "当前状态"], 1, 30),
  componentSeed("divider", "分隔", "在章节或叙事节奏之间插入语义分隔。", "navigation", "divider", "separator", "thematicBreak", [], 0, 1),
  componentSeed("cta", "行动提示", "在内容收束处给出单一、清晰、可执行的下一步。", "conversion", "container", "callToAction", "paragraphs", ["行动内容", "负责人和时间", "验收信号"]),

  componentSeed("outline", "文章大纲", "用层级化标题先锁定文章的论证路径和读者预期。", "structure", "collection", "list", "list", ["问题背景", "核心判断", "行动建议"], 1, 40),
  componentSeed("abstract", "摘要卡", "用一段可独立阅读的摘要压缩文章结论、方法和价值。", "structure", "container", "note", "callout", ["研究对象", "核心发现", "使用价值"]),
  componentSeed("key-points", "要点清单", "把一段长内容压缩成便于扫描和复述的关键结论。", "structure", "collection", "list", "list", ["关键结论", "支持证据", "实际影响"], 1, 12),
  componentSeed("contents", "目录", "为长文建立可点击、可回看的章节导航。", "navigation", "collection", "list", "list", ["问题背景", "证据与分析", "行动建议"], 1, 30),
  componentSeed("introduction", "引言", "交代研究背景、文章范围和读者进入正文前需要知道的边界。", "structure", "container", "note", "paragraphs", ["讨论背景", "覆盖范围", "阅读提示"]),
  componentSeed("conclusion", "结论", "集中回答文章提出的问题，并把结论转换成带有边界的行动建议。", "structure", "container", "note", "paragraphs", ["核心回答", "适用边界", "行动建议"]),
  componentSeed("section-summary", "章节小结", "在章节末尾复盘本节结论并自然引向下一节。", "structure", "container", "note", "callout", ["已经确认", "仍待核验", "下一节方向"]),
  componentSeed("executive-summary", "执行摘要", "让决策者在短时间内看懂背景、结论、风险和行动。", "structure", "container", "note", "callout", ["决策背景", "建议结论", "关键行动"]),
  componentSeed("premise", "前提声明", "公开推理所依赖的事实、假设和适用边界。", "information", "container", "note", "callout", ["事实前提", "工作假设", "适用边界"]),
  componentSeed("definition", "定义框", "统一关键术语，避免同一篇文章中出现概念漂移。", "information", "container", "note", "callout", ["术语名称", "本文定义", "不包含内容"]),
  componentSeed("context", "背景卡", "用简短背景解释问题从哪里来、为什么值得关注。", "structure", "container", "note", "callout", ["问题起因", "关键变化", "直接影响"]),
  componentSeed("evidence", "证据卡", "把主张、证据、来源和可信度放在同一块，便于复核。", "information", "container", "figure", "table", ["待证主张", "证据内容", "可信度限制"]),
  componentSeed("source-note", "来源注", "在局部内容旁标出原始来源、日期和引用范围。", "information", "container", "note", "paragraphs", ["来源笔记", "记录日期", "引用范围"]),
  componentSeed("fact-check", "事实核验", "把事实、推断和待查内容分开呈现，降低误引风险。", "information", "collection", "group", "table", ["已确认事实", "合理推断", "待核验事项"], 1, 30),
  componentSeed("warning", "风险警示", "对法律、隐私、安全、财务或操作风险给出明确提醒。", "information", "container", "note", "callout", ["风险内容", "可能影响", "缓解动作"]),
  componentSeed("tip", "实用技巧", "把经验浓缩成短小、可复制的操作建议。", "emphasis", "container", "note", "callout", ["推荐做法", "有效原因", "适用边界"]),
  componentSeed("example", "示例", "用一个具体情境展示抽象方法如何落地。", "information", "container", "figure", "paragraphs", ["具体情境", "实施过程", "可观察结果"]),
  componentSeed("case-study", "案例", "用背景、动作、结果和复盘完整讲清一个案例。", "information", "container", "figure", "paragraphs", ["背景与目标", "关键动作", "结果与复盘"]),
  componentSeed("faq", "问答集", "把读者高频问题和基于来源的回答集中呈现。", "conversation", "collection", "dialog", "paragraphs", ["适用情况", "常见误解", "下一步动作"], 2, 30),
  componentSeed("checklist", "核对清单", "把发布、交付或研究流程变成可勾选的检查项。", "navigation", "collection", "list", "list", ["内容准确", "来源完整", "目标端预览"], 1, 50),
  componentSeed("decision-matrix", "决策矩阵", "按统一维度比较多个方案，让选择依据可见。", "comparison", "collection", "group", "table", ["目标匹配度", "实施成本", "风险可控性"], 2, 20),
  componentSeed("pros-cons", "利弊卡", "把同一方案的收益、成本和潜在代价放在一起评估。", "comparison", "collection", "group", "table", ["可能收益", "需要承担的代价", "采用条件"], 2, 2),
  componentSeed("before-after", "前后对照", "用具体细节展示改写、优化或流程变化前后的差异。", "comparison", "collection", "group", "table", ["观察维度", "调整前", "调整后"], 2, 2),
  componentSeed("process", "流程图解", "按输入、处理、输出和反馈说明一条可复用流程。", "sequence", "collection", "list", "list", ["输入条件", "处理动作", "输出与反馈"], 1, 20),
  componentSeed("milestone", "里程碑", "标记阶段目标、交付物和验收信号，便于持续推进。", "sequence", "collection", "timeline", "list", ["阶段目标", "交付物", "验收信号"], 1, 20),
  componentSeed("schedule", "排期", "将任务、负责人、依赖和时间窗对齐到一个可执行计划。", "sequence", "collection", "timeline", "table", ["时间窗口", "任务与负责人", "依赖与验收"], 1, 30),
  componentSeed("roadmap", "路线图", "把长期方向拆成阶段性目标与可回看的决策节点。", "sequence", "collection", "timeline", "list", ["近期基础", "中期验证", "远期扩展"], 1, 20),
  componentSeed("chronology", "编年记录", "完整保存事件顺序、来源和当时的解释，适合长期项目。", "sequence", "collection", "timeline", "list", ["事件日期", "影响变化", "当时判断"], 1, 100),
  componentSeed("interview", "访谈卡", "把受访者原话、背景和编辑者观察分开呈现。", "conversation", "collection", "dialog", "paragraphs", ["访谈问题", "受访者原话", "编辑观察"], 2, 30),
  componentSeed("debate", "正反辩题", "将争议问题的支持与反对证据并列，避免把立场当事实。", "conversation", "collection", "group", "table", ["支持论据", "反对论据", "暂定判断"], 2, 2),
  componentSeed("testimonial", "用户证言", "保留用户反馈的原意，同时标出采集背景和隐私处理。", "conversation", "container", "blockquote", "blockquote", ["用户原话", "使用场景", "授权说明"]),
  componentSeed("figure", "图表说明", "为图片、图表或截图补充标题、数据口径和可读说明。", "media", "media", "figure", "figure", ["图表标题", "数据口径", "来源说明"]),
  componentSeed("image-gallery", "图片画廊", "以统一说明格式组织多张图片、截图或作品卡片。", "media", "collection", "figure", "figure", ["第一张图片", "第二张图片", "图片关系"], 1, 24),
  componentSeed("cover-card", "封面卡", "为文章、报告或社交内容提供标题、副标题和视觉焦点文案。", "media", "media", "figure", "figure", ["主标题", "副标题", "封面说明"]),
  componentSeed("infographic", "信息图", "把一组事实、步骤或数字组织成可快速理解的视觉摘要。", "media", "collection", "figure", "figure", ["一句话结论", "事实单元", "设计说明"], 1, 20),
  componentSeed("video-card", "视频卡", "将视频、关键时间点和文字摘要组合成可回看的内容单元。", "media", "media", "figure", "figure", ["视频摘要", "关键时间点", "授权来源"]),
  componentSeed("data-table", "数据表", "用统一口径呈现可排序、可复核的数据表格。", "information", "collection", "group", "table", ["数据项目", "指标数值", "口径备注"], 1, 100),
  componentSeed("formula", "公式卡", "集中展示公式、变量定义和代入示例，适合教学或论文。", "information", "container", "figure", "paragraphs", ["公式表达", "变量定义", "限制条件"]),
  componentSeed("code-snippet", "代码片段", "展示与文章论点直接相关的短代码，并说明输入、输出和安全边界。", "information", "leaf", "note", "paragraphs", ["最小代码", "输入与输出", "安全边界"]),
  componentSeed("footnotes", "脚注集", "集中维护外部链接、术语补充和引用说明，保持正文清爽。", "navigation", "collection", "note", "paragraphs", ["本地笔记", "外部链接", "术语补充"], 1, 100),
  componentSeed("author-signature", "作者签名", "合并作者、编辑、日期和联系方式，形成可追溯的结尾。", "conversion", "container", "note", "paragraphs", ["作者与编辑", "更新日期", "联系入口"]),
  componentSeed("related-reading", "延伸阅读", "推荐与当前内容有明确关系的本地笔记或已发布文章。", "navigation", "collection", "list", "list", ["背景阅读", "相反观点", "进阶材料"], 1, 12),
  componentSeed("next-steps", "后续动作", "把内容结论转换成负责人、时间和验收信号都明确的行动。", "conversion", "collection", "list", "list", ["具体动作", "负责人和截止时间", "验收结果"], 1, 20),
];

function componentMarkdown(seed) {
  const displayName = resourceName(seed.displayName, "组件");
  if (seed.blockKind === "divider") return "---";
  if (seed.markdownFallback === "blockquote") {
    return "> “在这里填写" + seed.fields[0] + "。”\n>\n> — " + seed.fields[1] + "\n\n**" + seed.fields[2] + "：** [补充可核验信息]";
  }
  if (seed.markdownFallback === "table") {
    return "### " + displayName + "\n\n| " + seed.fields.join(" | ") + " |\n| " + seed.fields.map(() => "---").join(" | ") + " |\n| [填写内容] | [填写内容] | [填写内容] |\n\n**来源：** [[替换为实际笔记标题]]";
  }
  if (seed.markdownFallback === "list") {
    return "### " + displayName + "\n\n" + seed.fields.map((field) => "- [ ] **" + field + "：** [填写基于知识库的内容]").join("\n") + "\n\n**来源：** [[替换为实际笔记标题]]";
  }
  if (seed.markdownFallback === "figure") {
    return "### " + displayName + "\n\n![替换为实际图片](assets/replace-me.png)\n\n" + seed.fields.map((field) => "**" + field + "：** [填写说明]").join("\n\n") + "\n\n**来源与授权：** [[替换为实际笔记标题]]";
  }
  const lines = seed.fields.map((field) => "**" + field + "：** [填写基于本地知识库的内容]");
  if (seed.markdownFallback === "callout") {
    return "> [!note] " + displayName + "\n> " + lines.join("\n> \n> ") + "\n>\n> **来源：** [[替换为实际笔记标题]]";
  }
  return "### " + displayName + "\n\n" + lines.join("\n\n") + "\n\n**来源：** [[替换为实际笔记标题]]";
}

function componentManifest(seed) {
  const displayName = resourceName(seed.displayName, "组件");
  return {
    schemaVersion: "1.0",
    manifestType: "component",
    catalogVersion: CATALOG_VERSION,
    id: seed.id,
    version: "1.0.0",
    displayName,
    description: seed.description,
    status: "active",
    category: seed.category,
    legacyIds: [],
    blockKind: seed.blockKind,
    slots: seed.slots,
    semantics: {
      role: seed.role,
      ariaLabel: displayName,
      markdownFallback: seed.markdownFallback,
    },
    constraints: {
      minItems: seed.minItems,
      maxItems: seed.maxItems,
      allowNestedComponents: false,
      spanLeaf: seed.blockKind !== "divider",
      allowScripts: false,
      allowExternalStyles: false,
    },
    templateMarkdown: componentMarkdown(seed),
    renderers: {
      markdown: "creation.component." + seed.id + ".markdown.v1",
      html: "creation.component." + seed.id + ".html.v1",
      wechatRichText: "creation.component." + seed.id + ".wechat.v1",
    },
    compatibility: {
      targets: ["markdown", "html", "wechatRichText"],
      minRuntimeVersion: MIN_RUNTIME_VERSION,
    },
    source: sourceFor("component"),
    license: licenseFor("component manifest and Markdown block"),
  };
}

function templateSeed(contentType, id, displayName, description, artifactType, angle, evidence, outcome, tags) {
  return { contentType, id, displayName, description, artifactType, angle, evidence, outcome, tags };
}

const TEMPLATE_SEEDS = [
  templateSeed("article", "deep-dive-analysis", "深度解析", "从问题定义、证据链和影响机制出发，形成可复核的深度判断。", "report", "拆解一个复杂问题的关键机制", "引用本地笔记中的事实、数据和反例", "让读者获得清晰判断与行动路径", ["深度", "分析", "证据"]),
  templateSeed("article", "industry-observation", "行业观察", "记录行业变化、参与者动作与可能的结构性机会。", "report", "描述一个行业正在发生的变化", "标注时间、来源和观察者推断", "沉淀未来可复用的观察框架", ["行业", "观察", "趋势"]),
  templateSeed("article", "knowledge-explainer", "知识解读", "把专业概念翻译成普通读者可以理解并继续查证的解释。", "webpage", "解释一个容易被误解的概念", "提供定义来源、例子和边界条件", "帮助读者正确使用这个概念", ["知识", "解释", "入门"]),
  templateSeed("article", "case-review", "案例复盘", "以目标、动作、结果和复盘还原一次实践，而不是只讲成功故事。", "report", "复盘一次具体实践的决策过程", "关联项目记录、指标和当事人笔记", "提炼下次可以复制的做法", ["案例", "复盘", "实践"]),
  templateSeed("article", "decision-guide", "决策指南", "把模糊选择转成明确条件、评估维度和推荐动作。", "report", "帮助读者在多个选择中做决定", "列出本地资料支持的利弊与约束", "给出条件化而非绝对化的建议", ["决策", "指南", "取舍"]),
  templateSeed("article", "product-comparison", "产品对比", "用统一标准比较产品能力、成本和适用场景，避免只看功能列表。", "presentation", "比较两个或多个产品的真实使用差异", "记录试用结果、价格口径和限制", "让读者知道自己该选什么以及为什么", ["产品", "对比", "评测"]),
  templateSeed("article", "trend-forecast", "趋势研判", "将已发生的信号、驱动因素与不确定性组织成可审阅的趋势判断。", "dashboard", "研判一个趋势未来可能如何发展", "分离已知事实、概率判断和未知变量", "给出观察指标与下一次复盘时间", ["趋势", "研判", "指标"]),
  templateSeed("article", "interview-feature", "人物访谈", "以受访者经验为主线，同时保留编辑事实核验和背景补充。", "webpage", "通过一位人物的经验解释一个主题", "使用访谈笔记、原话和背景资料", "让读者理解观点背后的处境", ["访谈", "人物", "故事"]),
  templateSeed("article", "field-notes", "调研手记", "记录一次调研的现场观察、方法选择和未决问题。", "report", "公开一次调研如何得出结论", "区分现场记录、样本信息和编辑推断", "保留可以供他人复查的观察路径", ["调研", "手记", "现场"]),
  templateSeed("article", "book-insight", "读书洞察", "从一本书的核心论点出发，连接本地知识与现实工作场景。", "webpage", "把一本书的观点转化成可用洞察", "标明章节、摘录和个人延伸判断", "让读者知道哪些观点值得继续实践", ["读书", "洞察", "方法"]),
  templateSeed("article", "course-notes", "课程整理", "将课程内容整理成目标、知识点、练习和复习路径。", "presentation", "把一门课程整理成可复习的知识地图", "关联讲义、作业和学习笔记", "让读者可以按路径继续学习", ["课程", "学习", "复习"]),
  templateSeed("article", "data-story", "数据故事", "让一组数据通过问题、发现和含义形成有上下文的叙事。", "dashboard", "解释数据背后的问题和变化", "保留数据口径、样本和计算过程", "让读者看到数据能支持什么、不能支持什么", ["数据", "故事", "口径"]),
  templateSeed("article", "policy-brief", "政策解读", "面向非专业读者解释政策背景、条款影响和执行注意事项。", "report", "把一项政策翻译为实际影响", "引用正式文本和本地执行记录", "帮助相关角色知道应该何时采取行动", ["政策", "解读", "影响"]),
  templateSeed("article", "problem-solution", "问题方案", "从一个可观察问题出发，提出分阶段、可验证的解决方案。", "interactiveTool", "把问题拆成优先级与实施方案", "用现状记录、约束和历史尝试支撑方案", "让方案拥有负责人、指标与复盘节点", ["问题", "方案", "实施"]),
  templateSeed("article", "weekly-digest", "周度简报", "把一周新增事实、项目进度和下周关注点压缩成易读通讯。", "email", "用固定节奏汇总本周最值得知道的内容", "链接到本地新增笔记和项目记录", "让收件人快速知道发生了什么与接下来做什么", ["周报", "通讯", "精选"]),

  templateSeed("wechat", "wechat-depth", "公众号深度文", "以清晰导语、章节节奏和来源注打造适合公众号阅读的深度文章。", "wechatArticle", "在移动阅读中讲清一个重要问题", "为每个关键事实绑定本地来源", "形成可转发、可复读的核心观点", ["公众号", "深度", "移动阅读"]),
  templateSeed("wechat", "wechat-hot-commentary", "热点评论", "快速回应热点，同时保留事实核验、观点边界和后续观察指标。", "wechatArticle", "对一个热点事件给出有依据的判断", "分开实时事实、背景资料与作者观点", "避免追逐情绪，留下可检验的评论", ["公众号", "热点", "评论"]),
  templateSeed("wechat", "wechat-tutorial", "公众号教程", "将复杂操作改写为读者可以收藏、照做和检查的步骤。", "wechatArticle", "教读者完成一项具体任务", "使用本地流程、截图说明和失败案例", "让读者完成动作并知道如何判断成功", ["公众号", "教程", "步骤"]),
  templateSeed("wechat", "wechat-case-study", "公众号案例", "用故事开场、证据展开和复盘收束，呈现一次真实实践。", "wechatArticle", "讲清一个案例如何发生和产生结果", "关联项目资料、指标和授权原话", "提炼可迁移的经验而非简单模仿", ["公众号", "案例", "复盘"]),
  templateSeed("wechat", "wechat-interview", "公众号访谈", "以问答节奏降低阅读门槛，同时保留背景、原话和编辑核验。", "wechatArticle", "通过访谈展示一个人的经验与判断", "引用采访记录并标注编辑补充", "让人物观点与真实情境同时可见", ["公众号", "访谈", "人物"]),
  templateSeed("wechat", "wechat-listicle", "公众号清单", "将主题拆成有优先级的清单，每项都给出理由与行动提示。", "wechatArticle", "用清单帮助读者快速完成筛选", "每项连接到对应本地来源或案例", "让清单能被复用而不是只适合一次阅读", ["公众号", "清单", "收藏"]),
  templateSeed("wechat", "wechat-brand-story", "公众号品牌故事", "用事实和人物讲品牌为何存在、如何做事以及正在改变什么。", "wechatArticle", "讲述品牌价值与真实行动的关系", "使用品牌档案、项目记录和客户反馈", "建立可信而不夸大的品牌印象", ["公众号", "品牌", "故事"]),
  templateSeed("wechat", "wechat-event-recap", "活动复盘文", "将活动亮点、现场反馈和后续动作整理为可分享的复盘。", "wechatArticle", "总结一次活动真正带来的结果", "关联议程、照片说明、反馈与指标", "让未到场的人也能理解价值并继续参与", ["公众号", "活动", "复盘"]),
  templateSeed("wechat", "wechat-product-release", "产品发布文", "用用户问题、功能变化和使用方式解释一次产品更新。", "wechatArticle", "说明新功能解决了什么具体问题", "引用版本记录、用户反馈和操作示例", "让读者知道是否值得升级以及如何开始", ["公众号", "产品", "发布"]),
  templateSeed("wechat", "wechat-research-summary", "研究摘要文", "将研究结论、方法限制和实际含义压缩成易传播的公众号内容。", "wechatArticle", "把一项研究解释给非专业读者", "保留研究对象、样本、方法和不确定性", "在易读与严谨之间取得平衡", ["公众号", "研究", "摘要"]),
  templateSeed("wechat", "wechat-book-club", "读书会文章", "以读书会问题为线索，连接书中观点和团队实际讨论。", "wechatArticle", "记录一次阅读如何改变讨论与行动", "引用章节、讨论纪要和后续实践", "让读者可以带着问题继续阅读", ["公众号", "读书会", "讨论"]),
  templateSeed("wechat", "wechat-newsletter", "公众号通讯", "形成固定栏目、精选链接和编辑按语，适合周期性发布。", "wechatArticle", "在固定栏目中汇总一段时间的变化", "链接本地新增内容并注明更新时间", "让读者形成稳定的阅读预期", ["公众号", "通讯", "栏目"]),
  templateSeed("wechat", "wechat-faq", "公众号问答", "从读者问题出发，给出简洁回答、证据链接和继续求证方式。", "wechatArticle", "集中回应一个主题下的高频疑问", "逐条绑定来源、适用条件和例外", "降低读者获得可靠答案的成本", ["公众号", "问答", "服务"]),
  templateSeed("wechat", "wechat-opinion", "公众号观点文", "明确观点、展开论证、回应反方并在结尾给出克制的判断。", "wechatArticle", "对一个议题提出可辩论的主张", "标出事实、推断、价值判断和反例", "让观点经得起读者的追问", ["公众号", "观点", "论证"]),
  templateSeed("wechat", "wechat-community", "社群运营文", "将社群经验、成员故事和参与方式组织成有温度的更新。", "wechatArticle", "介绍社群正在共同解决的问题", "使用活动记录、成员授权反馈和规则文档", "让新成员知道如何加入并贡献", ["公众号", "社群", "参与"]),

  templateSeed("xiaohongshu", "xhs-howto", "小红书教程笔记", "以强钩子、分步图解和收藏理由帮助读者快速上手。", "socialPost", "教会读者一个马上能用的小技巧", "引用本地实测步骤和失败提醒", "让读者收藏后可以照着完成", ["小红书", "教程", "收藏"]),
  templateSeed("xiaohongshu", "xhs-list", "小红书清单", "用短句、序号和选择理由整理一组值得保存的清单。", "socialPost", "提供一份有取舍的推荐清单", "说明每项来源、适用人群和限制", "让读者快速筛选并做出选择", ["小红书", "清单", "推荐"]),
  templateSeed("xiaohongshu", "xhs-review", "小红书体验评测", "把真实体验、优缺点和适用边界写成可信的短评。", "socialPost", "分享一次真实使用后的判断", "保留体验时间、环境、对照和原始记录", "避免夸大，让读者知道适不适合自己", ["小红书", "评测", "体验"]),
  templateSeed("xiaohongshu", "xhs-comparison", "小红书对比笔记", "用统一维度对比两种选择，突出读者最关心的差异。", "socialPost", "帮助读者在两个选项之间做选择", "列出价格、场景、结果和限制的来源", "给出条件化推荐而非绝对结论", ["小红书", "对比", "选择"]),
  templateSeed("xiaohongshu", "xhs-before-after", "小红书前后对照", "通过前后细节、过程记录和注意事项呈现变化。", "socialPost", "展示一项可观察的改变如何发生", "记录时间线、原始状态和过程证据", "让读者看到真实边界而非只看结果图", ["小红书", "对照", "变化"]),
  templateSeed("xiaohongshu", "xhs-itinerary", "小红书行程攻略", "将目的地、时间安排、预算和避坑信息整理成可执行攻略。", "socialPost", "帮助读者规划一段具体行程", "引用本地路线记录、营业信息和实测花费", "让读者按自己的约束调整安排", ["小红书", "行程", "攻略"]),
  templateSeed("xiaohongshu", "xhs-recipe", "小红书食谱", "以材料、步骤、时间和失败排查写出可复做的食谱。", "socialPost", "教读者完成一道可复做的料理", "记录实际份量、火候和替代材料", "让读者知道成功标准与常见失误", ["小红书", "食谱", "做法"]),
  templateSeed("xiaohongshu", "xhs-fitness", "小红书运动记录", "把目标、训练动作、感受和恢复安排写成可坚持的记录。", "socialPost", "分享一次有边界的运动实践", "关联训练日志、时长、负荷和身体反馈", "提醒读者根据自身情况调整", ["小红书", "运动", "记录"]),
  templateSeed("xiaohongshu", "xhs-study", "小红书学习笔记", "将学习目标、关键概念和复习动作压缩成易回看的笔记。", "socialPost", "分享一套可以复用的学习方法", "连接课程笔记、练习结果和复习计划", "让读者能从收藏开始一次小练习", ["小红书", "学习", "方法"]),
  templateSeed("xiaohongshu", "xhs-workplace", "小红书职场经验", "用具体场景讲清沟通、协作或职业选择中的一个难题。", "socialPost", "解决一个常见的职场小困惑", "区分个人经验、团队规则和可验证事实", "让读者获得一句可以立刻使用的话", ["小红书", "职场", "沟通"]),
  templateSeed("xiaohongshu", "xhs-home", "小红书居家方案", "从空间问题、预算和维护成本出发分享一套居家方案。", "socialPost", "展示一个真实可维护的居家改造", "记录尺寸、采购、预算和使用反馈", "让读者知道哪些地方不要盲目照搬", ["小红书", "居家", "改造"]),
  templateSeed("xiaohongshu", "xhs-beauty", "小红书护理记录", "以周期、肤质或使用条件记录护理体验和注意事项。", "socialPost", "分享一次有前后记录的护理体验", "标出使用周期、产品信息和个体差异", "让读者在了解边界后再做尝试", ["小红书", "护理", "记录"]),
  templateSeed("xiaohongshu", "xhs-parenting", "小红书育儿经验", "围绕一个具体育儿场景记录做法、反馈和可调整之处。", "socialPost", "回应一个具体而常见的育儿场景", "引用成长记录、专业资料和家庭实际反馈", "提供选择而不是制造焦虑", ["小红书", "育儿", "经验"]),
  templateSeed("xiaohongshu", "xhs-digital", "小红书数码体验", "把设备或软件的真实场景、设置步骤与限制讲明白。", "socialPost", "分享一项数码工具解决了什么问题", "记录版本、配置、耗时和对照结果", "让读者根据自己的需求判断是否值得", ["小红书", "数码", "体验"]),
  templateSeed("xiaohongshu", "xhs-shopping-guide", "小红书购物指南", "按预算、用途和优先级整理一份不夸大的购买建议。", "poster", "帮助读者完成一次有约束的购买", "使用本地试用、价格记录和售后信息", "让读者避开不适合自己的选项", ["小红书", "购物", "指南"]),

  templateSeed("contract", "service-agreement", "服务合同", "明确服务范围、交付标准、费用和双方责任的正式合同骨架。", "report", "约束一项持续或阶段性服务的合作关系", "以项目需求、报价和验收记录为依据", "让双方对交付、付款和争议处理有共同文本", ["合同", "服务", "交付"]),
  templateSeed("contract", "procurement-agreement", "采购合同", "规范采购标的、质量、交付、付款和售后责任。", "report", "约束一次货物或设备采购", "引用采购清单、规格书和报价确认", "降低交付与质量争议", ["合同", "采购", "质量"]),
  templateSeed("contract", "consulting-agreement", "咨询合同", "界定咨询目标、工作方式、成果形式与保密边界。", "report", "约束一项咨询或顾问服务", "关联咨询方案、会议纪要和成果验收", "让咨询成果能够被验收和复用", ["合同", "咨询", "成果"]),
  templateSeed("contract", "nda-mutual", "双向保密协议", "对双方披露的信息、保密期限和例外情形做对等约定。", "report", "保护合作双方的非公开信息", "以信息分类、访问范围和合规要求为依据", "在交流之前明确可披露与不可披露边界", ["合同", "保密", "信息"]),
  templateSeed("contract", "employment-offer", "聘用意向书", "记录岗位、薪酬、入职条件和双方确认流程，供正式合同前使用。", "report", "确认一次岗位聘用意向", "引用岗位说明、薪酬确认和入职安排", "减少入职前对关键条件的理解差异", ["合同", "聘用", "岗位"]),
  templateSeed("contract", "software-development", "软件开发合同", "约定需求、里程碑、源代码权利、验收和维护责任。", "report", "约束一项软件研发外包项目", "关联需求文档、迭代记录和验收标准", "让每个版本都有可追溯的交付依据", ["合同", "软件", "开发"]),
  templateSeed("contract", "content-licensing", "内容授权合同", "明确内容使用范围、期限、地域、署名和二次创作边界。", "report", "约束文字、图片或视频内容的授权使用", "引用资产清单、授权范围和权利证明", "避免授权范围与实际使用不一致", ["合同", "授权", "内容"]),
  templateSeed("contract", "brand-collaboration", "品牌合作协议", "约定合作目标、品牌使用、交付内容、审核和结算方式。", "report", "约束品牌与合作方的联合项目", "关联品牌规范、项目 brief 和审核记录", "确保双方对公开表达与交付结果有共识", ["合同", "品牌", "合作"]),
  templateSeed("contract", "lease-agreement", "租赁合同", "记录标的、期限、租金、维护、交接和退租条件。", "report", "约束一项场地或设备租赁", "引用资产清单、现场照片和交接记录", "让使用与返还状态可以核验", ["合同", "租赁", "交接"]),
  templateSeed("contract", "loan-agreement", "借款合同", "明确金额、用途、期限、利息、还款和违约处理。", "report", "约束一笔借款关系", "依据借款确认、付款记录和还款计划", "让金额与日期等关键事实清楚可查", ["合同", "借款", "还款"]),
  templateSeed("contract", "partnership-mou", "合作备忘录", "以阶段性共识记录合作目标、分工、资源投入与退出机制。", "report", "记录一项尚处探索期的合作", "关联双方会议纪要、目标和资源清单", "为后续正式协议提供可追溯基础", ["合同", "备忘录", "合作"]),
  templateSeed("contract", "data-processing", "数据处理协议", "明确数据类别、处理目的、访问权限、保存期限和事件响应。", "report", "约束一项受托数据处理活动", "引用数据字典、权限矩阵和安全要求", "让数据处理责任与审计路径清晰", ["合同", "数据", "合规"]),
  templateSeed("contract", "maintenance-agreement", "维护服务合同", "约定服务级别、响应时间、维护范围、升级和停机通知。", "report", "约束设备或软件的维护支持", "引用资产清单、服务级别和工单记录", "让故障响应和验收有量化标准", ["合同", "维护", "服务级别"]),
  templateSeed("contract", "event-services", "活动服务合同", "明确活动日期、场地、人员、设备、内容和取消责任。", "report", "约束一次活动执行服务", "引用活动方案、场地确认和供应商报价", "确保现场交付和取消条件可执行", ["合同", "活动", "执行"]),
  templateSeed("contract", "project-acceptance", "项目验收单", "以交付清单、测试结果、缺陷处理和签字确认完成项目验收。", "report", "记录一次项目交付是否达到标准", "引用需求、测试报告、变更记录和缺陷清单", "让付款或上线决定有完整证据", ["合同", "验收", "交付"]),

  templateSeed("paper", "empirical-study", "实证研究论文", "以研究问题、方法、结果和限制呈现实证分析，保留可复核口径。", "report", "验证一个可测量的研究假设", "记录样本、变量、分析步骤和原始数据来源", "给出可重复、有限定条件的结论", ["论文", "实证", "方法"]),
  templateSeed("paper", "literature-review", "文献综述", "按主题、方法和争议梳理已有研究，而不是简单罗列摘要。", "report", "解释一个领域的知识谱系与空白", "关联本地文献摘录、研究方法和争议点", "提出值得继续验证的研究问题", ["论文", "综述", "文献"]),
  templateSeed("paper", "theoretical-paper", "理论论文", "定义概念、提出命题并说明理论边界和可能的检验路径。", "report", "提出一个有边界的理论解释", "使用概念来源、逻辑链和反例", "让命题可以被后续研究讨论或检验", ["论文", "理论", "命题"]),
  templateSeed("paper", "case-study-paper", "案例研究论文", "以严谨的案例材料解释机制、情境与可迁移边界。", "report", "从一个案例中提炼机制解释", "记录案例选择、材料来源和编码过程", "明确哪些结论只适用于该案例", ["论文", "案例", "机制"]),
  templateSeed("paper", "survey-report", "调查研究报告", "呈现问卷设计、样本结构、结果和偏差说明。", "dashboard", "总结一组受访者对特定问题的看法", "保留问卷版本、样本口径和统计过程", "让读者理解结果的代表性边界", ["论文", "调查", "样本"]),
  templateSeed("paper", "experimental-report", "实验报告", "按实验目的、控制变量、结果和复现条件记录一次实验。", "report", "验证某个干预是否改变结果", "关联实验日志、参数、对照组和原始输出", "让读者能判断结果是否稳定", ["论文", "实验", "复现"]),
  templateSeed("paper", "methodology-paper", "方法论文", "说明一种研究或分析方法的步骤、适用范围与比较基线。", "report", "介绍并论证一种方法的有效边界", "引用方法来源、示例数据和比较结果", "让读者知道什么时候应该使用它", ["论文", "方法", "边界"]),
  templateSeed("paper", "policy-paper", "政策研究论文", "连接政策问题、证据分析、方案选项和实施约束。", "report", "为公共问题提出可实施的政策选项", "关联政策文本、案例、数据和利益相关者", "形成可审阅的政策建议与评估指标", ["论文", "政策", "建议"]),
  templateSeed("paper", "technical-whitepaper", "技术白皮书", "以架构、原理、接口和风险说明一项技术方案。", "webpage", "向技术与业务读者解释一个系统方案", "保留设计决策、测试结果和限制条件", "让读者能评估采用成本与收益", ["论文", "技术", "架构"]),
  templateSeed("paper", "graduation-thesis", "毕业论文", "提供从选题、综述、方法到答辩材料的一体化论文骨架。", "report", "完成一个完整、可答辩的研究项目", "整理导师意见、资料目录、数据与版本", "让研究过程和最终论证相互对应", ["论文", "毕业", "答辩"]),
  templateSeed("paper", "conference-paper", "会议论文", "在有限篇幅内呈现问题、贡献、方法、结果与局限。", "presentation", "为会议评审快速呈现研究贡献", "保留关键图表、实验设置和相关工作差异", "让评审能在短时间内理解贡献", ["论文", "会议", "贡献"]),
  templateSeed("paper", "journal-article", "期刊论文", "以完整论证、规范引用和充分限制说明支撑期刊投稿。", "report", "形成适合期刊审阅的完整研究文章", "维护引用账本、版本记录和同行意见", "让每个结论都能回到方法和证据", ["论文", "期刊", "投稿"]),
  templateSeed("paper", "research-proposal", "研究计划书", "将问题、意义、方法、进度和预期贡献写成可评审计划。", "report", "争取资源开展一项新研究", "关联前期调研、相关文献和可用数据", "让评审者知道研究为何可行且值得做", ["论文", "计划", "立项"]),
  templateSeed("paper", "systematic-review", "系统综述", "以检索、筛选、编码和综合规则保证综述过程可追溯。", "report", "系统整理一组研究并识别证据强弱", "保存检索式、纳入排除标准和编码表", "得出有范围、有等级的综合结论", ["论文", "系统综述", "证据"]),
  templateSeed("paper", "data-analysis-report", "数据分析报告", "以问题、数据、分析、可视化和决策建议连接一份分析工作。", "dashboard", "让分析结果能够支撑一次实际决策", "记录数据版本、清洗规则、指标和异常", "让读者知道结论如何落到行动", ["论文", "数据分析", "决策"]),
];

function articleMarkdown(seed) {
  return [
    "# " + seed.displayName,
    "",
    "> 模板定位：" + seed.description,
    "> 编辑提示：用实际主题替换提示文字；重要事实必须关联真实的本地知识库笔记。",
    "",
    "## 写作任务",
    "",
    "- **主题：** [填写文章主题]",
    "- **目标读者：** [填写读者及其当前问题]",
    "- **核心角度：** " + seed.angle,
    "- **期望结果：** " + seed.outcome,
    "",
    "## 标题候选",
    "",
    "1. [直接说明问题与收益的标题]",
    "2. [突出反差或新发现的标题]",
    "3. [适合目标读者搜索的标题]",
    "",
    "## 问题与背景",
    "",
    "[说明问题从哪里来、为什么现在值得讨论。不要把推断写成事实。]",
    "",
    "**本节来源：** [[替换为真实笔记标题]]",
    "",
    "## 证据与分析",
    "",
    seed.evidence + "。先呈现证据，再解释它支持什么判断、不能支持什么判断。",
    "",
    "- **事实：** [原文或数据可以直接支持的内容]",
    "- **分析：** [基于事实形成的推理]",
    "- **反例：** [可能推翻或限制结论的材料]",
    "",
    "**本节来源：** [[替换为真实笔记标题]]",
    "",
    "## 结论与行动",
    "",
    seed.outcome + "。将建议写成明确动作，并说明适用条件。",
    "",
    "### 下一步",
    "",
    "- [ ] [负责人] 在 [时间] 完成 [动作]",
    "- [ ] 用 [指标或反馈] 验证结果",
    "",
    "## 来源账本",
    "",
    "| 主张 | 本地来源 | 事实或推断 | 备注 |",
    "| --- | --- | --- | --- |",
    "| [关键主张] | [[真实笔记标题]] | [事实或推断] | [页码、日期或限制] |",
    "",
    "## 发布前检查",
    "",
    "- [ ] 标题与正文一致",
    "- [ ] 关键事实均有本地来源",
    "- [ ] 已标出限制、反例和待核验内容",
    "- [ ] 结论包含可执行的下一步",
  ].join("\n");
}

function wechatMarkdown(seed) {
  return [
    "# " + seed.displayName,
    "",
    "> 模板定位：" + seed.description,
    "> 编辑提示：适配移动端阅读节奏，所有事实仍须绑定真实的本地知识来源。",
    "",
    "## 备选标题",
    "",
    "- [标题一：直接点出读者问题]",
    "- [标题二：呈现关键反差]",
    "- [标题三：承诺具体但不过度的阅读收益]",
    "",
    "## 开场",
    "",
    "[用一个具体场景、问题或已核验事实进入主题。控制在三段以内。]",
    "",
    "> **导读：** " + seed.angle + "。",
    "",
    "## 第一部分｜读者为什么需要知道",
    "",
    "[交代背景、变化与影响。段落保持短，避免连续堆叠术语。]",
    "",
    "**来源：** [[替换为真实笔记标题]]",
    "",
    "## 第二部分｜证据告诉我们什么",
    "",
    seed.evidence + "。每组证据后写一句解释，并主动说明边界。",
    "",
    "| 已确认事实 | 编辑分析 |",
    "| --- | --- |",
    "| [事实与来源] | [它意味着什么] |",
    "",
    "## 第三部分｜可以怎么做",
    "",
    seed.outcome + "。",
    "",
    "1. **先做：** [最小动作]",
    "2. **再看：** [验证信号]",
    "3. **不要忽略：** [风险或不适用条件]",
    "",
    "## 结语",
    "",
    "[用一个可以引发真实讨论的问题收束，不使用空泛口号。]",
    "",
    "---",
    "",
    "**作者：** [作者或团队]",
    "",
    "**资料来源：** [[真实笔记一]]、[[真实笔记二]]",
    "",
    "### 发布检查",
    "",
    "- [ ] 已在预览中检查段落、图片和链接",
    "- [ ] 没有把模型推断写成来源事实",
    "- [ ] 标题、摘要与正文结论一致",
  ].join("\n");
}

function xiaohongshuMarkdown(seed) {
  return [
    "# " + seed.displayName,
    "",
    "> 模板定位：" + seed.description,
    "> 编辑提示：保留真实体验、适用条件和来源，不制造虚假稀缺或夸大结果。",
    "",
    "## 封面文案",
    "",
    "**主标题：** [12 字内说清问题或结果]",
    "",
    "**副标题：** [补充适用人群或关键条件]",
    "",
    "## 开头钩子",
    "",
    "[用一个真实场景说明为什么写这篇笔记，并在首屏交代结论。]",
    "",
    "## 正文卡片",
    "",
    "### 01｜先看结论",
    "",
    seed.angle + "。说明这条结论适合谁、不适合谁。",
    "",
    "### 02｜我的依据",
    "",
    seed.evidence + "。",
    "",
    "- **实际记录：** [时间、条件、过程]",
    "- **对照或变化：** [前后差异]",
    "- **限制：** [个体差异、环境或样本边界]",
    "",
    "### 03｜照着做",
    "",
    seed.outcome + "。",
    "",
    "1. [第一步：明确输入与动作]",
    "2. [第二步：说明判断标准]",
    "3. [第三步：写出失败时如何调整]",
    "",
    "## 避坑提醒",
    "",
    "- [容易误解的地方]",
    "- [不建议照搬的情况]",
    "- [需要专业人员确认的内容]",
    "",
    "## 来源与素材",
    "",
    "- **知识来源：** [[替换为真实笔记标题]]",
    "- **图片来源：** [自有图片、授权说明或无需图片]",
    "",
    "## 话题",
    "",
    "#[主题关键词] #[目标人群] #[使用场景]",
  ].join("\n");
}

function contractMarkdown(seed) {
  return [
    "# " + seed.displayName,
    "",
    "> 文档用途：" + seed.description,
    "> 风险提示：本模板仅提供可编辑内容骨架，不替代执业律师、税务或合规人员对具体交易的审查。",
    "",
    "**签署地点：** [城市]",
    "",
    "**签署日期：** [YYYY 年 MM 月 DD 日]",
    "",
    "## 一、合同主体",
    "",
    "**甲方：** [完整法定名称]",
    "",
    "**统一社会信用代码或证件号：** [填写]",
    "",
    "**联系地址与联系人：** [填写]",
    "",
    "**乙方：** [完整法定名称]",
    "",
    "**统一社会信用代码或证件号：** [填写]",
    "",
    "**联系地址与联系人：** [填写]",
    "",
    "## 二、合作背景与目的",
    "",
    seed.angle + "。将双方已经确认的背景写成事实，不加入未经确认的承诺。",
    "",
    "## 三、定义与合同文件",
    "",
    "1. **项目：** [定义项目或交易标的]",
    "2. **交付物：** [定义文件、服务、货物或权利]",
    "3. **工作日：** [约定计算口径]",
    "4. 本合同附件、经双方书面确认的变更单与本合同正文共同构成合同文件。",
    "",
    "## 四、范围、交付与验收",
    "",
    seed.evidence + "。将本地资料中的需求、规格和验收记录转写为明确条款。",
    "",
    "| 交付项 | 数量或范围 | 交付时间 | 验收标准 |",
    "| --- | --- | --- | --- |",
    "| [交付物] | [填写] | [日期] | [可核验标准] |",
    "",
    "验收异议应在 [天数] 个工作日内以书面形式提出，并列明未通过项及依据。",
    "",
    "## 五、费用、税费与支付",
    "",
    "**合同总额：** 人民币 [大写金额]（小写：¥[金额]）。",
    "",
    "**支付节点：** [预付款、里程碑款、验收款及各自条件]。",
    "",
    "**发票与税费：** [发票类型、税率、开票时间和收款账户确认方式]。",
    "",
    "## 六、双方权利与义务",
    "",
    "### 甲方义务",
    "",
    "- [按时提供必要资料、反馈和现场条件]",
    "- [按约定完成验收与付款]",
    "",
    "### 乙方义务",
    "",
    "- [按范围、质量和时间完成交付]",
    "- [及时报告风险并保护甲方资料]",
    "",
    "## 七、知识产权、保密与数据",
    "",
    "- **既有权利：** [双方在签署前各自拥有的权利归属]",
    "- **项目成果：** [所有权、使用权、署名权和许可范围]",
    "- **保密信息：** [范围、保密期限、例外和返还或销毁方式]",
    "- **个人信息与数据：** [处理目的、最小权限、保存期限和事件通知]",
    "",
    "## 八、变更、违约与解除",
    "",
    "任何范围、价格或排期变更应通过双方确认的书面变更单生效。",
    "",
    "**违约责任：** [逾期、质量不符、未付款、泄密等情形及处理方式]。",
    "",
    "**解除条件：** [可补救违约、不可补救违约、不可抗力及通知期限]。",
    "",
    "## 九、争议解决与其他",
    "",
    "本合同适用 [法律或法域]。争议应先友好协商；协商不成的，提交 [仲裁机构或有管辖权法院]。",
    "",
    "本合同一式 [份数] 份，双方各执 [份数] 份，自双方有效签署之日起生效。",
    "",
    "## 十、签署",
    "",
    "| 甲方 | 乙方 |",
    "| --- | --- |",
    "| 授权代表：[姓名] | 授权代表：[姓名] |",
    "| 签字或盖章：[签署] | 签字或盖章：[签署] |",
    "| 日期：[日期] | 日期：[日期] |",
    "",
    "## 附件与事实来源",
    "",
    "- 附件一：[需求、规格或资产清单]",
    "- 附件二：[报价、排期或验收标准]",
    "- 内部依据：[[替换为真实合同谈判或项目笔记]]",
    "",
    "### 审阅清单",
    "",
    "- [ ] 主体、金额、日期和账户已经人工核对",
    "- [ ] 权利归属、保密、数据与责任条款已经专业审查",
    "- [ ] 所有附件版本与正文引用一致",
    "- [ ] " + seed.outcome,
  ].join("\n");
}

function paperMarkdown(seed) {
  return [
    "# " + seed.displayName,
    "",
    "> 研究定位：" + seed.description,
    "> 编辑提示：所有引文、数据和结论必须可追溯；请根据目标院校、期刊或会议规范调整格式。",
    "",
    "**作者：** [姓名]",
    "",
    "**单位：** [机构或院系]",
    "",
    "**版本：** [YYYY-MM-DD / v1.0]",
    "",
    "## 摘要",
    "",
    "**背景：** [研究问题与现有缺口]",
    "",
    "**方法：** [样本、材料与分析方法]",
    "",
    "**结果：** [只写数据可以支持的核心发现]",
    "",
    "**结论：** " + seed.outcome + "。",
    "",
    "**关键词：** [关键词一]；[关键词二]；[关键词三]",
    "",
    "## 1. 引言",
    "",
    seed.angle + "。说明研究问题为何重要、本文贡献是什么，以及论证边界。",
    "",
    "### 1.1 研究问题",
    "",
    "- RQ1：[可回答、可证伪的研究问题]",
    "- RQ2：[与第一个问题互补的研究问题]",
    "",
    "### 1.2 研究贡献",
    "",
    "1. [理论、方法或经验贡献]",
    "2. [对实践或后续研究的具体价值]",
    "",
    "## 2. 文献与理论背景",
    "",
    "[按概念、方法或争议组织文献，不按作者逐篇罗列。]",
    "",
    "| 主题 | 代表来源 | 主要结论 | 与本研究关系 |",
    "| --- | --- | --- | --- |",
    "| [主题] | [[真实文献笔记]] | [结论] | [继承、修正或反驳] |",
    "",
    "## 3. 研究设计与方法",
    "",
    seed.evidence + "。",
    "",
    "### 3.1 研究对象与样本",
    "",
    "[说明纳入、排除、规模、时间范围与代表性限制。]",
    "",
    "### 3.2 数据与材料",
    "",
    "[记录来源、版本、授权、清洗与匿名化方式。]",
    "",
    "### 3.3 分析步骤",
    "",
    "1. [准备与预处理]",
    "2. [主要分析或编码]",
    "3. [稳健性、复现或质量检查]",
    "",
    "## 4. 结果",
    "",
    "[先报告结果，再解释含义。保留无显著结果、异常和缺失值。]",
    "",
    "| 指标或主题 | 结果 | 证据位置 |",
    "| --- | --- | --- |",
    "| [结果一] | [数值或归纳结果] | [[分析笔记]] |",
    "",
    "## 5. 讨论",
    "",
    "### 5.1 结果解释",
    "",
    "[说明结果如何回答研究问题，并与既有研究对话。]",
    "",
    "### 5.2 替代解释",
    "",
    "[列出至少一个可能的替代机制或反例。]",
    "",
    "### 5.3 局限",
    "",
    "- [样本、测量或材料限制]",
    "- [方法与外部效度限制]",
    "- [仍需进一步核验的问题]",
    "",
    "## 6. 结论",
    "",
    seed.outcome + "。不要超出数据与方法能够支持的范围。",
    "",
    "## 参考文献",
    "",
    "- [作者. 年份. 文献标题. 出版信息.]",
    "",
    "## 附录与研究账本",
    "",
    "- 数据或材料清单：[[真实笔记标题]]",
    "- 分析记录：[[真实笔记标题]]",
    "- 版本与修改说明：[[真实笔记标题]]",
    "",
    "### 投稿或答辩前检查",
    "",
    "- [ ] 摘要、结果和结论口径一致",
    "- [ ] 引用、图表和数据均可追溯",
    "- [ ] 方法足以支持复现或审阅",
    "- [ ] 局限与利益冲突已经披露",
  ].join("\n");
}

function templateMarkdown(seed) {
  if (seed.contentType === "wechat") return wechatMarkdown(seed);
  if (seed.contentType === "xiaohongshu") return xiaohongshuMarkdown(seed);
  if (seed.contentType === "contract") return contractMarkdown(seed);
  if (seed.contentType === "paper") return paperMarkdown(seed);
  return articleMarkdown(seed);
}

function inputFormatsFor(seed) {
  const base = seed.contentType === "xiaohongshu"
    ? ["plainText", "markdown", "image"]
    : seed.contentType === "paper"
      ? ["plainText", "markdown", "docx", "xlsx", "pdf"]
      : ["plainText", "markdown", "docx", "pdf"];
  if (["dashboard", "interactiveTool"].includes(seed.artifactType)) {
    return unique([...base, "json", "csv", "tsv", "xlsx"]);
  }
  return base;
}

function outputFormatsFor(seed) {
  if (seed.contentType === "xiaohongshu") return ["markdown", "html", "png", "jpeg"];
  if (["contract", "paper"].includes(seed.contentType)) return ["markdown", "html", "pdf"];
  return ["markdown", "html"];
}

function templateManifest(seed, markdown) {
  const entrypoint = seed.id + "/template.md";
  return {
    schemaVersion: "1.0",
    manifestType: "template",
    catalogVersion: CATALOG_VERSION,
    id: seed.id,
    version: "1.0.0",
    displayName: seed.displayName,
    description: seed.description,
    status: "active",
    contentType: seed.contentType,
    artifactType: seed.artifactType,
    tags: unique([...seed.tags, "原创模板", "本地知识库"]),
    inputFormats: inputFormatsFor(seed),
    outputFormats: outputFormatsFor(seed),
    entrypoint,
    files: [
      {
        path: entrypoint,
        kind: "markdown",
        editable: true,
        contentHash: hash(markdown),
      },
    ],
    editableSurfaces: seed.artifactType === "dashboard" ? ["content", "data"] : ["content"],
    agentCapabilities: ["creation.generate", "creation.edit"],
    sandbox: {
      network: false,
      shell: false,
      vaultWrite: false,
      allowTopNavigation: false,
      allowPopups: false,
      allowExternalScripts: false,
    },
    compatibility: {
      minRuntimeVersion: MIN_RUNTIME_VERSION,
      previewMode: "sandboxedIframe",
    },
    source: sourceFor("template"),
    license: licenseFor("template manifest and Markdown entrypoint"),
  };
}

function researchBoundaries() {
  return [
    "https://github.com/MrGeDiao/shuorenhua",
    "https://github.com/Aboudjem/humanizer-skill",
    "https://github.com/isjiamu/gzh-design-skill",
    "https://github.com/geekjourneyx/md2wechat-skill",
    "https://github.com/xiaohuailabs/xiaohu-wechat-format",
    "https://github.com/nexu-io/html-anything",
  ].map((repository) => ({
    repository,
    use: "capabilityResearchOnly",
    bundled: false,
  }));
}

function catalogFor(themes, components, templates) {
  const certifiedThemes = themes.filter((item) => item.compatibility.wechatCertification === "certified").length;
  const legacyThemes = themes.filter((item) => item.compatibility.wechatCertification === "legacyCompatible").length;
  return {
    schemaVersion: "1.0",
    catalogVersion: CATALOG_VERSION,
    status: "active",
    resourceLayout: {
      themeManifests: "resources/creation/themes/*.manifest.json",
      componentManifests: "resources/creation/components/*.manifest.json",
      templateManifests: "resources/creation/templates/*.manifest.json",
      templateEntrypoints: "resources/creation/templates/*/template.md",
      writingResources: "resources/creation/catalog/writing-resources.json",
    },
    coverage: {
      themes: {
        planned: 85,
        implemented: themes.length,
        wechatCertifiedPlanned: 48,
        wechatCertified: certifiedThemes,
        legacyCompatible: legacyThemes,
      },
      components: {
        planned: 53,
        implemented: components.length,
      },
      templates: {
        planned: 75,
        implemented: templates.length,
      },
      writingPatterns: {
        planned: 53,
        implemented: 53,
      },
      voices: {
        planned: 5,
        implemented: 5,
      },
      purposePresets: {
        planned: 9,
        implemented: 9,
      },
    },
    resources: {
      themes: themes.map((item) => ({
        id: item.id,
        manifest: "resources/creation/themes/" + item.id + ".manifest.json",
      })),
      components: components.map((item) => ({
        id: item.id,
        manifest: "resources/creation/components/" + item.id + ".manifest.json",
      })),
      templates: templates.map((item) => ({
        id: item.id,
        manifest: "resources/creation/templates/" + item.id + ".manifest.json",
        entrypoint: "resources/creation/templates/" + item.entrypoint,
      })),
      writingResources: {
        catalog: "resources/creation/catalog/writing-resources.json",
        writingPatterns: 53,
        voices: 5,
        purposePresets: 9,
      },
    },
    source: {
      policy: "yunspire_first_party",
      authoredBy: "Yunspire",
      repository: REPOSITORY,
      upstreamCodeCopied: false,
      upstreamPromptsCopied: false,
      upstreamAssetsCopied: false,
    },
    license: {
      scope: "yunspire_first_party_project_asset",
      notice: "The catalog, manifests, component Markdown, and template entrypoints are original Yunspire project assets governed by the repository LICENSE. No third-party templates, prompts, wording, code, or visual assets are bundled.",
    },
    researchBoundaries: researchBoundaries(),
  };
}

function assertUniqueIds(items, kind) {
  const ids = items.map((item) => item.id);
  assert(new Set(ids).size === ids.length, "Duplicate " + kind + " id");
  assert(items.every((item) => /^[a-z][a-z0-9-]{0,79}$/.test(item.id)), "Invalid " + kind + " id");
  assert(new Set(items.map((item) => item.displayName)).size === items.length, "Duplicate " + kind + " displayName");
}

function runtimeBundleFor(themes, components, templateBundles, catalog) {
  return {
    schemaVersion: "1.0",
    catalogVersion: catalog.catalogVersion,
    runtimeVersion: RUNTIME_VERSION,
    themes,
    components,
    templates: templateBundles.map(({ manifest, markdown }) => ({
      ...manifest,
      canonicalMarkdown: markdown,
    })),
  };
}

async function writeResources(themes, components, templateBundles, catalog, runtimeBundle) {
  await Promise.all([
    rm(themeDirectory, { recursive: true, force: true }),
    rm(componentDirectory, { recursive: true, force: true }),
    rm(templateDirectory, { recursive: true, force: true }),
  ]);
  await Promise.all([
    mkdir(themeDirectory, { recursive: true }),
    mkdir(componentDirectory, { recursive: true }),
    mkdir(templateDirectory, { recursive: true }),
    mkdir(dirname(catalogPath), { recursive: true }),
  ]);

  await Promise.all([
    ...themes.map((manifest) => writeFile(resolve(themeDirectory, manifest.id + ".manifest.json"), json(manifest), "utf8")),
    ...components.map((manifest) => writeFile(resolve(componentDirectory, manifest.id + ".manifest.json"), json(manifest), "utf8")),
    ...templateBundles.flatMap(({ manifest, markdown }) => {
      const entrypointPath = resolve(templateDirectory, manifest.entrypoint);
      return [
        mkdir(dirname(entrypointPath), { recursive: true }).then(() => writeFile(entrypointPath, markdown + "\n", "utf8")),
        writeFile(resolve(templateDirectory, manifest.id + ".manifest.json"), json(manifest), "utf8"),
      ];
    }),
  ]);
  await Promise.all([
    writeFile(catalogPath, json(catalog), "utf8"),
    writeFile(runtimeBundlePath, json(runtimeBundle), "utf8"),
  ]);
}

async function readJson(filePath) {
  return JSON.parse(await readFile(filePath, "utf8"));
}

async function manifestFiles(directory) {
  return (await readdir(directory))
    .filter((name) => name.endsWith(".manifest.json"))
    .sort();
}

async function selfCheck(expected) {
  const [themeFiles, componentFiles, templateFiles, catalog, runtimeBundle, writingResources] = await Promise.all([
    manifestFiles(themeDirectory),
    manifestFiles(componentDirectory),
    manifestFiles(templateDirectory),
    readJson(catalogPath),
    readJson(runtimeBundlePath),
    readJson(writingResourcesPath),
  ]);

  assert(themeFiles.length === 85, "Generated theme count must be 85");
  assert(componentFiles.length === 53, "Generated component count must be 53");
  assert(templateFiles.length === 75, "Generated template count must be 75");
  assert(catalog.status === "active", "Catalog must be active");
  assert(catalog.coverage.themes.implemented === 85, "Catalog theme count must be 85");
  assert(catalog.coverage.components.implemented === 53, "Catalog component count must be 53");
  assert(catalog.coverage.templates.implemented === 75, "Catalog template count must be 75");
  assert(runtimeBundle.catalogVersion === catalog.catalogVersion, "Runtime bundle catalog version is stale");
  assert(runtimeBundle.themes?.length === 85, "Runtime bundle must contain all 85 themes");
  assert(runtimeBundle.components?.length === 53, "Runtime bundle must contain all 53 components");
  assert(runtimeBundle.templates?.length === 75, "Runtime bundle must contain all 75 templates");
  assert(runtimeBundle.templates.every((template) => template.canonicalMarkdown?.startsWith("# ")), "Runtime template entrypoints must be embedded as canonical Markdown");
  assert(catalog.coverage.themes.wechatCertified === 0, "Unverified themes cannot be marked as WeChat certified");
  assert(catalog.coverage.themes.wechatCertifiedPlanned === 48, "WeChat certification plan must remain 48 themes");
  assert(catalog.coverage.themes.legacyCompatible === 4, "Exactly four legacy themes must remain compatible");
  assert(writingResources.writingPatterns?.length === 53, "Writing resource catalog must provide 53 patterns");
  assert(writingResources.voices?.length === 5, "Writing resource catalog must provide five voices");
  assert(writingResources.purposePresets?.length === 9, "Writing resource catalog must provide nine purpose presets");
  assert(catalog.coverage.writingPatterns.implemented === writingResources.writingPatterns.length, "Catalog writing pattern count is stale");
  assert(catalog.coverage.voices.implemented === writingResources.voices.length, "Catalog voice count is stale");
  assert(catalog.coverage.purposePresets.implemented === writingResources.purposePresets.length, "Catalog purpose preset count is stale");
  assert(catalog.resources.writingResources.catalog === "resources/creation/catalog/writing-resources.json", "Writing resource catalog must be registered");
  assert(catalog.resources.writingResources.writingPatterns === writingResources.writingPatterns.length, "Registered writing pattern count is stale");
  assert(catalog.resources.writingResources.voices === writingResources.voices.length, "Registered voice count is stale");

  const componentManifests = await Promise.all(componentFiles.map((name) => readJson(resolve(componentDirectory, name))));
  const componentTemplates = new Set();
  for (const component of componentManifests) {
    assert(component.status === "active", component.id + " must be active");
    assert(component.templateMarkdown.trim().length >= 3, component.id + " must have editable Markdown");
    assert([...component.displayName].length >= 3, component.id + " must have a semantic display name");
    assert(normalizedText(component.description).length >= 12, component.id + " must have a meaningful description");
    const normalizedTemplate = normalizedText(component.templateMarkdown);
    assert(!componentTemplates.has(normalizedTemplate), component.id + " must have unique component Markdown");
    componentTemplates.add(normalizedTemplate);
    assert(component.source.policy === "yunspire_first_party", component.id + " must be first party");
    assert(component.source.upstreamCodeCopied === false, component.id + " cannot copy upstream code");
  }

  const templateManifests = await Promise.all(templateFiles.map((name) => readJson(resolve(templateDirectory, name))));
  const templateContents = new Set();
  const contentTypes = new Set();
  const declaredMarkdown = new Set();
  for (const template of templateManifests) {
    const entrypointPath = resolve(templateDirectory, template.entrypoint);
    const markdown = await readFile(entrypointPath, "utf8");
    assert(template.status === "active", template.id + " must be active");
    assert(template.entrypoint === template.id + "/template.md", template.id + " has an invalid entrypoint");
    assert(template.files.length === 1 && template.files[0].kind === "markdown" && template.files[0].editable === true, template.id + " must declare one editable Markdown file");
    assert(template.files[0].contentHash === hash(markdown.trimEnd()), template.id + " content hash does not match");
    assert(markdown.startsWith("# " + template.displayName), template.id + " Markdown must start with its semantic title");
    assert(markdown.length >= 500, template.id + " Markdown is not substantive enough");
    assert(markdown.includes("[["), template.id + " Markdown must include a local-source editing surface");
    assert(normalizedText(template.description).length >= 12, template.id + " must have a meaningful description");
    const normalizedMarkdown = normalizedText(markdown);
    assert(!templateContents.has(normalizedMarkdown), template.id + " must have unique template Markdown");
    templateContents.add(normalizedMarkdown);
    contentTypes.add(template.contentType);
    declaredMarkdown.add(entrypointPath);
    assert(template.source.upstreamCodeCopied === false, template.id + " cannot copy upstream code");
  }

  for (const contentType of ["article", "wechat", "xiaohongshu", "contract", "paper"]) {
    assert(contentTypes.has(contentType), "Templates must cover " + contentType);
  }
  const templateEntries = await readdir(templateDirectory, { withFileTypes: true });
  for (const entry of templateEntries.filter((item) => item.isDirectory())) {
    const files = await readdir(resolve(templateDirectory, entry.name));
    for (const file of files.filter((name) => name.endsWith(".md"))) {
      assert(declaredMarkdown.has(resolve(templateDirectory, entry.name, file)), "Undeclared template Markdown: " + entry.name + "/" + file);
    }
  }

  const themeTokens = new Set();
  for (const file of themeFiles) {
    const theme = await readJson(resolve(themeDirectory, file));
    assert(theme.status === "active", theme.id + " must be active");
    assert([...theme.displayName].length >= 3, theme.id + " must have a semantic display name");
    assert(normalizedText(theme.description).length >= 12, theme.id + " must have a meaningful description");
    const tokenKey = JSON.stringify({
      palette: theme.palette,
      typography: theme.typography,
      spacing: theme.spacing,
      features: theme.features,
    });
    assert(!themeTokens.has(tokenKey), theme.id + " must have unique rendering tokens");
    themeTokens.add(tokenKey);
  }

  const artifactTypes = new Set(templateManifests.map((item) => item.artifactType));
  assert(artifactTypes.size === 9, "Templates must cover all nine artifact types");
  assert(expected.every((path) => path.startsWith(creationRoot)), "Self-check path escaped the creation resource root");

  return {
    themes: themeFiles.length,
    certified: catalog.coverage.themes.wechatCertified,
    legacy: catalog.coverage.themes.legacyCompatible,
    components: componentFiles.length,
    templates: templateFiles.length,
    entrypoints: templateManifests.length,
    artifactTypes: artifactTypes.size,
  };
}

assert(COMPONENT_SEEDS.length === 53, "Component seed count must be 53, received " + COMPONENT_SEEDS.length);
assert(TEMPLATE_SEEDS.length === 75, "Template seed count must be 75, received " + TEMPLATE_SEEDS.length);

const componentManifests = COMPONENT_SEEDS.map(componentManifest);
const componentIds = componentManifests.map((item) => item.id);
const generatedThemes = THEME_FAMILIES.flatMap((family) => THEME_VARIANTS.map((variant) => [family, variant]));
assert(generatedThemes.length === 81, "Generated theme seed count must be 81");
const themeManifests = [
  ...LEGACY_THEMES.map((seed) => legacyTheme(seed, componentIds)),
  ...generatedThemes.map(([family, variant]) => generatedTheme(family, variant, componentIds)),
];
const templateBundles = TEMPLATE_SEEDS.map((seed) => {
  const markdown = templateMarkdown(seed);
  return { manifest: templateManifest(seed, markdown), markdown };
});
const templateManifests = templateBundles.map((item) => item.manifest);

assert(themeManifests.length === 85, "Theme manifest count must be 85");
assert(themeManifests.every((item) => item.compatibility.wechatCertification !== "certified"), "Themes require real publication evidence before certification");
assert(themeManifests.filter((item) => item.compatibility.wechatCertification === "candidate").length === 81, "Generated themes must remain WeChat certification candidates");
assertUniqueIds(themeManifests, "theme");
assertUniqueIds(componentManifests, "component");
assertUniqueIds(templateManifests, "template");
assert(["ink", "jade", "vermilion", "graphite"].every((id) => componentIds.length === 53 && themeManifests.some((item) => item.id === id)), "Legacy theme ids must be preserved");
assert(["lead", "quote", "notice", "steps", "metrics", "compare", "dialogue", "timeline", "divider", "cta"].every((id) => componentIds.includes(id)), "Legacy component ids must be preserved");

const catalog = catalogFor(themeManifests, componentManifests, templateManifests);
const runtimeBundle = runtimeBundleFor(themeManifests, componentManifests, templateBundles, catalog);
const expectedPaths = [
  ...themeManifests.map((item) => resolve(themeDirectory, item.id + ".manifest.json")),
  ...componentManifests.map((item) => resolve(componentDirectory, item.id + ".manifest.json")),
  ...templateManifests.flatMap((item) => [
    resolve(templateDirectory, item.id + ".manifest.json"),
    resolve(templateDirectory, item.entrypoint),
  ]),
  catalogPath,
  runtimeBundlePath,
  writingResourcesPath,
];

await writeResources(themeManifests, componentManifests, templateBundles, catalog, runtimeBundle);
const summary = await selfCheck(expectedPaths);
console.log(
  "CREATION_RESOURCE_GENERATION_OK "
  + "themes=" + summary.themes
  + " certified=" + summary.certified
  + " legacy=" + summary.legacy
  + " components=" + summary.components
  + " templates=" + summary.templates
  + " markdownEntrypoints=" + summary.entrypoints
  + " artifactTypes=" + summary.artifactTypes,
);
