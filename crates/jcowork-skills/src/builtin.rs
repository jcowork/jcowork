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
];
