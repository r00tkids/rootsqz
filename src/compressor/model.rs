use super::context_map::{ContextStats, SharedTable};
use super::utils::{prob_squash, prob_stretch, U24_MAX};
use std::{collections::HashMap, vec};

pub trait Model {
    /// The stretched prediction of the next bit. A model with several mixer inputs returns the first.
    fn pred(&mut self) -> f64 {
        let mut inputs = Vec::new();
        self.pred_inputs(&mut inputs);
        inputs[0]
    }
    fn learn(&mut self, bit: u8);

    /// Appends the inputs that the model gives a mixer for the next bit, the same number for every bit
    fn pred_inputs(&mut self, out: &mut Vec<f64>) {
        out.push(self.pred());
    }

    /// Appends the stretched predictions that the submodels made in the last `pred` call,
    /// depth first, in the order of `ModelConfig::submodel_names`. Used by the report.
    fn submodel_preds(&self, _out: &mut Vec<f32>) {}

    /// A small number that tells what the model knows about the next bit, 0 for nothing.
    /// Valid after `inputs`. `ContextMixer` selects its weights by it.
    fn state(&self) -> u32 {
        0
    }

    /// A context that the model offers `ContextMixer` to select weights by, below 2^16.
    /// Valid after `inputs`.
    fn context(&self) -> u32 {
        0
    }

    /// Appends the inputs of a `ContextMixer`, which may be more than `pred_inputs`, the same
    /// number for every bit
    fn inputs(&mut self, out: &mut Vec<f64>) {
        self.pred_inputs(out);
    }

    /// The mixer weight sets and weights that the model and its submodels have allocated, which
    /// the decoder allocates too. Used by the search to estimate the memory of the decoder.
    fn weights(&self) -> (usize, usize) {
        (0, 0)
    }
}

#[derive(Clone, Copy)]
pub struct NOrderByteData(u32);

impl Default for NOrderByteData {
    fn default() -> Self {
        // Start with half probability
        Self(U24_MAX >> 1)
    }
}

impl NOrderByteData {
    pub(crate) fn count(&self) -> u32 {
        (self.0 & 0xFF000000) >> 24
    }

    pub(crate) fn prob(&self) -> i32 {
        (self.0 & U24_MAX) as i32
    }

    pub(crate) fn set_count(&mut self, new_count: u32) {
        self.0 = ((new_count << 24) & 0xFF000000) | self.0 & U24_MAX;
    }

    pub(crate) fn set_prob(&mut self, new_prob: i32) {
        self.0 = (new_prob as u32 & U24_MAX) | (self.0 & 0xFF000000);
    }
}

pub struct HashTable<Record> {
    table: Vec<Record>,
    hash_mask: usize,
}

fn hash(mut value: u32, shift: u32) -> u32 {
    const K_MUL: u32 = 0x9E35A7BD;
    value ^= value >> shift;
    K_MUL.wrapping_mul(value) >> shift
}

impl<Record> HashTable<Record>
where
    Record: Default + Clone,
{
    pub fn new(pow2_size: u32) -> Self {
        let context_size = (1 << pow2_size) as usize;
        tracing::debug!(
            "Hash table Size: {} MiB",
            (size_of::<Record>() * context_size) / (1024 * 1024)
        );

        Self {
            table: vec![Record::default(); context_size],
            hash_mask: context_size - 1,
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn get_mut<'a>(&'a mut self, key: u32) -> &'a mut Record {
        &mut self.table[key as usize & self.hash_mask]
    }
}

/// NOrderByte model for byte predictions
/// Can describe [0, 8] order models and partial models
/// It also supports being a word model
/// (using characters as window filters)
pub struct NOrderByte {
    ctx: u32,
    stats: ContextStats,

    magic_num: u32,
    prev_bytes: u64,
    mask: u64,
    is_word_model: bool,
    class_order: u8,
    transitions: Option<Vec<u16>>,
    transition_mask: usize,

    bit_ctx: u32,
}

