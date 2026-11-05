// ESM-style specifier: "./money.js" resolves to the TypeScript source.
import { fmt } from "../lib/money.js";

export type Price = number;
export const PriceTag = ({ cents }: { cents: number }) => <span>{fmt(cents)}</span>;
