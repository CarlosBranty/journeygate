/** Stable 32-bit seed derived from the Playwright test id. */
export function seedFromTestId(testId: string): number {
  return [...testId].reduce((h, c) => (Math.imul(h, 31) + c.charCodeAt(0)) >>> 0, 7);
}

/**
 * Replaces `target.random` with a linear congruential generator. Runs inside
 * the browser through `addInitScript`, so it must stay self-contained.
 */
export function installSeededRandom(seed: number, target: { random: () => number } = Math): void {
  let x = seed >>> 0;
  target.random = () => {
    x = (Math.imul(x, 1664525) + 1013904223) >>> 0;
    return x / 4294967296;
  };
}
