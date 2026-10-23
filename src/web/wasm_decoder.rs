//! The WebAssembly decoder: a module generated for the model config, like the JS decoder, that
//! computes bit for bit the same predictions. It mirrors the JS decoder in `js_source` (and so the
//! models in `compressor`), so keep them in step.
//!
//! The module is written as WebAssembly text and assembled with the `wat` crate, so packing needs
//! no other tools. It holds only the models of the config, with their parameters as constants.
//! The models share one memory with a static layout, which the generator computes from the
//! decoded length: the compressed stream, the decoded bytes (which are also the history that the
//! models read), the mixer inputs and the tables of the models. Weight sets of the mixer are
//! allocated after it as they are first used, as in the JS decoder.
//!
//! Floating point: WebAssembly has no fused multiply-add and rounds every f64 operation as IEEE 754
//! says, as JavaScript does, so the same operations in the same order give the same bits. Where
//! the JS decoder computes something in another way than this module, the comment says why both
//! give the same result.

use std::fmt::Write as _;

use anyhow::{bail, ensure, Context, Result};

use crate::compressor::compress_config::{ModelConfig, RecordConfig, StaticModelParams, StreamLayout};
use crate::compressor::mixer::{ApmConfig, ApmContext, Selector};
use crate::compressor::records::BLOCK;
use crate::compressor::utils::prob_stretch;

/// Options of the generated module
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmOptions {
    /// Use 128-bit SIMD (simd128) for the mixer
    pub simd: bool,
    /// Also export `p`, `q` and `l`, the prediction, its squash and the learning, for tests
    pub test_exports: bool,
}

impl Default for WasmOptions {
    fn default() -> Self {
        Self {
            simd: true,
            test_exports: false,
        }
    }
}

/// A generated decoder module and where the glue puts the stream and finds the decoded bytes.
///
/// The module exports its memory as `m` and `d(n)`, which decodes up to `n` more bits (unsigned)
/// and returns the number of bytes decoded so far. Before the first call, the compressed stream
/// goes to `input` in the memory; the decoded bytes are at `output`.
pub struct WasmDecoder {
    pub bytes: Vec<u8>,
    pub input: u32,
    pub output: u32,
}

const U24_MAX: u32 = 0xffffff;

/// Bytes of the record of a context model, see `counters` below
const CM_SIZE: u32 = 128;

/// The kinds of context models, whose `next` function computes the context of each byte
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    NOrderByte,
    Word,
    CharClass,
    IndirectByte,
    TokenPosition,
    JsContext,
    WasmContext,
    Record,
}

impl Kind {
    fn func(self) -> &'static str {
        match self {
            Kind::NOrderByte => "$next_norder",
            Kind::Word => "$next_word",
            Kind::CharClass => "$next_char_class",
            Kind::IndirectByte => "$next_indirect",
            Kind::TokenPosition => "$next_token",
            Kind::JsContext => "$next_js",
            Kind::WasmContext => "$next_wasm",
            Kind::Record => "$next_record",
        }
    }
}

/// A model of the mixer
enum Leaf {
    /// A context model of the shared counters, with its kind and the code that creates it
    Context(Kind, String),
    Match {
        min_len: u32,
        candidates: u32,
        flags: u32,
    },
    MatchPredictor {
        context_bytes: u32,
        confidence: f64,
        table_bits: u32,
    },
}

/// Allocates the static memory
struct Memory {
    end: u64,
}

impl Memory {
    fn alloc(&mut self, size: u64) -> u32 {
        // 16-byte aligned, for SIMD loads
        let at = (self.end + 15) & !15;
        self.end = at + size;
        at as u32
    }
}

/// A f64 constant as WebAssembly text, with all its bits
fn f64c(value: f64) -> String {
    format!("(f64.const {})", hexf(value))
}

fn hexf(value: f64) -> String {
    // The text format takes hexadecimal floats, which are exact
    if value == 0. {
        return if value.is_sign_negative() { "-0x0p+0".into() } else { "0x0p+0".into() };
    }
    let bits = value.to_bits();
    let sign = if bits >> 63 != 0 { "-" } else { "" };
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = bits & ((1 << 52) - 1);
    assert!(exp != 0 && exp != 0x7ff, "no subnormal, infinite or NaN constants");
    format!("{sign}0x1.{mantissa:013x}p{}", exp - 1023)
}

/// The hash of the Rust encoder, as `hash` in utils.js
fn hash(value: u32, shift: u32) -> u32 {
    (value ^ value >> shift).wrapping_mul(0x9E35A7BD) >> shift
}

/// The generated module for a model config, with stream models already added
/// (`CompressConfig::with_stream_models`). `encoded_len` and `decoded_len` are the lengths of
/// the compressed stream and of the decoded bytes.
pub fn generate(
    model_config: &ModelConfig,
    params: &StaticModelParams,
    layout: &StreamLayout,
    encoded_len: usize,
    decoded_len: usize,
    options: WasmOptions,
) -> Result<WasmDecoder> {
    let text = generate_text(model_config, params, layout, encoded_len, decoded_len, options)?;
    let bytes = wat::parse_str(&text.text).map_err(|e| {
        anyhow::anyhow!("Failed to assemble the WebAssembly decoder: {e}\n{}", numbered(&text.text, &e))
    })?;
    Ok(WasmDecoder {
        bytes: strip_custom_sections(&bytes)?,
        input: text.input,
        output: text.output,
    })
}

/// The lines of `text` around the error, for a generator bug
fn numbered(text: &str, error: &wat::Error) -> String {
    // The message names the place as <anon>:line:column
    let message = error.to_string();
    let line = message
        .split("<anon>:")
        .nth(1)
        .and_then(|rest| rest.split(':').next())
        .and_then(|line| line.parse::<usize>().ok())
        .unwrap_or(0);
    text.lines()
        .enumerate()
        .skip(line.saturating_sub(5))
        .take(10)
        .map(|(i, l)| format!("{:5} {l}\n", i + 1))
        .collect()
}

/// The module without its custom sections (the names that `wat` writes)
fn strip_custom_sections(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = bytes[..8].to_vec();
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let payload = payload?;
        if let wasmparser::Payload::CustomSection(_) = payload {
            continue;
        }
        if let Some((id, range)) = payload.as_section() {
            // The section header: its id and the length of its contents
            out.push(id);
            let range = range.start as usize..range.end as usize;
            let mut len = range.len() as u32;
            loop {
                let byte = (len & 127) as u8;
                len >>= 7;
                out.push(byte | if len > 0 { 128 } else { 0 });
                if len == 0 {
                    break;
                }
            }
            out.extend_from_slice(&bytes[range]);
        }
    }
    Ok(out)
}

pub(crate) struct ModuleText {
    pub text: String,
    input: u32,
    output: u32,
}

struct Gen<'a> {
    params: &'a StaticModelParams,
    layout: &'a StreamLayout,
    options: WasmOptions,
    decoded_len: u32,
    mem: Memory,
    leaves: Vec<Leaf>,
    /// The kinds of context models used, in the order of their table entries
    kinds: Vec<Kind>,
    /// Data segments: address and bytes
    data: Vec<(u32, Vec<u8>)>,
    js_variants: Vec<u32>,
}

/// Mixer of the config
enum Mix<'a> {
    Context {
        selectors: &'a [Selector],
        learning_rate: f64,
        final_learning_rate: f64,
        init_weight: f64,
        lr_boost: f64,
        apms: &'a [ApmConfig],
    },
    Plain,
}

pub(crate) fn generate_text(
    model_config: &ModelConfig,
    params: &StaticModelParams,
    layout: &StreamLayout,
    encoded_len: usize,
    decoded_len: usize,
    options: WasmOptions,
) -> Result<ModuleText> {
    let (models, mix) = match model_config {
        ModelConfig::ContextMixer {
            models,
            selectors,
            learning_rate,
            final_learning_rate,
            init_weight,
            lr_boost,
            apms,
        } => (
            models,
            Mix::Context {
                selectors,
                learning_rate: *learning_rate,
                final_learning_rate: *final_learning_rate,
                init_weight: *init_weight,
                lr_boost: *lr_boost,
                apms,
            },
        ),
        ModelConfig::Mixer { models } => (models, Mix::Plain),
        _ => bail!("The WebAssembly decoder needs a Mixer or ContextMixer as the outermost model"),
    };
    let decoded_len = u32::try_from(decoded_len).context("The input is too large")?;
    let mut gen = Gen {
        params,
        layout,
        options,
        decoded_len,
        mem: Memory { end: 16 },
        leaves: Vec::new(),
        kinds: Vec::new(),
        data: Vec::new(),
        js_variants: Vec::new(),
    };
    for model in models {
        gen.add_leaf(model)?;
    }
    gen.module(&mix, encoded_len as u32)
}

impl<'a> Gen<'a> {
    fn kind_index(&mut self, kind: Kind) -> usize {
        match self.kinds.iter().position(|&k| k == kind) {
            Some(i) => i,
            None => {
                self.kinds.push(kind);
                self.kinds.len() - 1
            }
        }
    }

    /// Code that creates a context model: `$cm(next, max_count, a, active)` and the further
    /// fields that `extra` stores in the record
    fn context(&mut self, kind: Kind, max_count: u32, a: u32, active: bool, extra: &str) -> Leaf {
        ensure_count(max_count);
        let next = self.kind_index(kind);
        let create = format!(
            "(call $cm (i32.const {next}) (i32.const {max_count}) (i32.const {}) (i32.const {}))",
            a as i32, active as u32
        );
        Leaf::Context(
            kind,
            if extra.is_empty() {
                create
            } else {
                format!("(local.set $r (global.get $cr)) {create} {extra}")
            },
        )
    }

    fn add_leaf(&mut self, model: &ModelConfig) -> Result<()> {
        let p = self.params;
        let leaf = match model {
            ModelConfig::NOrderByte {
                byte_mask,
                max_count,
            } => {
                let mask = u8::from_str_radix(byte_mask.trim_start_matches("0b"), 2).unwrap_or(0);
                self.context(Kind::NOrderByte, max_count.unwrap_or(15), mask as u32, true, "")
            }
            ModelConfig::Word => self.context(Kind::Word, p.word_max_count, 2166136261, true, ""),
            ModelConfig::CharClass { order, max_count } => {
                self.context(Kind::CharClass, max_count.unwrap_or(15), *order as u32, true, "")
            }
            ModelConfig::IndirectByte {
                context_bytes,
                max_count,
            } => {
                ensure!(*context_bytes <= 3, "IndirectByte context_bytes above 3");
                let table = self.mem.alloc(2 << (8 * context_bytes));
                self.context(
                    Kind::IndirectByte,
                    *max_count,
                    *context_bytes as u32,
                    true,
                    &format!("(i32.store offset=64 (local.get $r) (i32.const {table}))"),
                )
            }
            ModelConfig::TokenPosition => self.context(Kind::TokenPosition, p.token_max_count, 0, true, ""),
            ModelConfig::JsContext { variant, max_count } => {
                self.js_variants.push(*variant as u32);
                self.context(Kind::JsContext, max_count.unwrap_or(15), *variant as u32, false, "")
            }
            ModelConfig::WasmContext { fields, max_count } => {
                // The first one runs the parser, on all modules
                self.context(Kind::WasmContext, max_count.unwrap_or(15), *fields, false, "")
            }
            ModelConfig::Record(config) => self.record(config)?,
            ModelConfig::Match {
                min_len,
                candidates,
                expected_byte_context,
                second,
            } => {
                ensure!(*min_len >= 1, "Match min_len must be at least 1");
                Leaf::Match {
                    min_len: *min_len,
                    candidates: *candidates,
                    flags: *expected_byte_context as u32 | (*second as u32) << 1,
                }
            }
            ModelConfig::MatchPredictor {
                context_bytes,
                confidence,
                table_bits,
            } => Leaf::MatchPredictor {
                context_bytes: *context_bytes as u32,
                confidence: *confidence,
                table_bits: *table_bits,
            },
            ModelConfig::Mixer { .. } | ModelConfig::ContextMixer { .. } => {
                bail!("The WebAssembly decoder does not support mixers within mixers")
            }
            ModelConfig::AdaptiveProbabilityMap(_) => {
                bail!("The decoder has no AdaptiveProbabilityMap; use the apms of a ContextMixer")
            }
        };
        self.leaves.push(leaf);
        Ok(())
    }

    fn record(&mut self, config: &RecordConfig) -> Result<Leaf> {
        // The parameters as data: column, delta, the number of dists and the dists
        ensure!(config.dists.len() < 256, "Too many record dists");
        let mut params = vec![config.column as u8, config.delta as i8 as u8, config.dists.len() as u8];
        for &d in &config.dists {
            ensure!((-128..128).contains(&d), "Record dists must be -128 to 127 for the WebAssembly decoder");
            params.push(d as i8 as u8);
        }
        let at = self.mem.alloc(params.len() as u64);
        self.data.push((at, params));
        let max_count = config.max_count.unwrap_or(15);
        ensure_count(max_count);
        self.kind_index(Kind::Record);
        Ok(Leaf::Context(Kind::Record, format!("(call $record_new (i32.const {max_count}) (i32.const {at}))")))
    }
}

fn ensure_count(max_count: u32) {
    assert!(max_count <= 255, "max_count must be at most 255");
}

