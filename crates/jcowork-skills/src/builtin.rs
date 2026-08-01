//! Built-in system skills shared across all users.
//!
//! These are read-only skills pre-defined by the system.
//! Users can enable/disable them but cannot edit them.

/// A built-in skill definition.
#[derive(Debug, Clone)]
pub struct BuiltinSkill {
    /// Unique ID, always prefixed with "builtin:" (e.g. "builtin:write_ppt")
    pub id: &'static str,
    /// Short skill name (e.g. "write_ppt")
    pub name: &'static str,
    /// One-line description shown in the Skills Square UI
    pub description: &'static str,
    /// Full skill instructions injected into the system prompt when enabled
    pub content: &'static str,
    /// Hidden from the Skills Square UI (still functional when enabled)
    pub hidden: bool,
}

/// Return the full list of built-in skills.
pub fn builtin_skills() -> &'static [BuiltinSkill] {
    &BUILTIN_SKILLS
}

static BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        id: "builtin:write_ppt",
        name: "write_ppt",
        description: "Generate structured PowerPoint presentation outlines with slides, speaker notes, and visual layout suggestions.",
        hidden: true,
        content: r#"## Skill: write_ppt — PowerPoint Presentation Generator

When the user asks you to write a PPT, create a presentation, or make slides, follow this **two-phase workflow**:

---

## Phase 1: Outline

