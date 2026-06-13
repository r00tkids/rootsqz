use anyhow::{bail, Context, Result};

use crate::compressor::compress_config::ModelConfig;

pub const NORDER_RECORD_BYTES: usize = 4;
const MIXER_CONTEXT_ROWS: usize = 256 * 255;

pub fn render_model_assembly(model_config: &ModelConfig, table_pow2: u32) -> Result<String> {
    let mut generator = ModelAssemblyGenerator::new(table_pow2);
    let root = generator.render_model(model_config, "_rootsqz_model_ctx")?;
    Ok(generator.finish(root))
}

struct ModelAssemblyGenerator {
    src: String,
    next_id: usize,
    table_pow2: u32,
    uses_norder_table: bool,
}

struct RenderedModel {
    ctx_symbol: String,
    pred_symbol: &'static str,
    learn_symbol: &'static str,
}

impl ModelAssemblyGenerator {
    fn new(table_pow2: u32) -> Self {
        Self {
            src: String::new(),
            next_id: 0,
            table_pow2,
            uses_norder_table: false,
        }
    }

    fn finish(mut self, root: RenderedModel) -> String {
        self.src.push_str(&format!(
            r#"
.text
.align 2
.private_extern _rootsqz_model_predict
_rootsqz_model_predict:
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp
    bl      {pred}
    bl      _rootsqz_prob_squash
    ldp     x29, x30, [sp], #16
    ret

.private_extern _rootsqz_model_learn
_rootsqz_model_learn:
    b       {learn}
"#,
            pred = root.pred_symbol,
            learn = root.learn_symbol,
        ));

        if self.uses_norder_table {
            let table_bytes = (1usize << self.table_pow2) * NORDER_RECORD_BYTES;
            self.src.push_str(&format!(
                r#"
.zerofill __DATA,__bss,_rootsqz_norder_table,{table_bytes},2
"#
            ));
        }

        self.src
    }

    fn render_model(
        &mut self,
        model_config: &ModelConfig,
        ctx_symbol: &str,
    ) -> Result<RenderedModel> {
        match model_config {
            ModelConfig::NOrderByte { byte_mask } => {
                let byte_mask = u8::from_str_radix(byte_mask.trim_start_matches("0b"), 2)
                    .with_context(|| format!("Invalid NOrderByte mask {byte_mask}"))?;
                self.render_norder_model(ctx_symbol, byte_mask, false)
            }
            ModelConfig::Word => self.render_norder_model(ctx_symbol, 0, true),
            ModelConfig::Mixer { models } => self.render_mixer_model(ctx_symbol, models),
            ModelConfig::AdaptiveProbabilityMap(_) => {
                bail!("Mach-O assembly decompressor does not support AdaptiveProbabilityMap yet")
            }
        }
    }

    fn render_norder_model(
        &mut self,
        ctx_symbol: &str,
        byte_mask: u8,
        is_word: bool,
    ) -> Result<RenderedModel> {
        self.uses_norder_table = true;
        let table_len = 1usize << self.table_pow2;
        let (magic_num, prev_bytes, mask, pred_symbol, learn_symbol, is_word_flag) = if is_word {
            (
                model_hash(1337, 2),
                2166136261u64,
                u64::MAX,
                "_rootsqz_word_predict",
                "_rootsqz_word_learn",
                1u32,
            )
        } else {
            (
                model_hash(byte_mask as u32, 2),
                0u64,
                byte_mask_to_context_mask(byte_mask),
                "_rootsqz_norder_byte_predict",
                "_rootsqz_norder_byte_learn",
                0u32,
            )
        };

        self.src.push_str(".section __DATA,__data\n.p2align 3\n");
        if ctx_symbol.starts_with('_') {
            self.src
                .push_str(&format!(".private_extern {ctx_symbol}\n"));
        }
        self.src.push_str(&format!(
            r#"{ctx_symbol}:
    .long 0
    .long 1
    .long 0x{magic_num:08x}
    .long 15
    .quad 0x{prev_bytes:016x}
    .quad 0x{mask:016x}
    .long {is_word_flag}
    .long 0
    .quad _rootsqz_norder_table
    .quad {hash_mask}
"#,
            hash_mask = table_len - 1,
        ));

        Ok(RenderedModel {
            ctx_symbol: ctx_symbol.to_owned(),
            pred_symbol,
            learn_symbol,
        })
    }

