//! A reading-only MessagePack value model, written directly against the format
//! spec.
//!
//! Control payloads are decoded through this rather than through `serde`, for
//! three reasons the control layer depends on:
//!
//! - `session.update` is a merge, so "key absent" and "key present and null"
//!   are different instructions. A `serde` struct with `Option` fields collapses
//!   them; a value model keeps them apart.
//! - The rejection cases in `fixtures/protocol/` name a *specific* error. A
//!   value model can say which of "not MessagePack at all", "not a map", "no
//!   discriminator", and "a known message with a bad field" happened, rather
//!   than reporting one deserialization failure for all four.
//! - Every byte here arrives from a peer. Announced lengths are never turned
//!   into capacity and nesting is bounded, so a hostile payload cannot exhaust
//!   memory or the stack. A panic in this file is a remote kill of the worker
//!   that owns the agent's PTY.
//!
//! Encoding does not go through here: it goes through `rmp-serde`, whose
//! writers already pick the smallest representation for every value, which is
//! what makes the re-encoded bytes match the fixtures.

/// Deepest nesting accepted before a payload is called malformed.
///
/// The vocabulary's deepest real value is four levels (`attached` → an
/// attachment array → an attachment → its client), so this leaves ample room
/// while keeping a payload of nothing but array headers from recursing until
/// the stack ends.
const MAX_DEPTH: usize = 32;

/// One decoded MessagePack value.
///
/// Only what the control vocabulary can encounter is modeled; anything else
/// (extension types, in particular) decodes to [`Value::Other`], which every
/// field accessor treats as the wrong type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// `nil`.
    Nil,
    /// `true` / `false`.
    Bool(bool),
    /// Any integer that fits in an `i64`.
    Int(i64),
    /// An unsigned integer too large for an `i64`.
    Uint(u64),
    /// `float32` / `float64`.
    Float(f64),
    /// A UTF-8 string. Invalid UTF-8 is refused while reading.
    Str(String),
    /// A binary blob.
    Bin(Vec<u8>),
    /// An array.
    Array(Vec<Value>),
    /// A map, in the order the pairs appeared.
    Map(Vec<(Value, Value)>),
    /// A well-formed value of a type this vocabulary never uses.
    Other,
}

impl Value {
    /// The name this value's type is reported by in an error.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Nil => "null",
            Self::Bool(_) => "a boolean",
            Self::Int(_) | Self::Uint(_) => "an integer",
            Self::Float(_) => "a float",
            Self::Str(_) => "a string",
            Self::Bin(_) => "a binary blob",
            Self::Array(_) => "an array",
            Self::Map(_) => "a map",
            Self::Other => "an unsupported type",
        }
    }
}

