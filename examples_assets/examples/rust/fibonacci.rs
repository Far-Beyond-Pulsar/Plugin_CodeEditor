// An iterator, a generic function and unit tests in one small file.

/// An endless Fibonacci sequence: 0, 1, 1, 2, 3, 5, ...
pub struct Fibonacci {
    current: u64,
    next: u64,
}

impl Fibonacci {
    pub fn new() -> Self {
        Self { current: 0, next: 1 }
    }
}

impl Iterator for Fibonacci {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        let value = self.current;
        // checked_add ends the sequence instead of overflowing.
        let sum = self.current.checked_add(self.next)?;
        self.current = self.next;
        self.next = sum;
        Some(value)
    }
}

/// The largest item of a slice, or None when it is empty.
pub fn largest<T: PartialOrd + Copy>(items: &[T]) -> Option<T> {
    let mut best = *items.first()?;
    for &item in items {
        if item > best {
            best = item;
        }
    }
    Some(best)
}

fn main() {
    let first: Vec<u64> = Fibonacci::new().take(10).collect();
    println!("{first:?}");
    println!("largest: {:?}", largest(&first));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_zero_one_one_two() {
        let first: Vec<u64> = Fibonacci::new().take(5).collect();
        assert_eq!(first, vec![0, 1, 1, 2, 3]);
    }

    #[test]
    fn largest_of_empty_is_none() {
        assert_eq!(largest::<i32>(&[]), None);
        assert_eq!(largest(&[3, 9, 4]), Some(9));
    }
}
