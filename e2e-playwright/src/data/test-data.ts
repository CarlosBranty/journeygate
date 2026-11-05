/**
 * Client for the demo app's test-only API (docs/contracts/demo-app.md).
 * Every record it creates lives in one namespace that `cleanup()` deletes.
 */

export interface Card {
  number: string;
  expiry: string;
  cvc: string;
}

export interface Product {
  id: string;
  slug: string;
  name: string;
  priceCents: number;
  stock: number;
}

export interface OrderLine {
  productId: string;
  quantity: number;
  unitPriceCents: number;
}

export interface Order {
  id: string;
  number: string;
  namespace: string;
  status: string;
  lines: OrderLine[];
  totalCents: number;
  email: string | null;
  createdAt: string;
}

export interface ProductInput {
  name?: string;
  priceCents?: number;
  stock?: number;
}

interface ApiResponse {
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
export interface TestApi {
  post(url: string, options?: RequestOptions): Promise<ApiResponse>;
  get(url: string, options?: RequestOptions): Promise<ApiResponse>;
  delete(url: string, options?: RequestOptions): Promise<ApiResponse>;
}

export const CARDS = {
  approved: { number: '4242 4242 4242 4242', expiry: '12/34', cvc: '123' },
  declined: { number: '4000 0000 0000 0002', expiry: '12/34', cvc: '123' },
} as const satisfies Record<string, Card>;

export class TestData {
  readonly cards = CARDS;
  readonly namespace: string;
  private readonly api: TestApi;
  private readonly headers: Record<string, string>;
  private productCount = 0;

  constructor(api: TestApi, namespace: string, token: string) {
    this.api = api;
    this.namespace = namespace;
    this.headers = { 'x-test-token': token };
  }

  /** Seeds one product; defaults are a cheap mug with plenty of stock. */
  async product(input: ProductInput): Promise<Product> {
    this.productCount += 1;
    const product = {
      name: input.name ?? 'Blue mug',
      priceCents: input.priceCents ?? 1999,
      stock: input.stock ?? 10,
      slug: `${this.namespace}-p${this.productCount}`,
    };
    const res = await this.api.post('/__test__/seed', {
      headers: this.headers,
      data: { namespace: this.namespace, products: [product] },
    });
    await expectStatus(res, 201, 'seed');
    const body = (await res.json()) as { products: Product[] };
    const created = body.products[0];
    if (!created) throw new Error('seed failed: response has no product');
    return created;
  }

  /** Every order that has a line for this product, oldest first. */
  async ordersFor(productId: string): Promise<Order[]> {
    const res = await this.api.get(`/__test__/orders?productId=${encodeURIComponent(productId)}`, {
      headers: this.headers,
    });
    await expectStatus(res, 200, 'orders lookup');
    return ((await res.json()) as { orders: Order[] }).orders;
  }

  /** Deletes every product and order of the namespace. */
  async cleanup(): Promise<void> {
    const res = await this.api.delete(`/__test__/seed/${encodeURIComponent(this.namespace)}`, {
      headers: this.headers,
    });
    await expectStatus(res, 204, 'cleanup');
  }
}

async function expectStatus(res: ApiResponse, expected: number, what: string): Promise<void> {
  if (res.status() !== expected) {
    throw new Error(`${what} failed: ${res.status()} ${await res.text()}`);
  }
}