impl NOrderByte {
    pub fn new_norder_model(
        byte_mask: u8,
        hash_table: SharedTable,
        max_count: u32,
    ) -> Self {
        assert!(max_count <= 255);

        let mut bit_mask: u64 = 0;
        for i in 0..8 {
            bit_mask |= ((byte_mask >> i) & 1) as u64 * (0xff << (i * 8));
        }

        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(byte_mask as u32, 2),
            stats: ContextStats::new(hash_table, max_count),
            prev_bytes: 0,
            mask: bit_mask,
            is_word_model: false,
            class_order: 0,
            transitions: None,
            transition_mask: 0,
        }
    }

    pub fn new_word_model(
        hash_table: SharedTable,
        max_count: u32,
    ) -> Self {
        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(1337 as u32, 2),
            stats: ContextStats::new(hash_table, max_count),
            prev_bytes: 2166136261,
            mask: u64::MAX,
            is_word_model: true,
            class_order: 0,
            transitions: None,
            transition_mask: 0,
        }
    }

    pub fn new_char_class_model(
        order: u8,
        hash_table: SharedTable,
        max_count: u32,
    ) -> Self {
        assert!((1..=8).contains(&order));
        assert!(max_count <= 255);
        Self {
            ctx: 0,
            bit_ctx: 1,
            magic_num: hash(0x434c4153 ^ order as u32, 2),
            stats: ContextStats::new(hash_table, max_count),
            prev_bytes: 0,
            mask: (1u64 << (order * 3)) - 1,
            is_word_model: false,
            class_order: order,
            transitions: None,
            transition_mask: 0,
        }
    }

    /// Maps a recent byte suffix to its likely successor, then uses that successor
    /// as the bit model context. This differs from masking the recent bytes.
    pub fn new_indirect_model(
        context_bytes: u8,
        hash_table: SharedTable,
        max_count: u32,
    ) -> Self {
        assert!((1..=3).contains(&context_bytes));
        let mut model = Self::new_norder_model(0, hash_table, max_count);
        let size = 1usize << (8 * context_bytes);
        model.magic_num = hash(9191, 2);
        model.transitions = Some(vec![0; size]);
        model.transition_mask = size - 1;
        model
    }
}

fn char_class(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => 1,
        b'0'..=b'9' => 2,
        b' ' | b'\t' | b'\r' | b'\n' => 3,
        b'\'' | b'"' | b'`' => 4,
        b'(' | b'[' | b'{' => 5,
        b')' | b']' | b'}' => 6,
        b'.' | b',' | b':' | b';' | b'+' | b'-' | b'*' | b'=' | b'<' | b'>' | b'!' | b'&'
        | b'|' | b'%' | b'^' | b'~' | b'?' | b'/' | b'\\' => 7,
        _ => 0,
    }
}

impl Model for NOrderByte {
    fn state(&self) -> u32 {
        self.stats.count()
    }

    fn pred_inputs(&mut self, out: &mut Vec<f64>) {
        self.stats.pred(self.ctx ^ self.bit_ctx, self.bit_ctx, out);
    }

