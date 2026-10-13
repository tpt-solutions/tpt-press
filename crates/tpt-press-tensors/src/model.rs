//! The format-independent description of a model: its tensors and metadata.

use std::collections::BTreeMap;

/// One tensor, located inside a model file's data section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorInfo {
    /// Tensor name.
    pub name: String,
    /// Element type, such as `F32` or `BF16`, or a GGUF type such as `Q8_0`.
    pub dtype: String,
    /// Dimensions. Empty for a scalar.
    pub shape: Vec<u64>,
    /// Start of the tensor's bytes, relative to the start of the data section.
    pub begin: u64,
    /// End of the tensor's bytes, relative to the start of the data section.
    pub end: u64,
}

impl TensorInfo {
    /// Size of the tensor's bytes.
    pub fn byte_len(&self) -> u64 {
        self.end - self.begin
    }

    /// Number of elements, the product of the shape. A scalar has one element.
    pub fn element_count(&self) -> u64 {
        self.shape.iter().product()
    }
}

/// A parsed model file: its tensors and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// String metadata from the file.
    pub metadata: BTreeMap<String, String>,
    /// Tensors, sorted by name.
    pub tensors: Vec<TensorInfo>,
    /// Absolute byte offset in the file where the data section starts.
    pub data_start: u64,
    /// Length of the data section in bytes.
    pub data_len: u64,
}
