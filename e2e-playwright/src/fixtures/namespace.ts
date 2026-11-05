const RUN_ID_PATTERN = /^[A-Za-z0-9_.-]{1,64}$/;

export interface NamespaceParts {
  runId: string | undefined;
  testId: string;
  repeatEachIndex: number;
  retry: number;
}

/**
 * Per-test data namespace `${RUN_ID}-${testId}-x${repeatEachIndex}-r${retry}`,
 * so parallel shards, repeats and retries never share seeded records.
 */
export function dataNamespace({ runId, testId, repeatEachIndex, retry }: NamespaceParts): string {
  const run = runId || 'local';
  if (!RUN_ID_PATTERN.test(run)) {
    throw new Error(`RUN_ID must match ${RUN_ID_PATTERN.source}, got "${run}"`);
  }
  return `${run}-${testId}-x${repeatEachIndex}-r${retry}`;
}
