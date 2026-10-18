# 手机端优化执行计划(mobile 分支)

> 本文档是**持久化执行状态记录**。会话若中断,重连后先读「当前状态」一节,
> 找到第一个未完成任务,按「任务分解」继续执行;每完成/开始一项,更新对应状态标记。
>
> 状态标记:⬜ 未开始 | 🔄 进行中 | ✅ 完成 | ⚠️ 受阻(附原因)
> 改动均发生在 `/root/jcowork`(git 分支 `mobile`,工作区初始为 clean,可直接 `git diff` 审阅)。

## 当前状态(恢复时先看这里)

- 最后更新:2026-10-18(凌晨)
- **全部完成,已部署上线**(计划内全部 5 个 Phase + 用户追加的部署任务)
- 已验证:cargo workspace 全量测试通过(单测 136 + gateway 集成测试 48/48,含新增 2 个 feed 用例);
  `npm run build` 通过;lint 与基线持平(41 存量错误,0 新增);release 二进制部署到
  `/opt/jcowork` 并经 `./run.sh` 重启,健康检查 200、新前端 bundle 生效、
  `idx_cron_task_results_user_exec` 索引自动迁移创建、/api/feed 401 鉴权正常。
- 未做:真机浏览器窄屏人工验证(5.3;本环境无浏览器工具,已用代码断点+CSS line-clamp 保证一屏两条)
- 阻塞:无。git 未提交,待用户确认。

### 部署约定(用户 2026-10-17 追加)

- 测试全部通过后:`cargo build --release` → 替换 `/opt/jcowork/jcowork` 与 `/opt/jcowork/web/dist` →
  kill 旧进程 → 在 /opt/jcowork 下执行 `./run.sh` → curl 验证健康与 /api/feed。
- 服务端口 3000(.env `JCWORK_PORT=3000`),静态文件相对路径 `web/dist`。

## 需求原文(用户 2026-10-17)

1. 优化手机浏览体验,优化布局和功能。
2. 登录后,页面底部有三个 tab:①AI头条 ②联系人 ③我的。
3. AI头条:聚合「联系人(含公开用户)的周期任务与公开文档」+「本机已登录所有账号的周期任务与本地文档」,按时间倒序逐条呈现;持续下滑加载更多;点击看详情;**手机一屏至少两条**。
4. 我的:记忆备忘、技能连接、设置配置。

## 需求原文(用户 2026-10-18 追加)

5. AI头条呈现的周期任务与文档,都要有非常好的摘要,吸引人点击进去看。
   - 已确认:**只做 AI 摘要**,卡片标题保持任务名/文件名。
   - 方案(已批准,见 `/root/.qoder/plans/gleaming-boulder-owl.md`):
     - 入库时异步生成(cron_executor 成功落库后 / 手动 POST results / upload / reindex / set_public
       五个写路径挂 `SummaryService::ensure_*`),fire-and-forget,失败仅 log、卡片回退原文截断;
     - feed 读取时对无摘要的历史条目惰性补生成(自愈回填,inflight 去重 + Semaphore(3) 并发上限);
     - 跳过条件:非 success 任务、原文 <200 字符、文档解析占位符;
     - 存储:`cron_task_results.summary` / `documents.summary` 两列(ALTER 幂等迁移),
       documents UPSERT 内容变化时清空 summary 待重生成;LLM 用 `default_model` 非流式 + 60s 超时,
       提示词要求"匹配内容语言、≤60汉字、首句给最有价值信息点、不编造";
     - 前端 `AiFeed.tsx` 卡片改为优先 `item.summary`,回退 excerpt/snippet。
6. AI头条点开文档后的预览,高度/宽度要自适应手机屏幕。
   - 详情滚动容器改 `flex:1 + minHeight:0`;HTML iframe 由固定 `70vh` 改为 flex 撑满
     (容器 `overflow:hidden`、iframe `flex:1; width:100%; height:100%`),任意屏宽高自适应。
7. 文档要有独立打开的跳转入口,方便分享。
   - 新增独立查看路由 `#/doc?u={userId}&n={username}&p={path}&pub={0|1}`(`web/src/utils/docUrl.ts`),
     `App` 层解析命中即渲染纯 `DocViewer`(无登录墙、无应用外壳),否则渲染主应用;
   - `DocViewer.tsx`:复用分页内容接口(md→ResultMarkdown,html→sandbox iframe,其余→pre),
     滚动触底加载,头部带「复制链接」;
   - AiFeed 文档详情头部新增「独立打开」「分享」按钮(navigator.share,降级剪贴板复制);
   - 分享语义:公开文档(来源为公开用户,或本机账号的 is_public 文档)→ 链接**无需登录**可看;
     私有文档 → 链接仍可打开但需本机已登录账号;
   - 后端:`/api/public-users/{user_id}/documents/content` 从 protected 段挪到 public 段
     (handler 自带 ensure_public_user + `is_public=1` SQL 双重校验,挪动安全);
     `FeedItem::Document`/`FeedDocument` 增加 `is_public` 字段。
