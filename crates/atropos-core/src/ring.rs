//! Fixed-size ring buffer for sparklines (spec: 120 samples, `[f32; 120]`).

/// The last `N` samples; iteration is oldest first.
#[derive(Debug, Clone, Copy)]
pub struct Ring<const N: usize> {
    buf: [f32; N],
    /// Index of the next write.
    head: usize,
    len: usize,
}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self {
            buf: [0.0; N],
            head: 0,
            len: 0,
        }
    }
}

impl<const N: usize> Ring<N> {
    pub fn push(&mut self, v: f32) {
        if N == 0 {
            return;
        }
        self.buf[self.head] = v;
        self.head = (self.head + 1) % N;
        self.len = (self.len + 1).min(N);
    }

    /// Samples, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.head + N - self.len) % N.max(1);
        (0..self.len).map(move |i| self.buf[(start + i) % N])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The newest sample.
    #[must_use]
    pub fn last(&self) -> Option<f32> {
        (self.len > 0).then(|| self.buf[(self.head + N - 1) % N])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_partial_is_in_order() {
        let mut r = Ring::<4>::default();
        r.push(1.0);
        r.push(2.0);
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![1.0, 2.0]);
        assert_eq!(r.last(), Some(2.0));
    }

    #[test]
    fn ring_keeps_last_n_in_order() {
        let mut r = Ring::<3>::default();
        for v in 1..=7 {
            r.push(f32::from(u8::try_from(v).unwrap()));
        }
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![5.0, 6.0, 7.0]);
        assert_eq!(r.len(), 3);
        assert_eq!(r.last(), Some(7.0));
    }

    #[test]
    fn empty_ring_has_no_last() {
        assert_eq!(Ring::<3>::default().last(), None);
    }
}
