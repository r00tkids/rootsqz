//! The last step of the generated WebAssembly decoder: its locals coalesced, like binaryen's
//! coalesce-locals and reorder-locals. Two locals of a type share an index when one is never
//! written while the other holds a value that is read later, and the most used indices come
//! first. Fewer and smaller local indices make the code more alike from function to function,
//! which deflate likes. V8 allocates registers itself, so the speed stays the same.

use anyhow::{bail, ensure, Result};
use wasmparser::{FunctionBody, Operator, Parser, Payload, ValType};

fn leb(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 128);
    }
}

fn section(out: &mut Vec<u8>, id: u8, contents: &[u8]) {
    out.push(id);
    leb(out, contents.len() as u32);
    out.extend_from_slice(contents);
}

/// The module without its custom sections (unless `keep_custom`), with the locals of each
/// function coalesced
pub fn finish(bytes: &[u8], keep_custom: bool) -> Result<Vec<u8>> {
    let mut out = bytes[..8].to_vec();
    // The parameters of each type, and the type of each function
    let mut types: Vec<Vec<ValType>> = Vec::new();
    let mut funcs: Vec<u32> = Vec::new();
    let mut code = Vec::new();
    let mut index = 0;
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload?;
        match &payload {
            Payload::ImportSection(_) => bail!("The decoder has no imports"),
            Payload::TypeSection(reader) => {
                for group in reader.clone() {
                    for ty in group?.into_types() {
                        types.push(ty.unwrap_func().params().to_vec());
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for func in reader.clone() {
                    funcs.push(func?);
                }
            }
            Payload::CodeSectionStart { count, .. } => {
                ensure!(*count as usize == funcs.len(), "A body for each function");
                leb(&mut code, *count);
                continue;
            }
            Payload::CodeSectionEntry(body) => {
                let params = &types[funcs[index] as usize];
                index += 1;
                let body = coalesce(bytes, body, params)?;
                leb(&mut code, body.len() as u32);
                code.extend(body);
                if index == funcs.len() {
                    section(&mut out, 10, &code);
                }
                continue;
            }
            Payload::CustomSection(_) if !keep_custom => continue,
            _ => {}
        }
        if let Some((id, range)) = payload.as_section() {
            section(&mut out, id, &bytes[range.start as usize..range.end as usize]);
        }
    }
    Ok(out)
}

/// An instruction, as far as locals and control flow go
enum Op {
    Get(u32),
    Set(u32),
    Tee(u32),
    Block,
    Loop,
    If,
    Else,
    End,
    Br(u32),
    BrIf(u32),
    BrTable(Vec<u32>),
    /// return or unreachable
    Exit,
    Other,
}

fn type_byte(ty: ValType) -> Result<u8> {
    Ok(match ty {
        ValType::I32 => 0x7f,
        ValType::I64 => 0x7e,
        ValType::F32 => 0x7d,
        ValType::F64 => 0x7c,
        ValType::V128 => 0x7b,
        _ => bail!("Unexpected local type {ty:?}"),
    })
}

/// The body with its locals coalesced
fn coalesce(bytes: &[u8], body: &FunctionBody, params: &[ValType]) -> Result<Vec<u8>> {
    let mut types = params.to_vec();
    for local in body.get_locals_reader()? {
        let (count, ty) = local?;
        types.extend(std::iter::repeat_n(ty, count as usize));
    }
    let n = types.len();
    ensure!(n <= 128, "A function of the decoder has more than 128 locals");

    // The instructions and their bytes
    let mut ops = Vec::new();
    let mut starts = Vec::new();
    let mut reader = body.get_operators_reader()?;
    while !reader.eof() {
        let (op, offset) = reader.read_with_offset()?;
        starts.push(offset as usize);
        ops.push(match op {
            Operator::LocalGet { local_index } => Op::Get(local_index),
            Operator::LocalSet { local_index } => Op::Set(local_index),
            Operator::LocalTee { local_index } => Op::Tee(local_index),
            Operator::Block { .. } => Op::Block,
            Operator::Loop { .. } => Op::Loop,
            Operator::If { .. } => Op::If,
            Operator::Else => Op::Else,
            Operator::End => Op::End,
            Operator::Br { relative_depth } => Op::Br(relative_depth),
            Operator::BrIf { relative_depth } => Op::BrIf(relative_depth),
            Operator::BrTable { targets } => {
                let mut depths = targets.targets().collect::<Result<Vec<_>, _>>()?;
                depths.push(targets.default());
                Op::BrTable(depths)
            }
            Operator::Return | Operator::Unreachable => Op::Exit,
            Operator::Try { .. } | Operator::TryTable { .. } | Operator::Delegate { .. } => {
                bail!("Unexpected exception handling in the decoder")
            }
            _ => Op::Other,
        });
    }
    let body_end = body.range().end as usize;
    let len = ops.len();

    // The end of each block, loop and if, and the else of each if
    const NONE: usize = usize::MAX;
    let mut ends = vec![NONE; len];
    let mut elses = vec![NONE; len];
    let mut stack = Vec::new();
    for (k, op) in ops.iter().enumerate() {
        match op {
            Op::Block | Op::Loop | Op::If => stack.push(k),
            Op::Else => elses[*stack.last().unwrap()] = k,
            Op::End => {
                if let Some(start) = stack.pop() {
                    ends[start] = k;
                }
            }
            _ => {}
        }
    }

    // The instructions that can run next, NONE for leaving the function. A branch to a block or
    // an if goes to its end, to a loop to its start.
    let mut succs: Vec<Vec<usize>> = Vec::with_capacity(len);
    let mut stack: Vec<usize> = Vec::new();
    let target = |stack: &[usize], depth: u32| -> usize {
        match stack.len().checked_sub(1 + depth as usize) {
            None => NONE,
            Some(i) => match ops[stack[i]] {
                Op::Loop => stack[i],
                _ => ends[stack[i]],
            },
        }
    };
    let next = |k: usize| if k + 1 < len { k + 1 } else { NONE };
    for (k, op) in ops.iter().enumerate() {
        succs.push(match op {
            Op::Block | Op::Loop => {
                stack.push(k);
                vec![k + 1]
            }
            Op::If => {
                stack.push(k);
                vec![k + 1, if elses[k] != NONE { elses[k] + 1 } else { ends[k] }]
            }
            Op::Else => vec![ends[*stack.last().unwrap()]],
            Op::End => {
                stack.pop();
                vec![next(k)]
            }
            Op::Br(depth) => vec![target(&stack, *depth)],
            Op::BrIf(depth) => vec![k + 1, target(&stack, *depth)],
            Op::BrTable(depths) => depths.iter().map(|&depth| target(&stack, depth)).collect(),
            Op::Exit => Vec::new(),
            _ => vec![k + 1],
        });
    }

    // Liveness: the locals whose value may be read later, before and after each instruction
    let bit = |i: u32| 1u128 << i;
    let (uses, defs): (Vec<u128>, Vec<u128>) = ops
        .iter()
        .map(|op| match *op {
            Op::Get(i) => (bit(i), 0),
            Op::Set(i) | Op::Tee(i) => (0, bit(i)),
            _ => (0, 0),
        })
        .unzip();
    let mut live_in = vec![0u128; len];
    let mut live_out = vec![0u128; len];
    loop {
        let mut changed = false;
        for k in (0..len).rev() {
            let out = succs[k].iter().filter(|&&s| s != NONE).fold(0, |acc, &s| acc | live_in[s]);
            let live = uses[k] | (out & !defs[k]);
            if live != live_in[k] || out != live_out[k] {
                changed = true;
                live_in[k] = live;
                live_out[k] = out;
            }
        }
        if !changed {
            break;
        }
    }

    // Two locals interfere if one is written while the other is live. Before the first
    // instruction, the parameters hold their arguments and the other locals 0, so a local that is
    // read before it is written interferes with the parameters.
    let mut interferes = vec![0u128; n];
    for k in 0..len {
        if let Op::Set(d) | Op::Tee(d) = ops[k] {
            let others = live_out[k] & !bit(d);
            interferes[d as usize] |= others;
            for l in 0..n {
                if others & bit(l as u32) != 0 {
                    interferes[l] |= bit(d);
                }
            }
        }
    }
    let params_mask = (0..params.len()).fold(0u128, |acc, p| acc | bit(p as u32));
    for l in params.len()..n {
        if live_in.first().copied().unwrap_or(0) & bit(l as u32) != 0 {
            interferes[l] |= params_mask;
            for p in 0..params.len() {
                interferes[p] |= bit(l as u32);
            }
        }
    }

    // Each local joins the first index of its type whose locals it doesn't interfere with, the
    // most used first. The parameters keep theirs.
    let mut count = vec![0usize; n];
    for op in &ops {
        if let Op::Get(i) | Op::Set(i) | Op::Tee(i) = *op {
            count[i as usize] += 1;
        }
    }
    // The locals of each index, and how often it is used
    let mut slots: Vec<(ValType, u128, usize)> =
        params.iter().enumerate().map(|(p, &ty)| (ty, bit(p as u32), count[p])).collect();
    let mut order: Vec<usize> = (params.len()..n).filter(|&l| count[l] > 0).collect();
    order.sort_by_key(|&l| std::cmp::Reverse(count[l]));
    let mut slot_of = vec![NONE; n];
    for p in 0..params.len() {
        slot_of[p] = p;
    }
    for l in order {
        let free = slots
            .iter()
            .position(|&(ty, members, _)| ty == types[l] && members & interferes[l] == 0);
        let s = free.unwrap_or_else(|| {
            slots.push((types[l], 0, 0));
            slots.len() - 1
        });
        slots[s].1 |= bit(l as u32);
        slots[s].2 += count[l];
        slot_of[l] = s;
    }
    // The new indices: the parameters, then the other indices of each type, the most used first
    let mut rest: Vec<usize> = (params.len()..slots.len()).collect();
    // Grouped by type, so that the locals are declared in few runs
    rest.sort_by_key(|&s| (type_byte(slots[s].0).unwrap_or(0), std::cmp::Reverse(slots[s].2)));
    let mut new_index = vec![0u32; slots.len()];
    for (k, s) in (0..params.len()).chain(rest.iter().copied()).enumerate() {
        new_index[s] = k as u32;
    }

    // The body: the locals as runs of a type, then the instructions with the new indices
    let mut out = Vec::new();
    let mut runs: Vec<(u32, ValType)> = Vec::new();
    for &s in &rest {
        match runs.last_mut() {
            Some((count, ty)) if *ty == slots[s].0 => *count += 1,
            _ => runs.push((1, slots[s].0)),
        }
    }
    leb(&mut out, runs.len() as u32);
    for (count, ty) in runs {
        leb(&mut out, count);
        out.push(type_byte(ty)?);
    }
    for (k, op) in ops.iter().enumerate() {
        let start = starts[k];
        let end = if k + 1 < len { starts[k + 1] } else { body_end };
        match *op {
            Op::Get(i) | Op::Set(i) | Op::Tee(i) => {
                out.push(bytes[start]);
                leb(&mut out, new_index[slot_of[i as usize]]);
            }
            _ => out.extend_from_slice(&bytes[start..end]),
        }
    }
    Ok(out)
}
