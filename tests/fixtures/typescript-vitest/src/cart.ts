export const MAX_ITEMS = 3;

export class CartFullError extends Error {}

/** A cart of prices. */
export class Cart {
  private readonly items: number[] = [];

  add(price: number): void {
    if (this.items.length >= MAX_ITEMS) {
      throw new CartFullError();
    }
    this.items.push(price);
  }

  total(): number {
    return this.items.reduce((sum, p) => sum + p, 0);
  }
}

export function cartOf(...prices: number[]): Cart {
  const cart = new Cart();
  prices.forEach((p) => cart.add(p));
  return cart;
}
