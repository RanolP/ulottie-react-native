// The web `kzero` is a bare arrow, and the RN emitter's worklet pass only
// marks `function` declarations — so the one arrow that other worklets call
// carries its directive by hand. (Reanimated requires everything a worklet
// calls to be a worklet itself; an unmarked `kzero` throws at runtime the
// first time `resolve` returns it on the UI thread.)
const kzero = () => { 'worklet'; return 0; };
