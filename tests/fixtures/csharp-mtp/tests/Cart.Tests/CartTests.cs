using Example.Cart;

namespace Cart.Tests;

public class CartTests
{
    [Fact]
    public void AddsUpPrices()
    {
        Assert.Equal(3, Example.Cart.Cart.Of(1, 2).Total());
    }

    [Fact]
    public void RefusesAFourthItem()
    {
        var cart = Example.Cart.Cart.Of(1, 2, 3);
        Assert.Throws<CartFullException>(() => cart.Add(4));
    }
}