/// Decodes `buf` as exactly one MessagePack value.
///
/// Bytes after that value are an error, not something to ignore: a control
/// payload is one message, and a decoder that reads the first value and shrugs
/// at the rest accepts a payload it did not fully understand.
pub(crate) fn from_slice(buf: &[u8]) -> Result<Value, String> {
    let mut reader = Reader { buf, pos: 0 };
    let value = reader.value(0)?;
    if reader.pos != buf.len() {
        return Err(format!(
            "{} trailing bytes after a complete value",
            buf.len() - reader.pos
        ));
    }
    Ok(value)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or("length overflows the buffer")?;
        let bytes = self.buf.get(self.pos..end).ok_or_else(|| {
            format!(
                "payload ends {} bytes short of an announced {n}-byte field",
                end - self.buf.len()
            )
        })?;
        self.pos = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    /// Reads a big-endian unsigned integer of `n` bytes, `n <= 8`.
    fn uint(&mut self, n: usize) -> Result<u64, String> {
        let bytes = self.take(n)?;
        Ok(bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
    }

    /// Reads an `n`-byte length prefix.
    ///
    /// Goes through `try_from` rather than a cast so that a 32-bit build
    /// refuses a length it cannot represent instead of silently reading a
    /// truncated one — which would leave the reader at an offset the payload
    /// never described.
    fn len(&mut self, n: usize) -> Result<usize, String> {
        let announced = self.uint(n)?;
        usize::try_from(announced)
            .map_err(|_| format!("announced length {announced} does not fit in this address space"))
    }

    /// Reads a big-endian signed integer of `n` bytes, `n <= 8`.
    fn int(&mut self, n: usize) -> Result<i64, String> {
        let raw = self.uint(n)?;
        // Sign-extend from the width actually read.
        let shift = 64 - n * 8;
        Ok(((raw << shift) as i64) >> shift)
    }

    fn str(&mut self, len: usize) -> Result<Value, String> {
        let bytes = self.take(len)?;
        match std::str::from_utf8(bytes) {
            Ok(s) => Ok(Value::Str(s.to_string())),
            Err(e) => Err(format!("string is not valid UTF-8: {e}")),
        }
    }

    /// Reads `len` values, without reserving on `len`.
    ///
    /// A four-billion-element array header costs five bytes to send. Reserving
    /// on it would hand a peer an out-of-memory abort for that price; growing
    /// as elements actually arrive costs it a byte per element instead.
    fn array(&mut self, len: usize, depth: usize) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        for _ in 0..len {
            items.push(self.value(depth + 1)?);
        }
        Ok(items)
    }

    fn map(&mut self, len: usize, depth: usize) -> Result<Vec<(Value, Value)>, String> {
        let mut pairs = Vec::new();
        for _ in 0..len {
            let key = self.value(depth + 1)?;
            let value = self.value(depth + 1)?;
            pairs.push((key, value));
        }
        Ok(pairs)
    }

    /// Reads an extension body and discards it.
    fn ext(&mut self, len: usize) -> Result<Value, String> {
        let _type_tag = self.byte()?;
        let _data = self.take(len)?;
        Ok(Value::Other)
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(format!("value nested deeper than {MAX_DEPTH} levels"));
        }

        let marker = self.byte()?;
        let value = match marker {
            0x00..=0x7f => Value::Int(i64::from(marker)),
            0x80..=0x8f => Value::Map(self.map(usize::from(marker & 0x0f), depth)?),
            0x90..=0x9f => Value::Array(self.array(usize::from(marker & 0x0f), depth)?),
            0xa0..=0xbf => self.str(usize::from(marker & 0x1f))?,
            0xc0 => Value::Nil,
            // The one byte MessagePack reserves as never valid.
            0xc1 => return Err("0xc1 is not a valid MessagePack marker".to_string()),
            0xc2 => Value::Bool(false),
            0xc3 => Value::Bool(true),
            0xc4 => {
                let len = self.len(1)?;
                Value::Bin(self.take(len)?.to_vec())
            }
            0xc5 => {
                let len = self.len(2)?;
                Value::Bin(self.take(len)?.to_vec())
            }
            0xc6 => {
                let len = self.len(4)?;
                Value::Bin(self.take(len)?.to_vec())
            }
            0xc7 => {
                let len = self.len(1)?;
                self.ext(len)?
            }
            0xc8 => {
                let len = self.len(2)?;
                self.ext(len)?
            }
            0xc9 => {
                let len = self.len(4)?;
                self.ext(len)?
            }
            0xca => Value::Float(f64::from(f32::from_bits(self.uint(4)? as u32))),
            0xcb => Value::Float(f64::from_bits(self.uint(8)?)),
            0xcc => Value::Int(self.uint(1)? as i64),
            0xcd => Value::Int(self.uint(2)? as i64),
            0xce => Value::Int(self.uint(4)? as i64),
            0xcf => {
                let n = self.uint(8)?;
                match i64::try_from(n) {
                    Ok(n) => Value::Int(n),
                    Err(_) => Value::Uint(n),
                }
            }
            0xd0 => Value::Int(self.int(1)?),
            0xd1 => Value::Int(self.int(2)?),
            0xd2 => Value::Int(self.int(4)?),
            0xd3 => Value::Int(self.int(8)?),
            0xd4 => self.ext(1)?,
            0xd5 => self.ext(2)?,
            0xd6 => self.ext(4)?,
            0xd7 => self.ext(8)?,
            0xd8 => self.ext(16)?,
            0xd9 => {
                let len = self.len(1)?;
                self.str(len)?
            }
            0xda => {
                let len = self.len(2)?;
                self.str(len)?
            }
            0xdb => {
                let len = self.len(4)?;
                self.str(len)?
            }
            0xdc => {
                let len = self.len(2)?;
                Value::Array(self.array(len, depth)?)
            }
            0xdd => {
                let len = self.len(4)?;
                Value::Array(self.array(len, depth)?)
            }
            0xde => {
                let len = self.len(2)?;
                Value::Map(self.map(len, depth)?)
            }
            0xdf => {
                let len = self.len(4)?;
                Value::Map(self.map(len, depth)?)
            }
            0xe0..=0xff => Value::Int(i64::from(marker as i8)),
        };
        Ok(value)
    }
}
