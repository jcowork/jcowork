// Multi-account storage (localStorage).
//
// Each logged-in account is stored as an AuthState in a list keyed by userId.
// On first load, the legacy single-account key (jcowork_auth) is migrated
// into a 1-element array.

export interface AuthState {
  token: string;
  userId: string;
  username: string;
}

const ACCOUNTS_KEY = 'jcowork_accounts';
const ACTIVE_KEY = 'jcowork_active_account';
const LEGACY_KEY = 'jcowork_auth';

/// Load all stored accounts; migrates the legacy single-account key.
export function loadAccounts(): AuthState[] {
  try {
    const saved = localStorage.getItem(ACCOUNTS_KEY);
    if (saved) {
      const parsed = JSON.parse(saved) as AuthState[];
      if (Array.isArray(parsed) && parsed.length > 0) return parsed;
    }
    // Migrate legacy single-account storage
    const legacy = localStorage.getItem(LEGACY_KEY);
    if (legacy) {
      const single = JSON.parse(legacy) as AuthState;
      if (single?.token && single?.userId) {
        const accounts = [single];
        saveAccounts(accounts);
        localStorage.removeItem(LEGACY_KEY);
        return accounts;
      }
    }
  } catch {}
  return [];
}

/// Persist the account list (replaces any previous state).
export function saveAccounts(accounts: AuthState[]) {
  try {
    localStorage.setItem(ACCOUNTS_KEY, JSON.stringify(accounts));
  } catch {}
}

/// Get the currently active account's userId.
export function getActiveAccount(): string | null {
  return localStorage.getItem(ACTIVE_KEY);
}

/// Set the currently active account.
export function setActiveAccount(userId: string) {
  try {
    localStorage.setItem(ACTIVE_KEY, userId);
  } catch {}
}

/// Add or replace an account in the list (upsert by userId).
export function upsertAccount(account: AuthState): AuthState[] {
  const accounts = loadAccounts();
  const idx = accounts.findIndex((a) => a.userId === account.userId);
  if (idx >= 0) {
    accounts[idx] = account;
  } else {
    accounts.push(account);
  }
  saveAccounts(accounts);
  return accounts;
}

/// Remove an account by userId; returns the updated list.
export function removeAccount(userId: string): AuthState[] {
  const accounts = loadAccounts().filter((a) => a.userId !== userId);
  saveAccounts(accounts);
  return accounts;
}