8. 全部完成后,改动 git commit 并 push 到远程(用户已授权)。

### 摘要功能任务分解(2026-10-18)

- [x] S1 存储层:migration.rs / cron_store.rs / scheduler.rs / workspace_index.rs
      (summary 列、set_result_summary、set_document_summary、get_summary_source、FeedDocument)
- [x] S2 网关:summary.rs(SummaryService + prompt + sanitize + 单测)、AppState.summary
- [x] S3 接线:cron_executor / router/cron / upload / doc_index / feed(惰性回填)、
      server + desktop 两个 main.rs
- [x] S4 前端:AiFeed.tsx summary 字段与回退展示
- [x] S5 测试:集成测试 2 个(写入路径 + 读取回填)、cargo test --workspace、npm build
      (2026-10-18:summary 单测 8/8;api_integration 51/51 含 3 个新用例;workspace 全绿;
      npm build 通过;eslint 基线 41 零新增)
- [x] S6 部署:按既定约定上线 /opt/jcowork(2026-10-18 晚:增量 release 构建 8m42s;
      备份 jcowork.bak.2610181839 + web/dist.bak.2610181839 → 替换 → kill 165826 →
      ./run.sh 新 PID 207788;冒烟:health 200、cron_task_results.summary 开机已迁移、
      documents.summary 首开索引懒迁移实测生效、公开文档无 token 200(非公开属主 404)、
      /api/feed 无 token 401、新 bundle index-DtSfUFrE.js 200 且被 index.html 引用、
      WS 会话历史恢复、启动日志无错误)

### 追加需求任务分解(2026-10-18)

- [x] A1 后端:公开文档内容接口挪 public 段;FeedDocument/FeedItem 加 is_public
- [x] A2 前端:utils/docUrl.ts(路由解析/链接生成)、DocViewer.tsx、App.tsx 分流、
      AiFeed 详情头部「独立打开/分享」按钮、i18n 键
- [x] A3 前端:文档详情预览自适应(详情容器 flex:1/minHeight:0,iframe flex 撑满替代 70vh)
- [x] A4 测试与验证:cargo test --workspace(含 S5 集成测试)、npm build、eslint 基线
      (2026-10-18 全过:api_integration 51/51,含 test_public_document_content_requires_no_login)
- [ ] A5 部署 + git commit & push 远程

## 设计方案(已定)

### 总体

- 仅在窄屏(≤768px,与现有 `.mobile-topbar` 断点一致)启用底部三 Tab;桌面端布局不变。
- 顶部栏在移动端:根 Tab 页显示 ☰(抽屉导航保留,桌面功能不丢);进入详情/聊天后显示 ← 返回根 Tab。
- 聊天入口:联系人 Tab 点击本机账号 → 切换账号并进入聊天;抽屉里也有「对话」入口。

### 后端:新增聚合 Feed API(新建 `crates/jcowork-gateway/src/router/feed.rs`)

1. `GET /api/feed?offset=&limit=`(需登录)— 本人「周期任务结果 + 本地文档」按时间倒序合并分页。
2. `GET /api/public-users/{user_id}/feed?offset=&limit=`(需登录,目标须 is_public)— 该公开用户「周期任务结果 + 公开文档」合并分页。
3. `GET /api/feed/results/{result_id}`(需登录)— 单条任务结果全文;本人所有,或结果属主是公开用户。

实现要点:
- 两表各自取 `offset+limit` 行,归并后切 `[offset, offset+limit)`(offset-merge);
  `has_more` = 任一来源取满 `offset+limit` 行(尾部可能多一次空请求,可接受)。
- 时间戳归一化为 RFC3339 UTC:`indexed_at`(SQLite `datetime('now')` 形如
  `2026-10-16 12:00:00`)→ `2026-10-16T12:00:00Z`;`executed_at` 本就是 chrono RFC3339。
- 任务条目只带 `excerpt`(SQL `substr(output,1,400)`),全文走 results 接口。
- 文档条目复用 `IndexedDocument` 字段(file_path/dir_path/filename/content_type/size/snippet/indexed_at)。
- 新存储方法:
  - `CronStore` trait(jcowork-cron/src/scheduler.rs:182)+ `CronJobStore`:
    `list_user_results(user_id, limit, offset)`(LEFT JOIN cron_jobs 取 name/prompt)、
    `get_result(result_id)`;`CronScheduler` 增加委托方法。
  - `WorkspaceIndex`(jcowork-storage/src/workspace_index.rs):
    `list_recent(limit, offset)`、`list_public_recent(limit, offset)`(`ORDER BY indexed_at DESC, id DESC`)。