impl<'a> Gen<'a> {
    fn module(mut self, mix: &Mix, encoded_len: u32) -> Result<ModuleText> {
        let context = matches!(mix, Mix::Context { .. });
        let decoded_len = self.decoded_len;
        let input = self.mem.alloc(encoded_len as u64 + 8);
        let output = self.mem.alloc(decoded_len as u64 + 8);

        // The mixer inputs: 5 for each context model, 2 (1 with a plain Mixer) for each match model,
        // 1 for each match predictor, and the bias of the ContextMixer
        let n_inputs: u32 = self
            .leaves
            .iter()
            .map(|leaf| match leaf {
                Leaf::Context(..) => 5,
                Leaf::Match { .. } => 1 + context as u32,
                Leaf::MatchPredictor { .. } => 1,
            })
            .sum::<u32>()
            + context as u32;
        // One more, as SIMD takes the inputs in pairs
        let inputs = self.mem.alloc(8 * n_inputs as u64 + 8);
        // The state of each model for the selectors and APMs
        let states = self.mem.alloc(4 * self.leaves.len() as u64);

        let n_cm = self.leaves.iter().filter(|l| matches!(l, Leaf::Context(..))).count() as u32;
        let cm0 = self.mem.alloc((CM_SIZE * n_cm) as u64);
        let cm_states = self.mem.alloc((n_cm as u64) << 18);
        let n_match = self.leaves.iter().filter(|l| matches!(l, Leaf::Match { .. })).count() as u32;
        let n_mp = self.leaves.iter().filter(|l| matches!(l, Leaf::MatchPredictor { .. })).count() as u32;

        let mut text = String::new();
        let mut init = String::new();
        let mut funcs = String::new();
        let mut globals = String::new();

        let has = |k: Kind| self.kinds.contains(&k);
        let char_table = has(Kind::Word) || has(Kind::CharClass) || has(Kind::TokenPosition) || has(Kind::JsContext);
        let char_classes = if char_table {
            let at = self.mem.alloc(256);
            self.data.push((at, char_class_table()));
            at
        } else {
            0
        };

        // Shared state of the JS lexer: its stack of frames, and the map of the token after each
        // token if a model of variant 1 uses it
        let js = has(Kind::JsContext);
        let next_token = self.js_variants.contains(&1);
        let (frames, map_bits, map) = if js {
            let frames = self.mem.alloc(8 * (decoded_len as u64 + 2));
            let map_bits = (2 * decoded_len as u64 + 2).next_power_of_two().trailing_zeros().max(4);
            let map = if next_token { self.mem.alloc(12 << map_bits) } else { 0 };
            (frames, map_bits, map)
        } else {
            (0, 0, 0)
        };

        // The parser of WebAssembly modules: its stack, the last byte of each kind and position, the
        // last two values of each kind, the values of the fields, the immediates of each opcode
        // (from wasm.js) and the module ranges
        let wasm_parser = if has(Kind::WasmContext) {
            ensure!(!self.layout.wasm.is_empty(), "WasmContext is only used with WebAssembly modules");
            let stack = self.mem.alloc(64 * 8);
            let column = self.mem.alloc(64 * 32);
            let last = self.mem.alloc(64 * 4);
            let last2 = self.mem.alloc(64 * 4);
            let values = self.mem.alloc(14 * 4);
            let imm = self.mem.alloc(256);
            let js = include_str!("js_source/wasm.js");
            let table = js.split("let imm = \"").nth(1).and_then(|rest| rest.split('"').next()).context("The immediates in wasm.js")?;
            self.data.push((imm, table.bytes().map(|c| c - 48).collect()));
            let ranges: Vec<u8> = self.layout.wasm.iter().flat_map(|&(start, end)| [start.to_le_bytes(), end.to_le_bytes()]).flatten().collect();
            let at = self.mem.alloc(ranges.len() as u64);
            let end = at + ranges.len() as u32;
            self.data.push((at, ranges));
            [stack, column, last, last2, values, imm, at, end]
        } else {
            [0; 8]
        };

        // Record ranges, as gap, length and stride
        let records = if has(Kind::Record) {
            let mut end = 0;
            let mut bytes = Vec::new();
            for &[start, range_end, stride] in &self.layout.records {
                assert!(start % BLOCK == 0 && range_end % BLOCK == 0);
                let gap = start - std::mem::replace(&mut end, range_end);
                for v in [gap / BLOCK, (range_end - start) / BLOCK, stride] {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
            let at = self.mem.alloc(bytes.len() as u64);
            let len = bytes.len() as u32;
            self.data.push((at, bytes));
            (at, len)
        } else {
            (0, 0)
        };

        // Match models: a record of fields each, and their tables
        const MM_SIZE: u32 = 64;
        let mm0 = self.mem.alloc((MM_SIZE * n_match) as u64);
        // Their tables, which $mm_new takes in turn: the head, the chain, the probabilities and the
        // counts
        let mm_chain = (4 * (decoded_len + 2) + 15) & !15;
        let mm_region = self.mem.alloc(n_match as u64 * ((4 << 22) + mm_chain as u64 + 5 * (72 << 16)));
        const MP_SIZE: u32 = 48;
        let mp0 = self.mem.alloc((MP_SIZE * n_mp) as u64);
        let mut mp_tables = Vec::new();
        for leaf in &self.leaves {
            if let Leaf::MatchPredictor { table_bits, .. } = leaf {
                ensure!(*table_bits <= 24, "MatchPredictor table_bits above 24");
                // The keys, followed by the positions
                let keys = self.mem.alloc(8 << table_bits);
                mp_tables.push((keys, (1u32 << table_bits) - 1));
            }
        }

        // Create the models in order
        let mut mp_i = 0;
        for leaf in &self.leaves {
            match leaf {
                Leaf::Context(_, create) => init += create,
                Leaf::Match {
                    min_len,
                    candidates,
                    flags,
                } => {
                    write!(init, "(call $mm_new (i32.const {min_len}) (i32.const {candidates}) (i32.const {flags}))")?;
                }
                Leaf::MatchPredictor {
                    context_bytes,
                    confidence,
                    ..
                } => {
                    let (keys, table_mask) = mp_tables[mp_i];
                    let r = mp0 + MP_SIZE * mp_i as u32;
                    mp_i += 1;
                    let mask = if *context_bytes >= 4 { -1 } else { (1i32 << (8 * context_bytes)) - 1 };
                    write!(
                        init,
                        "(call $mp_new (i32.const {r}) (i32.const {mask}) (i32.const {keys}) (i32.const {table_mask}) {} {})",
                        f64c(prob_stretch(*confidence)),
                        f64c(prob_stretch(1. - confidence))
                    )?;
                }
            }
        }

        // APM tables
        let mut apm_tables = Vec::new();
        let mut apm_probs = (0, 0);
        if let Mix::Context { apms, .. } = mix {
            let mut sizes = Vec::new();
            for apm in apms.iter() {
                let contexts: u64 = match apm.ctx {
                    ApmContext::Order0 => 1 << 8,
                    ApmContext::Order1 => 1 << 16,
                    ApmContext::Order2 | ApmContext::Context(_) => {
                        ensure!((1..=24).contains(&apm.bits), "APM bits must be 1 to 24");
                        1 << (8 + apm.bits)
                    }
                    ApmContext::State(_) => 1 << 14,
                };
                sizes.push(17 * contexts);
            }
            // The probabilities of all, one after the other, as they start the same
            let total: u64 = sizes.iter().sum();
            let mut probs = self.mem.alloc(8 * total);
            apm_probs = (probs, 8 * total);
            for &len in &sizes {
                apm_tables.push((probs, self.mem.alloc(4 * len), len));
                probs += 8 * len as u32;
            }
        }
        if apm_probs.1 > 0 {
            // The buckets start at the probabilities of the stretched -8 to 8
            let (probs, len) = apm_probs;
            write!(
                init,
                "(local.set $r (i32.const 0)) (loop $bucket (f64.store (i32.add (i32.const {probs}) (i32.shl (local.get $r) (i32.const 3))) (call $squash (f64.convert_i32_s (i32.sub (local.get $r) (i32.const 8)))))
  (br_if $bucket (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 1))) (i32.const 17))))
(call $rep (i32.const {probs}) (i32.const 136) (i32.const {len}))\n"
            )?;
        }

        // Mixer weights: per selector a table of the weight set of each context, which the sets
        // are allocated from the heap for. A plain Mixer has a weight set per context.
        let mut set_tables = Vec::new();
        let mut plain = (0, 0, 0);
        match mix {
            Mix::Context { selectors, .. } => {
                for selector in selectors.iter() {
                    let contexts: u64 = match selector {
                        Selector::None => 1,
                        Selector::Order1 | Selector::State(_) => 1 << 16,
                        Selector::Known => 8 * (self.leaves.len() as u64 + 1),
                        Selector::Context(m) => {
                            if self.is_js(*m) {
                                1 << 22
                            } else {
                                1 << 8
                            }
                        }
                    };
                    set_tables.push(self.mem.alloc(4 * contexts));
                }
            }
            Mix::Plain => {
                let weights = self.mem.alloc(8 * n_inputs as u64);
                let present = self.mem.alloc(1 << 16);
                let context_weights = self.mem.alloc((8 * n_inputs as u64) << 16);
                plain = (weights, present, context_weights);
            }
        }

        // The counters last, as they are the largest
        let counter_bits = self.params.hash_table_pow2_size;
        ensure!((4..=28).contains(&counter_bits), "hash_table_pow2_size must be 4 to 28");
        let counters = self.mem.alloc(8 << counter_bits);
        let heap = (self.mem.end + 65535) & !65535;
        ensure!(heap < 1 << 32, "The decoder needs more than 4 GB of memory");
        let pages = heap >> 16;

        let scale = self.params.context_input_scale;
        let consts = [
            ("U24F", f64c(U24_MAX as f64)),
            ("IN0", f64c(0.5 * scale)),
            ("IN1", f64c(3. * scale)),
            ("IN2", f64c(0.4 * scale)),
            ("IN3", f64c(0.25 * scale)),
            ("IN4", f64c(1. * scale)),
            ("OFFSET", f64c(self.params.count_offset)),
            ("EPS", f64c(1. / U24_MAX as f64)),
            ("EPS1", f64c(1. - 1. / U24_MAX as f64)),
            ("LN2", f64c(std::f64::consts::LN_2)),
        ];
        let fill = |s: &str| -> String {
            let simd = self.options.simd;
            let mut s = s.replace("{STRETCHES}", if simd { STRETCHES_SIMD } else { STRETCHES })
                .replace("{CM_STATE}", if context { CM_STATE } else { "" }).replace("{MM_INPUT}", if context { MM_INPUT } else { "" });
            for (name, value) in &consts {
                s = s.replace(&format!("{{{name}}}"), value);
            }
            s.replace("{COUNTERS}", &counters.to_string())
                .replace("{SHIFT}", &(32 - counter_bits).to_string())
                .replace("{CM0}", &cm0.to_string())
                .replace("{CMEND}", &(cm0 + CM_SIZE * n_cm).to_string())
                .replace("{CMSTATES}", &cm_states.to_string())
                .replace("{INPUTS}", &inputs.to_string())
                .replace("{STATES}", &states.to_string())
                .replace("{OUT}", &output.to_string())
                .replace("{IN}", &input.to_string())
                .replace("{DLEN}", &decoded_len.to_string())
                .replace("{CHARS}", &char_classes.to_string())
                .replace("{FRAMES}", &frames.to_string())
                .replace("{MAP}", &map.to_string())
                .replace("{MAPSHIFT}", &(32 - map_bits).to_string())
                .replace("{MAPMASK}", &((1u32 << map_bits) - 1).to_string())
                .replace("{RECORDS}", &records.0.to_string())
                .replace("{RECORDS_END}", &(records.0 + records.1).to_string())
                .replace("{MM0}", &mm0.to_string())
                .replace("{MMEND}", &(mm0 + MM_SIZE * n_match).to_string())
                .replace("{MP0}", &mp0.to_string())
                .replace("{MPEND}", &(mp0 + MP_SIZE * n_mp).to_string())
                .replace("{MATCH_STRIDE}", &(8 + 8 * context as u32).to_string())
                .replace("{T_RECORD}", &self.kinds.iter().position(|&k| k == Kind::Record).unwrap_or(0).to_string())
                .replace("{MM_CHAIN}", &mm_chain.to_string())
                .replace("{MM_REGION}", &mm_region.to_string())
                .replace("{TOKEN_MAX}", &self.params.token_max_position.to_string())
                .replace("{TOKEN_MASK}", match self.params.token_context_bytes { 0 => "0", 1 => "255", _ => "65535" })
                .replace("{H1337}", &(hash(1337, 2) as i32).to_string())
                .replace("{H9191}", &(hash(9191, 2) as i32).to_string())
                .replace("{WSTACK4}", &(wasm_parser[0] + 4).to_string())
                .replace("{WSTACK}", &wasm_parser[0].to_string())
                .replace("{WCOL}", &wasm_parser[1].to_string())
                .replace("{WLAST2}", &wasm_parser[3].to_string())
                .replace("{WLAST}", &wasm_parser[2].to_string())
                .replace("{WVALUES}", &wasm_parser[4].to_string())
                .replace("{WIMM}", &wasm_parser[5].to_string())
                .replace("{WRANGES4}", &(wasm_parser[6] + 4).to_string())
                .replace("{WRANGES_END}", &wasm_parser[7].to_string())
                .replace("{WRANGES}", &wasm_parser[6].to_string())
        };

        // The core: coder, stretch and squash, memory fills
        funcs += &fill(CORE);
        if self.options.simd {
            funcs += &fill(CORE_SIMD);
        } else {
            funcs += &fill(CORE_SCALAR);
        }
        if n_cm > 0 {
            funcs += &fill(COUNTERS);
            let mut table = String::new();
            for kind in &self.kinds {
                table += kind.func();
                table += " ";
                funcs += &fill(match kind {
                    Kind::NOrderByte => NEXT_NORDER,
                    Kind::Word => NEXT_WORD,
                    Kind::CharClass => NEXT_CHAR_CLASS,
                    Kind::IndirectByte => NEXT_INDIRECT,
                    Kind::TokenPosition => NEXT_TOKEN,
                    Kind::JsContext => NEXT_JS,
                    Kind::WasmContext => NEXT_WASM,
                    Kind::Record => NEXT_RECORD,
                });
            }
            write!(
                text,
                "(type $next (func (param i32 i32)))\n(table {} funcref)\n(elem (i32.const 0) func {table})\n",
                self.kinds.len()
            )?;
        }
        if char_table {
            funcs += &fill(CHAR_CLASS);
        }
        if has(Kind::JsContext) {
            // The hash of each variant in use, chosen by field 56
            let mut variants: Vec<u32> = self.js_variants.clone();
            variants.sort();
            variants.dedup();
            let hash = |v: u32| match v {
                0 => "(call $js_mix (call $js_mix (call $js_mix (local.get $t) (i32.load (local.get $frame))) (i32.load offset=4 (local.get $frame))) (global.get $js_punct))",
                1 => "(call $js_mix (local.get $t) (call $map_get (global.get $js_prev)))",
                2 => "(call $js_mix (call $js_mix (local.get $t) (local.get $b)) (i32.or (i32.shl (i32.load (local.get $frame)) (i32.const 8)) (i32.add (global.get $js_depth) (i32.const 1))))",
                _ => "(call $js_mix (call $js_mix (local.get $t) (global.get $js_skel_lo)) (global.get $js_skel_hi))",
            };
            let mut chain = hash(*variants.last().unwrap()).to_owned();
            for &v in variants.iter().rev().skip(1) {
                chain = format!("(if (result i32) (i32.eq (local.get $v) (i32.const {v})) (then {}) (else {chain}))", hash(v));
            }
            funcs = funcs.replace("{JS_VARIANTS}", &chain);
            funcs += &fill(JS_LEXER);
            if next_token {
                funcs += &fill(JS_NEXT_TOKEN_MAP);
            } else {
                funcs += "(func $map_set (param i32 i32))\n";
            }
        }
        if n_match > 0 {
            funcs += &fill(MATCH);
        }
        if n_mp > 0 {
            funcs += &fill(MATCH_PREDICTOR);
        }
        if has(Kind::WasmContext) {
            funcs += &fill(WASM_PARSER);
        }

        // What happens at the end of each byte, before the models learn it
        let mut byte_end = String::new();
        if has(Kind::JsContext) {
            byte_end += "(call $lex (local.get $b))";
        }
        if has(Kind::WasmContext) {
            byte_end += "(call $wasm_update (local.get $b))";
            if self.layout.wasm.iter().any(|&(start, _)| start == 0) {
                init += "(call $wpush (i32.const 1) (i32.const 8))";
            }
        }

        // Predict and learn
        let mut predict = String::new();
        let mut learn_models = String::new();
        if n_cm > 0 {
            predict += "(call $cm_p)";
            learn_models += "(call $cm_l (local.get $bit))";
        }
        if n_match > 0 {
            predict += "(call $mm_p)";
            learn_models += "(call $mm_l (local.get $bit))";
        }
        if n_mp > 0 {
            predict += "(call $mp_p)";
            learn_models += "(call $mp_l (local.get $bit))";
        }

        match mix {
            Mix::Context {
                selectors,
                learning_rate,
                final_learning_rate,
                init_weight,
                lr_boost,
                apms,
            } => {
                write!(
                    init,
                    "(f64.store (i32.const {}) (f64.const 0.5))",
                    inputs + 8 * (n_inputs - 1)
                )?;
                funcs += &self.context_mixer(
                    selectors,
                    *learning_rate,
                    *final_learning_rate,
                    *init_weight,
                    *lr_boost,
                    apms,
                    &apm_tables,
                    &set_tables,
                    inputs,
                    n_inputs,
                    states,
                    &predict,
                    &learn_models,
                    &mut globals,
                )?;
            }
            Mix::Plain => {
                let (weights, present, context_weights) = plain;
                let mixer = &self.params.mixer;
                write!(
                    init,
                    "(f64.store (i32.const {weights}) {}) (call $rep (i32.const {weights}) (i32.const 8) (i32.const {}))",
                    f64c(1. / n_inputs as f64),
                    8 * n_inputs
                )?;
                funcs += &fill(PLAIN_MIXER)
                    .replace("{PREDICT}", &predict)
                    .replace("{LEARN_MODELS}", &learn_models)
                    .replace("{WEIGHTS}", &weights.to_string())
                    .replace("{PRESENT}", &present.to_string())
                    .replace("{CWEIGHTS}", &context_weights.to_string())
                    .replace("{N8}", &(8 * n_inputs).to_string())
                    .replace("{NSHIFT}", &n_inputs.to_string())
                    .replace("{LR}", &f64c(mixer.learning_rate))
                    .replace("{CLR}", &f64c(mixer.context_learning_rate))
                    .replace("{CSCALE}", &f64c(mixer.context_weight_scale));
            }
        }

        funcs += &fill(LEARN_BYTE).replace("{BYTE_END}", &byte_end);
        let test_exports = if self.options.test_exports {
            "(export \"p\" (func $p)) (export \"q\" (func $q)) (export \"l\" (func $l))"
        } else {
            ""
        };
        let data: String = self
            .data
            .iter()
            .map(|(at, bytes)| {
                let escaped: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
                format!("(data (i32.const {at}) \"{escaped}\")\n")
            })
            .collect();
        let module = format!(
            r#"(module
(memory (export "m") {pages})
{text}
(global $low (mut i32) (i32.const 0))
(global $high (mut i32) (i32.const -1))
(global $st (mut i32) (i32.const 0))
(global $ptr (mut i32) (i32.const {input}))
(global $pos (mut i32) (i32.const 0))
(global $bc (mut i32) (i32.const 1))
(global $bits (mut i32) (i32.const 0))
(global $H (mut i32) (i32.const 0))
(global $L (mut i32) (i32.const 0))
(global $heap (mut i32) (i32.const {heap}))
(global $cr (mut i32) (i32.const {cm0}))
(global $ip (mut i32) (i32.const {inputs}))
(global $sp (mut i32) (i32.const {cm_states}))
(global $mi (mut i32) (i32.const {states}))
(global $sqfd (mut f64) (f64.const 0))
{globals}
{funcs}
(func $init (local $r i32) {init})
(start $init)
(func (export "d") (param $n i32) (result i32)
  (if (i32.eq (global.get $ptr) (i32.const {input})) (then
    (global.set $st (i32.load (i32.const {input})))
    (global.set $st (i32.or (i32.or (i32.shl (global.get $st) (i32.const 24)) (i32.shl (i32.and (global.get $st) (i32.const 0xff00)) (i32.const 8)))
      (i32.or (i32.and (i32.shr_u (global.get $st) (i32.const 8)) (i32.const 0xff00)) (i32.shr_u (global.get $st) (i32.const 24)))))
    (global.set $ptr (i32.const {input4}))))
  (block $done (loop $next
    (br_if $done (i32.eq (global.get $pos) (i32.const {decoded_len})))
    (br_if $done (i32.eqz (local.get $n)))
    (call $l (call $code (call $q (call $p))))
    (local.set $n (i32.sub (local.get $n) (i32.const 1)))
    (br $next)))
  (global.get $pos))
{test_exports}
{data})"#,
            input4 = input + 4,
        );
        Ok(ModuleText {
            text: module,
            input,
            output,
        })
    }

    fn is_js(&self, model: usize) -> bool {
        matches!(self.leaves.get(model), Some(Leaf::Context(Kind::JsContext, _)))
    }

    /// The context of a model for a selector or APM, `Model::context`: the one of the lexer for
    /// a JsContext, else 0
    fn model_context(&self, model: usize) -> &'static str {
        if self.is_js(model) {
            "(call $js_context)"
        } else {
            "(i32.const 0)"
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn context_mixer(
        &self,
        selectors: &[Selector],
        learning_rate: f64,
        final_learning_rate: f64,
        init_weight: f64,
        lr_boost: f64,
        apms: &[ApmConfig],
        apm_tables: &[(u32, u32, u64)],
        set_tables: &[u32],
        inputs: u32,
        n_inputs: u32,
        states: u32,
        predict: &str,
        learn_models: &str,
        globals: &mut String,
    ) -> Result<String> {
        let k = selectors.len();
        ensure!(k > 0, "A ContextMixer needs a selector");
        let mut f = String::new();
        let n8 = 8 * n_inputs;
        // With SIMD, the weights are taken in pairs, and the inputs end with a 0 for an odd number
        let n8 = if self.options.simd { (n8 + 15) & !15 } else { n8 };
        let set_size = 8 + n8;
        let state = |m: usize| format!("(i32.load (i32.const {}))", states + 4 * m as u32);

        for s in 0..k {
            writeln!(
                globals,
                "(global $w{s} (mut i32) (i32.const 0)) (global $d{s} (mut f64) (f64.const 0)) (global $f{s} (mut f64) {})",
                f64c(1. / k as f64)
            )?;
        }
        for (j, _) in apms.iter().enumerate() {
            writeln!(globals, "(global $a{j} (mut i32) (i32.const 0))")?;
        }
        if !apms.is_empty() {
            globals.push_str("(global $af (mut f64) (f64.const 0))\n");
        }

        // A weight set of a context: allocated and filled with the initial weight when first used
        write!(
            f,
            r#"(func $set (param $at i32) (result i32) (local $w i32)
  (if (i32.eqz (local.tee $w (i32.load (local.get $at)))) (then
    (local.set $w (global.get $heap))
    (global.set $heap (i32.add (local.get $w) (i32.const {set_size})))
    (if (i32.gt_u (global.get $heap) (i32.shl (memory.size) (i32.const 16)))
      (then (drop (memory.grow (i32.const 256)))))
    (i32.store (local.get $at) (local.get $w))
    (f64.store offset=8 (local.get $w) {init_weight})
    (call $rep (i32.add (local.get $w) (i32.const 8)) (i32.const 8) (i32.const {n8}))))
  (local.get $w))
"#,
            init_weight = f64c(init_weight),
        )?;

        // Predict
        let mut p = String::from("(func $p (result f64) (local $i i32) (local $x f64) (local $known i32) (local $st f64) (local $b f64) (local $v v128) (local $u v128)\n");
        for s in 0..k {
            write!(p, "(local $s{s} f64) (local $p{s} i32)")?;
        }
        p += predict;
        p += "\n";
        if selectors.contains(&Selector::Known) {
            // The number of models that know their context
            write!(
                p,
                "(local.set $i (i32.const {states})) (loop $known
  (local.set $known (i32.add (local.get $known) (i32.ne (i32.load (local.get $i)) (i32.const 0))))
  (br_if $known (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 4))) (i32.const {}))))\n",
                states + 4 * self.leaves.len() as u32
            )?;
        }
        for (s, selector) in selectors.iter().enumerate() {
            let ctx = match selector {
                Selector::None => "(i32.const 0)".to_owned(),
                Selector::Order1 => {
                    "(i32.or (i32.shl (i32.and (global.get $L) (i32.const 255)) (i32.const 8)) (global.get $bc))".to_owned()
                }
                Selector::Known => {
                    "(i32.add (i32.shl (local.get $known) (i32.const 3)) (global.get $bits))".to_owned()
                }
                Selector::Context(m) => format!(
                    "(i32.or (i32.shl {} (i32.const 8)) (global.get $bc))",
                    self.model_context(*m)
                ),
                Selector::State(m) => format!(
                    "(i32.or (i32.shl (select (local.tee $i {}) (i32.const 255) (i32.lt_u (local.get $i) (i32.const 255))) (i32.const 8)) (global.get $bc))",
                    state(*m)
                ),
            };
            writeln!(
                p,
                "(global.set $w{s} (call $set (i32.add (i32.const {}) (i32.shl {ctx} (i32.const 2)))))",
                set_tables[s]
            )?;
        }
        // The dot products of the inputs with all weight sets, each summed from the first input
        p += &self.dot_products(k, inputs, n8);
        // Clamped to [-40, 40], and mixed by the final weights
        p += "(local.set $x (f64.const 0))\n";
        for s in 0..k {
            write!(
                p,
                "(global.set $d{s} (f64.min (f64.max (local.get $s{s}) (f64.const -40)) (f64.const 40)))
(local.set $x (f64.add (local.get $x) (f64.mul (global.get $d{s}) (global.get $f{s}))))\n"
            )?;
        }
        if apms.is_empty() {
            p += "(local.get $x))\n";
            // The squash of the final dot product, which learning takes, is that of the prediction
            f += "(func $q (param f64) (result f64) (global.set $sqfd (call $squash (local.get 0))) (global.get $sqfd))\n";
        } else {
            // The APMs interpolate between the buckets of the final dot product, with the same weights
            write!(
                p,
                r#"(global.set $sqfd (local.tee $st (call $squash (local.get $x))))
(local.set $b (f64.floor (local.tee $x (f64.add (f64.min (f64.max (local.get $x) (f64.const -8)) {max}) (f64.const 8)))))
(global.set $af (f64.sub (local.get $x) (local.get $b)))
(local.set $i (i32.trunc_f64_s (local.get $b)))
"#,
                max = f64c(7.999)
            )?;
            for (j, apm) in apms.iter().enumerate() {
                let ctx = match apm.ctx {
                    ApmContext::Order0 => "(i32.const 0)".to_owned(),
                    ApmContext::Order1 => "(i32.and (global.get $L) (i32.const 255))".to_owned(),
                    ApmContext::Order2 => format!(
                        "(i32.shr_u (i32.mul (i32.and (global.get $L) (i32.const 65535)) (i32.const 0x9E35A7BD)) (i32.const {}))",
                        32 - apm.bits
                    ),
                    ApmContext::State(m) => format!(
                        "(select (local.tee $known {}) (i32.const 63) (i32.lt_u (local.get $known) (i32.const 63)))",
                        state(m)
                    ),
                    ApmContext::Context(m) => format!(
                        "(i32.shr_u (i32.mul {} (i32.const 0x9E35A7BD)) (i32.const {}))",
                        self.model_context(m),
                        32 - apm.bits
                    ),
                };
                let (probs, _, _) = apm_tables[j];
                // The address of the first bucket: probs + ((ctx << 8 | bitCtx) * 17 + bucket) * 8
                write!(
                    p,
                    "(global.set $a{j} (i32.add (i32.const {probs}) (i32.shl (i32.add (i32.mul (i32.or (i32.shl {ctx} (i32.const 8)) (global.get $bc)) (i32.const 17)) (local.get $i)) (i32.const 3))))
(local.set $st (f64.add (local.get $st) (f64.mul (f64.sub (call $apm (global.get $a{j})) (global.get $sqfd)) {})))\n",
                    f64c(apm.weight)
                )?;
            }
            p += "(call $stretch (local.get $st)))\n";
            f += r#"(func $apm (param $a i32) (result f64)
  (f64.add (f64.mul (f64.load (local.get $a)) (f64.sub (f64.const 1) (global.get $af))) (f64.mul (f64.load offset=8 (local.get $a)) (global.get $af))))
