import { parseList } from '../config/env';

export interface BlockedHost {
  host: string;
  includeSubdomains: boolean;
}

/** Compiles GUARD_BLOCKED_DOMAINS (`*.example.com,cdn.example.org`). */
export function compileBlockedHosts(value: string | undefined): BlockedHost[] {
  return parseList(value).map((pattern) => {
    const lower = pattern.toLowerCase();
    return lower.startsWith('*.')
      ? { host: lower.slice(2), includeSubdomains: true }
      : { host: lower, includeSubdomains: false };
  });
}

/**
 * `*.example.com` matches `example.com` and any subdomain; a pattern without
 * `*.` matches that exact host only. Case-insensitive.
 */
export function isBlockedHost(hostname: string, blocked: readonly BlockedHost[]): boolean {
  const host = hostname.toLowerCase();
  return blocked.some((b) => host === b.host || (b.includeSubdomains && host.endsWith(`.${b.host}`)));
}