    fn learn(&mut self, bit: u8) {
        self.stats.update(bit);

        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            let current_byte = self.bit_ctx & 0xff;
            self.stats.byte_end(current_byte);

            if let Some(transitions) = &mut self.transitions {
                // A saturating vote keeps a stable candidate while allowing replacement.
                let old_context = self.prev_bytes as usize & self.transition_mask;
                let entry = &mut transitions[old_context];
                let candidate = (*entry & 255) as u8;
                let count = (*entry >> 8) as u8;
                *entry = if count == 0 || (count == 1 && candidate != current_byte as u8) {
                    256 | current_byte as u16
                } else if candidate == current_byte as u8 {
                    ((count.saturating_add(1) as u16) << 8) | current_byte as u16
                } else {
                    (((count - 1) as u16) << 8) | candidate as u16
                };
                self.prev_bytes = (self.prev_bytes << 8) | current_byte as u64;
            } else if self.is_word_model {
                let next_char = current_byte as u8 as char;
                if next_char.is_ascii_alphanumeric() || matches!(next_char, '_' | '.' | '[' | ']') {
                    // 32 bit FNV-1a, which the JS decoder computes with Math.imul
                    self.prev_bytes = (self.prev_bytes as u32 ^ next_char.to_ascii_lowercase() as u32)
                        .wrapping_mul(16777619) as u64;
                } else {
                    self.prev_bytes = 2166136261;
                }
            } else if self.class_order != 0 {
                self.prev_bytes = (self.prev_bytes << 3) | char_class(current_byte as u8) as u64;
            } else {
                self.prev_bytes = (self.prev_bytes << 8) | current_byte as u64;
            }

            let masked_prev_bytes = if let Some(transitions) = &self.transitions {
                let entry = transitions[self.prev_bytes as usize & self.transition_mask];
                (entry & 255) as u64
            } else {
                self.prev_bytes & self.mask
            };
            self.ctx = (hash((masked_prev_bytes >> 32) as u32, 3)
                .wrapping_mul(9)
                .wrapping_add(hash(masked_prev_bytes as u32, 3)))
            .wrapping_add(1) // To ensure ctx doesn't overlap between models
            .wrapping_mul(self.magic_num);

            // Reset bit_ctx
            self.bit_ctx = 1;
        }
    }
}

/// Predicts bits from the position in a lexical token and the last delimiter.
/// This state is independent of the Word model's rolling character hash.
pub struct TokenPosition {
    stats: ContextStats,
    context_bytes: u8,
    max_position: u8,
    kind: u8,
    position: u8,
    delimiter: u8,
    prev_bytes: u16,
    escaped: bool,
    bit_ctx: u32,
    ctx: u32,
}

impl TokenPosition {
    pub fn new(
        hash_table: SharedTable,
        max_count: u32,
        context_bytes: u8,
        max_position: u8,
    ) -> Self {
        assert!(max_count <= 255);
        assert!(context_bytes <= 2);
        assert!(max_position > 0);
        Self {
            stats: ContextStats::new(hash_table, max_count),
            context_bytes,
            max_position,
            kind: 0,
            position: 0,
            delimiter: 0,
            prev_bytes: 0,
            escaped: false,
            bit_ctx: 1,
            ctx: 0,
        }
    }

    fn advance(&mut self, byte: u8) {
        if self.kind >= 3 {
            let quote = match self.kind {
                3 => b'\'',
                4 => b'"',
                _ => b'`',
            };
            if self.escaped {
                self.escaped = false;
                self.position = self.position.saturating_add(1).min(self.max_position);
            } else if byte == b'\\' {
                self.escaped = true;
                self.position = self.position.saturating_add(1).min(self.max_position);
            } else if byte == quote {
                self.kind = 0;
                self.position = 0;
                self.delimiter = byte;
            } else {
                self.position = self.position.saturating_add(1).min(self.max_position);
            }
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            self.kind = match byte {
                b'\'' => 3,
                b'"' => 4,
                _ => 5,
            };
            self.position = 0;
        } else if byte.is_ascii_alphabetic()
            || byte == b'_'
            || byte == b'$'
            || (self.kind == 1 && byte.is_ascii_digit())
        {
            self.position = if self.kind == 1 {
                self.position.saturating_add(1).min(self.max_position)
            } else {
                1
            };
            self.kind = 1;
        } else if byte.is_ascii_digit() {
            self.position = if self.kind == 2 {
                self.position.saturating_add(1).min(self.max_position)
            } else {
                1
            };
            self.kind = 2;
        } else {
            self.kind = 0;
            self.position = 0;
            if !byte.is_ascii_whitespace() {
                self.delimiter = byte;
            }
        }
        self.prev_bytes = (self.prev_bytes << 8) | byte as u16;
        let recent = match self.context_bytes {
            0 => 0,
            1 => self.prev_bytes & 0xff,
            _ => self.prev_bytes,
        };
        let state =
            (self.kind as u32) | ((self.position as u32) << 3) | ((self.delimiter as u32) << 11);
        self.ctx = hash(state ^ ((recent as u32) << 19) ^ 0x6b8b4567, 3).wrapping_mul(0x85ebca6b);
    }
}

