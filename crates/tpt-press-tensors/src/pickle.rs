//! A restricted reader for the pickle streams inside PyTorch checkpoints.
//!
//! This is not a general pickle interpreter, and it never executes anything.
//! It understands a fixed set of opcodes, which covers what `torch.save` writes
//! for a state dict. Globals are only called if they are on an allowlist: the
//! storage types, `OrderedDict`, and the `_rebuild_tensor` family. Any other
//! global, opcode or object type is an error, so a hostile file cannot run code
//! or build objects it was not meant to.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::error::{Error, Result};

/// Deepest nesting of tuples, lists and dicts that is accepted.
pub const MAX_DEPTH: usize = 64;

/// Most memo entries a stream may create. A bigger stream is refused.
const MAX_MEMO: usize = 1 << 20;

/// A value built by the pickle stream. Containers are shared, as in Python, so
/// cloning a value is cheap and a memo reference shares its container.
#[derive(Clone)]
pub enum Value {
    /// `None`.
    None,
    /// `True` or `False`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(Rc<str>),
    /// A tuple.
    Tuple(Rc<Seq>),
    /// A list.
    List(Rc<Seq>),
    /// A dict. Its items are stored flat as key, value, key, value.
    Dict(Rc<Seq>),
    /// A global name such as `torch.FloatStorage`, not yet called.
    Global(Rc<str>),
    /// A typed storage, referenced from tensors by its key.
    Storage(Rc<Storage>),
    /// A tensor view over a storage.
    Tensor(Rc<TensorRef>),
    /// An integer too big for `Int`. Its value is not kept.
    Opaque,
    /// Stack marker. Never appears in a finished value.
    Mark,
}

/// A container's items, and the deepest nesting it has reached.
pub struct Seq {
    depth: Cell<usize>,
    items: RefCell<Vec<Value>>,
}

impl Value {
    /// A short description for error messages, such as `an int (3)`.
    pub fn describe(&self) -> String {
        match self {
            Value::None => "None".to_owned(),
            Value::Bool(b) => format!("a bool ({b})"),
            Value::Int(n) => format!("an int ({n})"),
            Value::Float(f) => format!("a float ({f})"),
            Value::Str(s) => format!("a string ({s:?})"),
            Value::Tuple(_) => "a tuple".to_owned(),
            Value::List(_) => "a list".to_owned(),
            Value::Dict(_) => "a dict".to_owned(),
            Value::Global(name) => format!("the global `{name}`"),
            Value::Storage(_) => "a storage".to_owned(),
            Value::Tensor(_) => "a tensor".to_owned(),
            Value::Opaque => "a large integer".to_owned(),
            Value::Mark => "a marker".to_owned(),
        }
    }
}

impl Seq {
    fn new(items: Vec<Value>, depth: usize) -> Rc<Self> {
        Rc::new(Self {
            depth: Cell::new(depth),
            items: RefCell::new(items),
        })
    }

    /// Records that a child of depth `child` now sits inside this container.
    fn grow_to(&self, child: usize) {
        self.depth.set(self.depth.get().max(child + 1));
    }

    /// A copy of the items, in order.
    pub fn items(&self) -> Vec<Value> {
        self.items.borrow().clone()
    }
}

/// Drops nested containers without recursing, so a deeply nested stream cannot
/// overflow the stack when it is freed.
impl Drop for Seq {
    fn drop(&mut self) {
        let mut pending = std::mem::take(self.items.get_mut());
        while let Some(value) = pending.pop() {
            let inner = match value {
                Value::Tuple(rc) | Value::List(rc) | Value::Dict(rc) => rc,
                _ => continue,
            };
            if let Ok(mut seq) = Rc::try_unwrap(inner) {
                pending.append(seq.items.get_mut());
            }
        }
    }
}

/// A typed storage: a flat array of one element type.
#[derive(Debug)]
pub struct Storage {
    /// Element type, in tpt-press dtype names.
    pub dtype: &'static str,
    /// The key of the storage's data file inside the checkpoint.
    pub key: String,
    /// Number of elements.
    pub numel: u64,
}