(func $apm_l (param $a i32) (param $n i32) (param $w f64) (param $bit f64) (param $limit i32) (local $c i32) (local $v f64)
  (i32.store (local.get $n) (local.tee $c (select (local.tee $c (i32.add (i32.load (local.get $n)) (i32.const 1))) (local.get $limit) (i32.lt_u (local.get $c) (local.get $limit)))))
  (f64.store (local.get $a) (f64.add (local.tee $v (f64.load (local.get $a))) (f64.div (f64.mul (f64.sub (local.get $bit) (local.get $v)) (local.get $w)) (f64.add (f64.convert_i32_u (local.get $c)) (f64.const 0.5))))))
(func $q (param f64) (result f64) (call $squash (local.get 0)))
"#;
            for (j, apm) in apms.iter().enumerate() {
                let _ = (apm, apm_tables[j]);
            }

        }
        f += &p;

        // Learn
        let mut l = String::from("(func $l (param $bit i32) (local $i i32) (local $x f64) (local $fb f64) (local $n i32) (local $v v128) (local $u v128)\n");
        for s in 0..k {
            write!(l, "(local $e{s} f64) (local $p{s} i32) (local $v{s} v128)")?;
        }
        l += "(local.set $fb (f64.convert_i32_u (local.get $bit)))\n";
        // The squashed dot products, two at once with SIMD
        for s in 0..k {
            if self.options.simd && s % 2 == 0 && s + 1 < k {
                write!(l, "(local.set $v (call $squash2 (f64x2.replace_lane 1 (f64x2.splat (global.get $d{s})) (global.get $d{}))))
(local.set $e{s} (f64x2.extract_lane 0 (local.get $v))) (local.set $e{} (f64x2.extract_lane 1 (local.get $v)))\n", s + 1, s + 1)?;
            } else if !(self.options.simd && s % 2 == 1) {
                write!(l, "(local.set $e{s} (call $squash (global.get $d{s})))\n")?;
            }
        }
        for s in 0..k {
            // err = (bit - squash(dot)) * (lr * (1 + lrBoost * 16 / (16 + count)))
            write!(
                l,
                "(i32.store (global.get $w{s}) (i32.add (local.tee $n (i32.load (global.get $w{s}))) (i32.const 1)))
(local.set $e{s} (f64.mul (f64.sub (local.get $fb) (local.get $e{s})) (f64.mul {lr} (f64.add (f64.const 1) (f64.div {boost} (f64.add (f64.const 16) (f64.convert_i32_u (local.get $n))))))))\n",
                lr = f64c(learning_rate),
                boost = if lr_boost * 16. == 0. { "(f64.const 0)".to_owned() } else { f64c(lr_boost * 16.) },
            )?;
        }
        l += &self.weight_updates(k, inputs, n8);
        write!(
            l,
            "(local.set $x (f64.mul (f64.sub (local.get $fb) (global.get $sqfd)) {}))\n",
            f64c(final_learning_rate)
        )?;
        for s in 0..k {
            write!(
                l,
                "(global.set $f{s} (f64.add (global.get $f{s}) (f64.mul (local.get $x) (global.get $d{s}))))\n"
            )?;
        }
        l += "(call $learn_byte (local.get $bit))\n";
        l += learn_models;
        for (j, apm) in apms.iter().enumerate() {
            let (probs, counts, _) = apm_tables[j];
            // The counts are at the same index as the probabilities
            write!(
                l,
                "(local.set $i (i32.add (i32.const {}) (i32.shr_u (i32.sub (global.get $a{j}) (i32.const {probs})) (i32.const 1))))
(call $apm_l (global.get $a{j}) (local.get $i) (f64.sub (f64.const 1) (global.get $af)) (local.get $fb) (i32.const {limit}))
(call $apm_l (i32.add (global.get $a{j}) (i32.const 8)) (i32.add (local.get $i) (i32.const 4)) (global.get $af) (local.get $fb) (i32.const {limit}))\n",
                counts,
                limit = apm.limit
            )?;
        }
        l += "(call $next_bit (local.get $bit)))\n";
        f += &l;
        Ok(f)
    }

    /// Code that sums the dot products of the inputs with the weight sets $w0.. into the locals
    /// $s0.., each in the order of the inputs
    fn dot_products(&self, k: usize, inputs: u32, n8: u32) -> String {
        let mut c = String::new();
        for s in 0..k {
            write!(c, "(local.set $s{s} (f64.const 0)) (local.set $p{s} (global.get $w{s}))").unwrap();
        }
        if self.options.simd {
            // The products of two inputs at once, added one after the other. A 0 input at the end
            // adds nothing: a sum from +0 is never -0.
            write!(c, "(local.set $i (i32.const 0))\n(loop $dot\n(local.set $v (v128.load offset={inputs} (local.get $i)))\n").unwrap();
            for s in 0..k {
                write!(
                    c,
                    "(local.set $s{s} (f64.add (f64.add (local.get $s{s}) (f64x2.extract_lane 0 (local.tee $u (f64x2.mul (v128.load offset=8 (i32.add (local.get $p{s}) (local.get $i))) (local.get $v))))) (f64x2.extract_lane 1 (local.get $u))))\n"
                )
                .unwrap();
            }
            write!(c, "(br_if $dot (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 16))) (i32.const {n8}))))\n").unwrap();
            return c;
        }
        c += "(local.set $i (i32.const 0))\n(loop $dot\n(local.set $x (f64.load offset=";
        write!(c, "{inputs} (local.get $i)))\n").unwrap();
        for s in 0..k {
            write!(
                c,
                "(local.set $s{s} (f64.add (local.get $s{s}) (f64.mul (f64.load offset=8 (i32.add (local.get $p{s}) (local.get $i))) (local.get $x))))\n"
            )
            .unwrap();
        }
        write!(
            c,
            "(br_if $dot (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 8))) (i32.const {n8}))))\n"
        )
        .unwrap();
        c
    }

    /// Code that adds the errors $e0.. times the inputs to the weight sets $w0..
    fn weight_updates(&self, k: usize, inputs: u32, n8: u32) -> String {
        let mut c = String::new();
        for s in 0..k {
            write!(c, "(local.set $p{s} (global.get $w{s}))").unwrap();
        }
        if self.options.simd {
            // Two weights at once
            for s in 0..k {
                write!(c, "(local.set $v{s} (f64x2.splat (local.get $e{s})))").unwrap();
            }
            write!(c, "(local.set $i (i32.const 0))\n(loop $update\n(local.set $v (v128.load offset={inputs} (local.get $i)))\n").unwrap();
            for s in 0..k {
                write!(
                    c,
                    "(v128.store offset=8 (local.tee $n (i32.add (local.get $p{s}) (local.get $i))) (f64x2.add (v128.load offset=8 (local.get $n)) (f64x2.mul (local.get $v{s}) (local.get $v))))\n"
                )
                .unwrap();
            }
            write!(c, "(br_if $update (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 16))) (i32.const {n8}))))\n").unwrap();
            return c;
        }
        c += "(local.set $i (i32.const 0))\n(loop $update\n";
        write!(c, "(local.set $x (f64.load offset={inputs} (local.get $i)))\n").unwrap();
        for s in 0..k {
            write!(
                c,
                "(f64.store offset=8 (local.tee $n (i32.add (local.get $p{s}) (local.get $i))) (f64.add (f64.load offset=8 (local.get $n)) (f64.mul (local.get $e{s}) (local.get $x))))\n"
            )
            .unwrap();
        }
        write!(
            c,
            "(br_if $update (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 8))) (i32.const {n8}))))\n"
        )
        .unwrap();
        c
    }
}

/// The character classes of CharClass (1 to 7, see char_class.js) of the ASCII characters; the
/// others are 0
fn char_class_table() -> Vec<u8> {
    (0u8..128)
        .map(|c| {
            let c = c as char;
            if c.is_ascii_alphabetic() || c == '_' || c == '$' {
                1
            } else if c.is_ascii_digit() {
                2
            } else if " \t\r\n".contains(c) {
                3
            } else if "'\"`".contains(c) {
                4
            } else if "([{".contains(c) {
                5
            } else if ")]}".contains(c) {
                6
            } else if ".,:;+*=<>!&|%^~?/\\-".contains(c) {
                7
            } else {
                0
            }
        })
        .collect()
}

/// The arithmetic decoder, stretch and squash (as utils.js), and fills of memory
const CORE: &str = r#"
;; Decodes a bit with the probability p that it is 1, as coder.js
(func $code (param $p f64) (result i32) (local $mid i32) (local $bit i32)
  (local.set $mid (i32.trunc_sat_f64_u (f64.add (f64.convert_i32_u (global.get $low))
    (f64.mul (f64.convert_i32_u (i32.sub (global.get $high) (global.get $low))) (local.get $p)))))
  (if (i32.ge_u (local.get $mid) (global.get $high)) (then (local.set $mid (i32.sub (global.get $high) (i32.const 1)))))
  (if (local.tee $bit (i32.le_u (global.get $st) (local.get $mid)))
    (then (global.set $high (local.get $mid)))
    (else (global.set $low (i32.add (local.get $mid) (i32.const 1)))))
  (block $done (loop $shift
    (br_if $done (i32.ge_u (i32.xor (global.get $high) (global.get $low)) (i32.const 0x1000000)))
    (global.set $low (i32.shl (global.get $low) (i32.const 8)))
    (global.set $high (i32.or (i32.shl (global.get $high) (i32.const 8)) (i32.const 255)))
    (global.set $st (i32.or (i32.shl (global.get $st) (i32.const 8)) (i32.load8_u (global.get $ptr))))
    (global.set $ptr (i32.add (global.get $ptr) (i32.const 1)))
    (br $shift)))
  (local.get $bit))

