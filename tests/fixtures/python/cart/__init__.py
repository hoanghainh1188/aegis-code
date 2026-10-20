"""Small package with one of each item the Python profile's symbol query knows."""

from dataclasses import dataclass, field

MAX_ITEMS = 3


class CartFullError(Exception):
    """Raised when a fourth item is added."""


@dataclass
class Cart:
    items: list[int] = field(default_factory=list)

    def add(self, price: int) -> None:
        if len(self.items) >= MAX_ITEMS:
            raise CartFullError
        self.items.append(price)

    @property
    def total(self) -> int:
        return sum(self.items)


def cart_of(*prices: int) -> Cart:
    cart = Cart()
    for price in prices:
        cart.add(price)
    return cart
