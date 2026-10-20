/** Small module with one of each item the TypeScript profile's symbol query knows. */

export const MAX_ITEMS = 3;

export type Total = number;

export interface Priced {
  total(): Total;
}

export class CartFullError extends Error {}

export class Cart implements Priced {
  private readonly items: number[] = [];

  add(price: number): void {
    if (this.items.length >= MAX_ITEMS) {
      throw new CartFullError();
    }
    this.items.push(price);
  }

  total(): Total {
    return this.items.reduce((sum, p) => sum + p, 0);
  }
}

export const cartOf = (...prices: number[]): Cart => {
  const cart = new Cart();
  for (const p of prices) {
    cart.add(p);
  }
  return cart;
};
