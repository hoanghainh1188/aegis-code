package example.cart;

import java.util.ArrayList;
import java.util.List;

public class Cart {
    private final List<Integer> items = new ArrayList<>();

    public void add(int price) {
        items.add(price);
    }

    public int total() {
        return items.stream().mapToInt(Integer::intValue).sum();
    }
}
