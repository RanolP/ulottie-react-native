// Where things live in the integer stream.
//
// The single source of truth on this side is `scene/flat.rs`; these must stay
// in step with it.
//
// One constant per slot rather than one table: a minifier will fold
// `H_PROGRAM` to `8` but cannot fold `head.PROGRAM`, so the table shipped whole
// in every self-contained module. Separate bindings also shake individually, so
// an animation without records carries none of the record bits.

// Fixed slots at the head of the stream. A zero means the section is absent.
export const H_FR = 1;
export const H_IP = 2;
export const H_OP = 3;
export const H_FLAGS = 4;
export const H_EASINGS = 5;
export const H_TIMELINES = 6;
export const H_GATES = 7;
// `[count, batchOffset × count]` — the document's own bindings, grouped by op.
export const H_PROGRAM = 8;
export const H_LAYERS = 9;
export const H_ASSETS = 10;
export const H_USES = 11;
export const H_REMAPS = 12;

// One precomp asset: `[template, program, timelines, records]`.
export const A_STRIDE = 4;
export const A_PROGRAM = 1;
export const A_RECORDS = 3;

// One instantiation: `[asset, elementBase, slotBase, parentSlot]`.
export const U_STRIDE = 4;
export const U_EL_BASE = 1;
export const U_SLOT_BASE = 2;
export const U_PARENT = 3;

// Presence bits in a layer record's first word.
export const R_NAME = 1;
export const R_PARENT = 2;
export const R_P = 4;
export const R_A = 8;
export const R_SC = 16;
export const R_R = 32;
export const R_O = 64;
export const R_H = 128;
export const R_EFFECTS = 256;