;; Repeats the first `unit` bytes at `at` until `len` bytes are filled
(func $rep (param $at i32) (param $unit i32) (param $len i32)
  (loop $double
    (if (i32.gt_u (i32.shl (local.get $unit) (i32.const 1)) (local.get $len))
      (then (memory.copy (i32.add (local.get $at) (local.get $unit)) (local.get $at) (i32.sub (local.get $len) (local.get $unit))) (return)))
    (memory.copy (i32.add (local.get $at) (local.get $unit)) (local.get $at) (local.get $unit))
    (local.set $unit (i32.shl (local.get $unit) (i32.const 1)))
    (br $double)))

;; v / 16777215 for v below 2^24: (v (2^-24 + 2^-48)) (1 + 2^-48) rounds the same for all of them,
;; and takes two multiplications instead of a division
(func $u24 (param $v i32) (result f64)
  (f64.mul (f64.mul (f64.convert_i32_u (local.get $v)) (f64.const 0x1.000001p-24)) (f64.const 0x1.0000000000010p+0)))

;; The hash of the Rust encoder, as hash in utils.js
(func $hash (param $v i32) (param $shift i32) (result i32)
  (i32.shr_u (i32.mul (i32.xor (local.get $v) (i32.shr_u (local.get $v) (local.get $shift))) (i32.const 0x9E35A7BD)) (local.get $shift)))
"#;

/// Stretch and squash, one value at a time
const CORE_SCALAR: &str = r#"
;; ln(p / (1 - p)), as probStretch: with x = p / (1 - p) = m 2^e and m in [1, 2], e ln 2 + the
;; series of 2 atanh((m - 1) / (m + 1)). probStretch halves or doubles x until it is in [1, 2],
;; which is exact, as is taking the exponent from the bits; x = 2^e for e > 0 stays at 2.
(func $stretch (param $p f64) (result f64) (local $x f64) (local $s f64) (local $t f64) (local $r f64) (local $i f64) (local $e i32)
  (local.set $x (f64.div (local.tee $p (f64.max {EPS} (f64.min {EPS1} (local.get $p))))
    (f64.sub (f64.const 1) (local.get $p))))
  (local.set $e (i32.sub (i32.wrap_i64 (i64.shr_u (i64.reinterpret_f64 (local.get $x)) (i64.const 52))) (i32.const 1023)))
  (local.set $x (f64.mul (local.get $x) (f64.reinterpret_i64 (i64.shl (i64.extend_i32_u (i32.sub (i32.const 1023) (local.get $e))) (i64.const 52)))))
  (if (i32.and (f64.eq (local.get $x) (f64.const 1)) (i32.gt_s (local.get $e) (i32.const 0)))
    (then (local.set $x (f64.const 2)) (local.set $e (i32.sub (local.get $e) (i32.const 1)))))
  (local.set $t (f64.add (local.tee $s (f64.div (f64.sub (local.get $x) (f64.const 1)) (f64.add (local.get $x) (f64.const 1)))) (local.get $s)))
  (local.set $s (f64.mul (local.get $s) (local.get $s)))
  (local.set $i (f64.const 1))
  (loop $term
    (local.set $r (f64.add (local.get $r) (f64.div (local.get $t) (local.get $i))))
    (local.set $t (f64.mul (local.get $t) (local.get $s)))
    (br_if $term (f64.lt (local.tee $i (f64.add (local.get $i) (f64.const 2))) (f64.const 24))))
  (f64.add (local.get $r) (f64.mul (f64.convert_i32_s (local.get $e)) {LN2})))

;; 1 / (1 + exp(-p)), as probSquash: exp(-p / 1024)^1024 with the Taylor series of exp(-p / 1024)
(func $squash (param $p f64) (result f64) (local $t f64) (local $r f64) (local $i f64)
  (local.set $p (f64.div (f64.neg (local.get $p)) (f64.const 1024)))
  (local.set $t (f64.const 1))
  (local.set $r (f64.const 1))
  (local.set $i (f64.const 1))
  (loop $term
    (local.set $r (f64.add (local.get $r) (local.tee $t (f64.mul (local.get $t) (f64.div (local.get $p) (local.get $i))))))
    (br_if $term (f64.lt (local.tee $i (f64.add (local.get $i) (f64.const 1))) (f64.const 14))))
  (local.set $i (f64.const 0))
  (loop $square
    (local.set $r (f64.mul (local.get $r) (local.get $r)))
    (br_if $square (f64.lt (local.tee $i (f64.add (local.get $i) (f64.const 1))) (f64.const 10))))
  (f64.div (f64.const 1) (f64.add (f64.const 1) (local.get $r))))

"#;

/// Stretch and squash of two values at once, as $stretch and $squash, whose operations they do in
/// both lanes; with SIMD, $stretch and $squash use them too
const CORE_SIMD: &str = r#"
(func $stretch2 (param $p v128) (result v128) (local $x v128) (local $e v128) (local $s v128) (local $t v128) (local $r v128) (local $m v128) (local $i f64)
  (local.set $x (f64x2.div (local.tee $p (f64x2.max (f64x2.splat {EPS}) (f64x2.min (f64x2.splat {EPS1}) (local.get $p))))
    (f64x2.sub (f64x2.splat (f64.const 1)) (local.get $p))))
  ;; x = m 2^e: the biased exponent, and x times 2^-e
  (local.set $e (i64x2.shr_u (local.get $x) (i32.const 52)))
  (local.set $x (f64x2.mul (local.get $x) (i64x2.shl (i64x2.sub (i64x2.splat (i64.const 2046)) (local.get $e)) (i32.const 52))))
  (local.set $e (i64x2.sub (local.get $e) (i64x2.splat (i64.const 1023))))
  ;; x = 2^e for e > 0 stays at 2
  (local.set $m (v128.and (f64x2.eq (local.get $x) (f64x2.splat (f64.const 1))) (i64x2.gt_s (local.get $e) (v128.const i64x2 0 0))))
  (local.set $x (v128.bitselect (f64x2.splat (f64.const 2)) (local.get $x) (local.get $m)))
  (local.set $e (i64x2.add (local.get $e) (local.get $m)))
  ;; The first term is t / 1 = t
  (local.set $r (local.tee $t (f64x2.add (local.tee $s (f64x2.div (f64x2.sub (local.get $x) (f64x2.splat (f64.const 1))) (f64x2.add (local.get $x) (f64x2.splat (f64.const 1))))) (local.get $s))))
  (local.set $t (f64x2.mul (local.get $t) (local.tee $s (f64x2.mul (local.get $s) (local.get $s)))))
  (local.set $i (f64.const 3))
  (loop $term
    (local.set $r (f64x2.add (local.get $r) (f64x2.div (local.get $t) (f64x2.splat (local.get $i)))))
    (local.set $t (f64x2.mul (local.get $t) (local.get $s)))
    (br_if $term (f64.lt (local.tee $i (f64.add (local.get $i) (f64.const 2))) (f64.const 24))))
  (f64x2.add (local.get $r) (f64x2.mul (f64x2.convert_low_i32x4_s (i8x16.shuffle 0 1 2 3 8 9 10 11 0 1 2 3 0 1 2 3 (local.get $e) (local.get $e))) (f64x2.splat {LN2}))))
(func $squash2 (param $p v128) (result v128) (local $t v128) (local $r v128) (local $i f64)
  ;; The first term is 1 (p / 1) = p
  (local.set $r (f64x2.add (f64x2.splat (f64.const 1)) (local.tee $t (local.tee $p (f64x2.div (f64x2.neg (local.get $p)) (f64x2.splat (f64.const 1024)))))))
  (local.set $i (f64.const 2))
  (loop $term
    (local.set $r (f64x2.add (local.get $r) (local.tee $t (f64x2.mul (local.get $t) (f64x2.div (local.get $p) (f64x2.splat (local.get $i)))))))
    (br_if $term (f64.lt (local.tee $i (f64.add (local.get $i) (f64.const 1))) (f64.const 14))))
  (local.set $i (f64.const 0))
  (loop $square
    (local.set $r (f64x2.mul (local.get $r) (local.get $r)))
    (br_if $square (f64.lt (local.tee $i (f64.add (local.get $i) (f64.const 1))) (f64.const 10))))
  (f64x2.div (f64x2.splat (f64.const 1)) (f64x2.add (f64x2.splat (f64.const 1)) (local.get $r))))
(func $stretch (param $p f64) (result f64) (f64x2.extract_lane 0 (call $stretch2 (f64x2.splat (local.get $p)))))
(func $squash (param $p f64) (result f64) (f64x2.extract_lane 0 (call $squash2 (f64x2.splat (local.get $p)))))
"#;

/// The context models of the shared counters, as counters.js. A model is a record of CM_SIZE bytes:
/// 0 context, 4 whether it has one, 8 address of its slot, 12 address of its state counter,
/// 16 address of the slot of the run, 20 the run info, 24 expected byte of a WasmContext,
/// 28 bits of the byte so far after a leading 1, 32 address of its inputs, 36 max count,
/// 40 next function, 44 address of its state counters, 48 the xor of the bits it learns (255 xor
/// the expected byte of a Record), 52 address of its state for the mixer, 56.. its own fields
const COUNTERS: &str = r#"
(func $cm (param $next i32) (param $max i32) (param $a i32) (param $act i32) (local $r i32)
  (i32.store offset=40 (local.tee $r (global.get $cr)) (local.get $next))
  (i32.store offset=36 (local.get $r) (local.get $max))
  (i32.store offset=56 (local.get $r) (local.get $a))
  (i32.store offset=4 (local.get $r) (local.get $act))
  (i32.store offset=28 (local.get $r) (i32.const 1))
  (i32.store offset=32 (local.get $r) (global.get $ip))
  (i32.store offset=52 (local.get $r) (global.get $mi))
  (i32.store offset=44 (local.get $r) (global.get $sp))
  (i32.store (global.get $sp) (i32.const 0x7fffff))
  (call $rep (global.get $sp) (i32.const 4) (i32.const 0x40000))
  (global.set $sp (i32.add (global.get $sp) (i32.const 0x40000)))
  (global.set $ip (i32.add (global.get $ip) (i32.const 40)))
  (global.set $mi (i32.add (global.get $mi) (i32.const 4)))
  (global.set $cr (i32.add (local.get $r) (i32.const 128))))

;; Math.imul(hash(hi, 3) * 9 + hash(lo, 3) + 1, magic)
(func $context_hash (param $hi i32) (param $lo i32) (param $magic i32) (result i32)
  (i32.mul (i32.add (i32.add (i32.mul (call $hash (local.get $hi) (i32.const 3)) (i32.const 9)) (call $hash (local.get $lo) (i32.const 3))) (i32.const 1)) (local.get $magic)))

;; Moves the counter at `at` towards bit, as learnCounter
(func $learn (param $at i32) (param $bit i32) (param $max i32) (local $v i32) (local $count i32)
  (local.set $count (i32.shr_u (local.tee $v (i32.load (local.get $at))) (i32.const 24)))
  (local.set $count (i32.add (local.get $count) (i32.lt_u (local.get $count) (local.get $max))))
  (i32.store (local.get $at) (i32.or (i32.and (i32.add (local.tee $v (i32.and (local.get $v) (i32.const 0xffffff)))
    (i32.trunc_f64_s (f64.mul {U24F} (f64.div (f64.sub (f64.convert_i32_u (local.get $bit)) (call $u24 (local.get $v)))
      (f64.add (f64.convert_i32_u (local.get $count)) {OFFSET})))))
    (i32.const 0xffffff)) (i32.shl (local.get $count) (i32.const 24)))))

;; The inputs of all context models, as CounterModel.p
(func $cm_p (local $v v128) (local $r i32) (local $bc i32) (local $e i32) (local $k i32) (local $c i32) (local $slot i32) (local $info i32) (local $w i32) (local $in i32) (local $p f64) (local $count i32)
  ;; First the slot of each model in turn, without branches, so that their memory loads overlap.
  ;; The model takes the counter, slot info and state counter as it finds them, before the next
  ;; model may take the slot over.
  (local.set $r (i32.const {CM0}))
  (loop $slot
    (if (i32.load offset=4 (local.get $r)) (then
      (local.set $bc (i32.load offset=28 (local.get $r)))
      (local.set $e (i32.load offset=24 (local.get $r)))
      ;; The key: the context and the bits so far, or for a WasmContext, while they are those of
      ;; the expected byte, their number and the next expected bit
      (local.set $k (i32.xor (i32.load (local.get $r))
        (if (result i32) (i32.eq (i32.shr_u (local.get $e) (i32.sub (i32.const 8) (global.get $bits))) (local.get $bc))
          (then (i32.or (i32.or (i32.const 256) (i32.shl (global.get $bits) (i32.const 1)))
            (i32.and (i32.shr_u (local.get $e) (i32.sub (i32.const 7) (global.get $bits))) (i32.const 1))))
          (else (local.get $bc)))))
      (local.set $c (i32.or (i32.shr_u (i32.mul (local.get $k) (i32.const 0x2545F491)) (i32.const 20)) (i32.const 1)))
      (i32.store offset=8 (local.get $r) (local.tee $slot (i32.add (i32.const {COUNTERS}) (i32.shl (i32.shr_u (i32.mul (i32.xor (local.get $k) (i32.shr_u (local.get $k) (i32.const 15))) (i32.const 0x2C1B3C6D)) (i32.const {SHIFT})) (i32.const 3)))))
      ;; A key with another check takes the slot over
      (local.set $k (i32.ne (i32.shr_u (local.tee $info (i32.load offset=4 (local.get $slot))) (i32.const 20)) (local.get $c)))
      (i32.store (local.get $slot) (local.tee $w (select (i32.const 0x7fffff) (i32.load (local.get $slot)) (local.get $k))))
      (i32.store offset=4 (local.get $slot) (local.tee $info (select (i32.shl (local.get $c) (i32.const 20)) (local.get $info) (local.get $k))))
      (i32.store offset=88 (local.get $r) (local.get $w))
      ;; The run of the byte, kept in the slot of its first bit
      (if (i32.lt_u (local.get $bc) (i32.const 2)) (then
        (i32.store offset=16 (local.get $r) (local.get $slot))
        (i32.store offset=20 (local.get $r) (local.get $info))))
      (i32.store offset=12 (local.get $r) (local.tee $c (i32.add (i32.load offset=44 (local.get $r))
        (i32.shl (i32.or (i32.shl (i32.and (i32.shr_u (local.get $info) (i32.const 8)) (i32.const 255)) (i32.const 8)) (local.get $bc)) (i32.const 2)))))
      (i32.store offset=92 (local.get $r) (i32.load (local.get $c)))))
    (br_if $slot (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 128))) (i32.const {CMEND}))))
  ;; Then the inputs
  (local.set $r (i32.const {CM0}))
  (loop $model
    (local.set $in (i32.load offset=32 (local.get $r)))
    (local.set $count (i32.const 0))
    (if (i32.load offset=4 (local.get $r))
      (then
        (local.set $bc (i32.load offset=28 (local.get $r)))
        (local.set $p (call $u24 (i32.and (local.tee $w (i32.load offset=88 (local.get $r))) (i32.const 0xffffff))))
        {STRETCHES}
        (f64.store offset=8 (local.get $in) (f64.mul (f64.sub (local.get $p) (f64.const 0.5)) {IN1}))
        (local.set $info (i32.load offset=20 (local.get $r)))
        (f64.store offset=24 (local.get $in)
          (if (result f64) (i32.and
              (i32.ne (local.tee $c (i32.and (i32.shr_u (local.get $info) (i32.const 16)) (i32.const 15))) (i32.const 0))
              (i32.eq (i32.shr_u (i32.or (i32.and (local.get $info) (i32.const 255)) (i32.const 256)) (i32.sub (i32.const 8) (global.get $bits))) (local.get $bc)))
            (then (f64.mul (f64.convert_i32_s (select (local.get $c) (i32.sub (i32.const 0) (local.get $c))
              (i32.and (i32.shr_u (local.get $info) (i32.sub (i32.const 7) (global.get $bits))) (i32.const 1)))) {IN3}))
            (else (f64.const 0))))
        (f64.store offset=32 (local.get $in) (select (f64.const 0) {IN4} (local.tee $count (i32.shr_u (local.get $w) (i32.const 24))))))
      (else
        (f64.store (local.get $in) (f64.const 0))
        (f64.store offset=8 (local.get $in) (f64.const 0))
        (f64.store offset=16 (local.get $in) (f64.const 0))
        (f64.store offset=24 (local.get $in) (f64.const 0))
        (f64.store offset=32 (local.get $in) (f64.const 0))))
    ;; A Record predicts whether each bit is the one of the expected byte: its inputs are negated
    ;; for an expected 0
    (if (i32.and (i32.shr_u (i32.load offset=48 (local.get $r)) (i32.sub (i32.const 7) (global.get $bits))) (i32.const 1))
      (then
        (f64.store (local.get $in) (f64.neg (f64.load (local.get $in))))
        (f64.store offset=8 (local.get $in) (f64.neg (f64.load offset=8 (local.get $in))))
        (f64.store offset=16 (local.get $in) (f64.neg (f64.load offset=16 (local.get $in))))
        (f64.store offset=24 (local.get $in) (f64.neg (f64.load offset=24 (local.get $in))))
        (f64.store offset=32 (local.get $in) (f64.neg (f64.load offset=32 (local.get $in))))))
    {CM_STATE}
    (br_if $model (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 128))) (i32.const {CMEND})))))

