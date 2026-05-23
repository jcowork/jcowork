#!/usr/bin/env python3
"""
Web search via Playwright headless browser.
Primary: www.sogou.com  (reliable Chinese search, good relevance)
Fallback: cn.bing.com   (may have CDN quality issues on some networks)

Usage:
    python bing_search.py <query> [num_results]

Outputs JSON array to stdout:
    [{"title": "...", "url": "...", "snippet": "..."}, ...]
"""

import sys
import json
import asyncio
import urllib.parse
from playwright.async_api import async_playwright

# Prefer system Chrome/Chromium (less likely to be detected as bot)
SYSTEM_CHROME_PATHS = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium-browser",
]


def find_chrome() -> str | None:
    import os, shutil
    for path in SYSTEM_CHROME_PATHS:
        if os.path.exists(path):
            return path
    return shutil.which("google-chrome") or shutil.which("chromium")


async def _make_browser(p):
    chrome_path = find_chrome()
    launch_kwargs = dict(
        headless=True,
        args=[
            "--no-sandbox",
            "--disable-blink-features=AutomationControlled",
            "--disable-dev-shm-usage",
        ],
    )
    if chrome_path:
        launch_kwargs["executable_path"] = chrome_path
    browser = await p.chromium.launch(**launch_kwargs)
    context = await browser.new_context(
        user_agent=(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
            "AppleWebKit/537.36 (KHTML, like Gecko) "
            "Chrome/124.0.0.0 Safari/537.36"
        ),
        locale="zh-CN",
        timezone_id="Asia/Shanghai",
        extra_http_headers={"Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8"},
    )
    await context.add_init_script(
        "Object.defineProperty(navigator,'webdriver',{get:()=>undefined});"
    )
    return browser, context


async def search_sogou(query: str, num_results: int = 20) -> list[dict]:
    """Search via Sogou WAP interface — reliable for Chinese queries, bypasses desktop anti-bot."""
    async with async_playwright() as p:
        browser, context = await _make_browser(p)
        # Use mobile viewport + UA for WAP interface (avoids desktop bot detection)
        page = await context.new_page()
        await page.set_viewport_size({"width": 390, "height": 844})
        await page.set_extra_http_headers({"User-Agent": "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"})

        encoded = urllib.parse.quote(query)
        url = f"https://wap.sogou.com/web/searchList.jsp?keyword={encoded}&pid=sogou-waps-1000000&ie=utf8"
        await page.goto(url, wait_until="domcontentloaded", timeout=20000)
        try:
            await page.wait_for_selector("a[href]", timeout=8000)
        except Exception:
            pass

        raw = await page.evaluate("""
            () => {
                const out = [];
                const seen = new Set();

                // WAP result cards: links containing article titles
                // Skip:
                //  1. javascript: hrefs
                //  2. Sogou search-suggestion/navigation links (any link whose FINAL destination
                //     is a searchList page — these show up as the `url=` param inside the href)
                //  3. Pure wap.sogou.com domain links that aren't content proxy URLs (id= prefix)
                const isSogouSearchLink = (href) => {
                    // Direct searchList links
                    if (href.includes('wap.sogou.com/web/searchList') ||
                        href.includes('m.sogou.com/web/searchList') ||
                        href.includes('m.sogou.com.web/searchList')) return true;
                    // Proxied links where the inner `url=` param points back to a searchList
                    try {
                        const inner = decodeURIComponent((href.match(/[?&]url=([^&]+)/) || [])[1] || '');
                        if (inner.includes('searchList.jsp')) return true;
                    } catch(e) {}
                    return false;
                };

                for (const a of document.querySelectorAll('a[href]')) {
                    const href = a.href || '';
                    if (!href || href.startsWith('javascript')) continue;
                    if (isSogouSearchLink(href)) continue;
                    // Accept: Sogou content-proxy URLs (id= prefix) OR external URLs
                    const isContentProxy = href.startsWith('https://wap.sogou.com/web/id=');
                    const isExternal = href.startsWith('http') && !href.includes('wap.sogou.com') && !href.includes('m.sogou.com');
                    if (!isContentProxy && !isExternal) continue;

                    const title = a.textContent.replace(/\\s+/g, ' ').trim();
                    // Skip noise: short/numeric titles, video UI elements, author attribution
                    if (title.length < 6 || seen.has(title)) continue;
                    if (/^[\\d\\s:]+$/.test(title)) continue;           // pure numbers/times
                    if (/^看完整视频/.test(title)) continue;              // video preview labels
                    if (/企鹅号[··]/.test(title)) continue;              // any author attribution
                    if (/^微信公众号[··]/.test(title)) continue;           // WeChat account attribution
                    if (/^\\d{4}年\\d{1,2}月\\d{1,2}日$/.test(title)) continue; // bare dates
                    if (/^查看更多/.test(title)) continue;                // navigation buttons
                    if (/- 精选视频$/.test(title)) continue;              // section headers
                    seen.add(title);

                    // Try to find a nearby snippet in the parent container
                    let snippet = '';
                    let el = a.parentElement;
                    for (let i = 0; i < 4 && el; i++, el = el.parentElement) {
                        const ps = el.querySelectorAll('p, [class*="abstract"], [class*="snippet"], [class*="desc"], [class*="txt"]');
                        for (const p of ps) {
                            const t = p.textContent.replace(/\\s+/g, ' ').trim();
                            if (t.length > 20 && t !== title) {
                                snippet = t.slice(0, 200);
                                break;
                            }
                        }
                        if (snippet) break;
                    }

                    out.push({ title, url: href, snippet });
                    if (out.length >= 25) break;
                }
                return out;
            }
        """)
        await browser.close()
        return (raw or [])[:num_results]


