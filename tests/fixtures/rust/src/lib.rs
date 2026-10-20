//! Small crate with one of each item the Rust profile's symbol query knows.

pub const MAX_ITEMS: usize = 3;

pub type Total = u32;

/// A shopping cart.
#[derive(Debug, Default)]
pub struct Cart {
    items: Vec<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CartError {
    Full,
}

pub trait Priced {
    fn total(&self) -> Total;
}

impl Cart {
    pub fn add(&mut self, price: u32) -> Result<(), CartError> {
        if self.items.len() >= MAX_ITEMS {
            return Err(CartError::Full);
        }
        self.items.push(price);
        Ok(())
    }
}

impl Priced for Cart {
    fn total(&self) -> Total {
        self.items.iter().sum()
    }
}

#[macro_export]
macro_rules! cart {
    ($($price:expr),*) => {{
        let mut cart = $crate::Cart::default();
        $(cart.add($price).unwrap();)*
        cart
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_up_prices() {
        assert_eq!(cart![1, 2].total(), 3);
    }

    #[test]
    fn refuses_a_fourth_item() {
        let mut cart = cart![1, 2, 3];
        assert_eq!(cart.add(4), Err(CartError::Full));
    }
}