/// A tensor: a view of a storage with a shape and strides, in elements.
#[derive(Debug)]
pub struct TensorRef {
    /// The storage the tensor reads from.
    pub storage: Rc<Storage>,
    /// Offset of the first element, in elements.
    pub offset: u64,
    /// Dimensions.
    pub shape: Vec<u64>,
    /// Strides, in elements.
    pub stride: Vec<u64>,
}

/// Storage classes on the allowlist, and their element types.
fn storage_dtype(global: &str) -> Option<&'static str> {
    Some(match global {
        "torch.FloatStorage" => "F32",
        "torch.DoubleStorage" => "F64",
        "torch.HalfStorage" => "F16",
        "torch.BFloat16Storage" => "BF16",
        "torch.LongStorage" => "I64",
        "torch.IntStorage" => "I32",
        "torch.ShortStorage" => "I16",
        "torch.CharStorage" => "I8",
        "torch.ByteStorage" => "U8",
        "torch.BoolStorage" => "BOOL",
        _ => return None,
    })
}

/// Parses a pickle stream and returns the object it holds.
///
/// Only the opcodes and globals listed in the module docs are accepted. Any
/// other opcode is an error, and so is any global that is not allowlisted.
pub fn load(data: &[u8]) -> Result<Value> {
    Ok(load_prefix(data)?.value)
}

/// One pickle read from the start of a stream, with what it reveals.
pub struct Prefix {
    /// The object the pickle holds.
    pub value: Value,
    /// Every storage the pickle refers to, in the order they first appear.
    pub storages: Vec<Rc<Storage>>,
    /// Bytes of the stream the pickle used, up to and including its STOP.
    pub consumed: usize,
}

/// Reads one pickle from the start of `data`. A legacy PyTorch file holds several
/// pickles in a row, and `consumed` says where the next one starts.
pub fn load_prefix(data: &[u8]) -> Result<Prefix> {
    let mut loader = Loader {
        data,
        pos: 0,
        stack: Vec::new(),
        memo: HashMap::new(),
        storages: Vec::new(),
        containers: RefCell::new(Vec::new()),
    };
    // The copy is a plain tree, and it is made before the loader's containers
    // are cleared. A pickle can make a container hold itself, which reference
    // counting cannot free, so the loader clears them all.
    let outcome = loader.run().and_then(|value| detach(&value, 0));
    let storages = std::mem::take(&mut loader.storages);
    let consumed = loader.pos;
    loader.release();
    Ok(Prefix {
        value: outcome?,
        storages,
        consumed,
    })
}

struct Loader<'a> {
    data: &'a [u8],
    pos: usize,
    stack: Vec<Value>,
    memo: HashMap<u64, Value>,
    storages: Vec<Rc<Storage>>,
    /// Every container made, so that they can all be cleared when loading ends.
    containers: RefCell<Vec<Weak<Seq>>>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidModel(format!("PyTorch pickle: {}", message.into()))
}

