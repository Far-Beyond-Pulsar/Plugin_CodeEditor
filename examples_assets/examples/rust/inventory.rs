//! A small inventory: structs, enums, a trait and error handling.
//!
//! Open it in the code editor to try syntax highlighting, folding and
//! rust-analyzer's completions and diagnostics.

use std::collections::HashMap;
use std::fmt;

/// How rare an item is; drives its price multiplier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rarity {
    Common,
    Rare,
    Epic,
}

impl Rarity {
    /// The price multiplier for this rarity.
    pub fn multiplier(self) -> u32 {
        match self {
            Rarity::Common => 1,
            Rarity::Rare => 5,
            Rarity::Epic => 20,
        }
    }
}

/// One kind of item.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub base_price: u32,
    pub rarity: Rarity,
}

/// What can go wrong when changing an inventory.
#[derive(Debug, PartialEq, Eq)]
pub enum InventoryError {
    /// Not enough of the item is held.
    NotEnough { name: String, have: u32, need: u32 },
}

impl fmt::Display for InventoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InventoryError::NotEnough { name, have, need } => {
                write!(f, "need {need} {name}, have {have}")
            }
        }
    }
}

impl std::error::Error for InventoryError {}

/// Anything that can be given a price.
pub trait Priced {
    fn price(&self) -> u32;
}

impl Priced for Item {
    fn price(&self) -> u32 {
        self.base_price * self.rarity.multiplier()
    }
}

/// Item counts by name.
#[derive(Default)]
pub struct Inventory {
    counts: HashMap<String, u32>,
}

impl Inventory {
    pub fn add(&mut self, item: &Item, count: u32) {
        *self.counts.entry(item.name.clone()).or_insert(0) += count;
    }

    pub fn remove(&mut self, item: &Item, count: u32) -> Result<(), InventoryError> {
        let have = self.counts.get(&item.name).copied().unwrap_or(0);
        if have < count {
            return Err(InventoryError::NotEnough {
                name: item.name.clone(),
                have,
                need: count,
            });
        }
        self.counts.insert(item.name.clone(), have - count);
        Ok(())
    }

    pub fn count(&self, item: &Item) -> u32 {
        self.counts.get(&item.name).copied().unwrap_or(0)
    }
}

fn main() {
    let sword = Item {
        name: "Iron Sword".into(),
        base_price: 40,
        rarity: Rarity::Rare,
    };
    let mut bag = Inventory::default();
    bag.add(&sword, 2);
    println!("{} costs {}", sword.name, sword.price());
    match bag.remove(&sword, 3) {
        Ok(()) => println!("removed"),
        Err(error) => println!("could not remove: {error}"),
    }
    println!("left: {}", bag.count(&sword));
}
