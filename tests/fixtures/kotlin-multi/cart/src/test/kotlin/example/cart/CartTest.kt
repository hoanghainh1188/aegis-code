package example.cart

import kotlin.test.Test
import kotlin.test.assertEquals

class CartTest {
    @Test
    fun addsUpPrices() {
        assertEquals(3, Cart(listOf(1, 2)).total())
    }
}