;; Learns the bit in all context models, and computes the context of the next byte at its end
(func $cm_l (param $bit i32) (local $r i32) (local $b i32) (local $bc i32) (local $slot i32) (local $s i32) (local $info i32)
  (local.set $r (i32.const {CM0}))
  (loop $model
    (local.set $b (i32.xor (local.get $bit) (i32.and (i32.shr_u (i32.load offset=48 (local.get $r)) (i32.sub (i32.const 7) (global.get $bits))) (i32.const 1))))
    (if (i32.load offset=4 (local.get $r)) (then
      (call $learn (local.tee $slot (i32.load offset=8 (local.get $r))) (local.get $b) (i32.load offset=36 (local.get $r)))
      (call $learn (i32.load offset=12 (local.get $r)) (local.get $b) (i32.const 127))
      ;; The bit history state: the last 7 bits after a leading 1
      (local.set $s (i32.or (i32.shl (select (local.tee $s (i32.and (i32.shr_u (local.tee $info (i32.load offset=4 (local.get $slot))) (i32.const 8)) (i32.const 255))) (i32.const 1) (local.get $s)) (i32.const 1)) (local.get $b)))
      (i32.store offset=4 (local.get $slot) (i32.or (i32.and (local.get $info) (i32.const 0xffff00ff))
        (i32.shl (select (i32.or (i32.and (local.get $s) (i32.const 127)) (i32.const 128)) (local.get $s) (i32.gt_u (local.get $s) (i32.const 255))) (i32.const 8))))))
    (if (i32.gt_u (local.tee $bc (i32.or (i32.shl (i32.load offset=28 (local.get $r)) (i32.const 1)) (local.get $b))) (i32.const 255)) (then
      ;; The run of the context of the byte, if no other context took its slot
      (if (i32.load offset=4 (local.get $r)) (then
        (local.set $info (i32.load offset=20 (local.get $r)))
        (if (i32.eq (i32.shr_u (local.tee $s (i32.load offset=4 (local.tee $slot (i32.load offset=16 (local.get $r))))) (i32.const 20)) (i32.shr_u (local.get $info) (i32.const 20)))
          (then (i32.store offset=4 (local.get $slot) (i32.or (i32.or (i32.and (local.get $s) (i32.const 0xfff0ff00))
            (i32.shl (i32.add (select
              (select (local.tee $s (i32.and (i32.shr_u (local.get $info) (i32.const 16)) (i32.const 15))) (i32.const 14) (i32.lt_u (local.get $s) (i32.const 14)))
              (i32.const 0)
              (i32.eq (i32.and (local.get $info) (i32.const 255)) (i32.and (local.get $bc) (i32.const 255)))) (i32.const 1)) (i32.const 16)))
            (i32.and (local.get $bc) (i32.const 255))))))))
      (call_indirect (type $next) (local.get $r) (i32.and (local.get $bc) (i32.const 255)) (i32.load offset=40 (local.get $r)))
      (local.set $bc (i32.const 1))))
    (i32.store offset=28 (local.get $r) (local.get $bc))
    (br_if $model (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 128))) (i32.const {CMEND})))))
"#;

/// The first and third input of a context model: the stretched probabilities of its counter and
/// of the state counter of its bit history, scaled
const STRETCHES: &str = "(f64.store (local.get $in) (f64.mul (call $stretch (local.get $p)) {IN0}))
        (f64.store offset=16 (local.get $in) (f64.mul (call $stretch (call $u24 (i32.and (i32.load offset=92 (local.get $r)) (i32.const 0xffffff)))) {IN2}))";
const STRETCHES_SIMD: &str = "(local.set $v (call $stretch2 (f64x2.replace_lane 1 (f64x2.splat (local.get $p)) (call $u24 (i32.and (i32.load offset=92 (local.get $r)) (i32.const 0xffffff))))))
        (f64.store (local.get $in) (f64.mul (f64x2.extract_lane 0 (local.get $v)) {IN0}))
        (f64.store offset=16 (local.get $in) (f64.mul (f64x2.extract_lane 1 (local.get $v)) {IN2}))";

/// The state of a context model for a ContextMixer: the count of its counter, or 0
const CM_STATE: &str = "(i32.store (i32.load offset=52 (local.get $r)) (local.get $count))";

/// The second input of a match model for a ContextMixer, the expected bit as -1 or 1 times the
/// length bucket of the state, and the state
const MM_INPUT: &str = "(f64.store offset=8 (local.get $in) (f64.mul (select (f64.const 1) (f64.const -1) (i32.load offset=36 (local.get $r)))
      (f64.convert_i32_s (select (i32.shr_s (i32.sub (local.get $state) (i32.const 1)) (i32.const 1)) (i32.const 0) (local.get $state)))))
    (i32.store (i32.load offset=32 (local.get $r)) (local.get $state))";

/// The next functions of the context models, which compute the context of the next byte
const NEXT_NORDER: &str = r#"
;; NOrderByte: the previous 8 bytes, of which bit i of the mask (field 56) keeps byte i
(func $next_norder (param $r i32) (param $b i32) (local $m i32)
  (i32.store (local.get $r) (call $context_hash
    (i32.and (global.get $H) (i32.mul (i32.and (i32.mul (i32.shr_u (local.tee $m (i32.load offset=56 (local.get $r))) (i32.const 4)) (i32.const 0x204081)) (i32.const 0x01010101)) (i32.const 255)))
    (i32.and (global.get $L) (i32.mul (i32.and (i32.mul (i32.and (local.get $m) (i32.const 15)) (i32.const 0x204081)) (i32.const 0x01010101)) (i32.const 255)))
    (call $hash (local.get $m) (i32.const 2)))))
"#;

const NEXT_WORD: &str = r#"
;; Word: the hash of the word so far (field 56), of letters (in lower case), digits and _.[]
(func $next_word (param $r i32) (param $b i32)
  (i32.store offset=56 (local.get $r) (if (result i32) (i32.or
        (i32.and (i32.lt_u (i32.sub (call $char_class (local.get $b)) (i32.const 1)) (i32.const 2)) (i32.ne (local.get $b) (i32.const 36)))
        (i32.or (i32.eq (local.get $b) (i32.const 46)) (i32.eq (i32.or (local.get $b) (i32.const 6)) (i32.const 95))))
      (then (i32.mul (i32.xor (i32.load offset=56 (local.get $r))
        (i32.or (local.get $b) (i32.shl (i32.lt_u (i32.sub (local.get $b) (i32.const 65)) (i32.const 26)) (i32.const 5)))) (i32.const 16777619)))
      (else (i32.const 2166136261))))
  (i32.store (local.get $r) (call $context_hash (i32.const 0) (i32.load offset=56 (local.get $r)) (i32.const {H1337}))))
"#;

const NEXT_CHAR_CLASS: &str = r#"
;; CharClass: the classes of the previous `order` (field 56) bytes, kept in field 60
(func $next_char_class (param $r i32) (param $b i32)
  (i32.store offset=60 (local.get $r) (i32.or (i32.shl (i32.load offset=60 (local.get $r)) (i32.const 3)) (call $char_class (local.get $b))))
  (i32.store (local.get $r) (call $context_hash (i32.const 0)
    (i32.and (i32.load offset=60 (local.get $r)) (i32.sub (i32.shl (i32.const 1) (i32.mul (i32.load offset=56 (local.get $r)) (i32.const 3))) (i32.const 1)))
    (call $hash (i32.xor (i32.const 0x434c4153) (i32.load offset=56 (local.get $r))) (i32.const 2)))))
"#;

const NEXT_INDIRECT: &str = r#"
;; IndirectByte: the byte that most often followed the last `contextBytes` (field 56) bytes, with
;; the last bytes in field 60 and the table of transitions at field 64
(func $next_indirect (param $r i32) (param $b i32) (local $mask i32) (local $at i32) (local $t i32) (local $count i32) (local $cand i32)
  (local.set $mask (i32.sub (i32.shl (i32.const 1) (i32.shl (i32.load offset=56 (local.get $r)) (i32.const 3))) (i32.const 1)))
  (local.set $cand (i32.and (local.tee $t (i32.load16_u (local.tee $at (i32.add (i32.load offset=64 (local.get $r))
    (i32.shl (i32.and (i32.load offset=60 (local.get $r)) (local.get $mask)) (i32.const 1)))))) (i32.const 255)))
  (local.set $count (i32.shr_u (local.get $t) (i32.const 8)))
  ;; A saturating vote keeps a stable candidate while allowing replacement
  (i32.store16 (local.get $at)
    (if (result i32) (i32.or (i32.eqz (local.get $count)) (i32.and (i32.eq (local.get $count) (i32.const 1)) (i32.ne (local.get $cand) (local.get $b))))
      (then (i32.or (i32.const 256) (local.get $b)))
      (else (if (result i32) (i32.eq (local.get $cand) (local.get $b))
        (then (i32.or (i32.shl (select (local.tee $count (i32.add (local.get $count) (i32.const 1))) (i32.const 255) (i32.lt_u (local.get $count) (i32.const 255))) (i32.const 8)) (local.get $b)))
        (else (i32.or (i32.shl (i32.sub (local.get $count) (i32.const 1)) (i32.const 8)) (local.get $cand)))))))
  (i32.store offset=60 (local.get $r) (local.tee $t (i32.or (i32.shl (i32.load offset=60 (local.get $r)) (i32.const 8)) (local.get $b))))
  (i32.store (local.get $r) (call $context_hash (i32.const 0)
    (i32.load8_u (i32.add (i32.load offset=64 (local.get $r)) (i32.shl (i32.and (local.get $t) (local.get $mask)) (i32.const 1))))
    (i32.const {H9191}))))
"#;

const NEXT_TOKEN: &str = r#"
;; TokenPosition: the kind of the token (field 56), the position in it (60), the last delimiter (64),
;; the last bytes (68) and whether the last byte escaped the next (72)
(func $next_token (param $r i32) (param $b i32) (local $kind i32) (local $pos i32) (local $quote i32) (local $class i32)
  (local.set $kind (i32.load offset=56 (local.get $r)))
  (local.set $pos (i32.load offset=60 (local.get $r)))
  (local.set $quote (select (i32.const 0) (select (i32.const 1) (select (i32.const 2) (i32.const -1) (i32.eq (local.get $b) (i32.const 96))) (i32.eq (local.get $b) (i32.const 34))) (i32.eq (local.get $b) (i32.const 39))))
  (local.set $class (call $char_class (local.get $b)))
  (if (i32.gt_u (local.get $kind) (i32.const 2))
    (then
      (if (i32.load offset=72 (local.get $r))
        (then (i32.store offset=72 (local.get $r) (i32.const 0)) (local.set $pos (call $token_step (local.get $pos))))
        (else (if (i32.eq (local.get $b) (i32.const 92))
          (then (i32.store offset=72 (local.get $r) (i32.const 1)) (local.set $pos (call $token_step (local.get $pos))))
          (else (if (i32.eq (local.get $quote) (i32.sub (local.get $kind) (i32.const 3)))
            (then (local.set $kind (i32.const 0)) (local.set $pos (i32.const 0)) (i32.store offset=64 (local.get $r) (local.get $b)))
            (else (local.set $pos (call $token_step (local.get $pos))))))))))
    (else (if (i32.ge_s (local.get $quote) (i32.const 0))
      (then (local.set $kind (i32.add (local.get $quote) (i32.const 3))) (local.set $pos (i32.const 0)))
      (else (if (i32.or (i32.eq (local.get $class) (i32.const 1)) (i32.and (i32.eq (local.get $kind) (i32.const 1)) (i32.eq (local.get $class) (i32.const 2))))
        (then (local.set $pos (select (call $token_step (local.get $pos)) (i32.const 1) (i32.eq (local.get $kind) (i32.const 1)))) (local.set $kind (i32.const 1)))
        (else (if (i32.eq (local.get $class) (i32.const 2))
          (then (local.set $pos (select (call $token_step (local.get $pos)) (i32.const 1) (i32.eq (local.get $kind) (i32.const 2)))) (local.set $kind (i32.const 2)))
          (else
            (local.set $kind (i32.const 0)) (local.set $pos (i32.const 0))
            (if (i32.eqz (i32.or (i32.eq (local.get $class) (i32.const 3)) (i32.eq (local.get $b) (i32.const 12))))
              (then (i32.store offset=64 (local.get $r) (local.get $b))))))))))))
  (i32.store offset=56 (local.get $r) (local.get $kind))
  (i32.store offset=60 (local.get $r) (local.get $pos))
  (i32.store offset=68 (local.get $r) (i32.and (i32.or (i32.shl (i32.load offset=68 (local.get $r)) (i32.const 8)) (local.get $b)) (i32.const 65535)))
  (i32.store (local.get $r) (i32.mul (call $hash (i32.xor (i32.xor
      (i32.or (i32.or (local.get $kind) (i32.shl (local.get $pos) (i32.const 3))) (i32.shl (i32.load offset=64 (local.get $r)) (i32.const 11)))
      (i32.shl (i32.and (i32.load offset=68 (local.get $r)) (i32.const {TOKEN_MASK})) (i32.const 19)))
    (i32.const 0x6b8b4567)) (i32.const 3)) (i32.const 0x85ebca6b))))
(func $token_step (param $pos i32) (result i32)
  (select (local.tee $pos (i32.add (local.get $pos) (i32.const 1))) (i32.const {TOKEN_MAX}) (i32.lt_u (local.get $pos) (i32.const {TOKEN_MAX}))))
"#;

const NEXT_JS: &str = r#"
;; JsContext: a hash of the lexer state, by the variant (field 56); null within 32 bytes after a
;; control byte below tab
(func $next_js (param $r i32) (param $b i32) (local $t i32) (local $h i32) (local $v i32) (local $frame i32)
  (local.set $t (select (global.get $js_token) (i32.const 0) (global.get $js_kind)))
  (local.set $frame (i32.add (i32.const {FRAMES}) (i32.shl (global.get $js_depth) (i32.const 3))))
  (local.set $v (i32.load offset=56 (local.get $r)))
  (local.set $h (call $js_mix {JS_VARIANTS} (local.get $v)))
  (i32.store offset=4 (local.get $r) (i32.ge_u (global.get $js_run) (i32.const 32)))
  (i32.store (local.get $r) (i32.and (i32.mul (i32.xor (local.get $h) (i32.shr_u (local.get $h) (i32.const 15))) (i32.const 0x85EBCA6B)) (i32.const -256))))
"#;

/// WasmContext, as wasm.js: the parser follows the modules byte by byte, with the fields still to
/// come on a stack of kind and count, the next one last. Kinds below 19 are bytes, the others
/// LEB128 numbers.
const WASM_PARSER: &str = r#"
(global $wk (mut i32) (i32.const 0))
(global $wsp (mut i32) (i32.const 0))
(global $wpos (mut i32) (i32.const 0))
(global $wsec (mut i32) (i32.const 0))
(global $wsec_end (mut i32) (i32.const 0))
(global $wbody_end (mut i32) (i32.const 0))
(global $wprefix (mut i32) (i32.const 0))
(global $wleb (mut i32) (i32.const 0))
(global $wleb_bytes (mut i32) (i32.const 0))
(global $wfield_pos (mut i32) (i32.const 0))
(global $wbytes (mut i64) (i64.const 0))
(func $windex (result i32) (local $i i32)
  (select (local.tee $i (select (global.get $wleb_bytes) (global.get $wfield_pos) (i32.gt_u (global.get $wk) (i32.const 18))))
    (i32.const 31) (i32.lt_u (local.get $i) (i32.const 31))))
(func $wtop (result i32)
  (if (result i32) (global.get $wsp)
    (then (i32.load offset={WSTACK} (i32.shl (i32.sub (global.get $wsp) (i32.const 1)) (i32.const 3))))
    (else (i32.const 0))))
(func $wpush (param $k i32) (param $n i32) (local $at i32)
  (if (local.get $n) (then
    (i32.store offset={WSTACK} (local.tee $at (i32.shl (global.get $wsp) (i32.const 3))) (local.get $k))
    (i32.store offset={WSTACK4} (local.get $at)
      (select (local.get $n) (i32.const 0x1000000) (i32.lt_u (local.get $n) (i32.const 0x1000000))))
    (global.set $wsp (i32.add (global.get $wsp) (i32.const 1)))
    (global.set $wfield_pos (i32.const 0))
    (global.set $wk (local.get $k)))))
