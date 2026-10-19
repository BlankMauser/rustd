pub fn helped(x: u32) -> u32 {
    x + 100
}

pub fn generic_help<T: Clone>(x: &T) -> (T, T) {
    (x.clone(), x.clone())
}

#[cfg(test)]
mod tests {
    #[test]
    fn helped_adds() {
        assert_eq!(super::helped(1), 101);
    }
}
