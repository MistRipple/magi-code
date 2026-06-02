export type AccessProfile = 'read_only' | 'restricted' | 'full_access';

export const DEFAULT_ACCESS_PROFILE: AccessProfile = 'restricted';
export const ACCESS_PROFILE_STORAGE_KEY = 'magi-composer-access-profile';

export function isAccessProfile(value: unknown): value is AccessProfile {
  return value === 'read_only' || value === 'restricted' || value === 'full_access';
}

export function normalizeAccessProfile(value: unknown): AccessProfile {
  return isAccessProfile(value) ? value : DEFAULT_ACCESS_PROFILE;
}

function getBrowserLocalStorage(): Storage | null {
  if (typeof window !== 'undefined' && window.localStorage) {
    return window.localStorage;
  }
  if (typeof globalThis !== 'undefined') {
    const candidate = (globalThis as { localStorage?: Storage }).localStorage;
    return candidate ?? null;
  }
  return null;
}

export function readStoredAccessProfile(): AccessProfile {
  try {
    return normalizeAccessProfile(getBrowserLocalStorage()?.getItem(ACCESS_PROFILE_STORAGE_KEY));
  } catch {
    return DEFAULT_ACCESS_PROFILE;
  }
}

export function writeStoredAccessProfile(profile: AccessProfile): void {
  try {
    getBrowserLocalStorage()?.setItem(ACCESS_PROFILE_STORAGE_KEY, normalizeAccessProfile(profile));
  } catch {
    // localStorage 不可用时，调用方的内存态仍然生效。
  }
}
