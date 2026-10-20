package example.cart;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

class CartTest {
    @Test
    void addsUpPrices() {
        assertEquals(3, Cart.of(1, 2).total());
    }

    @Test
    void refusesAFourthItem() {
        Cart cart = Cart.of(1, 2, 3);
        assertThrows(Cart.CartFullException.class, () -> cart.add(4));
    }
}