async def search_bing(query: str, num_results: int = 20) -> list[dict]:
    """Fallback search via cn.bing.com."""
    async with async_playwright() as p:
        browser, context = await _make_browser(p)
        page = await context.new_page()
        encoded = urllib.parse.quote(query)
        url = f"https://cn.bing.com/search?q={encoded}&setlang=zh-CN&cc=CN"
        await page.goto(url, wait_until="networkidle", timeout=30000)
        try:
            await page.wait_for_selector("#b_results li.b_algo", timeout=10000)
        except Exception:
            pass

        raw = await page.evaluate("""
            () => {
                const items = document.querySelectorAll('#b_results li.b_algo');
                const out = [];
                for (const item of items) {
                    const h2a = item.querySelector('h2 > a');
                    const title = h2a ? h2a.textContent.trim() : '';
                    const url   = h2a ? (h2a.href || h2a.getAttribute('href') || '') : '';
                    let snippet = '';
                    for (const sel of ['.b_caption p', '.b_algoSlug', '.b_snippetBigText', 'p']) {
                        const el = item.querySelector(sel);
                        if (el && el.textContent.trim()) {
                            snippet = el.textContent.trim();
                            break;
                        }
                    }
                    if (title || url) out.push({ title, url, snippet });
                }
                return out;
            }
        """)
        await browser.close()
        return (raw or [])[:num_results]


async def search(query: str, num_results: int = 20) -> list[dict]:
    """Search with Sogou primary, Bing fallback if Sogou returns < 3 results."""
    results = await search_sogou(query, num_results)
    if len(results) < 3:
        # Sogou failed or returned too few results, try Bing
        bing_results = await search_bing(query, num_results)
        if len(bing_results) > len(results):
            results = bing_results
    return results


def main():
    if len(sys.argv) < 2:
        print(json.dumps({"error": "Usage: bing_search.py <query> [num_results]"}))
        sys.exit(1)

    query = sys.argv[1]
    num_results = int(sys.argv[2]) if len(sys.argv) > 2 else 20

    try:
        results = asyncio.run(search(query, num_results))
        print(json.dumps(results, ensure_ascii=False))
    except Exception as e:
        print(json.dumps({"error": str(e)}))
        sys.exit(1)


if __name__ == "__main__":
    main()
