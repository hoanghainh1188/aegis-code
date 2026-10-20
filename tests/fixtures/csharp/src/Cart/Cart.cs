namespace Example.Cart;

/// <summary>Small file with one of each item the C# profile's symbol query knows.</summary>
public interface IPriced
{
    int Total();
}

public enum Color
{
    Red,
    Blue,
}

public record Point(int X, int Y);

public struct Money
{
    public int Cents { get; init; }

    public readonly int Dollars() => Cents / 100;
}

public delegate void CartChanged(Cart cart);

public class CartFullException : Exception { }

public class Cart : IPriced
{
    public const int MaxItems = 3;

    private readonly List<int> _items = new();

    public Cart() { }

    public int Count => _items.Count;

    public void Add(int price)
    {
        if (_items.Count >= MaxItems)
        {
            throw new CartFullException();
        }
        _items.Add(price);
    }

    public int Total() => _items.Sum();

    public static Cart Of(params int[] prices)
    {
        var cart = new Cart();
        foreach (var p in prices)
        {
            cart.Add(p);
        }
        return cart;
    }
}
