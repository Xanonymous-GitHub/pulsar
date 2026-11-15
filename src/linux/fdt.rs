//! Builds a flattened device tree (the "DTB" a boot loader hands to a Linux kernel), version 17
//! of the format, as described in the Devicetree Specification 0.4, chapter 5.
//!
//! ```ignore
//! let mut tree = Fdt::new();
//! tree.begin_node("");            // the root
//! tree.prop_str("model", "demo");
//! tree.begin_node("chosen");
//! tree.prop_str("bootargs", "console=ttyS0");
//! tree.end_node();
//! tree.end_node();
//! let blob: Vec<u8> = tree.finish();
//! ```
//!
//! Only what a small machine description needs is here: nodes, and properties that are empty,
//! cells (32-bit numbers), or strings. Everything in the format is big-endian.
//!
//! This file imports nothing from the crate, so it is tested on the host:
//!
//! ```sh
//! rustc --edition 2024 --test src/linux/fdt.rs -o /tmp/t && /tmp/t
//! ```

/// The first four bytes of every device tree blob.
pub const MAGIC: u32 = 0xd00d_feed;

/// The format version written, and the oldest one a reader may assume.
const VERSION: u32 = 17;
const LAST_COMPATIBLE_VERSION: u32 = 16;

/// Size of the header: ten 32-bit fields.
const HEADER_BYTES: usize = 40;
/// The memory reservation block holds no reservation, only its terminator (two 64-bit zeros).
const RESERVATIONS_BYTES: usize = 16;

const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const END: u32 = 9;

/// A device tree under construction.
#[derive(Debug, Default)]
pub struct Fdt {
    /// The structure block: the nodes and properties as tokens.
    structure: Vec<u8>,
    /// The strings block: every property name once, each ending in a zero byte.
    strings: Vec<u8>,
    /// How many nodes are open.
    depth: usize,
}

