package example.cart

class Cart(private val prices: List<Int>) {
    fun total(): Int = prices.sum()
}
