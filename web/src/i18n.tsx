import { createContext, useContext, useState } from 'react';
import type { ReactNode } from 'react';

type Lang = 'zh' | 'en';

const translations = {
  zh: {
    // Sidebar
    chat: '对话',
    documents: '文档',
    schedule: '日程',
    memory: '记忆',
    skills: '技能',
    settings: '设置',
    signedInAs: '已登录',
    navigation: '导航',
    logout: '退出登录',

    // Login
    username: '用户名',
    password: '密码',
    login: '登录',
    register: '注册',
    alreadyHaveAccount: '已有账号？',
    dontHaveAccount: '没有账号？',
    authFailed: '认证失败，请重试',

    // Chat
    typeMessage: '输入消息...',
    send: '发送',
    browseWorkspace: '浏览工作区文件',
    referenceUrl: '引用URL',
    enterUrl: '输入URL...',
    add: '添加',
    cancel: '取消',
    copied: '已复制',
    copy: '复制',
    more: '更多',
    collapse: '收起',
    thinking: '思考中...',
    callingTool: '调用工具',
    toolCompleted: '工具完成',
    stop: '停止',
    generationStopped: '已停止生成',

    // Documents
    newFolder: '新建文件夹',
    upload: '上传',
    folderName: '文件夹名称...',
    create: '创建',
    creating: '创建中...',
    uploading: '上传中...',
    currentPath: '当前路径',
    root: '根目录',
    loading: '加载中...',
    noFiles: '暂无文件',
    preview: '预览',
    download: '下载',
    delete: '删除',
    confirmDelete: '确认删除',
    myDocuments: '我的文档',
    newSubfolder: '新建子文件夹',
    uploadHere: '上传到这里',
    openInBrowser: '在浏览器中打开',
    close: '关闭',
    selectFilePreview: '选择文件预览',
    clickFolderToExpand: '点击文件夹展开 · 点击文件查看',
    workspaceEmpty: '工作区为空',
    useChatToCreate: '使用对话来创建文件和项目！',
    rows: '行',
    excelPreviewFirst: '预览前',
    excelNoTables: '该 Excel 没有可显示的数据表',

    // Schedule
    reminders: '提醒',
    cronJobs: '定时任务',
    addReminder: '添加提醒',
    noReminders: '暂无提醒',
    noCronJobs: '暂无定时任务',
    message: '内容',
    time: '时间',
    enabled: '已启用',
    disabled: '已禁用',
    addCronJob: '添加定时任务',
    cronSchedule: '计划',
    cronPrompt: '提示词',
    lastRun: '上次运行',
    never: '从未',
    enable: '启用',
    disable: '禁用',
    remove: '删除',
    noMessage: '无内容',
    trySayingReminder: '试试在对话中说"提醒我下午3点开会"',
    trySayingCron: '试试在对话中说"每天早上9点提醒我写日报"',
    lastRunLabel: '上次运行',

    // Memory
    searchMemories: '搜索记忆...',
    search: '搜索',
    noMemories: '暂无记忆',
    edit: '编辑',
    save: '保存',
    category: '分类',
    clear: '清除',
    refresh: '刷新',
    noMatchingMemories: '未找到匹配的记忆',
    tryDifferentSearch: '试试其他关键词',
    trySaying: '试试在对话中说"记住，我喜欢深色主题"',
    removeMemory: '删除',

    // Skills
    skillsSquare: '技能广场',
    skillsEnabled: '个技能已启用 — 启用的技能会在每次新会话中注入到 Agent',
    all: '全部',
    builtIn: '内置',
    mySkills: '我的技能',
    loadingSkills: '加载技能中...',
    noCustomSkills: '暂无自定义技能',
    noSkillsFound: '未找到技能',
    enableSkill: '启用技能',
    disableSkill: '禁用技能',
    skillInstructions: '技能指令（启用后注入到系统提示词）',
    clickToCollapse: '点击收起',
    clickToPreview: '点击预览指令',

    // Settings
    modelProvider: '模型提供商',
    agentIdentity: 'Agent 身份设定',
    identityPlaceholder: '设定 Agent 的个性与行为...',
    identitySaved: '已保存',
    saveIdentity: '保存',
    feishuConfig: '飞书配置',
    appId: 'App ID',
    appSecret: 'App Secret',
    verificationToken: 'Verification Token',
    encryptKey: 'Encrypt Key',
    saveFeishu: '保存',
    deleteFeishu: '删除',
    saved: '已保存',
    deleted: '已删除',
    loadingProviders: '加载提供商中...',
    backToChat: '返回对话',
    provider: '提供商',
    model: '模型',
    providersAvailable: '个可用',
    activeSelection: '当前选择',
    agentIdentityDesc: '自定义 Agent 的身份和核心行为。留空则使用默认身份。',
    saving: '保存中...',
    resetDefault: '重置为默认',
    failedToSave: '保存失败',
    feishuIntegration: '飞书集成',
    connected: '已连接',
    feishuDesc: '配置飞书应用以启用机器人消息。发送到机器人的消息将由 Agent 处理。',
    keepCurrent: '留空则保持当前值',
    enterSecret: '输入应用密钥',
    tokenFromFeishu: '从飞书开发者控制台获取',
    optional: '可选',
    feishuMemoryDesc: 'Agent 具有跨会话的持久记忆。保存的事实包括用户偏好、环境信息和稳定的约定。',
    feishuSkillsDesc: '技能是 Agent 从经验中创建的可复用工作流。它们在使用过程中通过补丁机制自我改进。',

    // Language
    language: '语言',
    chinese: '中文',
    english: 'English',
  },
  en: {
    // Sidebar
    chat: 'Chat',
    documents: 'Documents',
    schedule: 'Schedule',
    memory: 'Memory',
    skills: 'Skills',
    settings: 'Settings',
    signedInAs: 'Signed in as',
    navigation: 'Navigation',
    logout: 'Logout',

    // Login
    username: 'Username',
    password: 'Password',
    login: 'Login',
    register: 'Register',
    alreadyHaveAccount: 'Already have an account?',
    dontHaveAccount: "Don't have an account?",
    authFailed: 'Authentication failed. Please try again.',

    // Chat
    typeMessage: 'Type a message...',
    send: 'Send',
    browseWorkspace: 'Browse workspace files',
    referenceUrl: 'Reference URL',
    enterUrl: 'Enter URL...',
    add: 'Add',
    cancel: 'Cancel',
    copied: 'Copied!',
    copy: 'Copy',
    more: 'more',
    collapse: 'collapse',
    thinking: 'Thinking...',
    callingTool: 'Calling tool',
    toolCompleted: 'Tool completed',
    stop: 'Stop',
    generationStopped: 'Generation stopped',

    // Documents
    newFolder: 'New Folder',
    upload: 'Upload',
    folderName: 'Folder name...',
    create: 'Create',
    creating: 'Creating...',
    uploading: 'Uploading...',
    currentPath: 'Current path',
    root: 'Root',
    loading: 'Loading...',
    noFiles: 'No files yet',
    preview: 'Preview',
    download: 'Download',
    delete: 'Delete',
    confirmDelete: 'Confirm delete',
    myDocuments: 'My Documents',
    newSubfolder: 'New subfolder here',
    uploadHere: 'Upload here',
    openInBrowser: 'Open in Browser',
    close: 'Close',
    selectFilePreview: 'Select a file to preview',
    clickFolderToExpand: 'Click a folder to expand · Click a file to view',
    workspaceEmpty: 'Your workspace is empty',
    useChatToCreate: 'Use chat to create files and projects!',
    rows: 'rows',
    excelPreviewFirst: 'showing first',
    excelNoTables: 'No displayable tables in this Excel file',

    // Schedule
    reminders: 'Reminders',
    cronJobs: 'Cron Jobs',
    addReminder: 'Add Reminder',
    noReminders: 'No reminders yet',
    noCronJobs: 'No cron jobs yet',
    message: 'Message',
    time: 'Time',
    enabled: 'Enabled',
    disabled: 'Disabled',
    addCronJob: 'Add Cron Job',
    cronSchedule: 'Schedule',
    cronPrompt: 'Prompt',
    lastRun: 'Last run',
    never: 'Never',
    enable: 'Enable',
    disable: 'Disable',
    remove: 'Remove',
    noMessage: 'No message',
    trySayingReminder: 'Try saying "提醒我下午3点开会" in chat',
    trySayingCron: 'Try saying "每天早上9点提醒我写日报" in chat',
    lastRunLabel: 'Last run',

    // Memory
    searchMemories: 'Search memories...',
    search: 'Search',
    noMemories: 'No memories yet',
    edit: 'Edit',
    save: 'Save',
    category: 'Category',
    clear: 'Clear',
    refresh: 'Refresh',
    noMatchingMemories: 'No matching memories found',
    tryDifferentSearch: 'Try a different search term',
    trySaying: 'Try saying "记住，我喜欢深色主题" in chat',
    removeMemory: 'Delete',

    // Skills
    skillsSquare: 'Skills Square',
    skillsEnabled: 'skill(s) enabled — active skills are injected into the agent on each new session',
    all: 'All',
    builtIn: 'Built-in',
    mySkills: 'My Skills',
    loadingSkills: 'Loading skills...',
    noCustomSkills: 'No custom skills yet.',
    noSkillsFound: 'No skills found.',
    enableSkill: 'Enable skill',
    disableSkill: 'Disable skill',
    skillInstructions: 'Skill instructions (injected into system prompt when enabled)',
    clickToCollapse: 'Click to collapse',
    clickToPreview: 'Click to preview instructions',

    // Settings
    modelProvider: 'Model Provider',
    agentIdentity: 'Agent Identity',
    identityPlaceholder: 'Define the agent personality and behavior...',
    identitySaved: 'Saved',
    saveIdentity: 'Save',
    feishuConfig: 'Feishu Config',
    appId: 'App ID',
    appSecret: 'App Secret',
    verificationToken: 'Verification Token',
    encryptKey: 'Encrypt Key',
    saveFeishu: 'Save',
    deleteFeishu: 'Delete',
    saved: 'Saved',
    deleted: 'Deleted',
    loadingProviders: 'Loading providers...',
    backToChat: 'Back to Chat',
    provider: 'Provider',
    model: 'Model',
    providersAvailable: 'available',
    activeSelection: 'Active Selection',
    agentIdentityDesc: 'Customize how the agent identifies itself and its core behavior. Leave empty to use the default identity.',
    saving: 'Saving...',
    resetDefault: 'Reset to default',
    failedToSave: 'Failed to save',
    feishuIntegration: 'Feishu Integration',
    connected: 'Connected',
    feishuDesc: 'Configure your Feishu app to enable bot messaging. Messages sent to your bot will be processed by your agent.',
    keepCurrent: 'Leave empty to keep current',
    enterSecret: 'Enter app secret',
    tokenFromFeishu: 'Token from Feishu Developer Console',
    optional: 'Optional',
    feishuMemoryDesc: 'Your agent has persistent memory across sessions. Saved facts include user preferences, environment details, and stable conventions.',
    feishuSkillsDesc: 'Skills are reusable workflows that the agent creates from experience. They self-improve during use via the patch mechanism.',

    // Language
    language: 'Language',
    chinese: '中文',
    english: 'English',
  },
} as const;

type TranslationKey = keyof typeof translations.zh;

interface I18nContextType {
  lang: Lang;
  setLang: (lang: Lang) => void;
  t: (key: TranslationKey) => string;
}

const I18nContext = createContext<I18nContextType>({
  lang: 'zh',
  setLang: () => {},
  t: (key) => translations.zh[key],
});

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(() => {
    const saved = localStorage.getItem('jcowork_lang');
    return (saved === 'en' ? 'en' : 'zh') as Lang;
  });

  const setLang = (newLang: Lang) => {
    setLangState(newLang);
    localStorage.setItem('jcowork_lang', newLang);
  };

  const t = (key: TranslationKey): string => {
    return translations[lang][key] || translations.zh[key] || key;
  };

  return (
    <I18nContext.Provider value={{ lang, setLang, t }}>
      {children}
    </I18nContext.Provider>
  );
}

export function useLang() {
  return useContext(I18nContext);
}

export function useT() {
  const { t } = useContext(I18nContext);
  return t;
}