impl Fdt {
    /// An empty tree. The first node to begin is the root, whose name is the empty string.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            structure: Vec::new(),
            strings: Vec::new(),
            depth: 0,
        }
    }

    fn word(&mut self, value: u32) {
        self.structure.extend_from_slice(&value.to_be_bytes());
    }

    /// Fills the structure block up to the next multiple of four bytes.
    fn align(&mut self) {
        self.structure
            .resize(self.structure.len().next_multiple_of(4), 0);
    }

    /// Opens a node inside the node that is open now. Close it with [`end_node`](Self::end_node).
    pub fn begin_node(&mut self, name: &str) {
        self.word(BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.align();
        self.depth += 1;
    }

    /// Closes the node opened last.
    pub fn end_node(&mut self) {
        self.word(END_NODE);
        self.depth = self.depth.saturating_sub(1);
    }

    /// The offset of `name` in the strings block, adding it if it is not there yet.
    fn name_offset(&mut self, name: &str) -> u32 {
        let mut offset = 0;
        for known in self.strings.split_inclusive(|&byte| byte == 0) {
            if known.strip_suffix(&[0]) == Some(name.as_bytes()) {
                return offset;
            }
            offset += u32::try_from(known.len()).unwrap_or(u32::MAX);
        }
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        offset
    }

    /// Adds a property with raw bytes as its value to the open node.
    pub fn prop(&mut self, name: &str, value: &[u8]) {
        let name_offset = self.name_offset(name);
        self.word(PROP);
        self.word(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.word(name_offset);
        self.structure.extend_from_slice(value);
        self.align();
    }

    /// Adds a property without a value (a flag such as `ranges`).
    pub fn prop_empty(&mut self, name: &str) {
        self.prop(name, &[]);
    }

    /// Adds a property made of 32-bit cells.
    pub fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let bytes: Vec<u8> = cells.iter().flat_map(|cell| cell.to_be_bytes()).collect();
        self.prop(name, &bytes);
    }

    /// Adds a property that is one string.
    pub fn prop_str(&mut self, name: &str, value: &str) {
        self.prop_strs(name, &[value]);
    }

    /// Adds a property that is a list of strings (for example `compatible`).
    pub fn prop_strs(&mut self, name: &str, values: &[&str]) {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        self.prop(name, &bytes);
    }

    /// Closes any node still open and returns the finished blob.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        while self.depth > 0 {
            self.end_node();
        }
        self.word(END);

        let structure_offset = HEADER_BYTES + RESERVATIONS_BYTES;
        let strings_offset = structure_offset + self.structure.len();
        let total = strings_offset + self.strings.len();
        // A tree is a few kilobytes: these always fit.
        let field = |value: usize| u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes();

        let mut blob = Vec::with_capacity(total);
        blob.extend_from_slice(&MAGIC.to_be_bytes());
        blob.extend_from_slice(&field(total));
        blob.extend_from_slice(&field(structure_offset));
        blob.extend_from_slice(&field(strings_offset));
        blob.extend_from_slice(&field(HEADER_BYTES));
        blob.extend_from_slice(&VERSION.to_be_bytes());
        blob.extend_from_slice(&LAST_COMPATIBLE_VERSION.to_be_bytes());
        blob.extend_from_slice(&0_u32.to_be_bytes()); // the boot processor is number 0
        blob.extend_from_slice(&field(self.strings.len()));
        blob.extend_from_slice(&field(self.structure.len()));
        blob.resize(structure_offset, 0);
        blob.extend_from_slice(&self.structure);
        blob.extend_from_slice(&self.strings);
        blob
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(blob: &[u8], index: usize) -> usize {
        u32::from_be_bytes(blob[index * 4..index * 4 + 4].try_into().unwrap()) as usize
    }

    fn sample() -> Vec<u8> {
        let mut tree = Fdt::new();
        tree.begin_node("");
        tree.prop_cells("#address-cells", &[2]);
        tree.prop_str("model", "demo");
        tree.begin_node("soc");
        tree.prop_empty("ranges");
        tree.prop_strs("compatible", &["a,b", "c"]);
        tree.begin_node("uart@10000000");
        tree.prop_cells("reg", &[0, 0x1000_0000, 0, 0x100]);
        tree.prop_str("compatible", "ns16550a");
        tree.finish()
    }

    #[test]
    fn the_header_describes_the_blob() {
        let blob = sample();
        assert_eq!(field(&blob, 0), MAGIC as usize);
        assert_eq!(field(&blob, 1), blob.len(), "totalsize");
        assert_eq!(
            field(&blob, 2),
            56,
            "the structure follows the header and the empty reservation list"
        );
        assert_eq!(field(&blob, 4), 40, "off_mem_rsvmap");
        assert_eq!((field(&blob, 5), field(&blob, 6)), (17, 16));
        assert_eq!(
            field(&blob, 3),
            field(&blob, 2) + field(&blob, 9),
            "strings follow the structure"
        );
        assert_eq!(field(&blob, 3) + field(&blob, 8), blob.len());
        assert!(blob[40..56].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn the_structure_is_aligned_and_balanced() {
        let blob = sample();
        let (start, size) = (field(&blob, 2), field(&blob, 9));
        assert_eq!(size % 4, 0);
        let structure = &blob[start..start + size];
        let strings = &blob[field(&blob, 3)..];

        // Walk the tokens the way a kernel does.
        let (mut at, mut depth, mut names, mut props) = (0, 0_i32, Vec::new(), Vec::new());
        loop {
            let token = u32::from_be_bytes(structure[at..at + 4].try_into().unwrap());
            at += 4;
            match token {
                BEGIN_NODE => {
                    let end = at + structure[at..].iter().position(|&byte| byte == 0).unwrap();
                    names.push(String::from_utf8(structure[at..end].to_vec()).unwrap());
                    at = (end + 1).next_multiple_of(4);
                    depth += 1;
                }
                END_NODE => depth -= 1,
                PROP => {
                    let length =
                        u32::from_be_bytes(structure[at..at + 4].try_into().unwrap()) as usize;
                    let name_at =
                        u32::from_be_bytes(structure[at + 4..at + 8].try_into().unwrap()) as usize;
                    let name_end = name_at
                        + strings[name_at..]
                            .iter()
                            .position(|&byte| byte == 0)
                            .unwrap();
                    props.push((
                        String::from_utf8(strings[name_at..name_end].to_vec()).unwrap(),
                        length,
                    ));
                    at = (at + 8 + length).next_multiple_of(4);
                }
                END => break,
                other => panic!("unknown token {other}"),
            }
        }
        assert_eq!(at, size, "END is the last token");
        assert_eq!(depth, 0, "finish closed the nodes left open");
        assert_eq!(names, ["", "soc", "uart@10000000"]);
        assert_eq!(
            props,
            [
                ("#address-cells".to_owned(), 4),
                ("model".to_owned(), 5),
                ("ranges".to_owned(), 0),
                ("compatible".to_owned(), 6),
                ("reg".to_owned(), 16),
                ("compatible".to_owned(), 9),
            ]
        );
    }

    #[test]
    fn property_names_are_stored_once() {
        let blob = sample();
        let strings = &blob[field(&blob, 3)..];
        assert_eq!(strings, b"#address-cells\0model\0ranges\0compatible\0reg\0");
    }

    #[test]
    fn a_name_that_is_the_end_of_another_is_not_confused_with_it() {
        let mut tree = Fdt::new();
        tree.begin_node("");
        tree.prop_empty("interrupt-controller");
        tree.prop_empty("controller");
        let blob = tree.finish();
        assert_eq!(
            &blob[field(&blob, 3)..],
            b"interrupt-controller\0controller\0"
        );
    }

    #[test]
    fn cells_and_strings_are_big_endian_and_terminated() {
        let mut tree = Fdt::new();
        tree.begin_node("");
        tree.prop_cells("reg", &[0x8000_0000]);
        tree.prop_str("s", "hi");
        let blob = tree.finish();
        let structure = &blob[56..];
        // BEGIN_NODE, "" padded, PROP, len 4, name 0, value
        assert_eq!(
            &structure[8..24],
            &[0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0, 0x80, 0, 0, 0]
        );
        // PROP, len 3, name 4, "hi\0" and one byte of padding
        assert_eq!(
            &structure[24..40],
            &[0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0, 4, b'h', b'i', 0, 0]
        );
    }
}
