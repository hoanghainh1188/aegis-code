import assert from "node:assert/strict";
import { test } from "node:test";

import { CartFullError, cartOf } from "../src/cart.ts";

test("adds up prices", () => {
  assert.equal(cartOf(1, 2).total(), 3);
});

test("refuses a fourth item", () => {
  const cart = cartOf(1, 2, 3);
  assert.throws(() => cart.add(4), CartFullError);
});
