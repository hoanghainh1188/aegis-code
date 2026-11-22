package example.cart

/** Small class with one of each item the Kotlin profile's symbol query knows. */
interface Priced {
    fun total(): Int
}

class Cart : Priced {
    private val items = mutableListOf<Int>()

    fun add(price: Int) {
        if (items.size >= MAX_ITEMS) {
            throw CartFullException()
        }
        items += price
    }

    override fun total(): Int = items.sum()

    companion object {
        const val MAX_ITEMS = 3

        fun of(vararg prices: Int): Cart = Cart().apply { prices.forEach(::add) }
    }

    /** Raised when a fourth item is added. */
    class CartFullException : RuntimeException()
}
