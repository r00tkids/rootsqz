// Follows the WebAssembly modules in the stream byte by byte, like WasmParser in wasm.rs.
// wasmStack holds the fields still to come, the next one last, as [kind, count]. Kinds below 19 are bytes,
// the others LEB128 numbers
let wasmKind = 0, wasmRanges, wasmStack = [], wasmPos = 0, wasmSec = 0, wasmSecEnd = 0, wasmBodyEnd = 0,
    wasmPrefix = 0, wasmLeb = 0, wasmLebBytes = 0, wasmFieldPos = 0, wasmBytes = [0, 0, 0, 0, 0, 0, 0, 0],
    wasmOps = [0, 0, 0], wasmLast = [], wasmLast2 = [], wasmColumn = [];

let wasmMix = (hash, value) => (hash = Math.imul(hash ^ value, 0x9E3779B1) >>> 0, (hash ^ hash >>> 15) >>> 0);

let wasmIndex = () => Math.min(wasmKind > 18 ? wasmLebBytes : wasmFieldPos, 31);

let wasmPush = (kind, count = 1) => {
    if (count) {
        wasmStack.push([kind, Math.min(count, 1 << 24)]);
        wasmFieldPos = 0;
        wasmKind = kind;
    }
};

let wasmUpdate = byte => {
    let kind = wasmKind, value = byte, top = wasmStack.at(-1), pos = wasmPos + 1;
    if (kind) {
        wasmColumn[kind * 32 + wasmIndex()] = byte;
        if (kind > 18) {
            if (wasmLebBytes < 5) wasmLeb = (wasmLeb | (byte & 127) << 7 * wasmLebBytes) >>> 0;
            wasmLebBytes++;
            value = byte > 127 ? -1 : wasmLeb;
        }
        wasmFieldPos++;
        if (value >= 0) {
            wasmLeb = wasmLebBytes = 0;
            wasmLast2[kind] = wasmLast[kind];
            wasmLast[kind] = value;
            if (top[1] > 1) top[1]--;
            else {
                wasmStack.pop();
                wasmFieldPos = 0;
            }
            wasmKind = wasmStack.at(-1)?.[0] | 0;
            let count = Math.min(value, 1 << 24);
            // Push the fields that follow
            if (kind == 2) {
                wasmSec = value;
                wasmPush(19);
            }
            if (kind == 19) {
                wasmSecEnd = pos + count;
                if (wasmSec == 7 || wasmSec == 10) wasmPush(20);
            }
            if (kind == 21) wasmPush(3, count);
            if (kind == 27) {
                wasmBodyEnd = pos + count;
                wasmPush(28);
            }
            if (kind == 28) wasmPush(29, count);
            if (kind == 29) wasmPush(6);
            if (kind == 12) {
                wasmOps = [value, wasmOps[0], wasmOps[1]];
                // The immediates of the instruction
                let imm = "00OOO0000000PPQ0RoRoHH0000000000TTTUUSS0VVVVVVVVVVVVVVVVVVVVVVVXXYZ>?".charCodeAt(value) - 48 | 0;
                if (imm == 63) {
                    wasmPush(35);
                    imm = 24;
                }
                if (value > 0xfb && value < 0xff) {
                    wasmPrefix = value;
                    imm = 30;
                }
                if (imm) wasmPush(imm, value == 0x43 ? 4 : value == 0x44 ? 8 : 1);
            }
            if (kind == 30) {
                wasmOps[0] = wasmPrefix << 9 | value & 511;
                if (wasmPrefix == 0xfc) {
                    if (value > 7 && value < 18) wasmPush(40, 2 - (value & 1));
                } else if (value < 12 || value == 92 || value == 93) wasmPush(38);
                else if (value == 12 || value == 13) wasmPush(4 + value, 16);
                else if (value > 20 && value < 35) wasmPush(17);
                else if (value > 83 && value < 92) {
                    wasmPush(17);
                    wasmPush(38);
                }
            }
            if (kind == 33) wasmPush(32, count + 1);
            if (kind == 38) wasmPush(39);
        }
    }
    wasmBytes = [byte, ...wasmBytes.slice(0, 7)];
    wasmPos = pos;
    if (wasmRanges.some(range => range[0] == pos)) {
        wasmStack = [];
        wasmSecEnd = 0;
        wasmPush(1, 8);
    } else if (wasmRanges.some(range => range[1] == pos)) wasmStack = [];
    else if (kind && wasmKind != 1) {
        if (pos == wasmSecEnd || wasmSec == 10 && pos == wasmBodyEnd) wasmStack = [];
        // Push the next record of the section, instruction of the function, or section
        if (!wasmStack.length) {
            if (pos >= wasmSecEnd) wasmPush(2);
            else if (wasmSec == 7) {
                wasmPush(26);
                wasmPush(11);
                wasmPush(21);
            } else if (wasmSec == 10) wasmPush(pos < wasmBodyEnd ? 12 : 27);
            else wasmPush(4);
        }
    }
    wasmKind = wasmStack.at(-1)?.[0] | 0;
};

// Predicts the bytes of the modules from what the parser knows about the next one: what is in the
// context is given by the bits of fields. The first one of them runs the parser, on the modules at ranges
let WasmContext = (fields, maxCount, ranges, expected = 0, ctx) => {
    if (ranges) {
        wasmRanges = ranges;
        ranges.some(range => !range[0]) && wasmPush(1, 8);
    }
    return CounterModel(maxCount, byte => {
        ranges && wasmUpdate(byte);
        let kind = wasmKind, index = wasmIndex(), column = wasmColumn[kind * 32 + index] | 0,
            last = wasmLast[kind] >>> 0, last2 = wasmLast2[kind] >>> 0,
            hash = wasmMix(wasmMix(fields + 0x5741534d, kind | index << 8), wasmSec), next;
        [...wasmBytes, ...wasmOps, last, column, last2]
            .map((value, i) => fields >> i & 1 && (hash = wasmMix(hash, value)));
        // The byte of the last field at the same position, or the LEB128 byte of the larger of the
        // last two values plus one
        next = Math.max(last, last2) + 1;
        expected = fields & 0x4000 && 256 | (kind < 19 ? column :
            index < 5 && (next / 2 ** (7 * index) & 127 | (next >= 2 ** (7 * index + 7)) << 7));
        return kind ? ctx = Math.imul(hash, 0x2545F491) : null;
    // While the byte so far is the expected one, the number of bits and the next expected bit
    }, null, (bitCtx, bits = 31 - Math.clz32(bitCtx)) =>
        ctx ^ (expected >> 8 - bits == bitCtx ? 256 | bits << 1 | expected >> 7 - bits & 1 : bitCtx));
};
