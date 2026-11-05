import money = require("../lib/money");
import { Cart } from "./cart";

export const Pay = () => <button>{money.fmt(new Cart().total().length)}</button>;
