import { describe, expect, it } from "vitest";

import { CartFullError, cartOf } from "../src/cart";

describe("cart", () => {
  it("adds up prices", () => {
    expect(cartOf(1, 2).total()).toBe(3);
  });

  it("refuses a fourth item", () => {
    const cart = cartOf(1, 2, 3);
    expect(() => cart.add(4)).toThrow(CartFullError);
  });
});