- 迁移:migration.rs 索引区(~:195)追加
  `CREATE INDEX IF NOT EXISTS idx_cron_task_results_user_exec ON cron_task_results(user_id, executed_at)`。
- 路由注册:router/mod.rs `build_router()` protected 段(~:450-543)。

### 前端:移动端外壳 + 三个 Tab(主要改 `web/src/App.tsx`)

- 新状态:`mobileTab('feed'|'contacts'|'mine')`、`mobileChatOpen`、`isNarrow`(resize 监听)。
- 渲染优先级(窄屏):任一详情视图(设置/文档/周期任务/记忆/技能/管理/公开主页)> 聊天(mobileChatOpen)> 根 Tab 内容。
- 聊天组件保持全账号常挂载(WS 后台流式),仅显隐切换 —— 不动现有机制。
- 底部 TabBar 为 flex 布局子元素(非 fixed),根 Tab 时显示;`padding-bottom: env(safe-area-inset-bottom)`。
- 新组件:
  - `web/src/components/AiFeed.tsx`:多源 k 路归并(in-flight 防抖)、滚动触底增量加载、
    任务/文档卡片(紧凑:标题1行+摘要2-3行+元信息,保证一屏≥2条,用 CSS line-clamp)、
    点击弹全屏详情(任务=全文 Markdown,复用 Schedule 的 ResultMarkdown;文档=分页文本预览,
    本地走 `/api/workspace/index/content`,公开走 `/api/public-users/{id}/documents/content`)。
  - `web/src/components/ContactsTab.tsx`:本机账号列表(切换→聊天,流式/未读圆点,移除账号)+
    公开用户列表(→PublicProfile)+ 添加账号。
  - `web/src/components/MineTab.tsx`:当前账号头部 + 记忆备忘/技能连接/设置配置 三个入口。
- 数据源:每个本机账号 → `/api/feed`(各自 token);每个「未本机登录的公开用户」→
  `/api/public-users/{id}/feed`(活跃 token;与本机账号按 userId 去重)。
- i18n(web/src/i18n.tsx,zh 在 :7,en 在 :516)新增键:tabFeed/tabMine/feedEmpty/feedTask/
  feedDocument/feedNoMore/feedLoadingMore/feedSourceError/feedViewAll?/myAccounts/back 等。
- CSS(web/src/index.css,移动断言在 :232-255):`.mobile-tabbar`、`.clamp-1/2/3`、safe-area。

### 既有页面的移动端适配(布局)

- Schedule.tsx(:449 双栏 380px+右侧):窄屏改纵向,右侧预览高度 45%。
- Documents.tsx(树+预览):窄屏纵向,树区高度 ~38%。
- PublicProfile.tsx(:376 双栏 280px+预览):同上。
- Memory/Settings/SkillsSquare/AdminUsers:检查后按需小改(SkillsSquare 已有 1 列 media query)。

## 任务分解

### Phase 1 后端 Feed API

- [x] 1.1 ✅ jcowork-cron:`UserTaskResultEntry` 结构 + `CronStore` trait 增
  `list_user_results`/`get_result` + `CronScheduler` 委托方法(scheduler.rs)
- [x] 1.2 ✅ jcowork-storage:cron_store.rs 实现上述两法(JOIN cron_jobs;list 用 substr 截断)
- [x] 1.3 ✅ jcowork-storage:migration.rs 加 `idx_cron_task_results_user_exec` 索引
- [x] 1.4 ✅ jcowork-storage:workspace_index.rs 增 `list_recent`/`list_public_recent`
- [x] 1.5 ✅ gateway:新建 router/feed.rs(三个 handler + 归并/归一化辅助)
- [x] 1.6 ✅ gateway:router/mod.rs 注册路由
- [x] 1.7 ✅ `cargo check --workspace` 通过;`cargo test -p jcowork-cron -p jcowork-storage` 通过
      (2026-10-18 随 S5 全仓测试复验)

### Phase 2 前端基础

- [x] 2.1 ✅ i18n.tsx 新增 zh/en 键
- [x] 2.2 ✅ index.css:`.mobile-tabbar`、clamp 工具类、safe-area
- [x] 2.3 ✅ Schedule.tsx 导出 `ResultMarkdown` 供复用

### Phase 3 三个 Tab 页面

- [x] 3.1 ✅ AiFeed.tsx(数据源归并 + 卡片 + 无限滚动 + 两种详情)
- [x] 3.2 ✅ ContactsTab.tsx
- [x] 3.3 ✅ MineTab.tsx
- [x] 3.4 ✅ App.tsx 移动外壳:mobileTab/mobileChatOpen/isNarrow、顶栏返回键、TabBar、根视图渲染、
  抽屉/联系人进入聊天时置 mobileChatOpen

### Phase 4 既有页面移动适配

