package example.tax

import kotlin.test.Test
import kotlin.test.assertEquals

class TaxTest {
    @Test
    fun isATenth() {
        assertEquals(5, Tax.of(50))
    }
}