impl Model for TokenPosition {
    fn state(&self) -> u32 {
        self.stats.count()
    }

    fn pred_inputs(&mut self, out: &mut Vec<f64>) {
        self.stats.pred(self.ctx ^ self.bit_ctx, self.bit_ctx, out);
    }

    fn learn(&mut self, bit: u8) {
        self.stats.update(bit);
        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            let byte = (self.bit_ctx & 255) as u8;
            self.stats.byte_end(byte as u32);
            self.advance(byte);
            self.bit_ctx = 1;
        }
    }
}

fn js_mix(a: u32, b: u32) -> u32 {
    a.wrapping_mul(0x9E3779B1).wrapping_add(b)
}

/// One level of bracket nesting seen by `JsLexer`: its opening bracket, and the token before
/// the last `:` in it, which is the key of a property in an object literal.
#[derive(Clone, Copy, Default)]
struct JsFrame {
    open: u8,
    key: u32,
}

/// Splits minified JavaScript into tokens, as far as `JsContext` needs it.
/// Identifiers, numbers and strings are tokens, kept as hashes; other bytes are punctuation.
pub struct JsLexer {
    /// Quote of the string the lexer is in, or 0
    quote: u8,
    /// Class of the current token: 0 none, 1 identifier, 2 number, 3 string
    kind: u8,
    /// Hash of the current token so far
    token: u32,
    /// Hash of the last completed token
    prev_token: u32,
    /// Last punctuation byte
    punct: u8,
    /// Classes of the last tokens and the punctuation between them, one byte each
    skeleton: u64,
    frames: Vec<JsFrame>,
    /// The token that followed each token the last time, by its hash
    next_token: HashMap<u32, u32>,
}

impl JsLexer {
    fn new() -> Self {
        Self {
            quote: 0,
            kind: 0,
            token: 0,
            prev_token: 0,
            punct: 0,
            skeleton: 0,
            frames: vec![JsFrame::default()],
            next_token: HashMap::new(),
        }
    }

    fn end_token(&mut self) {
        if self.kind != 0 {
            self.next_token.insert(self.prev_token, self.token);
            self.prev_token = self.token;
            self.skeleton = (self.skeleton << 8) | self.kind as u64;
            self.kind = 0;
        }
    }

    fn update(&mut self, c: u8) {
        if self.quote != 0 {
            // Escapes are not worth their bytes in the decoder
            if c == self.quote {
                self.quote = 0;
                self.end_token();
                return;
            }
            self.token = js_mix(self.token, c as u32);
            return;
        }
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || (c == b'.' && self.kind == 2) {
            if self.kind == 0 {
                self.kind = if c.is_ascii_digit() { 2 } else { 1 };
                self.token = self.kind as u32;
            }
            self.token = js_mix(self.token, c as u32);
            return;
        }
        self.end_token();
        if matches!(c, b'"' | b'\'' | b'`') {
            self.quote = c;
            self.kind = 3;
            self.token = 3;
        } else if !matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            self.punct = c;
            self.skeleton = (self.skeleton << 8) | c as u64;
            match c {
                b'(' | b'[' | b'{' => {
                    self.frames.push(JsFrame { open: c, key: 0 })
                }
                b')' | b']' | b'}' if self.frames.len() > 1 => {
                    self.frames.pop();
                }
                b':' => self.frames.last_mut().unwrap().key = self.prev_token,
                _ => {}
            }
        }
    }

    /// Hash of the context of a variant, for the byte after `last_byte`
    fn context(&self, variant: u8, last_byte: u8) -> u32 {
        let frame = self.frames.last().unwrap();
        let token = if self.kind == 0 { 0 } else { self.token };
        match variant {
            // The bracket, property key and punctuation around the token
            0 => js_mix(
                js_mix(js_mix(token, frame.open as u32), frame.key),
                self.punct as u32,
            ),
            // The token that followed the last token the last time
            1 => js_mix(token, *self.next_token.get(&self.prev_token).unwrap_or(&0)),
            // The last byte and the bracket nesting
            2 => js_mix(
                js_mix(token, last_byte as u32),
                (frame.open as u32) << 8 | self.frames.len() as u32,
            ),
            // The classes of the last tokens and the punctuation between them
            _ => js_mix(
                js_mix(token, self.skeleton as u32),
                (self.skeleton >> 32) as u32,
            ),
        }
    }
}

