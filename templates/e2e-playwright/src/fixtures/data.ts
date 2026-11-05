/**
 * Generic client for the app's test-only seed endpoint (GUARD_SEED_PATH,
 * docs/contracts/runner-env.md of the guard repository). Every record it
 * creates lives in the test's namespace, which `cleanup()` deletes.
 *
 * Your app decides what a seed body means: `seed({ users: [{ role: 'admin' }] })`
 * posts `{ users: [...], namespace }` and returns the response body.
 */

export interface ApiResponse {
  status(): number;
  ok(): boolean;
  json(): Promise<unknown>;
  text(): Promise<string>;
}

interface RequestOptions {
  data?: unknown;
  headers?: Record<string, string>;
}

/** The subset of Playwright's APIRequestContext that TestData needs. */
export interface SeedApi {
  post(url: string, options?: RequestOptions): Promise<ApiResponse>;
  delete(url: string, options?: RequestOptions): Promise<ApiResponse>;
}

/** Path of the test seed endpoint: GUARD_SEED_PATH, else the contract default. */
export function seedPath(value: string | undefined): string {
  return value ? value : '/__test__/seed';
}

export class TestData {
  readonly namespace: string;
  private readonly api: SeedApi;
  private readonly path: string;
  private readonly headers: Record<string, string>;
  private seeded = false;

  constructor(api: SeedApi, namespace: string, path: string, headers: Record<string, string>) {
    this.api = api;
    this.namespace = namespace;
    this.path = path;
    this.headers = headers;
  }

  /** POSTs `{ ...input, namespace }` to the seed endpoint; returns its JSON body. */
  async seed<T = unknown>(input: Record<string, unknown>): Promise<T> {
    this.seeded = true;
    const res = await this.api.post(this.path, {
      headers: this.headers,
      data: { ...input, namespace: this.namespace },
    });
    if (!res.ok()) {
      throw new Error(`seed failed: ${res.status()} ${await res.text()}`);
    }
    return (await res.json()) as T;
  }

  /** DELETEs `<seed path>/<namespace>` when this test seeded anything. */
  async cleanup(): Promise<void> {
    if (!this.seeded) return;
    const res = await this.api.delete(`${this.path}/${encodeURIComponent(this.namespace)}`, {
      headers: this.headers,
    });
    if (!res.ok()) {
      throw new Error(`cleanup failed: ${res.status()} ${await res.text()}`);
    }
  }
}