impl Loader<'_> {
    fn run(&mut self) -> Result<Value> {
        loop {
            let op = self.byte()?;
            match op {
                // PROTO: protocol version. FRAME: a length hint, not needed.
                0x80 => {
                    let version = self.byte()?;
                    if version > 5 {
                        return Err(invalid(format!("unsupported protocol {version}")));
                    }
                }
                0x95 => {
                    self.take(8)?;
                }
                // STOP: the top of the stack is the result.
                b'.' => {
                    let value = self.pop()?;
                    if !self.stack.is_empty() {
                        return Err(invalid("values left on the stack at STOP"));
                    }
                    return Ok(value);
                }
                b'(' => self.stack.push(Value::Mark),
                b'N' => self.stack.push(Value::None),
                0x88 => self.stack.push(Value::Bool(true)),
                0x89 => self.stack.push(Value::Bool(false)),
                b'K' => {
                    let v = self.byte()?;
                    self.stack.push(Value::Int(i64::from(v)));
                }
                b'M' => {
                    let v = self.u16()?;
                    self.stack.push(Value::Int(i64::from(v)));
                }
                b'J' => {
                    let v = self.i32()?;
                    self.stack.push(Value::Int(i64::from(v)));
                }
                0x8a => {
                    let len = self.byte()? as usize;
                    if len > 8 {
                        // Too big for an i64, such as the legacy file's magic number.
                        // It is kept as an opaque value, since nothing reads it.
                        self.take(len)?;
                        self.stack.push(Value::Opaque);
                        continue;
                    }
                    let bytes = self.take(len)?;
                    let mut value: i64 = 0;
                    for (i, b) in bytes.iter().enumerate() {
                        value |= i64::from(*b) << (8 * i);
                    }
                    // Sign-extend a negative number.
                    if len > 0 && len < 8 && bytes[len - 1] & 0x80 != 0 {
                        value |= -1i64 << (8 * len);
                    }
                    self.stack.push(Value::Int(value));
                }
                b'G' => {
                    let bytes = self.take(8)?;
                    let mut raw = [0u8; 8];
                    raw.copy_from_slice(bytes);
                    self.stack.push(Value::Float(f64::from_be_bytes(raw)));
                }
                b'X' => {
                    let len = self.u32()? as usize;
                    self.string(len)?;
                }
                0x8c => {
                    let len = self.byte()? as usize;
                    self.string(len)?;
                }
                0x8d => {
                    let len = self.u64()?;
                    let len = usize::try_from(len).map_err(|_| invalid("string is too long"))?;
                    self.string(len)?;
                }
                b']' => self.stack.push(Value::List(self.new_seq(Vec::new(), 1))),
                b'}' => self.stack.push(Value::Dict(self.new_seq(Vec::new(), 1))),
                b')' => self.stack.push(Value::Tuple(self.new_seq(Vec::new(), 1))),
                b't' => {
                    let items = self.pop_mark()?;
                    self.stack.push(self.tuple(items)?);
                }
                0x85 => {
                    let a = self.pop()?;
                    self.stack.push(self.tuple(vec![a])?);
                }
                0x86 => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.stack.push(self.tuple(vec![a, b])?);
                }
                0x87 => {
                    let c = self.pop()?;
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.stack.push(self.tuple(vec![a, b, c])?);
                }
                b'a' => {
                    let item = self.pop()?;
                    let list = self.top_seq(Container::List)?;
                    self.add_item(&list, item)?;
                }
                b'e' => {
                    let items = self.pop_mark()?;
                    let list = self.top_seq(Container::List)?;
                    for item in items {
                        self.add_item(&list, item)?;
                    }
                }
                b's' => {
                    let value = self.pop()?;
                    let key = self.pop()?;
                    let dict = self.top_seq(Container::Dict)?;
                    self.add_pair(&dict, key, value)?;
                }
                b'u' => {
                    let items = self.pop_mark()?;
                    if items.len() % 2 != 0 {
                        return Err(invalid("SETITEMS needs key and value pairs"));
                    }
                    let dict = self.top_seq(Container::Dict)?;
                    let mut pairs = items.into_iter();
                    while let (Some(key), Some(value)) = (pairs.next(), pairs.next()) {
                        self.add_pair(&dict, key, value)?;
                    }
                }
                b'q' => {
                    let index = u64::from(self.byte()?);
                    self.put(index)?;
                }
                b'r' => {
                    let index = u64::from(self.u32()?);
                    self.put(index)?;
                }
                0x94 => {
                    let index = self.memo.len() as u64;
                    self.put(index)?;
                }
                b'h' => {
                    let index = u64::from(self.byte()?);
                    self.get(index)?;
                }
                b'j' => {
                    let index = u64::from(self.u32()?);
                    self.get(index)?;
                }
                // GLOBAL: module and name, each on its own line.
                b'c' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    self.stack
                        .push(Value::Global(format!("{module}.{name}").into()));
                }
                0x93 => {
                    let name = self.pop()?;
                    let module = self.pop()?;
                    let (Value::Str(module), Value::Str(name)) = (module, name) else {
                        return Err(invalid("STACK_GLOBAL needs two strings"));
                    };
                    self.stack
                        .push(Value::Global(format!("{module}.{name}").into()));
                }
                b'R' => {
                    let args = self.pop()?;
                    let callable = self.pop()?;
                    let value = self.call(callable, args)?;
                    self.stack.push(value);
                }
                // BINPERSID: a persistent id, which torch uses for storages.
                b'Q' => {
                    let pid = self.pop()?;
                    let value = self.persistent(pid)?;
                    self.stack.push(value);
                }
                // BUILD: sets attributes on an object. Only dicts are accepted,
                // and their attributes, such as `_metadata`, carry no tensors.
                b'b' => {
                    let _state = self.pop()?;
                    let target = self.pop()?;
                    if !matches!(target, Value::Dict(_)) {
                        return Err(invalid("BUILD is only allowed on dicts"));
                    }
                    self.stack.push(target);
                }
                b'0' => {
                    self.pop()?;
                }
                b'2' => {
                    let top = self
                        .stack
                        .last()
                        .cloned()
                        .ok_or_else(|| invalid("empty stack"))?;
                    self.stack.push(top);
                }
                other => {
                    return Err(invalid(format!(
                        "unsupported opcode 0x{other:02x} at byte {}",
                        self.pos - 1
                    )))
                }
            }
        }
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| invalid("stream ends early"))?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(b);
        Ok(u64::from_le_bytes(raw))
    }

    /// Reads a line, without its newline.
    fn line(&mut self) -> Result<String> {
        let rest = &self.data[self.pos..];
        let end = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| invalid("unterminated GLOBAL line"))?;
        let text = std::str::from_utf8(&rest[..end]).map_err(|_| invalid("GLOBAL is not UTF-8"))?;
        self.pos += end + 1;
        Ok(text.to_owned())
    }

    fn string(&mut self, len: usize) -> Result<()> {
        let bytes = self.take(len)?;
        let text: Rc<str> = std::str::from_utf8(bytes)
            .map_err(|_| invalid("string is not UTF-8"))?
            .into();
        self.stack.push(Value::Str(text));
        Ok(())
    }

    fn pop(&mut self) -> Result<Value> {
        match self.stack.pop() {
            Some(Value::Mark) | None => Err(invalid("stack underflow")),
            Some(value) => Ok(value),
        }
    }

    /// Pops everything above the most recent mark, oldest first, and removes the mark.
    fn pop_mark(&mut self) -> Result<Vec<Value>> {
        let mark = self
            .stack
            .iter()
            .rposition(|v| matches!(v, Value::Mark))
            .ok_or_else(|| invalid("no MARK on the stack"))?;
        let mut items = self.stack.split_off(mark);
        items.remove(0);
        Ok(items)
    }

    fn tuple(&self, items: Vec<Value>) -> Result<Value> {
        let depth = self.depth_of(&items)?;
        Ok(Value::Tuple(self.new_seq(items, depth)))
    }

    /// The depth a container holding `items` would have. Refuses nesting past `MAX_DEPTH`.
    fn depth_of(&self, items: &[Value]) -> Result<usize> {
        let depth = items.iter().map(child_depth).max().unwrap_or(0) + 1;
        if depth > MAX_DEPTH {
            return Err(invalid("values are nested too deeply"));
        }
        Ok(depth)
    }

    /// The container on top of the stack, which must be of kind `kind`.
    fn top_seq(&self, kind: Container) -> Result<Rc<Seq>> {
        match (self.stack.last(), kind) {
            (Some(Value::List(seq)), Container::List)
            | (Some(Value::Dict(seq)), Container::Dict) => Ok(seq.clone()),
            _ => Err(invalid("container opcode applied to the wrong value")),
        }
    }

    fn add_item(&self, list: &Rc<Seq>, item: Value) -> Result<()> {
        let child = child_depth(&item);
        list.grow_to(child);
        check_depth(list.depth.get())?;
        list.items.borrow_mut().push(item);
        Ok(())
    }

    fn add_pair(&self, dict: &Rc<Seq>, key: Value, value: Value) -> Result<()> {
        if !matches!(key, Value::Str(_) | Value::Int(_) | Value::Bool(_)) {
            return Err(invalid("dict keys must be strings or numbers"));
        }
        dict.grow_to(child_depth(&value));
        check_depth(dict.depth.get())?;
        let mut items = dict.items.borrow_mut();
        items.push(key);
        items.push(value);
        Ok(())
    }

    fn put(&mut self, index: u64) -> Result<()> {
        if self.memo.len() >= MAX_MEMO {
            return Err(invalid("too many memo entries"));
        }
        let top = self
            .stack
            .last()
            .cloned()
            .ok_or_else(|| invalid("PUT on an empty stack"))?;
        self.memo.insert(index, top);
        Ok(())
    }

    fn get(&mut self, index: u64) -> Result<()> {
        let value = self
            .memo
            .get(&index)
            .cloned()
            .ok_or_else(|| invalid("GET of an unset memo entry"))?;
        self.stack.push(value);
        Ok(())
    }

    /// Calls an allowlisted global with `args`.
    fn call(&self, callable: Value, args: Value) -> Result<Value> {
        let Value::Global(name) = callable else {
            return Err(invalid("REDUCE on something other than an allowed global"));
        };
        let Value::Tuple(args) = args else {
            return Err(invalid("REDUCE arguments must be a tuple"));
        };
        let args = args.items();
        match &*name {
            "collections.OrderedDict" => Ok(Value::Dict(self.new_seq(Vec::new(), 1))),
            "torch._utils._rebuild_tensor_v2" | "torch._utils._rebuild_tensor" => {
                tensor_from_args(&args)
            }
            // A parameter is a tensor with a flag and hooks, which are ignored.
            "torch._utils._rebuild_parameter" => match args.first() {
                Some(tensor @ Value::Tensor(_)) => Ok(tensor.clone()),
                _ => Err(invalid("_rebuild_parameter needs a tensor")),
            },
            other => Err(invalid(format!("global `{other}` is not allowed"))),
        }
    }

    /// Turns a persistent id, `("storage", type, key, location, numel)`, into a storage.
    fn persistent(&mut self, pid: Value) -> Result<Value> {
        let Value::Tuple(seq) = pid else {
            return Err(invalid("persistent id must be a tuple"));
        };
        let items = seq.items();
        // Zip checkpoints write five items. Legacy ones add a trailing None.
        let (tag, kind, key, numel) = match items.as_slice() {
            [Value::Str(tag), Value::Global(kind), Value::Str(key), _location, Value::Int(numel)] => {
                (tag, kind, key, numel)
            }
            [Value::Str(tag), Value::Global(kind), Value::Str(key), _location, Value::Int(numel), Value::None] => {
                (tag, kind, key, numel)
            }
            _ => return Err(invalid("persistent id has an unexpected shape")),
        };
        if &**tag != "storage" {
            return Err(invalid("persistent id is not a storage"));
        }
        let dtype = storage_dtype(kind)
            .ok_or_else(|| invalid(format!("storage type `{kind}` is not allowed")))?;
        let numel = u64::try_from(*numel).map_err(|_| invalid("negative storage size"))?;
        let storage = Rc::new(Storage {
            dtype,
            key: key.to_string(),
            numel,
        });
        self.storages.push(storage.clone());
        Ok(Value::Storage(storage))
    }
}