const TEXT_RUN: u32 = 32;

/// Predicts bits from a context that `JsLexer` derives from the tokens of JavaScript.
pub struct JsContext {
    stats: ContextStats,
    variant: u8,
    lexer: JsLexer,
    /// Bytes since the last control byte below tab, which text does not have, up to `TEXT_RUN`
    text_run: u32,
    bit_ctx: u32,
    ctx: u32,
}

impl JsContext {
    pub fn new(
        variant: u8,
        hash_table: SharedTable,
        max_count: u32,
    ) -> Self {
        assert!(max_count <= 255);
        Self {
            stats: ContextStats::new(hash_table, max_count),
            variant,
            lexer: JsLexer::new(),
            text_run: 0,
            bit_ctx: 1,
            ctx: 0,
        }
    }
}

impl Model for JsContext {
    fn state(&self) -> u32 {
        self.stats.count()
    }

    fn pred_inputs(&mut self, out: &mut Vec<f64>) {
        if self.text_run < TEXT_RUN {
            return self.stats.pred_none(out);
        }
        self.stats.pred(self.ctx ^ self.bit_ctx, self.bit_ctx, out);
    }

    /// The bracket the lexer is in, the class of the token and the last punctuation
    fn context(&self) -> u32 {
        let lexer = &self.lexer;
        let open = match lexer.frames.last().unwrap().open {
            b'(' => 1,
            b'[' => 2,
            b'{' => 3,
            _ => 0,
        };
        (open << 2 | lexer.kind as u32) << 8 | lexer.punct as u32
    }

    fn learn(&mut self, bit: u8) {
        if self.text_run >= TEXT_RUN {
            self.stats.update(bit);
        }
        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;
        if self.bit_ctx >= 256 {
            let byte = (self.bit_ctx & 255) as u8;
            if self.text_run >= TEXT_RUN {
                self.stats.byte_end(byte as u32);
            }
            self.text_run = if byte < 9 {
                0
            } else {
                (self.text_run + 1).min(TEXT_RUN)
            };
            self.lexer.update(byte);
            let h = js_mix(self.lexer.context(self.variant, byte), self.variant as u32);
            self.ctx = (h ^ (h >> 15)).wrapping_mul(0x85EBCA6B) & !255;
            self.bit_ctx = 1;
        }
    }
}

/// Predicts bits by copying the byte following an earlier occurrence of the
/// current byte context. Both the history and lookup table have fixed bounds.
pub struct MatchPredictor {
    history: Vec<u8>,
    keys: Vec<u32>,
    positions: Vec<usize>,
    position: usize,
    context: u32,
    context_mask: u32,
    table_mask: usize,
    match_position: Option<usize>,
    partial_byte: u16,
    confidence: f64,
}

impl MatchPredictor {
    pub fn new(context_bytes: u32, confidence: f64, table_bits: u32) -> Self {
        assert!((1..=4).contains(&context_bytes));
        assert!((0.5..1.0).contains(&confidence));
        assert!((8..=20).contains(&table_bits));
        let table_size = 1usize << table_bits;
        Self {
            history: vec![0; 1 << 16],
            keys: vec![0; table_size],
            positions: vec![0; table_size],
            position: 0,
            context: 0,
            context_mask: if context_bytes == 4 {
                u32::MAX
            } else {
                (1 << (8 * context_bytes)) - 1
            },
            table_mask: table_size - 1,
            match_position: None,
            partial_byte: 1,
            confidence,
        }
    }
}

