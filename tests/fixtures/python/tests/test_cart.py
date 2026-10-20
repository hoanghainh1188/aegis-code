import pytest

from cart import CartFullError, cart_of


def test_adds_up_prices():
    assert cart_of(1, 2).total == 3


def test_refuses_a_fourth_item():
    cart = cart_of(1, 2, 3)
    with pytest.raises(CartFullError):
        cart.add(4)
