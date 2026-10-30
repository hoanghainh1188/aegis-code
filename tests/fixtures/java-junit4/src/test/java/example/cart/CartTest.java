package example.cart;

import static org.junit.Assert.assertEquals;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.Test;

public class CartTest {
    @Test
    public void addsUpPrices() throws Exception {
        // Leaves a mark, so a test can tell whether the suite ran.
        Files.writeString(Path.of("target/tests-ran"), "yes");
        Cart cart = new Cart();
        cart.add(1);
        cart.add(2);
        assertEquals(3, cart.total());
    }
}