;; Whether a module range starts (0) or ends (4) at pos
(func $wrange (param $pos i32) (param $at i32) (result i32) (local $found i32)
  (loop $range
    (local.set $found (i32.or (local.get $found) (i32.eq (i32.load (local.get $at)) (local.get $pos))))
    (br_if $range (i32.lt_u (local.tee $at (i32.add (local.get $at) (i32.const 8))) (i32.const {WRANGES_END}))))
  (local.get $found))
(func $wasm_update (param $b i32) (local $kind i32) (local $value i32) (local $pos i32) (local $count i32) (local $imm i32) (local $top i32)
  (local.set $kind (global.get $wk))
  (local.set $value (local.get $b))
  (local.set $pos (i32.add (global.get $wpos) (i32.const 1)))
  (if (local.get $kind) (then
    (i32.store8 offset={WCOL} (i32.add (i32.shl (local.get $kind) (i32.const 5)) (call $windex)) (local.get $b))
    (local.set $count (i32.const 1))
    (if (i32.gt_u (local.get $kind) (i32.const 18)) (then
      (if (i32.lt_u (global.get $wleb_bytes) (i32.const 5)) (then
        (global.set $wleb (i32.or (global.get $wleb) (i32.shl (i32.and (local.get $b) (i32.const 127)) (i32.mul (global.get $wleb_bytes) (i32.const 7)))))))
      (global.set $wleb_bytes (i32.add (global.get $wleb_bytes) (i32.const 1)))
      (local.set $value (global.get $wleb))
      ;; count is 0 while the number goes on
      (local.set $count (i32.le_u (local.get $b) (i32.const 127)))))
    (global.set $wfield_pos (i32.add (global.get $wfield_pos) (i32.const 1)))
    (if (local.get $count) (then
      (global.set $wleb (i32.const 0))
      (global.set $wleb_bytes (i32.const 0))
      (i32.store offset={WLAST2} (i32.shl (local.get $kind) (i32.const 2)) (i32.load offset={WLAST} (i32.shl (local.get $kind) (i32.const 2))))
      (i32.store offset={WLAST} (i32.shl (local.get $kind) (i32.const 2)) (local.get $value))
      (if (i32.gt_u (local.tee $count (i32.load offset={WSTACK4} (local.tee $top (i32.shl (i32.sub (global.get $wsp) (i32.const 1)) (i32.const 3))))) (i32.const 1))
        (then (i32.store offset={WSTACK4} (local.get $top) (i32.sub (local.get $count) (i32.const 1))))
        (else (global.set $wsp (i32.sub (global.get $wsp) (i32.const 1))) (global.set $wfield_pos (i32.const 0))))
      (global.set $wk (call $wtop))
      (local.set $count (select (local.get $value) (i32.const 0x1000000) (i32.lt_u (local.get $value) (i32.const 0x1000000))))
      ;; Push the fields that follow
      (if (i32.eq (local.get $kind) (i32.const 2)) (then (global.set $wsec (local.get $value)) (call $wpush (i32.const 19) (i32.const 1))))
      (if (i32.eq (local.get $kind) (i32.const 19)) (then
        (global.set $wsec_end (i32.add (local.get $pos) (local.get $count)))
        (if (i32.or (i32.eq (global.get $wsec) (i32.const 7)) (i32.eq (global.get $wsec) (i32.const 10))) (then (call $wpush (i32.const 20) (i32.const 1))))))
      (if (i32.eq (local.get $kind) (i32.const 21)) (then (call $wpush (i32.const 3) (local.get $count))))
      (if (i32.eq (local.get $kind) (i32.const 27)) (then
        (global.set $wbody_end (i32.add (local.get $pos) (local.get $count)))
        (call $wpush (i32.const 28) (i32.const 1))))
      (if (i32.eq (local.get $kind) (i32.const 28)) (then (call $wpush (i32.const 29) (local.get $count))))
      (if (i32.eq (local.get $kind) (i32.const 29)) (then (call $wpush (i32.const 6) (i32.const 1))))
      (if (i32.eq (local.get $kind) (i32.const 12)) (then
        (global.set $wop2 (global.get $wop1))
        (global.set $wop1 (global.get $wop0))
        (global.set $wop0 (local.get $value))
        ;; The immediates of the instruction
        (if (i32.eq (local.tee $imm (i32.load8_u offset={WIMM} (local.get $value))) (i32.const 63)) (then
          (call $wpush (i32.const 35) (i32.const 1))
          (local.set $imm (i32.const 24))))
        (if (i32.lt_u (i32.sub (local.get $value) (i32.const 0xfc)) (i32.const 3)) (then
          (global.set $wprefix (local.get $value))
          (local.set $imm (i32.const 30))))
        (if (local.get $imm) (then
          (call $wpush (local.get $imm) (select (i32.const 4) (select (i32.const 8) (i32.const 1) (i32.eq (local.get $value) (i32.const 0x44))) (i32.eq (local.get $value) (i32.const 0x43))))))))
      (if (i32.eq (local.get $kind) (i32.const 30)) (then
        (global.set $wop0 (i32.or (i32.shl (global.get $wprefix) (i32.const 9)) (i32.and (local.get $value) (i32.const 511))))
        (if (i32.eq (global.get $wprefix) (i32.const 0xfc))
          (then (if (i32.lt_u (i32.sub (local.get $value) (i32.const 8)) (i32.const 10)) (then
            (call $wpush (i32.const 40) (i32.sub (i32.const 2) (i32.and (local.get $value) (i32.const 1)))))))
          (else (if (i32.or (i32.lt_u (local.get $value) (i32.const 12)) (i32.lt_u (i32.sub (local.get $value) (i32.const 92)) (i32.const 2)))
            (then (call $wpush (i32.const 38) (i32.const 1)))
            (else (if (i32.lt_u (i32.sub (local.get $value) (i32.const 12)) (i32.const 2))
              (then (call $wpush (i32.add (i32.const 4) (local.get $value)) (i32.const 16)))
              (else (if (i32.lt_u (i32.sub (local.get $value) (i32.const 21)) (i32.const 14))
                (then (call $wpush (i32.const 17) (i32.const 1)))
                (else (if (i32.lt_u (i32.sub (local.get $value) (i32.const 84)) (i32.const 8)) (then
                  (call $wpush (i32.const 17) (i32.const 1))
                  (call $wpush (i32.const 38) (i32.const 1))))))))))))))
      (if (i32.eq (local.get $kind) (i32.const 33)) (then (call $wpush (i32.const 32) (i32.add (local.get $count) (i32.const 1)))))
      (if (i32.eq (local.get $kind) (i32.const 38)) (then (call $wpush (i32.const 39) (i32.const 1))))))))
  (global.set $wbytes (i64.or (i64.shl (global.get $wbytes) (i64.const 8)) (i64.extend_i32_u (local.get $b))))
  (global.set $wpos (local.get $pos))
  (if (call $wrange (local.get $pos) (i32.const {WRANGES}))
    (then
      (global.set $wsp (i32.const 0))
      (global.set $wsec_end (i32.const 0))
      (call $wpush (i32.const 1) (i32.const 8)))
    (else (if (call $wrange (local.get $pos) (i32.const {WRANGES4}))
      (then (global.set $wsp (i32.const 0)))
      (else (if (i32.and (i32.ne (local.get $kind) (i32.const 0)) (i32.ne (global.get $wk) (i32.const 1))) (then
        (if (i32.or (i32.eq (local.get $pos) (global.get $wsec_end)) (i32.and (i32.eq (global.get $wsec) (i32.const 10)) (i32.eq (local.get $pos) (global.get $wbody_end))))
          (then (global.set $wsp (i32.const 0))))
        ;; Push the next record of the section, instruction of the function, or section
        (if (i32.eqz (global.get $wsp)) (then
          (if (i32.ge_u (local.get $pos) (global.get $wsec_end))
            (then (call $wpush (i32.const 2) (i32.const 1)))
            (else (if (i32.eq (global.get $wsec) (i32.const 7))
              (then (call $wpush (i32.const 26) (i32.const 1)) (call $wpush (i32.const 11) (i32.const 1)) (call $wpush (i32.const 21) (i32.const 1)))
              (else (if (i32.eq (global.get $wsec) (i32.const 10))
                (then (call $wpush (select (i32.const 12) (i32.const 27) (i32.lt_u (local.get $pos) (global.get $wbody_end))) (i32.const 1)))
                (else (call $wpush (i32.const 4) (i32.const 1))))))))))))))))
  (global.set $wk (call $wtop)))
(global $wop0 (mut i32) (i32.const 0))
(global $wop1 (mut i32) (i32.const 0))
(global $wop2 (mut i32) (i32.const 0))
(func $wmix (param $h i32) (param $v i32) (result i32)
  (i32.xor (local.tee $h (i32.mul (i32.xor (local.get $h) (local.get $v)) (i32.const 0x9E3779B1))) (i32.shr_u (local.get $h) (i32.const 15))))
"#;

const NEXT_WASM: &str = r#"
;; WasmContext: what is in the context is given by the bits of the fields (field 56)
(func $next_wasm (param $r i32) (param $b i32) (local $f i32) (local $kind i32) (local $index i32) (local $h i32) (local $i i32) (local $v i32) (local $next i64) (local $sh i64)
  (local.set $f (i32.load offset=56 (local.get $r)))
  (local.set $kind (global.get $wk))
  (local.set $index (call $windex))
  ;; The values that the fields choose: the last 8 bytes, the last 3 opcodes, the last value of
  ;; the kind, the byte of the last field at the same position, the value before the last
  (i32.store offset={WVALUES} (i32.const 32) (global.get $wop0))
  (i32.store offset={WVALUES} (i32.const 36) (global.get $wop1))
  (i32.store offset={WVALUES} (i32.const 40) (global.get $wop2))
  (i32.store offset={WVALUES} (i32.const 44) (i32.load offset={WLAST} (i32.shl (local.get $kind) (i32.const 2))))
  (i32.store offset={WVALUES} (i32.const 48) (i32.load8_u offset={WCOL} (i32.add (i32.shl (local.get $kind) (i32.const 5)) (local.get $index))))
  (i32.store offset={WVALUES} (i32.const 52) (i32.load offset={WLAST2} (i32.shl (local.get $kind) (i32.const 2))))
  (local.set $h (call $wmix (call $wmix (i32.add (local.get $f) (i32.const 0x5741534d)) (i32.or (local.get $kind) (i32.shl (local.get $index) (i32.const 8)))) (global.get $wsec)))
  (loop $field
    (if (i32.and (i32.shr_u (local.get $f) (local.get $i)) (i32.const 1)) (then
      (local.set $h (call $wmix (local.get $h)
        (if (result i32) (i32.lt_u (local.get $i) (i32.const 8))
          (then (i32.wrap_i64 (i64.and (i64.shr_u (global.get $wbytes) (i64.extend_i32_u (i32.shl (local.get $i) (i32.const 3)))) (i64.const 255))))
          (else (i32.load offset={WVALUES} (i32.shl (local.get $i) (i32.const 2)))))))))
    (br_if $field (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 1))) (i32.const 14))))
  ;; The expected byte: the one of the last field at the same position, or the LEB128 byte of the
  ;; larger of the last two values plus one
  (local.set $next (i64.add (i64.extend_i32_u (select (local.tee $v (i32.load offset={WVALUES} (i32.const 44))) (local.tee $i (i32.load offset={WVALUES} (i32.const 52))) (i32.gt_u (local.get $v) (local.get $i)))) (i64.const 1)))
  (i32.store offset=24 (local.get $r) (select
    (i32.or (i32.const 256)
      (if (result i32) (i32.lt_u (local.get $kind) (i32.const 19))
        (then (i32.load offset={WVALUES} (i32.const 48)))
        (else (if (result i32) (i32.lt_u (local.get $index) (i32.const 5))
          (then (i32.or (i32.wrap_i64 (i64.and (i64.shr_u (local.get $next) (local.tee $sh (i64.extend_i32_u (i32.mul (local.get $index) (i32.const 7))))) (i64.const 127)))
            (i32.shl (i64.ge_u (local.get $next) (i64.shl (i64.const 1) (i64.add (local.get $sh) (i64.const 7)))) (i32.const 7))))
          (else (i32.const 0))))))
    (i32.const 0)
    (i32.and (local.get $f) (i32.const 0x4000))))
  (i32.store offset=4 (local.get $r) (i32.ne (local.get $kind) (i32.const 0)))
  (i32.store (local.get $r) (i32.mul (local.get $h) (i32.const 0x2545F491))))
"#;

const NEXT_RECORD: &str = r#"
;; Record: the context of the next byte in the record ranges, as next in record_model.js. Fields:
;; 56 magic, 60 column, 64 delta, 68 address of the dists, 72 index of the next range, 76 start,
;; 80 end and 84 stride of the range. The expected byte is in field 48, as 255 xor it.
;; A Record model with the parameters at `at`: column, delta, the number of dists and the dists. Its
;; hash is the mix of the delta, the column and the dists.
(func $record_new (param $max i32) (param $at i32) (local $r i32) (local $h i32) (local $i i32)
  (local.set $h (call $record_mix (i32.load8_s offset=1 (local.get $at)) (i32.load8_u (local.get $at))))
  (block $done (loop $dist
    (br_if $done (i32.eq (local.get $i) (i32.load8_u offset=2 (local.get $at))))
    (local.set $h (call $record_mix (local.get $h) (i32.load8_s offset=3 (i32.add (local.get $at) (local.get $i)))))
    (local.set $i (i32.add (local.get $i) (i32.const 1)))
    (br $dist)))
  (local.set $r (global.get $cr))
  (call $cm (i32.const {T_RECORD}) (local.get $max) (local.get $h) (i32.const 0))
  (i32.store offset=60 (local.get $r) (i32.load8_u (local.get $at)))
  (i32.store offset=64 (local.get $r) (i32.load8_s offset=1 (local.get $at)))
  (i32.store offset=68 (local.get $r) (i32.add (local.get $at) (i32.const 2)))
  (i32.store offset=72 (local.get $r) (i32.const {RECORDS}))
  (i32.store offset=48 (local.get $r) (i32.const 255))
  (call $next_record (local.get $r) (i32.const 0)))
