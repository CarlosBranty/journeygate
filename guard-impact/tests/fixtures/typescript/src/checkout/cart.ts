import { fmt } from "../lib/money";
import type { Price } from "../components/Price";
import lodash from "lodash";

export class Cart {
  total(): string {
    return fmt(0);
  }
}