impl Model for MatchPredictor {
    fn pred(&mut self) -> f64 {
        match self.match_position {
            Some(pos) if pos < self.position && self.position - pos <= self.history.len() => {
                let shift = 7 - self.partial_byte.ilog2();
                let bit = (self.history[pos & (self.history.len() - 1)] >> shift) & 1;
                prob_stretch(if bit == 1 {
                    self.confidence
                } else {
                    1.0 - self.confidence
                })
            }
            _ => 0.0,
        }
    }

    fn learn(&mut self, bit: u8) {
        if let Some(pos) = self.match_position {
            let shift = 7 - self.partial_byte.ilog2();
            if pos >= self.position
                || self.position - pos > self.history.len()
                || (self.history[pos & (self.history.len() - 1)] >> shift) & 1 != bit
            {
                self.match_position = None;
            }
        }
        self.partial_byte = (self.partial_byte << 1) | bit as u16;
        if self.partial_byte >= 256 {
            let byte = self.partial_byte as u8;
            let history_slot = self.position & (self.history.len() - 1);
            self.history[history_slot] = byte;
            self.position += 1;
            self.context = ((self.context << 8) | byte as u32) & self.context_mask;
            let slot = self.context.wrapping_mul(0x9E35A7BD) as usize & self.table_mask;
            if let Some(pos) = self.match_position {
                self.match_position = Some(pos + 1);
            } else {
                let prior = self.positions[slot];
                if prior != 0
                    && self.keys[slot] == self.context
                    && self.position - (prior - 1) <= self.history.len()
                {
                    self.match_position = Some(prior - 1);
                }
            }
            self.keys[slot] = self.context;
            self.positions[slot] = self.position + 1;
            self.partial_byte = 1;
        }
    }
}

/// Predicts the byte that followed an earlier occurrence of the last `min_len` bytes, found
/// through a hash chain over the whole history. How far the expected bit is trusted is learnt per
/// match length. After a mispredicted byte the match carries on, as data often differs from an
/// earlier copy in a byte or two, until it mispredicts `MATCH_MAX_MISSES` bytes in a row.
pub struct MatchModel {
    history: Vec<u8>,
    head: Vec<u32>,
    chain: Vec<u32>,
    min_len: usize,
    candidates: u32,
    expected_byte_context: bool,
    second: bool,
    /// Position in `history` of the expected byte, valid while `active`
    match_ptr: usize,
    active: bool,
    /// Verified length of the match, 0 after a mispredicted byte
    len: u32,
    misses: u32,
    byte_missed: bool,
    partial_byte: u32,
    counters: Vec<NOrderByteData>,
    counter_idx: usize,
    expected_bit: u8,
    /// 0 if the model predicts nothing for the current bit, else 1 + the expected bit + 2 times
    /// a bucket of the length: 0 after a mispredicted byte, else 1 + log2 of the length, up to 12
    state: u32,
}

const MATCH_HASH_BITS: u32 = 22;
const MATCH_MAX_MISSES: u32 = 8;
const MATCH_MAX_LEN: u32 = 65535;

impl MatchModel {
    /// `candidates` is how many earlier occurrences are compared: the longest is taken, or with
    /// `second` the most recent one that predicts another byte than the most recent occurrence.
    /// `expected_byte_context` learns the trust per expected byte as well.
    pub fn new(min_len: u32, candidates: u32, expected_byte_context: bool, second: bool) -> Self {
        assert!((1..=32).contains(&min_len));
        assert!(candidates >= 1);
        Self {
            history: Vec::new(),
            head: vec![0; 1 << MATCH_HASH_BITS],
            chain: vec![0],
            min_len: min_len as usize,
            candidates,
            expected_byte_context,
            second,
            match_ptr: 0,
            active: false,
            len: 0,
            misses: 0,
            byte_missed: false,
            partial_byte: 1,
            // 64 length buckets, then one per number of misses
            counters: vec![NOrderByteData::default(); (64 + MATCH_MAX_MISSES as usize) << 16],
            counter_idx: 0,
            expected_bit: 0,
            state: 0,
        }
    }