(func $next_record (param $r i32) (param $b i32) (local $pos i32) (local $h i32) (local $start i32) (local $stride i32) (local $d i32) (local $n i32)
  (local $w i32) (local $ws i32) (local $k i32) (local $mw i32) (local $mk i32) (local $p i32)
  (local.set $pos (global.get $pos))
  (local.set $h (i32.load offset=56 (local.get $r)))
  ;; The range of the position
  (block $found (loop $range
    (br_if $found (i32.lt_u (local.get $pos) (i32.load offset=80 (local.get $r))))
    (br_if $found (i32.eq (local.tee $d (i32.load offset=72 (local.get $r))) (i32.const {RECORDS_END})))
    (i32.store offset=76 (local.get $r) (local.tee $n (i32.add (i32.load offset=80 (local.get $r)) (i32.shl (i32.load (local.get $d)) (i32.const 9)))))
    (i32.store offset=80 (local.get $r) (i32.add (local.get $n) (i32.shl (i32.load offset=4 (local.get $d)) (i32.const 9))))
    (i32.store offset=84 (local.get $r) (i32.load offset=8 (local.get $d)))
    (i32.store offset=72 (local.get $r) (i32.add (local.get $d) (i32.const 12)))
    (br $range)))
  (local.set $start (i32.load offset=76 (local.get $r)))
  (local.set $stride (i32.load offset=84 (local.get $r)))
  (i32.store offset=4 (local.get $r) (i32.const 0))
  (if (i32.or (i32.lt_u (local.get $pos) (local.get $start)) (i32.ge_u (local.get $pos) (i32.load offset=80 (local.get $r)))) (then (return)))
  ;; The position in the record
  (local.set $k (i32.rem_u (i32.sub (local.get $pos) (local.get $start)) (local.get $stride)))
  (if (i32.load offset=60 (local.get $r)) (then (local.set $h (call $record_mix (local.get $h) (local.get $k)))))
  ;; The bytes at the dists: bytes back if positive, records back if negative, 256 before the start
  (local.set $n (i32.load8_u (local.tee $d (i32.load offset=68 (local.get $r)))))
  (block $done (loop $dist
    (br_if $done (i32.eqz (local.get $n)))
    (local.set $w (select (local.tee $w (i32.load8_s (local.tee $d (i32.add (local.get $d) (i32.const 1))))) (i32.mul (i32.sub (i32.const 0) (local.get $w)) (local.get $stride)) (i32.gt_s (local.get $w) (i32.const 0))))
    (local.set $h (call $record_mix (local.get $h)
      (select (i32.load8_u offset={OUT} (i32.sub (local.get $pos) (local.get $w))) (i32.const 256) (i32.le_u (local.get $w) (local.get $pos)))))
    (local.set $n (i32.sub (local.get $n) (i32.const 1)))
    (br $dist)))
  ;; expected = 255, field 48 is 255 xor it
  (i32.store offset=48 (local.get $r) (i32.const 0))
  (if (local.tee $d (i32.load offset=64 (local.get $r))) (then
    ;; The byte that continues the little endian words of w = |delta| bytes at the same place in the
    ;; last records: linearly from the last two if delta is positive, as in the last one if
    ;; negative. Modulo 2^(8 w), all of it is the same in wrapping 32-bit arithmetic.
    (local.set $w (select (local.get $d) (i32.sub (i32.const 0) (local.get $d)) (i32.gt_s (local.get $d) (i32.const 0))))
    ;; The start of the word, and k, the bytes of it before the position
    (local.set $ws (i32.sub (local.get $pos) (local.tee $k (i32.rem_u (local.get $k) (local.get $w)))))
    (if (i32.or
        (i32.gt_u (i32.add (local.get $ws) (local.get $w)) (i32.add (i32.sub (local.get $pos) (i32.rem_u (i32.sub (local.get $pos) (local.get $start)) (local.get $stride))) (local.get $stride)))
        (i32.lt_u (local.get $ws) (i32.add (local.get $start) (i32.mul (local.get $stride) (i32.add (i32.const 1) (i32.gt_s (local.get $d) (i32.const 0)))))))
      (then (return)))
    ;; m - 1 and km - 1, with m = 2^(8 w) and km = 2^(8 k)
    (local.set $mw (i32.shr_u (i32.const -1) (i32.sub (i32.const 32) (i32.shl (local.get $w) (i32.const 3)))))
    (local.set $mk (i32.sub (i32.shl (i32.const 1) (local.tee $n (i32.shl (local.get $k) (i32.const 3)))) (i32.const 1)))
    (local.set $p (i32.and (i32.load offset={OUT} (i32.sub (local.get $ws) (local.get $stride))) (local.get $mw)))
    (if (i32.gt_s (local.get $d) (i32.const 0)) (then
      (local.set $p (i32.and (i32.sub (i32.shl (local.get $p) (i32.const 1))
        (i32.load offset={OUT} (i32.sub (local.get $ws) (i32.shl (local.get $stride) (i32.const 1))))) (local.get $mw)))))
    ;; d = (word(ws, k) - p) mod km, less km from half of it on
    (local.set $d (i32.and (i32.sub (i32.load offset={OUT} (local.get $ws)) (local.get $p)) (local.get $mk)))
    (if (i32.gt_u (local.get $d) (i32.shr_u (local.get $mk) (i32.const 1)))
      (then (local.set $d (i32.sub (i32.sub (local.get $d) (local.get $mk)) (i32.const 1)))))
    (i32.store offset=48 (local.get $r) (i32.xor (i32.and (i32.shr_u (i32.and (i32.add (local.get $p) (local.get $d)) (local.get $mw)) (local.get $n)) (i32.const 255)) (i32.const 255)))
    (local.set $h (call $record_mix (local.get $h) (local.get $k)))))
  (i32.store offset=4 (local.get $r) (i32.const 1))
  (i32.store (local.get $r) (call $record_mix (local.get $h) (i32.shr_u (local.get $h) (i32.const 15)))))
(func $record_mix (param $h i32) (param $v i32) (result i32)
  (i32.rotl (i32.mul (i32.xor (local.get $h) (local.get $v)) (i32.const 0x9E3779B1)) (i32.const 13)))
"#;

/// Character classes of the bytes, see `char_class_table`
const CHAR_CLASS: &str = r#"
(func $char_class (param $b i32) (result i32) (i32.load8_u offset={CHARS} (local.get $b)))
"#;

/// The JS lexer of JsContext, shared by all its models
const JS_LEXER: &str = r#"
(global $js_quote (mut i32) (i32.const 0))
(global $js_kind (mut i32) (i32.const 0))
(global $js_token (mut i32) (i32.const 0))
(global $js_prev (mut i32) (i32.const 0))
(global $js_punct (mut i32) (i32.const 0))
(global $js_skel_lo (mut i32) (i32.const 0))
(global $js_skel_hi (mut i32) (i32.const 0))
(global $js_depth (mut i32) (i32.const 0))
(global $js_run (mut i32) (i32.const 0))
(func $js_mix (param $a i32) (param $b i32) (result i32) (i32.add (i32.mul (local.get $a) (i32.const 0x9E3779B1)) (local.get $b)))
(func $js_skel (param $x i32)
  (global.set $js_skel_hi (i32.or (i32.shl (global.get $js_skel_hi) (i32.const 8)) (i32.shr_u (global.get $js_skel_lo) (i32.const 24))))
  (global.set $js_skel_lo (i32.or (i32.shl (global.get $js_skel_lo) (i32.const 8)) (local.get $x))))
(func $js_end_token
  (if (global.get $js_kind) (then
    (call $map_set (global.get $js_prev) (global.get $js_token))
    (global.set $js_prev (global.get $js_token))
    (call $js_skel (global.get $js_kind))
    (global.set $js_kind (i32.const 0)))))
;; The context of the lexer for the mixer: the bracket, the class of the token and the last punctuation
(func $js_context (result i32) (local $open i32)
  (local.set $open (i32.load (i32.add (i32.const {FRAMES}) (i32.shl (global.get $js_depth) (i32.const 3)))))
  (i32.or (i32.shl (i32.or (i32.shl
    (select (i32.const 1) (select (i32.const 2) (select (i32.const 3) (i32.const 0) (i32.eq (local.get $open) (i32.const 123))) (i32.eq (local.get $open) (i32.const 91))) (i32.eq (local.get $open) (i32.const 40)))
    (i32.const 2)) (global.get $js_kind)) (i32.const 8)) (global.get $js_punct)))
(func $lex (param $c i32) (local $class i32)
  (global.set $js_run (select (i32.const 0) (i32.add (global.get $js_run) (i32.const 1)) (i32.lt_u (local.get $c) (i32.const 9))))
  (local.set $class (call $char_class (local.get $c)))
  (if (global.get $js_quote)
    (then (if (i32.eq (local.get $c) (global.get $js_quote))
      (then (global.set $js_quote (i32.const 0)) (call $js_end_token))
      (else (global.set $js_token (call $js_mix (global.get $js_token) (local.get $c))))))
    (else (if (i32.or (i32.lt_u (i32.sub (local.get $class) (i32.const 1)) (i32.const 2)) (i32.and (i32.eq (local.get $c) (i32.const 46)) (i32.eq (global.get $js_kind) (i32.const 2))))
      (then
        (if (i32.eqz (global.get $js_kind)) (then
          (global.set $js_kind (select (i32.const 2) (i32.const 1) (i32.eq (local.get $class) (i32.const 2))))
          (global.set $js_token (global.get $js_kind))))
        (global.set $js_token (call $js_mix (global.get $js_token) (local.get $c))))
      (else
        (call $js_end_token)
        (if (i32.eq (local.get $class) (i32.const 4))
          (then (global.set $js_quote (local.get $c)) (global.set $js_token (i32.const 3)) (global.set $js_kind (i32.const 3)))
          (else (if (i32.ne (local.get $class) (i32.const 3))
            (then
              (global.set $js_punct (local.get $c))
              (call $js_skel (local.get $c))
              (if (i32.eq (local.get $class) (i32.const 5))
                (then
                  (global.set $js_depth (i32.add (global.get $js_depth) (i32.const 1)))
                  (i64.store (i32.add (i32.const {FRAMES}) (i32.shl (global.get $js_depth) (i32.const 3))) (i64.extend_i32_u (local.get $c))))
                (else (if (i32.and (i32.eq (local.get $class) (i32.const 6)) (i32.ne (global.get $js_depth) (i32.const 0)))
                  (then (global.set $js_depth (i32.sub (global.get $js_depth) (i32.const 1))))
                  (else (if (i32.eq (local.get $c) (i32.const 58))
                    (then (i32.store offset=4 (i32.add (i32.const {FRAMES}) (i32.shl (global.get $js_depth) (i32.const 3))) (global.get $js_prev)))))))))))))))))
"#;

/// The map of the token after each token, for JsContext variant 1: an open addressing table of
/// key, value and whether it is used
const JS_NEXT_TOKEN_MAP: &str = r#"
(func $map_find (param $key i32) (result i32) (local $i i32) (local $at i32)
  (local.set $i (i32.shr_u (i32.mul (local.get $key) (i32.const 0x9E3779B1)) (i32.const {MAPSHIFT})))
  (loop $probe
    (if (i32.load offset=8 (local.tee $at (i32.add (i32.const {MAP}) (i32.mul (local.get $i) (i32.const 12))))) (then
      (if (i32.ne (i32.load (local.get $at)) (local.get $key)) (then
        (local.set $i (i32.and (i32.add (local.get $i) (i32.const 1)) (i32.const {MAPMASK})))
        (br $probe))))))
  (local.get $at))
(func $map_get (param $key i32) (result i32) (i32.load offset=4 (call $map_find (local.get $key))))
(func $map_set (param $key i32) (param $value i32) (local $at i32)
  (i32.store (local.tee $at (call $map_find (local.get $key))) (local.get $key))
  (i32.store offset=4 (local.get $at) (local.get $value))
  (i32.store offset=8 (local.get $at) (i32.const 1)))
"#;

/// Match models, as match_model.js. A record of 64 bytes each: 0 min length, 4 candidates, 8 flags,
/// 12 match pointer, 16 active, 20 length, 24 misses, 28 whether the byte missed, 32 address of
/// the counter, 36 expected bit, 40 state, 44 address of the inputs, 48 head, 52 chain, 56 probs,
/// 60 counts, and the address of the state for the mixer at 0 of the next... see $mm_new
const MATCH: &str = r#"
(global $mr (mut i32) (i32.const {MM0}))
(global $tp (mut i32) (i32.const {MM_REGION}))
(func $tables (param $size i32) (result i32) (local $at i32)
  (local.set $at (global.get $tp))
  (global.set $tp (i32.add (local.get $at) (local.get $size)))
  (local.get $at))
(func $mm_new (param $min i32) (param $cand i32) (param $flags i32) (local $r i32) (local $probs i32)
  (global.set $mr (i32.add (local.tee $r (global.get $mr)) (i32.const 64)))
  (i32.store (local.get $r) (local.get $min))
  (i32.store offset=4 (local.get $r) (local.get $cand))
  (i32.store offset=8 (local.get $r) (local.get $flags))
  (i32.store offset=44 (local.get $r) (global.get $ip))
  (i32.store offset=48 (local.get $r) (call $tables (i32.const 0x1000000)))
  (i32.store offset=52 (local.get $r) (call $tables (i32.const {MM_CHAIN})))
  (i32.store offset=56 (local.get $r) (local.tee $probs (call $tables (i32.const 0x1200000))))
  (i32.store offset=60 (local.get $r) (call $tables (i32.const 0x480000)))
  (i32.store offset=32 (local.get $r) (global.get $mi))
  (i32.store (local.get $probs) (i32.const 0x7fffff))
  (call $rep (local.get $probs) (i32.const 4) (i32.const 0x1200000))
  (global.set $ip (i32.add (global.get $ip) (i32.const {MATCH_STRIDE})))
  (global.set $mi (i32.add (global.get $mi) (i32.const 4))))
(func $mm_p (local $r i32) (local $e i32) (local $len i32) (local $log i32) (local $state i32) (local $p f64) (local $in i32) (local $i i32)
  (local.set $r (i32.const {MM0}))
  (loop $model
    (local.set $in (i32.load offset=44 (local.get $r)))
    (local.set $state (i32.const 0))
    (local.set $p (f64.const 0))
    (if (i32.and (i32.ne (i32.load offset=16 (local.get $r)) (i32.const 0)) (i32.eqz (i32.load offset=28 (local.get $r)))) (then
      (local.set $e (i32.load8_u offset={OUT} (i32.load offset=12 (local.get $r))))
      (local.set $log (i32.sub (i32.const 31) (i32.clz (local.tee $len (i32.load offset=20 (local.get $r))))))
      (i32.store offset=36 (local.get $r) (local.tee $i (i32.and (i32.shr_u (local.get $e) (i32.sub (i32.const 7) (global.get $bits))) (i32.const 1))))
      (local.set $state (i32.add (i32.add (i32.const 1) (local.get $i))
        (i32.shl (select (select (i32.add (local.get $log) (i32.const 1)) (i32.const 12) (i32.lt_s (local.get $log) (i32.const 11))) (i32.const 0) (local.get $len)) (i32.const 1))))
      (local.set $i (i32.or (i32.shl (i32.or (i32.shl
          (if (result i32) (i32.eqz (local.get $len))
            (then (i32.add (i32.const 63) (i32.load offset=24 (local.get $r))))
            (else (if (result i32) (i32.lt_u (local.get $len) (i32.const 16))
              (then (local.get $len))
              (else (i32.add (i32.add (i32.const 16) (i32.shl (i32.sub (local.get $log) (i32.const 4)) (i32.const 2)))
                (i32.and (i32.shr_u (local.get $len) (i32.sub (local.get $log) (i32.const 2))) (i32.const 3)))))))
          (i32.const 8)) (select (local.get $e) (i32.const 0) (i32.and (i32.load offset=8 (local.get $r)) (i32.const 1)))) (i32.const 8)) (global.get $bc)))
      (i32.store offset=40 (local.get $r) (local.get $i))
      (local.set $p (call $stretch (f64.div (f64.convert_i32_s (i32.load (i32.add (i32.load offset=56 (local.get $r)) (i32.shl (local.get $i) (i32.const 2))))) {U24F})))
      (if (i32.eqz (i32.load offset=36 (local.get $r))) (then (local.set $p (f64.neg (local.get $p)))))))
    (f64.store (local.get $in) (local.get $p))
    {MM_INPUT}
    (br_if $model (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 64))) (i32.const {MMEND})))))
(func $mm_l (param $bit i32) (local $r i32) (local $i i32) (local $count i32) (local $pos i32) (local $hash i32) (local $slot i32) (local $cand i32) (local $n i32) (local $best i32) (local $bestPtr i32) (local $first i32) (local $l i32) (local $min i32) (local $chain i32)
  (local.set $r (i32.const {MM0}))
  (loop $model
    (if (i32.and (i32.ne (i32.load offset=16 (local.get $r)) (i32.const 0)) (i32.eqz (i32.load offset=28 (local.get $r)))) (then
      (i32.store8 (local.tee $i (i32.add (i32.load offset=60 (local.get $r)) (i32.load offset=40 (local.get $r))))
        (local.tee $count (select (local.tee $count (i32.add (i32.load8_u (local.get $i)) (i32.const 1))) (i32.const 255) (i32.lt_u (local.get $count) (i32.const 255)))))
      (i32.store (local.tee $i (i32.add (i32.load offset=56 (local.get $r)) (i32.shl (i32.load offset=40 (local.get $r)) (i32.const 2))))
        (i32.add (local.tee $n (i32.load (local.get $i))) (i32.trunc_f64_s (f64.mul {U24F} (f64.div
          (f64.sub (f64.convert_i32_u (local.tee $l (i32.eq (local.get $bit) (i32.load offset=36 (local.get $r))))) (f64.div (f64.convert_i32_s (local.get $n)) {U24F}))
          (f64.add (f64.convert_i32_u (local.get $count)) (f64.const 0.2)))))))
      (if (i32.eqz (local.get $l)) (then
        (i32.store offset=20 (local.get $r) (i32.const 0))
        (i32.store offset=24 (local.get $r) (local.tee $n (i32.add (i32.load offset=24 (local.get $r)) (i32.const 1))))
        (if (i32.lt_u (local.get $n) (i32.const 8))
          (then (i32.store offset=28 (local.get $r) (i32.const 1)))
          (else (i32.store offset=16 (local.get $r) (i32.const 0))))))))
    (if (i32.eq (global.get $bits) (i32.const 7)) (then
      (local.set $pos (global.get $pos))
      (i32.store offset=28 (local.get $r) (i32.const 0))
      (if (i32.load offset=16 (local.get $r)) (then
        (if (local.tee $n (i32.load offset=20 (local.get $r))) (then
          (i32.store offset=20 (local.get $r) (select (local.tee $n (i32.add (local.get $n) (i32.const 1))) (i32.const 65535) (i32.lt_u (local.get $n) (i32.const 65535))))))
        (i32.store offset=12 (local.get $r) (i32.add (i32.load offset=12 (local.get $r)) (i32.const 1)))))
      (local.set $chain (i32.add (i32.load offset=52 (local.get $r)) (i32.shl (local.get $pos) (i32.const 2))))
      (if (i32.lt_u (local.get $pos) (local.tee $min (i32.load (local.get $r))))
        (then (i32.store (local.get $chain) (i32.const 0)))
        (else
          (local.set $i (i32.sub (local.get $pos) (local.get $min)))
          (local.set $hash (i32.const 0))
          (loop $h
            (local.set $hash (i32.add (i32.mul (i32.xor (local.get $hash) (i32.load8_u offset={OUT} (local.get $i))) (i32.const 0x2F0F3D1)) (i32.const 1)))
            (br_if $h (i32.ne (local.tee $i (i32.add (local.get $i) (i32.const 1))) (local.get $pos))))
          (local.set $slot (i32.add (i32.load offset=48 (local.get $r)) (i32.shl (i32.and (i32.shr_u (local.get $hash) (i32.const 8)) (i32.const 0x3FFFFF)) (i32.const 2))))
          (if (i32.eqz (i32.load offset=20 (local.get $r))) (then
            (local.set $cand (i32.load (local.get $slot)))
            (local.set $best (i32.const 0))
            (local.set $bestPtr (i32.const 0))
            (local.set $first (i32.const -1))
            (local.set $n (i32.const 0))
            (block $done (loop $candidate
              (br_if $done (i32.ge_u (local.get $n) (i32.load offset=4 (local.get $r))))
              (br_if $done (i32.eqz (local.get $cand)))
              (local.set $l (i32.const 0))
              (block $end (loop $compare
                (br_if $end (i32.ge_u (local.get $l) (i32.const 65535)))
                (br_if $end (i32.ge_u (local.get $l) (local.get $cand)))
                (br_if $end (i32.ne (i32.load8_u offset={OUT} (i32.sub (i32.sub (local.get $cand) (i32.const 1)) (local.get $l)))
                  (i32.load8_u offset={OUT} (i32.sub (i32.sub (local.get $pos) (i32.const 1)) (local.get $l)))))
                (local.set $l (i32.add (local.get $l) (i32.const 1)))
                (br $compare)))
              (if (i32.and (i32.load offset=8 (local.get $r)) (i32.const 2))
                (then (if (i32.ge_u (local.get $l) (local.get $min)) (then
                  (if (i32.lt_s (local.get $first) (i32.const 0))
                    (then (local.set $first (i32.load8_u offset={OUT} (local.get $cand))))
                    (else (if (i32.ne (local.get $first) (i32.load8_u offset={OUT} (local.get $cand))) (then
                      (local.set $best (local.get $l))
                      (local.set $bestPtr (local.get $cand))
                      (br $done))))))))
                (else (if (i32.gt_u (local.get $l) (local.get $best)) (then
                  (local.set $best (local.get $l))
                  (local.set $bestPtr (local.get $cand))))))
              (local.set $cand (i32.load (i32.add (i32.load offset=52 (local.get $r)) (i32.shl (local.get $cand) (i32.const 2)))))
              (local.set $n (i32.add (local.get $n) (i32.const 1)))
              (br $candidate)))
            (if (i32.ge_u (local.get $best) (local.get $min)) (then
              (i32.store offset=20 (local.get $r) (local.get $best))
              (i32.store offset=12 (local.get $r) (local.get $bestPtr))
              (i32.store offset=16 (local.get $r) (i32.const 1))
              (i32.store offset=24 (local.get $r) (i32.const 0))))))
          (i32.store (local.get $chain) (i32.load (local.get $slot)))
          (i32.store (local.get $slot) (local.get $pos))))))
    (br_if $model (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 64))) (i32.const {MMEND})))))
