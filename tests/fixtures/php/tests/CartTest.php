<?php

declare(strict_types=1);

namespace Tests\Unit;

use Example\Shop\Cart;
use PHPUnit\Framework\TestCase;

final class CartTest extends TestCase
{
    public function testAddsPrices(): void
    {
        $cart = new Cart();
        $cart->add(150);
        $cart->add(250);
        self::assertSame(400, $cart->total());
    }
}
