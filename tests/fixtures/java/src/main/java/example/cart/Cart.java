package example.cart;

import java.util.ArrayList;
import java.util.List;

/** Small class with one of each item the Java profile's symbol query knows. */
public class Cart implements Priced {
    public static final int MAX_ITEMS = 3;

    private final List<Integer> items = new ArrayList<>();

    public Cart() {}

    public void add(int price) {
        if (items.size() >= MAX_ITEMS) {
            throw new CartFullException();
        }
        items.add(price);
    }

    @Override
    public int total() {
        return items.stream().mapToInt(Integer::intValue).sum();
    }

    public static Cart of(int... prices) {
        Cart cart = new Cart();
        for (int p : prices) {
            cart.add(p);
        }
        return cart;
    }

    /** Raised when a fourth item is added. */
    public static class CartFullException extends RuntimeException {}
}
