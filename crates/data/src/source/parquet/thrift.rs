//! A Thrift compact-protocol walker for the Parquet footer and page headers. It builds nothing:
//! it checks that the bytes are a well-formed struct (every list, map and binary length within
//! the bytes left, nesting at most [`MAX_DEPTH`]) and reports integer fields by their path of
//! field ids, so the parquet crate only ever parses input already known to be sane.

use std::io::Read;

/// Deepest struct/container nesting accepted (real footers nest about 7 deep).
const MAX_DEPTH: usize = 32;

/// A value reported to the walker's callback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// An integer (or boolean, as 0/1) field.
    Int(i64),
    /// A list or set field and its element count (reported before the elements).
    List(u64),
}

pub type Fields<'a> = dyn FnMut(&[i16], Value) -> Result<(), String> + 'a;

pub struct Thrift<R> {
    input: R,
    /// Bytes still allowed.
    left: u64,
    /// Bytes consumed so far.
    pub read: u64,
}

impl<R: Read> Thrift<R> {
    pub fn new(input: R, left: u64) -> Self {
        Self {
            input,
            left,
            read: 0,
        }
    }

    /// Walks one struct, calling `f` with each integer or list field's path.
    pub fn walk(&mut self, f: &mut Fields<'_>) -> Result<(), String> {
        self.walk_struct(0, &mut Vec::new(), f)
    }

    fn byte(&mut self) -> Result<u8, String> {
        let mut b = [0];
        if self.left == 0 || self.input.read_exact(&mut b).is_err() {
            return Err("truncated metadata".into());
        }
        self.left -= 1;
        self.read += 1;
        Ok(b[0])
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("malformed metadata integer".into())
    }

    fn zigzag(&mut self) -> Result<i64, String> {
        let n = self.varint()?;
        Ok((n >> 1) as i64 ^ -((n & 1) as i64))
    }

    fn skip(&mut self, n: u64) -> Result<(), String> {
        if n > self.left {
            return Err("metadata length past its end".into());
        }
        let copied = std::io::copy(&mut (&mut self.input).take(n), &mut std::io::sink())
            .map_err(|e| e.to_string())?;
        if copied != n {
            return Err("truncated metadata".into());
        }
        self.left -= n;
        self.read += n;
        Ok(())
    }

    fn walk_struct(
        &mut self,
        depth: usize,
        path: &mut Vec<i16>,
        f: &mut Fields<'_>,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("metadata nested too deeply".into());
        }
        let mut last: i16 = 0;
        loop {
            let b = self.byte()?;
            if b == 0 {
                return Ok(());
            }
            let delta = i16::from(b >> 4);
            let id = if delta == 0 {
                i16::try_from(self.zigzag()?).map_err(|_| "bad metadata field id")?
            } else {
                last.checked_add(delta).ok_or("bad metadata field id")?
            };
            last = id;
            path.push(id);
            self.walk_value(b & 0x0f, depth, path, f)?;
            path.pop();
        }
    }

    fn walk_value(
        &mut self,
        ty: u8,
        depth: usize,
        path: &mut Vec<i16>,
        f: &mut Fields<'_>,
    ) -> Result<(), String> {
        match ty {
            1 => f(path, Value::Int(1)),
            2 => f(path, Value::Int(0)),
            3 => {
                let b = self.byte()?;
                f(path, Value::Int(i64::from(b as i8)))
            }
            4..=6 => {
                let v = self.zigzag()?;
                f(path, Value::Int(v))
            }
            7 => self.skip(8),
            8 => {
                let n = self.varint()?;
                self.skip(n)
            }
            9 | 10 => {
                let h = self.byte()?;
                let n = match h >> 4 {
                    15 => self.varint()?,
                    n => u64::from(n),
                };
                // Every element takes at least one byte.
                if n > self.left {
                    return Err("metadata list longer than its bytes".into());
                }
                f(path, Value::List(n))?;
                for _ in 0..n {
                    self.walk_element(h & 0x0f, depth + 1, path, f)?;
                }
                Ok(())
            }
            11 => {
                let n = self.varint()?;
                if n == 0 {
                    return Ok(());
                }
                if n > self.left {
                    return Err("metadata map longer than its bytes".into());
                }
                let kv = self.byte()?;
                for _ in 0..n {
                    self.walk_element(kv >> 4, depth + 1, path, f)?;
                    self.walk_element(kv & 0x0f, depth + 1, path, f)?;
                }
                Ok(())
            }
            12 => self.walk_struct(depth + 1, path, f),
            _ => Err(format!("unknown metadata type {ty}")),
        }
    }

    /// A list, set or map element: booleans take a whole byte there.
    fn walk_element(
        &mut self,
        ty: u8,
        depth: usize,
        path: &mut Vec<i16>,
        f: &mut Fields<'_>,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("metadata nested too deeply".into());
        }
        match ty {
            1 | 2 => self.byte().map(drop),
            _ => self.walk_value(ty, depth, path, f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(bytes: &[u8]) -> Result<Vec<(Vec<i16>, Value)>, String> {
        let mut seen = Vec::new();
        let mut t = Thrift::new(bytes, bytes.len() as u64);
        t.walk(&mut |p, v| {
            seen.push((p.to_vec(), v));
            Ok(())
        })?;
        Ok(seen)
    }

    #[test]
    fn walks_fields_and_rejects_hostile_lengths() {
        // {1: i32 -3, 2: struct {1: i64 5}, 3: list<i32> [1, 2]}
        let ok = [
            0x15, 0x05, 0x1C, 0x16, 0x0A, 0x00, 0x19, 0x25, 0x02, 0x04, 0x00,
        ];
        assert_eq!(
            walk(&ok).unwrap(),
            [
                (vec![1], Value::Int(-3)),
                (vec![2, 1], Value::Int(5)),
                (vec![3], Value::List(2)),
                (vec![3], Value::Int(1)),
                (vec![3], Value::Int(2)),
            ]
        );
        // A list claiming 2^31 elements, a binary longer than the input, endless nesting,
        // an over-long varint and a truncation are errors, not allocations or panics.
        let bomb = [0x19, 0xF5, 0x80, 0x80, 0x80, 0x80, 0x08, 0x00];
        let long_binary = [0x18, 0xFF, 0x7F, 0x00];
        let deep = [0x1C; 64];
        let varint = [
            0x15, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
        ];
        for bad in [&bomb[..], &long_binary, &deep, &varint, &ok[..5], &[0x1F]] {
            assert!(walk(bad).is_err(), "{bad:?}");
        }
    }
}