#[derive(Clone, Copy)]
enum Container {
    List,
    Dict,
}

fn child_depth(value: &Value) -> usize {
    match value {
        Value::Tuple(seq) | Value::List(seq) | Value::Dict(seq) => seq.depth.get(),
        _ => 0,
    }
}

fn check_depth(depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("values are nested too deeply"));
    }
    Ok(())
}

/// Builds a tensor from `_rebuild_tensor_v2(storage, offset, size, stride, ...)`.
fn tensor_from_args(args: &[Value]) -> Result<Value> {
    let [Value::Storage(storage), Value::Int(offset), size, stride, ..] = args else {
        return Err(invalid(
            "_rebuild_tensor needs a storage, offset, size and stride",
        ));
    };
    let offset = u64::try_from(*offset).map_err(|_| invalid("negative tensor offset"))?;
    let shape = int_tuple(size)?;
    let stride = int_tuple(stride)?;
    if shape.len() != stride.len() {
        return Err(invalid("tensor shape and stride differ in rank"));
    }
    Ok(Value::Tensor(Rc::new(TensorRef {
        storage: storage.clone(),
        offset,
        shape,
        stride,
    })))
}

fn int_tuple(value: &Value) -> Result<Vec<u64>> {
    let Value::Tuple(seq) = value else {
        return Err(invalid("tensor shape and strides must be tuples"));
    };
    seq.items()
        .iter()
        .map(|v| match v {
            Value::Int(n) => u64::try_from(*n).map_err(|_| invalid("negative tensor dimension")),
            _ => Err(invalid("tensor shape and strides must hold integers")),
        })
        .collect()
}

