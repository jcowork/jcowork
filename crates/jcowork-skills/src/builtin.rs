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
        name: "web_search",
        description: "Search the web using a headless browser (Sogou WAP), retrieve top results, and answer questions based on live internet content.",
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
- If Round 1 returns unrelated results, try rephrasing or splitting the query

### Search loop (max 3 rounds)

Round 1:
1. Formulate a concise, specific query.
2. Call `web_search` with the query and `num_results: 20`.
3. Read the titles and snippets carefully.
4. If sufficient information found, synthesize and respond.

Round 2 (if Round 1 is insufficient):
5. Refine the query — try different keywords, add year/location, or focus on a sub-topic.
6. Call `web_search` again.
7. If now sufficient, answer; otherwise proceed to Round 3.

Round 3 (final):
8. Try one more targeted query with a different approach.
9. Answer based on all gathered information.
10. If still insufficient, state clearly what was and wasn't found.

### Answer guidelines
- Cite sources: "According to [title](url)..."
- Synthesize across multiple results; don't just paste snippets.
- Favor authoritative/recent sources when results conflict.
- Keep answers focused and structured."#,
    },
    BuiltinSkill {
        id: "builtin:seven_habits",
        name: "seven_habits",
        description: "Track your practice of the 7 Habits of Highly Effective People with self-assessment and guided reflection.",
        content: r#"## Skill: seven_habits — The 7 Habits of Highly Effective People

You help the user track and reflect on their practice of Stephen Covey's 7 Habits, each from a specific life-role perspective.

### Habit entries in memory
On first activation, check memory for entries with category='habit'. If fewer than 7 habit entries exist, create all 7 using memory_save with category='habit':

1. 【7习惯·1】积极主动 · 管理经营者 | ⬜待讨论
2. 【7习惯·2】以终为始 · 同事及下属 | ⬜待讨论
3. 【7习惯·3】要事第一 · 父母 | ⬜待讨论
4. 【7习惯·4】双赢思维 · 配偶 | ⬜待讨论
5. 【7习惯·5】知彼解己 · 子女兄弟 | ⬜待讨论
6. 【7习惯·6】统合综效 · 同学朋友 | ⬜待讨论
7. 【7习惯·7】不断更新 · 身体·智力·精神·社会情感 | ⬜待讨论

IMPORTANT: When creating the 7 habit entries, make all 7 memory_save calls in a single turn. Do NOT split them across multiple turns.

### Status icons
- ⬜ 待讨论 — not yet discussed
- 🟡 探索中 — initial reflection started
- 🟢 践行中 — clear self-assessment, actively practicing
- 🔵 深化中 — revisited and deepened understanding

### Proactive guidance
- Each habit is anchored to a life role. When conversation touches that role, connect it to the corresponding habit and invite reflection:
  - 管理经营/工作决策 → 习惯1（积极主动·管理经营者）
  - 职场方向/同事关系 → 习惯2（以终为始·同事及下属）
  - 育儿/家庭时间安排 → 习惯3（要事第一·父母）
  - 夫妻沟通/家庭决策 → 习惯4（双赢思维·配偶）
  - 与父母兄弟相处 → 习惯5（知彼解己·子女兄弟）
  - 朋友社交/团队协作 → 习惯6（统合综效·同学朋友）
  - 健身/学习/冥想/社交 → 习惯7（不断更新·身体·智力·精神·社会情感）
- Once per session, if the user hasn't discussed any habit yet, pick one habit (rotate through all 7 over time) and gently ask: "作为[角色]，你对【7习惯·N】有什么体会？"
- After the user shares their reflection, use memory_update to update the entry with the status icon and a concise self-assessment summary (under 50 chars).
- Do NOT force the discussion — if the user changes topic, follow them naturally.

### Updating entries
When the user discusses a habit:
1. Use memory_recall to find the entry (category=habit, content contains the habit name)
2. Use memory_update with the entry's id to update content, e.g.:
   【7习惯·3】要事第一 · 父母 | 🟢践行中 | 陪孩子时间有保障但容易忽视自己
3. Keep the 【7习惯·N】 prefix intact so entries stay identifiable and sortable."#,
    },
];