    fn bucket(&self) -> u32 {
        let len = self.len;
        if len == 0 {
            63 + self.misses
        } else if len < 16 {
            len
        } else {
            // Four buckets per power of two
            let log = 31 - len.leading_zeros();
            16 + (log - 4) * 4 + ((len >> (log - 2)) & 3)
        }
    }

    fn find_match(&mut self) {
        let pos = self.history.len();
        if pos < self.min_len {
            self.chain.push(0);
            return;
        }
        let mut hash: u32 = 0;
        for &byte in &self.history[pos - self.min_len..] {
            hash = (hash ^ byte as u32).wrapping_mul(0x2F0F3D1).wrapping_add(1);
        }
        let slot = (hash >> 8) as usize & ((1 << MATCH_HASH_BITS) - 1);
        if self.len == 0 {
            let mut candidate = self.head[slot] as usize;
            let mut best_len = 0;
            let mut best_ptr = 0;
            let mut first_byte = None;
            for _ in 0..self.candidates {
                if candidate == 0 {
                    break;
                }
                let mut len = 0;
                while len < MATCH_MAX_LEN as usize
                    && len < candidate
                    && self.history[candidate - 1 - len] == self.history[pos - 1 - len]
                {
                    len += 1;
                }
                if self.second {
                    if len >= self.min_len {
                        match first_byte {
                            None => first_byte = Some(self.history[candidate]),
                            Some(byte) if byte != self.history[candidate] => {
                                best_len = len;
                                best_ptr = candidate;
                                break;
                            }
                            _ => {}
                        }
                    }
                } else if len > best_len {
                    best_len = len;
                    best_ptr = candidate;
                }
                candidate = self.chain[candidate] as usize;
            }
            if best_len >= self.min_len {
                self.len = best_len as u32;
                self.match_ptr = best_ptr;
                self.active = true;
                self.misses = 0;
            }
        }
        // chain[pos] links position pos to the previous position with the same hash
        self.chain.push(self.head[slot]);
        self.head[slot] = pos as u32;
    }
}

impl Model for MatchModel {
    fn pred(&mut self) -> f64 {
        self.state = 0;
        if !self.active || self.byte_missed {
            return 0.;
        }
        let expected_byte = self.history[self.match_ptr];
        self.expected_bit = (expected_byte >> (self.partial_byte.leading_zeros() - 24)) & 1;
        let len_bucket = if self.len == 0 { 0 } else { (32 - self.len.leading_zeros()).min(12) };
        self.state = 1 + self.expected_bit as u32 + 2 * len_bucket;
        let context = if self.expected_byte_context {
            expected_byte as usize
        } else {
            0
        };
        self.counter_idx = (((self.bucket() as usize) << 8) + context << 8) + self.partial_byte as usize;
        let p = prob_stretch(self.counters[self.counter_idx].prob() as f64 / U24_MAX as f64);
        if self.expected_bit == 1 {
            p
        } else {
            -p
        }
    }

    fn state(&self) -> u32 {
        self.state
    }

    /// The prediction, and the expected bit as -1 or 1 times the length bucket of the state
    fn inputs(&mut self, out: &mut Vec<f64>) {
        let p = self.pred();
        out.push(p);
        let len_bucket = self.state.saturating_sub(1) / 2;
        out.push(if self.expected_bit == 1 {
            len_bucket as f64
        } else {
            -(len_bucket as f64)
        });
    }