### Default Visual Style: Minimalist Tech (简约科技风)
Unless the user specifies otherwise:
- **Color palette**: Dark background (#0D1117), accent electric blue (#00BFFF) or cyan (#00E5FF), white/light-gray text
- **Typography**: Inter / PingFang SC / Microsoft YaHei; Title 36–44pt bold; Body 18–22pt
- **Layout**: Generous whitespace, left- or center-aligned, minimal decoration
- **Icons**: Flat line icons, geometric shapes, subtle circuit-board grid texture
- **Data viz**: No 3D / no drop-shadows; thin-stroke bar or line charts
- **Animations**: Fade-in or slide-in only

### Before Writing, Confirm If Not Specified
- Topic / title
- Audience: corporate / academic / general public?
- Purpose: informational / persuasive / training?
- Language: Chinese / English / bilingual?
- Slide count (default 10–12)
- Style override?

### Outline Format (one block per slide)
```
## Slide N: [Title]
**Layout**: [Title Slide / Content / Two Column / Data Chart / Q&A]
**Background**: [color/texture]
**Content**:
- Bullet 1
- Bullet 2
- Bullet 3
**Visual element**: [icon / chart / diagram description]
**Speaker Notes**: [presenter talking points]
```

### Slide Structure
1. Title Slide — title, subtitle, author/date, logo placeholder
2. Agenda — 3–6 topics with icons
3. Content Slides — one key idea each, 3–5 bullets
4. Data/Chart Slides — chart + 1-sentence insight headline
5. Summary — 3 key takeaways
6. Q&A / Thank You — contact info, QR placeholder

### Design Principles
- ONE idea per slide; 6×6 rule (max 6 bullets × 6 words)
- Visuals > text; consistent grid alignment; WCAG AA contrast

---

## Phase 2: HTML Presentation (ALWAYS generate after the outline)

After producing the outline, **immediately and automatically** output a complete, self-contained HTML file that renders the presentation in a browser. Do NOT ask the user whether to generate it — always generate it.

### HTML Requirements
- **Single file**: all CSS and JS inline, zero external dependencies
- **Slide dimensions**: 16:9 ratio (e.g. 1280×720px), centered in viewport
- **Navigation**: Left/Right arrow keys + on-screen Prev/Next buttons; slide counter (e.g. "3 / 10")
- **Minimalist Tech theme** (unless user chose a different style):
  - Background: `#0D1117`; Accent: `#00BFFF`; Text: `#E6EDF3`
  - Font stack: `'Inter', 'PingFang SC', 'Microsoft YaHei', sans-serif`
  - Title slide: large centered title, thin accent underline, subtle grid background via CSS
  - Content slides: left-aligned title in accent color, bulleted body text, bottom slide number
  - Subtle fade transition between slides (`opacity` + `transition: opacity 0.3s`)
- **Slide types**:
  - Title slide: full-bleed dark bg, large title, smaller subtitle
  - Agenda: numbered list with accent bullet dots
  - Content: title + bullet list
  - Two-column: side-by-side divs
  - Data/Chart: placeholder `<div class="chart-placeholder">` with chart description text
  - Q&A / Thank You: centered large text
- **Speaker notes**: hidden by default; toggle with `N` key, shown as a bottom overlay panel
- **Print/export hint**: include a `<style media="print">` block that shows one slide per page
- **Action buttons** (fixed top-right corner, always visible):
  - **Download button**: labeled "Download HTML"; on click, creates a Blob from the page's full outerHTML and triggers a download as `presentation.html`
  - **Copy button**: labeled "Copy HTML"; on click, copies the page's full outerHTML to clipboard; briefly changes label to "Copied!" for 2 seconds then reverts
  - Style: small semi-transparent dark pills with white text, `position: fixed; top: 16px; right: 16px; z-index: 9999`, hover opacity change

### HTML Output
Output the HTML inside a fenced code block:

```html
<!DOCTYPE html>
<!-- full self-contained presentation -->
<html lang="zh">…</html>
```

The file must be complete and runnable — the user should be able to save it as `presentation.html` and open it directly in any browser."#,
    },
    BuiltinSkill {
        id: "builtin:write_research_report",
        name: "write_research_report",
        description: "Analyze listed-company financial documents (PDFs) and generate professional research reports with financial analysis, valuation, and investment insights.",
        hidden: true,
        content: r#"## Skill: write_research_report — Listed Company Research Report Generator

When the user asks you to write a research report, analyze a stock, or evaluate a listed company, follow this **two-phase workflow**:

> **Prerequisite**: The `jcowork-report-search` service must be running (port 3001). It automatically indexes PDFs from `~/.jcowork/data/reports/{company_name}/`.
> To start it: `cargo run --bin jcowork-report-search`

---

## Phase 1: Document Discovery & Targeted Search

### Step 1: Identify the target company
Ask the user:
- Which company? (stock code / name)
- What type of report? (full research / quick update)
- Any specific focus? (financials / strategy / valuation / risk)

### Step 2: Discover available documents
Call `report_list_companies` to confirm the company is indexed:
```
report_list_companies({})
```

### Step 3: Multi-query targeted search
Make **4-6 targeted searches** to gather all necessary information. Use Chinese keywords matching annual report terminology:

```
// Financial performance
report_search({ query: "营业收入 净利润 毛利率", company: "XXX", doc_type: "年报", limit: 15 })

// Balance sheet & cash flow
report_search({ query: "总资产 负债率 经营活动现金流", company: "XXX", limit: 10 })

// Key metrics & EPS
report_search({ query: "每股收益 净资产收益率 ROE", company: "XXX", limit: 10 })

// Business model & competitive position
report_search({ query: "主营业务 核心竞争力 市场份额", company: "XXX", limit: 15 })

// Strategy & outlook
report_search({ query: "发展战略 未来展望 管理层讨论", company: "XXX", limit: 10 })

// Risk factors
report_search({ query: "风险因素 行业竞争 市场风险", company: "XXX", limit: 10 })
```

Also query broker research reports for consensus view and valuation benchmarks:
```
report_search({ query: "目标价 估值 评级 买入", company: "XXX", doc_type: "研报", limit: 10 })
```

---

## Phase 2: Research Report Generation

### Report Structure
Generate a comprehensive research report in Markdown with these sections:

```
# [Company Name] ([Stock Code]) 研究报告
## 核心观点 (Investment Highlights)
## 公司概况 (Company Overview)
## 商业模式与竞争壁垒 (Business Model & Moat)
## 行业分析 (Industry Analysis)
## 财务分析 (Financial Analysis)
### 收入与盈利能力
### 资产负债表健康度
### 现金流分析
### 核心财务指标表
## 估值分析 (Valuation)
### 相对估值 (P/E, P/B, EV/EBITDA)
### 同行比较
## 风险提示 (Risk Factors)
## 投资建议 (Investment Recommendation)
## 附录：主要财务数据
```

### Writing Guidelines
- **Language**: Chinese for A-share companies
- **Data-driven**: Every claim backed by specific numbers from search results
- **Tables**: Use Markdown tables for all year-over-year comparisons
- **Tone**: Objective, professional, institutional-grade
- **Disclaimer**: Always end with: "本报告仅供参考，不构成投资建议。投资有风险，入市需谨慎。"

### Financial Table Format
```
| 指标 | 2022A | 2023A | 2024A | YoY |
|------|-------|-------|-------|-----|
| 营业收入(亿元) | 100 | 120 | 145 | +20.8% |
| 归母净利润(亿元) | 15 | 20 | 28 | +40.0% |
| 毛利率 | 35% | 38% | 40% | +2pp |
| ROE | 12% | 15% | 18% | +3pp |
```

IMPORTANT: Do NOT generate HTML unless the user explicitly asks for it. The Markdown report is the final deliverable."#,
    },
    BuiltinSkill {
        id: "builtin:web_search",
        name: "网页搜索",
        description: "通过无头浏览器搜索网页，获取最新信息和实时内容，基于搜索结果回答问题。",
        hidden: false,
        content: r#"## Skill: web_search — Web Search & Answer

When a question requires up-to-date or real-world information that you don't know from training data, use the `web_search` tool to find answers on the internet.

### When to use this skill
- Questions about current events, news, prices, or recent data
- Questions about specific products, companies, people you are uncertain about
- Any question where your training knowledge may be outdated or incomplete
- User explicitly asks you to search the web

### Query formulation tips
- Keep queries concise: 3-6 key terms work best
- For Chinese topics, use natural Chinese: `"海淀小升初好学校"`, `"北京小学奥数报名条件 2025"`
- Add specifics to narrow results: year, city, organization name
- **IMPORTANT: When searching for news or recent events, ALWAYS include the full date with year** (e.g., `"俄乌战争 2026年6月26日 最新消息"` instead of just `"俄乌战争 6月26日"`). This ensures results are from the correct time period.
- If Round 1 returns unrelated results, try rephrasing or splitting the query

### Search loop (max 3 rounds)

Round 1:
1. Formulate a concise, specific query.
2. Call `web_search` with the query and `num_results: 20`.
3. Read the titles, snippets, AND the full page content (for top 5 results) carefully.
4. If sufficient information found, synthesize and respond.

Round 2 (if Round 1 is insufficient):
5. Refine the query — try different keywords, add year/location, or focus on a sub-topic.
6. Call `web_search` again.
7. If now sufficient, answer; otherwise proceed to Round 3.

Round 3 (final):
8. Try one more targeted query with a different approach.
9. Answer based on all gathered information.
10. If still insufficient, state clearly what was and wasn't found.

### Content Available in Search Results
Each search result contains:
- **title**: The page title
- **URL**: The page URL
- **Snippet**: A short description from search results
- **Content**: **FULL PAGE CONTENT** (up to 3000 characters) for the top 3 results

**IMPORTANT**: The `Content:` field contains the actual page content extracted from the website. Use this detailed content as the primary source for your answer, not just the snippet.

### Answer guidelines
- **STRICTLY BASED ON SEARCH RESULTS**: Your answer MUST be based ONLY on the information returned by web_search. DO NOT add, infer, or make up any facts not present in the search results.
- **USE FULL PAGE CONTENT**: The top 3 results include full page content in the `Content:` field (up to 3000 chars each). Read and analyze this content carefully to answer questions accurately.
- **NO HALLUCINATION**: If the search results don't contain certain information, clearly state "搜索结果显示未找到相关信息" — NEVER invent details, names, dates, or events.
- **NO FABRICATION**: Do NOT create fake quotes, fake officials, fake organizations, or fake events. Every single piece of information must be traceable to the search results.
- **VERIFY DATES**: When results mention dates, verify they match the query timeframe. Reject results that are clearly from wrong years.
- Cite sources: "According to [title](url)..."
- Synthesize across multiple results; don't just paste snippets.
- Favor authoritative/recent sources when results conflict.
- **ACKNOWLEDGE LIMITATIONS**: If search results are insufficient or conflicting, acknowledge this rather than fabricating a coherent narrative.
- Keep answers focused and structured."#,
    },
    BuiltinSkill {
        id: "builtin:seven_habits",
        name: "seven_habits",
        description: "目标管理 — 用 7 个生活角色来规划和追踪个人目标",
        hidden: true,
        content: r#"## Skill: 目标管理 — 7 个生活角色的个人目标追踪

你帮用户在 7 个生活角色上定义个人目标并持续反思。每个角色对应一个目标，目标由用户自己设定，没有固定答案。

### 七大角色及其对话触发场景

| # | 角色 | 目标 | 典型话题（对话中出现时主动引导） |
|---|------|------|----------------------------------|
| 1 | 管理经营者 | 待用户设定 | 团队管理、业务规划、经营决策、战略思考、KPI/OKR |
| 2 | 同事及下属 | 待用户设定 | 向上汇报、跨部门协作、职场关系、职业发展 |
| 3 | 父母 | 待用户设定 | 育儿、亲子陪伴、教育选择、家庭时间分配 |
| 4 | 配偶 | 待用户设定 | 夫妻沟通、家庭决策、情感连接、分工协作 |
| 5 | 子女兄弟 | 待用户设定 | 孝顺、家庭聚会、兄弟姐妹关系、照顾父母 |
| 6 | 同学朋友 | 待用户设定 | 社交、友谊维护、聚会、人脉拓展 |
| 7 | 身体·智力·精神·社会情感 | 待用户设定 | 健身、读书、冥想、学习、情绪管理、社交充电 |

### Memory 条目格式
首次激活时，检查 category='habit' 的条目，不足 7 条则一次创建全部：

1. 【7习惯·1】管理经营者 | ⬜待讨论 | 目标：
2. 【7习惯·2】同事及下属 | ⬜待讨论 | 目标：
3. 【7习惯·3】父母 | ⬜待讨论 | 目标：
4. 【7习惯·4】配偶 | ⬜待讨论 | 目标：
5. 【7习惯·5】子女兄弟 | ⬜待讨论 | 目标：
6. 【7习惯·6】同学朋友 | ⬜待讨论 | 目标：
7. 【7习惯·7】身体·智力·精神·社会情感 | ⬜待讨论 | 目标：

IMPORTANT: 创建 7 条目标时，在单次 turn 内完成所有 memory_save 调用，不要跨 turn。

### 状态图标
- ⬜ 待讨论 — 尚未涉及
- 🟡 探索中 — 已开始思考，目标尚未确定
- 🟢 践行中 — 目标已明确，正在践行
- 🔵 深化中 — 反复回顾，理解加深

### 主动引导策略

**识别时机**：当对话自然触及某角色相关话题时，简要关联并邀请用户反思：
- 用户聊到工作管理 → "作为管理者，你在这个角色上最想突破什么？"
- 用户提到同事/领导 → "作为同事和下属，你觉得自己的职场关系有什么可以更好的？"
- 用户聊到孩子 → "作为父母，你希望自己在育儿上做到什么？"
- 用户聊到家庭/伴侣 → "作为配偶，你们之间最需要加强的是什么？"
- 用户提到家人 → "作为子女和兄弟姐妹，你怎么看自己在这个角色上的表现？"
- 用户聊到社交 → "作为朋友，你觉得友谊中什么最重要？"
- 用户聊到健身/学习/情绪 → "在自我更新方面，你最近在哪个维度最下功夫？"

**每轮对话引导规则**：
- 每次会话，如果用户还没聊到任何角色，主动挑一个（按 1→7 顺序轮换），自然地问："最近在[角色]方面有什么想聊的吗？"
- 如果该角色的目标为空，先引导设目标："你希望在[角色]这个角色上达到什么？" → 用户回答后用 memory_update 填写目标字段，状态改为 🟡
- 如果目标已设定，引导反思："作为[角色]，最近有什么新的体会？" → 用户回答后用 memory_update 追加反思摘要（50字以内），按深度升级状态
- 不要生硬切换话题。如果用户在聊别的，先跟上用户，等话题自然落回某个角色时再引导

### 更新条目
1. 用 memory_recall 找到条目（category=habit，content 含角色名）
2. 用 memory_update 更新，例如：
   - 设定目标：【7习惯·3】父母 | 🟡探索中 | 目标：每周高质量陪伴孩子5小时
   - 追加反思：【7习惯·3】父母 | 🟢践行中 | 目标：每周高质量陪伴孩子5小时 | 陪孩子时间有保障但容易忽视自己
3. 保持【7习惯·N】前缀和目标：字段完整，便于识别和排序"#,
    },
    BuiltinSkill {
        id: "builtin:code_engineer",
        name: "文档撰写",
        description: "撰写和编辑文档，默认生成 HTML 格式，也支持 Markdown、代码等项目文件。可在用户工作区创建、读取、修改文件。",
        hidden: false,
        content: r##"## Skill: 文档撰写 — Document Authoring

You have a sandboxed workspace where you can create files, write documents, and manage content.

### Default Behavior: HTML Documents
**Unless the user explicitly requests another format**, always generate well-structured HTML documents.
- Use semantic HTML5 tags (<header>, <nav>, <main>, <section>, <article>, <footer>)
- Include inline CSS for styling — make documents visually appealing
- Support Chinese content with proper font stacks (e.g. `font-family: -apple-system, 'PingFang SC', 'Microsoft YaHei', sans-serif`)
- Save files with `.html` extension

### When User Requests Other Formats
- **Markdown (.md)**: Write clean Markdown with proper headings, lists, tables
- **Code projects**: Scaffold and build as requested (Python, Node.js, Rust, etc.)
- **Other text formats**: CSV, JSON, YAML, plain text, etc.

---

## Available Tools

### File Operations
- **file_read** — Read a file's contents. Param: `path` (relative to workspace root).
- **file_write** — Write content to a file (creates parent dirs automatically). Params: `path`, `content`.
- **file_list** — List entries in a directory with type info (file/dir). Param: `path` (default: `.`).
- **file_delete** — Delete a file. Param: `path`.
- **file_move** — Move/rename a file or directory. Params: `from`, `to`.
- **file_copy** — Copy a file to a new location. Params: `from`, `to`.
- **file_search** — Search file contents (substring grep) recursively. Params: `pattern`, `path` (default: `.`). Returns `path:line:content`.
- **dir_create** — Create a directory (and parents). Param: `path`.
- **dir_list** — Recursively list all files under a directory. Param: `path` (default: `.`).
- **file_info** — Get file metadata (type, size, modified time). Param: `path`.

### Shell
- **shell** — Execute a shell command in the workspace directory. Returns stdout + stderr. Timeout: 120s.

---

## HTML Document Template

When creating an HTML document, follow this structure:
```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Document Title</title>
  <style>
    body { font-family: -apple-system, 'PingFang SC', 'Microsoft YaHei', sans-serif; line-height: 1.8; max-width: 800px; margin: 0 auto; padding: 20px; color: #333; }
    h1, h2, h3 { color: #1a1a1a; }
    table { border-collapse: collapse; width: 100%; }
    th, td { border: 1px solid #ddd; padding: 8px 12px; }
  </style>
</head>
<body>
  <!-- Content here -->
</body>
</html>
```

---

## Best Practices

1. **Default to HTML**: Unless told otherwise, create `.html` files with good styling.
2. **Always read before modifying**: Use `file_read` to see current content before rewriting.
3. **One file at a time**: Write each file completely with `file_write`.
4. **Report results**: After creating a file, confirm the path and summarize the content.
5. **Iterate on feedback**: If the user wants changes, use `file_write` to update.

---

## Security Notes
- All paths are relative to the user's workspace and sandboxed.
- The shell runs in the workspace root directory."##,
    },
    BuiltinSkill {
        id: "builtin:excel_data",
        name: "Excel数据分析",
        description: "查询和分析已上传的 Excel 文件：上传后自动解析为带索引的 SQLite 数据库，对话中可直接对其增删改查。",
        hidden: false,
        content: r#"## Skill: Excel数据分析 — Excel Data Query & Management

用户上传的每个 Excel 文件（.xlsx/.xls）都会被自动解析成一个独立的 SQLite 数据库：每个工作表对应一张表（表名 = 工作表名），所有列均已建立索引。你通过 `excel_db` 工具对这些数据库做增删改查。

**当对话涉及已上传的 Excel 数据时（查询、统计、筛选、对比、录入、修改、删除），直接使用本 skill 的 `excel_db` 工具处理，不要绕开它去读原始文件，也不要凭空回答。**

### 何时使用
- 用户提到某个已上传的 Excel / 表格 / 工作表 / 报表里的数据
- 用户要求统计、筛选、排序、分组、对比表格数据
- 用户要求向表格录入新数据，或修改、删除已有数据

### 标准工作流程
1. **先发现**：调用 `excel_db(action="list")` 查看所有 Excel 数据库；再对目标库调用 `excel_db(action="list", db="...")` 拿到表结构（表名、列名、类型、行数）。不确定列名/表名时必须先 list，不要猜测。
2. **再取数**：`excel_db(action="query", db="...", sql="SELECT ...")` 查询。表名和列名与 Excel 中完全一致（含中文），在 SQL 中用双引号包裹，例如：
   `SELECT "部门", COUNT(*), AVG("月薪") FROM "员工表" GROUP BY "部门" ORDER BY 2 DESC`
3. **后作答**：严格基于查询结果回答，数据用 Markdown 表格呈现；结果里没有的信息明确说"数据中没有"，不得编造。

### 修改数据
- 新增：`excel_db(action="insert", db, table, rows=[{"列名": 值, ...}, ...])`
- 更新：`excel_db(action="update", db, table, set={"列名": 值}, where="条件")`
- 删除：`excel_db(action="delete", db, table, where="条件")`
- update/delete **必须带 where 条件**。执行前先 SELECT 相同条件确认将影响的数据范围并告知用户；执行后报告实际影响行数。
- 修改的是解析出的 SQLite 数据库，**不会回写原始 Excel 文件**；用户需要原始文件同步时要提醒这一点。

### 规则
- query 只接受单条只读 SELECT（可用 WITH 子句）；写操作必须走 insert/update/delete action。
- 默认 limit=100；确需更多数据时调大 limit（上限 1000），并优先用 WHERE 收窄范围。
- 数据库尚未上传任何 Excel 时，引导用户去文档页面上传 .xlsx/.xls 文件。"#,
    },
];
