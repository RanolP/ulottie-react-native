// Where things live in the integer stream.
//
// The single source of truth on this side is `scene/flat.rs`; these must stay
// in step with it.
//
// One constant per slot rather than one table: a minifier will fold
// `H_BINDINGS` to `11` but cannot fold `head.BINDINGS`, so the table shipped
// whole in every self-contained module. Separate bindings also shake
// individually, so an animation without records carries none of the record
// bits.

// Fixed slots at the head of the stream. A zero means the section is absent.
export const H_FR = 1;
export const H_IP = 2;
export const H_OP = 3;
export const H_FLAGS = 4;
export const H_EASINGS = 5;
export const H_TIMELINES = 6;
export const H_GATES = 7;
export const H_SLOTS = 8;
export const H_BIND_GATE = 9;
export const H_SCOPES = 10;
export const H_BINDINGS = 11;
export const H_LAYERS = 12;
export const H_ASSETS = 13;
export const H_USES = 14;
export const H_REMAPS = 15;
export const H_TEMPLATES = 16;

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