    fn learn(&mut self, bit: u8) {
        if self.active && !self.byte_missed {
            // Learns how often the expected bit is right
            let inst = &mut self.counters[self.counter_idx];
            let count = (inst.count() + 1).min(255);
            let hit = (bit == self.expected_bit) as u8;
            let mut prob = inst.prob();
            prob += (U24_MAX as f64 * ((hit as f64 - prob as f64 / U24_MAX as f64) / (count as f64 + 0.2)))
                as i32;
            inst.set_count(count);
            inst.set_prob(prob);
            if hit == 0 {
                self.len = 0;
                self.misses += 1;
                if self.misses < MATCH_MAX_MISSES {
                    self.byte_missed = true;
                } else {
                    self.active = false;
                }
            }
        }
        self.partial_byte = (self.partial_byte << 1) | bit as u32;
        if self.partial_byte >= 256 {
            self.history.push(self.partial_byte as u8);
            self.partial_byte = 1;
            self.byte_missed = false;
            if self.active {
                if self.len != 0 {
                    self.len = (self.len + 1).min(MATCH_MAX_LEN);
                }
                self.match_ptr += 1;
            }
            self.find_match();
        }
    }
}

pub struct LnMixerPred {
    pub models: Vec<Box<dyn Model>>,
    /// Inputs of the last `pred`, and the index of the first input of each model in them
    last_p: Vec<f64>,
    first_input: Vec<usize>,
    /// Weights independent of the context, one per input
    model_weights: Vec<f64>,
    weights: Vec<Vec<Vec<f64>>>,
    prev_byte: u32,
    bit_ctx: u32,
    last_total_p: f64,
    learning_rate: f64,
    context_learning_rate: f64,
    context_weight_scale: f64,
}

impl LnMixerPred {
    pub fn new(
        models: Vec<Box<dyn Model>>,
        learning_rate: f64,
        context_learning_rate: f64,
        context_weight_scale: f64,
    ) -> Self {
        Self {
            first_input: vec![0; models.len()],
            last_p: vec![],
            model_weights: vec![],
            last_total_p: 0.,
            models,
            weights: vec![vec![vec![]; 255]; 256],
            bit_ctx: 1,
            prev_byte: 0,
            learning_rate,
            context_learning_rate,
            context_weight_scale,
        }
    }
}

impl Model for LnMixerPred {
    fn pred(&mut self) -> f64 {
        self.last_p.clear();
        for (i, model) in self.models.iter_mut().enumerate() {
            self.first_input[i] = self.last_p.len();
            model.pred_inputs(&mut self.last_p);
        }
        if self.model_weights.is_empty() {
            // Default weight, adjusted by learning later
            self.model_weights = vec![1. / self.last_p.len() as f64; self.last_p.len()];
        }

        let weights = &self.weights[self.prev_byte as usize][self.bit_ctx as usize - 1];
        let mut sum = 0.;
        for (i, p) in self.last_p.iter().enumerate() {
            let weight = if weights.is_empty() {
                // We don't have 1-order weight so just use the context independent weight
                self.model_weights[i]
            } else {
                // Mix in 1-order weight with context independent weight, without fused
                // multiply-add, as the JS decoder can't fuse
                weights[i] * self.context_weight_scale + self.model_weights[i]
            };
            sum += p * weight;
        }

        self.last_total_p = prob_squash(sum);
        sum
    }

    fn learn(&mut self, bit: u8) {
        let weights = &mut self.weights[self.prev_byte as usize][self.bit_ctx as usize - 1];

        if weights.is_empty() {
            weights.extend_from_slice(&self.model_weights);
        }

        let pred_err = bit as f64 - self.last_total_p;

        for model in &mut self.models {
            model.learn(bit);
        }
        for (i, p) in self.last_p.iter().enumerate() {
            self.model_weights[i] += self.learning_rate * pred_err * p;
            weights[i] += self.context_learning_rate * pred_err * p;
        }

        self.bit_ctx = (self.bit_ctx << 1) | bit as u32;

        if self.bit_ctx >= 256 {
            self.bit_ctx &= 0xff;
            self.prev_byte = self.bit_ctx;
            self.bit_ctx = 1;
        }
    }

    fn submodel_preds(&self, out: &mut Vec<f32>) {
        for (model, &first) in self.models.iter().zip(&self.first_input) {
            out.push(self.last_p[first] as f32);
            model.submodel_preds(out);
        }
    }
}