- [x] 4.1 ✅ Schedule.tsx 窄屏纵向
- [x] 4.2 ✅ Documents.tsx 窄屏纵向
- [x] 4.3 ✅ PublicProfile.tsx 窄屏纵向
- [x] 4.4 ✅ Memory/Settings/AdminUsers 检查并小改(如需)

### Phase 5 构建与验证

- [x] 5.1 ✅ `cd web && npm run build`(tsc+vite)通过(2026-10-17;2026-10-18 摘要+DocViewer 改动后
      重建通过,eslint 对基线 41 errors 零新增,4 个新文件 0 错误)
- [x] 5.2 ✅(集成测试 48/48 覆盖;生产实例冒烟验证)启动服务,API 级验证:注册/登录两账号、建 cron job、写结果、索引文档,
  curl `/api/feed` 与 `/api/public-users/{id}/feed` 验证归并排序、分页 has_more、时间归一化;
  curl `/api/feed/results/{id}` 验证属主/公开两种路径
- [ ] 5.3 ⚠️ 本环境无浏览器工具,未做真机人工验证;三 Tab/无限滚动/详情逻辑由集成测试与 tsc 覆盖,一屏两条由紧凑卡片+line-clamp CSS 保证
- [x] 5.4 ✅ 文档已更新;git 未提交(待用户确认)

## 执行日志(追加)

- 2026-10-17 计划创建。调研结论:后端无任何聚合 feed 端点(需新建);前端 App.tsx 以
  布尔视图切换,需加 mobileTab 层;CronStore 仅 CronJobStore 一个实现,加 trait 方法安全;
  迁移索引可直接 IF NOT EXISTS 追加;executed_at=chrono RFC3339,indexed_at=SQLite UTC naive。
- 2026-10-17 晚 Phase 4 落地:新增 `web/src/utils/useIsNarrow.ts`(共享窄屏 hook,断点 768 与
  App.tsx 一致);Schedule/Documents/PublicProfile 三处双栏窄屏改纵向堆叠,预览区分别占
  45%/62%/62% 高度,未选中预览时列表占满全高;Documents 窄屏隐藏拖拽条。Memory/Settings/
  AdminUsers 检查:单列或 flex-wrap、弹窗≤360px,无需修改。`npm run build` 通过。
- 2026-10-18 凌晨 lint 对照基线(HEAD 41 errors):修复 AiFeed 全部 6 个新增错误——
  `failedSources`/`allDone` 由渲染期读 `sourcesRef.current` 改为 `sourceStats` state 快照
  (reload/ensureCount finally 处更新);初始 effect 改 `setTimeout(reload,0)` 避免同步 setState。
  修复后 4 个新文件 0 错误,`npm run build` 通过;存量 41 个错误为基线遗留(App/Chat/Memory 等)。
- 2026-10-18 凌晨 验证与部署:`cargo test --workspace` 单测全过(136 个,gateway 集成测试单跑
  48/48 通过,含 feed 新用例);release 构建 68 分钟;部署流程:备份
  `/opt/jcowork/jcowork.bak.10180302` 与 `web/dist.bak.10180302` → kill 117597 → 替换二进制+dist
  → `chmod +x run.sh`(原本无执行权限)→ `./run.sh`(新 PID 164358,端口 3000)。
  冒烟验证:/api/health 200、index 200、新 bundle index-QuDGZVQz.js 200、
  /api/feed 未带 token 401、启动日志无错误、WS 会话历史恢复、
  `idx_cron_task_results_user_exec` 索引已自动建到生产库。磁盘:清理 pip 缓存(3.1G)后
  曾满盘,当前 ~1.4G 余量,后续留意。
- 2026-10-18 晚 AI 摘要落地(需求 #5/#6/#7):存储层 `cron_task_results`/`documents` 增
  `summary` 列(ALTER 吞错迁移)+ 写读方法;gateway 新增 `summary.rs`(SummaryService:
  inflight 去重 + Semaphore(3) + 60s timeout + sanitize;短内容/失败任务/解析占位符跳过);
  接线:cron_executor(成功后)/router:store_cron_job_result、upload、doc_index、feed(读取
  惰性回填);server/desktop 两 main 装配。前端 AiFeed 卡片优先 `item.summary`,回退
  原文截断;文档详情预览改 flex 自适应屏宽高;新增「独立打开/分享」入口 —— docUrl.ts
  (`#/doc?u=&n=&p=&pub=` hash 路由)、DocViewer.tsx(公开文档免登录查看)、公开内容接口
  挪到 public 路由段。测试:summary 单测 8/8、api_integration 51/51(3 个新用例:
  写入钩子生成 / 读取回填 / 公开文档无 token 可读)、cargo test --workspace 全绿、
  npm build 通过、eslint 基线零新增。磁盘清理:删 debug/incremental(2.5G)后 ~2.7G 余。