    fn render_mixer_model(
        &mut self,
        ctx_symbol: &str,
        models: &[ModelConfig],
    ) -> Result<RenderedModel> {
        if models.is_empty() {
            bail!("Mach-O assembly decompressor cannot render an empty mixer")
        }

        let mixer_id = self.alloc_id();
        let child_ctxs_symbol = format!("L_rootsqz_mixer_{mixer_id}_child_contexts");
        let pred_fns_symbol = format!("L_rootsqz_mixer_{mixer_id}_predict_fns");
        let learn_fns_symbol = format!("L_rootsqz_mixer_{mixer_id}_learn_fns");
        let base_weights_symbol = format!("L_rootsqz_mixer_{mixer_id}_base_weights");
        let ctx_weights_symbol = format!("L_rootsqz_mixer_{mixer_id}_ctx_weights");
        let ctx_init_symbol = format!("L_rootsqz_mixer_{mixer_id}_ctx_init");
        let last_p_symbol = format!("L_rootsqz_mixer_{mixer_id}_last_p");

        let mut children = Vec::with_capacity(models.len());
        for model in models {
            let child_symbol = format!("L_rootsqz_mixer_{mixer_id}_child_{}", children.len());
            children.push(self.render_model(model, &child_symbol)?);
        }

        let num_models = children.len();
        let ctx_weight_bytes = MIXER_CONTEXT_ROWS * num_models * size_of::<f64>();
        let initial_weight = 1.0 / num_models as f64;

        self.src.push_str(".section __DATA,__data\n.p2align 3\n");
        if ctx_symbol.starts_with('_') {
            self.src
                .push_str(&format!(".private_extern {ctx_symbol}\n"));
        }
        self.src.push_str(&format!(
            r#"{ctx_symbol}:
    .long {num_models}
    .long 1
    .long 0
    .long 0
    .double 0.0
    .quad {child_ctxs_symbol}
    .quad {pred_fns_symbol}
    .quad {learn_fns_symbol}
    .quad {base_weights_symbol}
    .quad {ctx_weights_symbol}
    .quad {ctx_init_symbol}
    .quad {last_p_symbol}

.p2align 3
{child_ctxs_symbol}:
"#
        ));
        for child in &children {
            self.src
                .push_str(&format!("    .quad {}\n", child.ctx_symbol));
        }

        self.src.push_str(&format!("{pred_fns_symbol}:\n"));
        for child in &children {
            self.src
                .push_str(&format!("    .quad {}\n", child.pred_symbol));
        }

        self.src.push_str(&format!("{learn_fns_symbol}:\n"));
        for child in &children {
            self.src
                .push_str(&format!("    .quad {}\n", child.learn_symbol));
        }

        self.src.push_str(&format!("{base_weights_symbol}:\n"));
        for _ in &children {
            self.src
                .push_str(&format!("    .double {:.17}\n", initial_weight));
        }

        self.src.push_str(&format!(
            r#"{last_p_symbol}:
    .space {last_p_bytes}

.section __DATA,__bss
.p2align 3
{ctx_init_symbol}:
    .space {ctx_init_bytes}
.p2align 3
{ctx_weights_symbol}:
    .space {ctx_weight_bytes}
"#,
            last_p_bytes = num_models * size_of::<f64>(),
            ctx_init_bytes = MIXER_CONTEXT_ROWS,
        ));

        Ok(RenderedModel {
            ctx_symbol: ctx_symbol.to_owned(),
            pred_symbol: "_rootsqz_ln_mixer_predict_stretched",
            learn_symbol: "_rootsqz_ln_mixer_learn",
        })
    }

    fn alloc_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

pub fn model_hash(mut value: u32, shift: u32) -> u32 {
    value ^= value >> shift;
    0x9E35_A7BDu32.wrapping_mul(value) >> shift
}

pub fn byte_mask_to_context_mask(byte_mask: u8) -> u64 {
    let mut bit_mask = 0u64;
    for i in 0..8 {
        bit_mask |= ((byte_mask >> i) & 1) as u64 * (0xffu64 << (i * 8));
    }
    bit_mask
}
