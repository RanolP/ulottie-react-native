// Descaling factors for the wire's per-column shifts.
//
// A column carries the smallest power of ten that represents it exactly, so
// reading one back is a single multiply by its reciprocal. Shared because the
// property reader, the clock table and the gate table all need it.

export const INV = [1, 0.1, 0.01, 0.001];
