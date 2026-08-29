// Multi-conversation chat storage (localStorage).
//
// Each conversation stores its messages plus lastInputAt (timestamp of the
// last user message). A conversation with messages and no new input for more
// than HISTORY_THRESHOLD_MS (1 hour) is treated as a historical task chat and
// listed in the sidebar dropdown.

export interface Message {
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: number;
  streaming?: boolean;
  details?: string;
}

export interface Conversation {
  id: string;
  title: string;
  messages: Message[];
  createdAt: number;
  lastInputAt: number;
}

export const HISTORY_THRESHOLD_MS = 60 * 60 * 1000; // 1 hour

const CONVS_KEY = (userId: string) => `jcowork_convs_${userId}`;
const ACTIVE_KEY = (userId: string) => `jcowork_active_conv_${userId}`;
const LEGACY_KEY = (userId: string) => `jcowork_chat_${userId}`;

export function newConversationId(): string {
  return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
}

// Title = first user query, truncated
export function titleFromMessages(messages: Message[]): string {
  const first = messages.find((m) => m.role === 'user');
  if (!first) return '';
  const t = first.content.trim().replace(/\s+/g, ' ');
  return t.length > 40 ? t.slice(0, 40) + '…' : t;
}

/// Load all conversations; migrates the legacy single-chat key on first run.
export function loadConversations(userId: string): Conversation[] {
  try {
    const saved = localStorage.getItem(CONVS_KEY(userId));
    if (saved) {
      const parsed = JSON.parse(saved) as Conversation[];
      if (Array.isArray(parsed)) return parsed;
    }
    // Migrate legacy single-conversation storage
    const legacy = localStorage.getItem(LEGACY_KEY(userId));
    if (legacy) {
      const msgs = (JSON.parse(legacy) as Message[]).filter((m) => !m.streaming);
      if (msgs.length > 0) {
        const lastUser = [...msgs].reverse().find((m) => m.role === 'user');
        const conv: Conversation = {
          id: newConversationId(),
          title: titleFromMessages(msgs),
          messages: msgs,
          createdAt: msgs[0]?.timestamp ?? Date.now(),
          lastInputAt: lastUser?.timestamp ?? msgs[msgs.length - 1]?.timestamp ?? Date.now(),
        };
        localStorage.setItem(CONVS_KEY(userId), JSON.stringify([conv]));
        localStorage.removeItem(LEGACY_KEY(userId));
        return [conv];
      }
    }
  } catch {}
  return [];
}

export function saveConversations(userId: string, convs: Conversation[]) {
  try {
    // Never persist conversations that have no messages yet
    const toSave = convs.filter((c) => c.messages.length > 0);
    localStorage.setItem(CONVS_KEY(userId), JSON.stringify(toSave));
  } catch {}
}

export function getActiveConvId(userId: string): string | null {
  return localStorage.getItem(ACTIVE_KEY(userId));
}

export function setActiveConvId(userId: string, id: string) {
  try {
    localStorage.setItem(ACTIVE_KEY(userId), id);
  } catch {}
}

export function createConversation(userId: string): { convs: Conversation[]; id: string } {
  const convs = loadConversations(userId);
  const conv: Conversation = {
    id: newConversationId(),
    title: '',
    messages: [],
    createdAt: Date.now(),
    lastInputAt: Date.now(),
  };
  const next = [...convs, conv];
  saveConversations(userId, next);
  setActiveConvId(userId, conv.id);
  return { convs: next, id: conv.id };
}

/// Replace a conversation's messages; recompute title from the first query.
/// Upserts: an in-memory-only (not yet persisted) conversation is inserted
/// here when its first message arrives.
export function updateConvMessages(userId: string, convId: string, messages: Message[]): Conversation[] {
  const loaded = loadConversations(userId);
  const exists = loaded.some((c) => c.id === convId);
  const base = exists
    ? loaded
    : [...loaded, { id: convId, title: '', messages: [], createdAt: Date.now(), lastInputAt: Date.now() } as Conversation];
  const convs = base.map((c) =>
    c.id === convId
      ? {
          ...c,
          messages,
          title: messages.some((m) => m.role === 'user') ? titleFromMessages(messages) : '',
        }
      : c
  );
  saveConversations(userId, convs);
  return convs;
}

/// Record a new user input: bump lastInputAt (drives the 1h history rule).
export function touchConversation(userId: string, convId: string): Conversation[] {
  const convs = loadConversations(userId).map((c) =>
    c.id === convId ? { ...c, lastInputAt: Date.now() } : c
  );
  saveConversations(userId, convs);
  return convs;
}

export function deleteConversation(userId: string, convId: string): Conversation[] {
  const convs = loadConversations(userId).filter((c) => c.id !== convId);
  saveConversations(userId, convs);
  return convs;
}

/// Historical task chats: has messages and no user input for over 1 hour.
export function isHistory(conv: Conversation, now: number = Date.now()): boolean {
  return conv.messages.length > 0 && now - conv.lastInputAt > HISTORY_THRESHOLD_MS;
}