"#;

/// Match predictors, as match_predictor.js: a record of 48 bytes each: 0 address of the keys
/// (followed by the positions), 4 mask of the table, 8 mask of the context, 16 context, 20 match
/// (-1 for none), 24 address of the input, 32 and 40 the stretched probabilities of the expected
/// bit and of the other
const MATCH_PREDICTOR: &str = r#"
(func $mp_new (param $r i32) (param $mask i32) (param $keys i32) (param $table i32) (param $p1 f64) (param $p0 f64)
  (i32.store (local.get $r) (local.get $keys))
  (i32.store offset=4 (local.get $r) (local.get $table))
  (i32.store offset=8 (local.get $r) (local.get $mask))
  (i32.store offset=20 (local.get $r) (i32.const -1))
  (i32.store offset=24 (local.get $r) (global.get $ip))
  (f64.store offset=32 (local.get $r) (local.get $p1))
  (f64.store offset=40 (local.get $r) (local.get $p0))
  (global.set $ip (i32.add (global.get $ip) (i32.const 8)))
  (global.set $mi (i32.add (global.get $mi) (i32.const 4))))
;; The bit of the byte that followed the last occurrence of the context, within the last 64 KiB
(func $mp_expected (param $r i32) (result i32)
  (i32.and (i32.shr_u (i32.load8_u offset={OUT} (i32.load offset=20 (local.get $r))) (i32.sub (i32.const 7) (global.get $bits))) (i32.const 1)))
(func $mp_p (local $r i32)
  (local.set $r (i32.const {MP0}))
  (loop $model
    (f64.store (i32.load offset=24 (local.get $r))
      (if (result f64) (i32.lt_s (i32.load offset=20 (local.get $r)) (i32.const 0))
        (then (f64.const 0))
        (else (f64.load offset=32 (i32.add (local.get $r) (i32.shl (i32.eqz (call $mp_expected (local.get $r))) (i32.const 3)))))))
    (br_if $model (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 48))) (i32.const {MPEND})))))
(func $mp_l (param $bit i32) (local $r i32) (local $ctx i32) (local $slot i32) (local $prior i32) (local $m i32)
  (local.set $r (i32.const {MP0}))
  (loop $model
    (if (i32.ge_s (i32.load offset=20 (local.get $r)) (i32.const 0)) (then
      (if (i32.ne (call $mp_expected (local.get $r)) (local.get $bit)) (then (i32.store offset=20 (local.get $r) (i32.const -1))))))
    (if (i32.eq (global.get $bits) (i32.const 7)) (then
      (i32.store offset=16 (local.get $r) (local.tee $ctx (i32.and (i32.or (i32.shl (i32.load offset=16 (local.get $r)) (i32.const 8)) (i32.and (global.get $L) (i32.const 255))) (i32.load offset=8 (local.get $r)))))
      (local.set $slot (i32.add (i32.load (local.get $r)) (i32.shl (i32.and (i32.mul (local.get $ctx) (i32.const 0x9E35A7BD)) (i32.load offset=4 (local.get $r))) (i32.const 2))))
      (local.set $prior (i32.load (i32.add (local.get $slot) (i32.shl (i32.add (i32.load offset=4 (local.get $r)) (i32.const 1)) (i32.const 2)))))
      (if (i32.ge_s (local.tee $m (i32.load offset=20 (local.get $r))) (i32.const 0))
        (then (i32.store offset=20 (local.get $r) (i32.add (local.get $m) (i32.const 1))))
        (else (if (i32.and (i32.and (i32.ne (local.get $prior) (i32.const 0)) (i32.eq (i32.load (local.get $slot)) (local.get $ctx)))
            (i32.lt_s (i32.sub (global.get $pos) (local.get $prior)) (i32.const 65536)))
          (then (i32.store offset=20 (local.get $r) (i32.sub (local.get $prior) (i32.const 1)))))))
      (i32.store (local.get $slot) (local.get $ctx))
      (i32.store (i32.add (local.get $slot) (i32.shl (i32.add (i32.load offset=4 (local.get $r)) (i32.const 1)) (i32.const 2))) (i32.add (global.get $pos) (i32.const 1)))))
    (br_if $model (i32.ne (local.tee $r (i32.add (local.get $r) (i32.const 48))) (i32.const {MPEND})))))
"#;

/// What happens at the end of each bit and byte. The models read the decoded bytes, so the byte is
/// written and counted before they learn its last bit.
const LEARN_BYTE: &str = r#"
(func $learn_byte (param $bit i32) (local $b i32)
  (if (i32.eq (global.get $bits) (i32.const 7)) (then
    (i32.store8 offset={OUT} (global.get $pos) (local.tee $b (i32.and (i32.or (i32.shl (global.get $bc) (i32.const 1)) (local.get $bit)) (i32.const 255))))
    (global.set $pos (i32.add (global.get $pos) (i32.const 1)))
    (global.set $H (i32.or (i32.shl (global.get $H) (i32.const 8)) (i32.shr_u (global.get $L) (i32.const 24))))
    (global.set $L (i32.or (i32.shl (global.get $L) (i32.const 8)) (local.get $b)))
    {BYTE_END})))
(func $next_bit (param $bit i32)
  (global.set $bc (i32.or (i32.shl (global.get $bc) (i32.const 1)) (local.get $bit)))
  (global.set $bits (i32.add (global.get $bits) (i32.const 1)))
  (if (i32.gt_u (global.get $bc) (i32.const 255)) (then
    (global.set $bc (i32.const 1))
    (global.set $bits (i32.const 0)))))
"#;

/// The Mixer of mixer.js: weights for all bits, plus weights per previous byte and bits so far
const PLAIN_MIXER: &str = r#"
(global $total (mut f64) (f64.const 0))
(func $p (result f64) (local $i i32) (local $j i32) (local $sum f64) (local $present i32)
  {PREDICT}
  (local.set $present (i32.load8_u offset={PRESENT} (local.tee $i (i32.or (i32.shl (i32.and (global.get $L) (i32.const 255)) (i32.const 8)) (global.get $bc)))))
  (local.set $i (i32.mul (local.get $i) (i32.const {N8})))
  (loop $input
    (local.set $sum (f64.add (local.get $sum) (f64.mul (f64.load offset={INPUTS} (local.get $j))
      (if (result f64) (local.get $present)
        (then (f64.add (f64.mul (f64.load offset={CWEIGHTS} (local.get $i)) {CSCALE}) (f64.load offset={WEIGHTS} (local.get $j))))
        (else (f64.load offset={WEIGHTS} (local.get $j)))))))
    (local.set $i (i32.add (local.get $i) (i32.const 8)))
    (br_if $input (i32.ne (local.tee $j (i32.add (local.get $j) (i32.const 8))) (i32.const {N8}))))
  (local.get $sum))
(func $q (param f64) (result f64) (global.set $total (call $squash (local.get 0))) (global.get $total))
(func $l (param $bit i32) (local $i i32) (local $j i32) (local $err f64) (local $x f64)
  (local.set $err (f64.sub (f64.convert_i32_u (local.get $bit)) (global.get $total)))
  (local.set $i (i32.mul (local.tee $j (i32.or (i32.shl (i32.and (global.get $L) (i32.const 255)) (i32.const 8)) (global.get $bc))) (i32.const {N8})))
  ;; The weights of the context start as a copy of the weights for all bits
  (if (i32.eqz (i32.load8_u offset={PRESENT} (local.get $j))) (then
    (i32.store8 offset={PRESENT} (local.get $j) (i32.const 1))
    (memory.copy (i32.add (i32.const {CWEIGHTS}) (local.get $i)) (i32.const {WEIGHTS}) (i32.const {N8}))))
  (call $learn_byte (local.get $bit))
  {LEARN_MODELS}
  (local.set $j (i32.const 0))
  (loop $input
    (local.set $x (f64.load offset={INPUTS} (local.get $j)))
    (f64.store offset={WEIGHTS} (local.get $j) (f64.add (f64.load offset={WEIGHTS} (local.get $j)) (f64.mul (f64.mul {LR} (local.get $err)) (local.get $x))))
    (f64.store offset={CWEIGHTS} (local.get $i) (f64.add (f64.load offset={CWEIGHTS} (local.get $i)) (f64.mul (f64.mul {CLR} (local.get $err)) (local.get $x))))
    (local.set $i (i32.add (local.get $i) (i32.const 8)))
    (br_if $input (i32.ne (local.tee $j (i32.add (local.get $j) (i32.const 8))) (i32.const {N8}))))
  (call $next_bit (local.get $bit)))
"#;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::compressor::compress_config::CompressConfig;
    use crate::compressor::model_finder::{
        create_default_compress_config, SIZE_64K_CONFIG_JSON, SIZE_64K_FAST_CONFIG_JSON,
    };
    use crate::compressor::utils::prob_squash;
    use std::path::PathBuf;
    use std::process::Command;

    /// Compares the predictions of the WebAssembly decoder with those of the encoder, bit for bit
    pub fn check_predictions(name: &str, config: &CompressConfig, input: &[u8], layout: &StreamLayout, simd: bool) {
        let mut model = config.create_model_for(layout).unwrap();
        let mut expected = Vec::new();
        for byte in input {
            for i in (0..8).rev() {
                let pred = model.pred();
                expected.extend_from_slice(&pred.to_le_bytes());
                expected.extend_from_slice(&prob_squash(pred).to_le_bytes());
                model.learn((byte >> i) & 1);
            }
        }
        let decoder = generate(
            &config.with_stream_models(layout),
            &config.static_model_params,
            layout,
            0,
            input.len(),
            WasmOptions { simd, test_exports: true },
        )
        .unwrap();
        let dir = PathBuf::from(format!("testout/wasm_predictions_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input.bin"), input).unwrap();
        std::fs::write(dir.join("decoder.wasm"), &decoder.bytes).unwrap();
        let script = r#"
import fs from 'fs';
let dir = process.argv[2];
let input = fs.readFileSync(dir + '/input.bin');
let { instance } = await WebAssembly.instantiate(fs.readFileSync(dir + '/decoder.wasm'));
let { p, q, l } = instance.exports;
let preds = new Float64Array(input.length * 16), n = 0;
for (let byte of input) {
    for (let i = 7; i >= 0; i--) {
        let pred = p();
        preds[n++] = pred;
        preds[n++] = q(pred);
        l((byte >> i) & 1);
    }
}
fs.writeFileSync(dir + '/preds.bin', new Uint8Array(preds.buffer));
"#;
        std::fs::write(dir.join("predict.mjs"), script).unwrap();
        assert!(Command::new("node")
            .arg(dir.join("predict.mjs"))
            .arg(&dir)
            .status()
            .expect("Failed to run node")
            .success());
        let actual = std::fs::read(dir.join("preds.bin")).unwrap();
        assert_eq!(actual.len(), expected.len());
        if let Some(index) =
            (0..expected.len() / 8).find(|i| expected[i * 8..i * 8 + 8] != actual[i * 8..i * 8 + 8])
        {
            let value = |data: &[u8]| f64::from_le_bytes(data[index * 8..][..8].try_into().unwrap());
            panic!(
                "{name}: the wasm {} of bit {} (byte {}) is {:e}, the Rust one {:e}",
                ["prediction", "squashed prediction"][index % 2],
                index / 2,
                index / 16,
                value(&actual),
                value(&expected),
            );
        }
    }

    #[test]
    fn smoke() {
        let input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        for (name, json) in [("64k_fast", SIZE_64K_FAST_CONFIG_JSON), ("64k", SIZE_64K_CONFIG_JSON)] {
            let mut config: CompressConfig = serde_json::from_str(json).unwrap();
            config.static_model_params.hash_table_pow2_size = 20;
            check_predictions(name, &config, &input, &StreamLayout::default(), false);
        }
        check_predictions("4k", &create_default_compress_config(), &input, &StreamLayout::default(), false);
    }
}

#[cfg(test)]
mod single_model_tests {
    use super::tests::check_predictions;
    use crate::compressor::compress_config::{CompressConfig, StreamLayout};

    fn mixer_of(models: &str, selectors: &str, apms: &str) -> CompressConfig {
        serde_json::from_str(&format!(
            r#"{{"static_model_params":{{"hash_table_pow2_size":18,"count_offset":0.1,"context_input_scale":1.75}},
            "model":{{"type":"ContextMixer","models":[{models}],"selectors":[{selectors}],"learning_rate":0.0056,
            "final_learning_rate":0.0012,"init_weight":0.02,"lr_boost":3.0,"apms":[{apms}]}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn each_model() {
        let input = std::fs::read("tests/ray_tracer/index.js").unwrap();
        let input = &input[..4000];
        for (name, model) in [
            ("norder", r#"{"type":"NOrderByte","byte_mask":"0b00000011","max_count":255}"#),
            ("word", r#"{"type":"Word"}"#),
            ("char_class", r#"{"type":"CharClass","order":3,"max_count":30}"#),
            ("js0", r#"{"type":"JsContext","variant":0,"max_count":255}"#),
            ("js1", r#"{"type":"JsContext","variant":1,"max_count":255}"#),
            ("js2", r#"{"type":"JsContext","variant":2,"max_count":255}"#),
            ("js3", r#"{"type":"JsContext","variant":3,"max_count":255}"#),
            ("match", r#"{"type":"Match","min_len":6,"candidates":1,"expected_byte_context":false,"second":false}"#),
            ("match2", r#"{"type":"Match","min_len":2,"candidates":32,"expected_byte_context":true,"second":false}"#),
            ("match_second", r#"{"type":"Match","min_len":1,"candidates":8,"expected_byte_context":true,"second":true}"#),
            ("token", r#"{"type":"TokenPosition"}"#),
            ("indirect", r#"{"type":"IndirectByte","context_bytes":2,"max_count":15}"#),
            ("indirect1", r#"{"type":"IndirectByte","context_bytes":1,"max_count":15}"#),
            ("match_predictor", r#"{"type":"MatchPredictor","context_bytes":2,"confidence":0.97,"table_bits":16}"#),
            ("match_predictor4", r#"{"type":"MatchPredictor","context_bytes":4,"confidence":0.9,"table_bits":12}"#),
        ] {
            let config = mixer_of(model, r#""None""#, "");
            check_predictions(&format!("single_{name}"), &config, input, &StreamLayout::default(), false);
        }
    }
}
