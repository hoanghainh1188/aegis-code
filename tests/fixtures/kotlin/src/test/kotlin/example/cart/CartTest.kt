package example.cart

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class CartTest {
    @Test
    fun addsUpPrices() {
        assertEquals(3, Cart.of(1, 2).total())
    }

    @Test
    fun refusesAFourthItem() {
        val cart = Cart.of(1, 2, 3)
        assertFailsWith<Cart.CartFullException> { cart.add(4) }
    }
}
