<?php

declare(strict_types=1);

namespace Example\Shop;

final class Cart
{
    /** @var list<int> */
    private array $prices = [];

    public function add(int $cents): void
    {
        $this->prices[] = $cents;
    }

    public function total(): int
    {
        return array_sum($this->prices);
    }
}