impl Loader<'_> {
    /// Makes a container, and records it so that it can be cleared later.
    fn new_seq(&self, items: Vec<Value>, depth: usize) -> Rc<Seq> {
        let seq = Seq::new(items, depth);
        self.containers.borrow_mut().push(Rc::downgrade(&seq));
        seq
    }

    /// Clears every container made while loading, which breaks any cycles among
    /// them. Nothing made here is used after this.
    fn release(&mut self) {
        self.memo.clear();
        self.stack.clear();
        for weak in self.containers.borrow().iter() {
            if let Some(seq) = weak.upgrade() {
                seq.items.borrow_mut().clear();
            }
        }
    }
}

/// A copy of `value` that shares no containers with the loader. A pickle can
/// make a container hold itself, so the copy stops at `MAX_DEPTH`, and a cyclic
/// value is refused there.
fn detach(value: &Value, depth: usize) -> Result<Value> {
    Ok(match value {
        Value::Tuple(seq) => Value::Tuple(detach_seq(seq, depth)?),
        Value::List(seq) => Value::List(detach_seq(seq, depth)?),
        Value::Dict(seq) => Value::Dict(detach_seq(seq, depth)?),
        other => other.clone(),
    })
}

fn detach_seq(seq: &Seq, depth: usize) -> Result<Rc<Seq>> {
    if depth >= MAX_DEPTH {
        return Err(invalid(
            "values are nested too deeply, or they contain themselves",
        ));
    }
    let items = seq
        .items()
        .iter()
        .map(|item| detach(item, depth + 1))
        .collect::<Result<Vec<_>>>()?;
    Ok(Seq::new(items, depth + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_opcodes() {
        // 'i' (INST) builds arbitrary objects and is not supported.
        assert!(load(b"\x80\x02i").is_err());
    }

    #[test]
    fn rejects_truncated_streams() {
        assert!(load(b"\x80\x02X\x10\x00\x00\x00ab").is_err());
        assert!(load(b"").is_err());
    }

    #[test]
    fn rejects_globals_outside_the_allowlist() {
        let stream = b"\x80\x02cos\nsystem\n(X\x02\x00\x00\x00lstR.";
        let err = load(stream).err().expect("must be refused");
        assert!(err.to_string().contains("os.system"), "{err}");
    }

    #[test]
    fn reads_plain_values_and_dicts() {
        // {"a": 1} built with EMPTY_DICT, MARK-free SETITEM.
        let stream = b"\x80\x02}q\x00X\x01\x00\x00\x00aK\x01s.";
        let Value::Dict(seq) = load(stream).unwrap() else {
            panic!("expected a dict");
        };
        let items = seq.items();
        assert!(matches!(&items[0], Value::Str(s) if &**s == "a"));
        assert!(matches!(items[1], Value::Int(1)));
    }

    #[test]
    fn refuses_deep_nesting() {
        let mut stream = b"\x80\x02".to_vec();
        stream.push(b')'); // EMPTY_TUPLE
        for _ in 0..MAX_DEPTH + 2 {
            stream.extend_from_slice(b"\x85"); // TUPLE1
        }
        stream.push(b'.');
        assert!(load(&stream).is_err());
    }

    #[test]
    fn a_list_that_contains_itself_is_refused() {
        // EMPTY_LIST, memoise it, fetch it back, and APPEND it to itself.
        let stream = b"\x80\x02]q\x00h\x00a.";
        let err = load(stream).err().expect("a cyclic value must be refused");
        assert!(err.to_string().contains("contain themselves"), "{err}");
    }
}
