import React, {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  type ComponentType,
  type ReactElement,
} from 'react';
import { View, type StyleProp, type ViewStyle } from 'react-native';
import * as rnsvg from 'react-native-svg';
import Animated, {
  makeMutable,
  runOnUI,
  useAnimatedProps,
  useFrameCallback,
  useSharedValue,
  type SharedValue,
} from 'react-native-reanimated';

// --- the compiler's module contract (reanimated-aot target) ---

export interface UlottieNode {
  /** react-native-svg component name ('Svg' | 'G' | 'Path' | ...). */
  type: string;
  /** Present only on elements the animation writes to; the root Svg never has one. */
  slot?: number;
  staticProps: Record<string, unknown>;
  children?: UlottieNode[];
}

export interface UlottieMeta {
  fr: number;
  ip: number;
  op: number;
  width: number;
  height: number;
}

interface ElHandle {
  i: number;
  p: Record<string, unknown>;
  d: number;
  q: ElHandle[];
}

export interface UlottieHandles {
  els: ElHandle[];
  dirty: ElHandle[];
  apply: (frame: number) => void;
  fr: number;
  ip: number;
  op: number;
}

export interface UlottieModule {
  tree: UlottieNode;
  meta: UlottieMeta;
  init: () => UlottieHandles;
}

// --- component surface ---

export interface UlottieProps {
  /** 0..1 — pins the frame (frame = ip + progress·(op−ip)) and suspends autoplay while set. */
  progress?: number;
  autoplay?: boolean;
  loop?: boolean;
  speed?: number;
  style?: StyleProp<ViewStyle>;
}

export interface UlottieRef {
  play(): void;
  pause(): void;
  /** Jump to a 0..1 progress. Ignored while the `progress` prop pins the frame. */
  seek(progress: number): void;
  goToFrame(frame: number): void;
}

type Props = Record<string, unknown>;

function svgComponent(type: string): ComponentType<Props> {
  const comp = (rnsvg as unknown as Record<string, ComponentType<Props> | undefined>)[type];
  if (!comp) {
    throw new Error(
      `ulottie: react-native-svg does not export "${type}" — this animation needs react-native-svg >= 15.15`,
    );
  }
  return comp;
}

const animatedCache = new Map<string, ComponentType<Props>>();
function animatedSvgComponent(type: string): ComponentType<Props> {
  let comp = animatedCache.get(type);
  if (!comp) {
    comp = Animated.createAnimatedComponent(svgComponent(type)) as ComponentType<Props>;
    animatedCache.set(type, comp);
  }
  return comp;
}

function AnimatedSlot(props: {
  comp: ComponentType<Props>;
  sv: SharedValue<Props>;
  staticProps: Props;
  children?: ReactElement[];
}): ReactElement {
  const sv = props.sv;
  const animatedProps = useAnimatedProps(() => {
    'worklet';
    return sv.value;
  });
  return React.createElement(
    props.comp,
    { ...props.staticProps, animatedProps },
    props.children,
  );
}

function buildNode(
  node: UlottieNode,
  slotSVs: Record<number, SharedValue<Props>>,
  key: string,
): ReactElement {
  const children = node.children?.map((c, i) => buildNode(c, slotSVs, String(i)));
  if (node.slot === undefined) {
    return React.createElement(svgComponent(node.type), { key, ...node.staticProps }, children);
  }
  // The animated layer owns `display` for slotted elements: an animated
  // `display: undefined` (visible) could not reliably override a static
  // `display: 'none'`, so the static value seeds the slot's shared value
  // instead (see createUlottie) and is dropped from the JSX props here.
  const { display: _display, ...staticProps } = node.staticProps;
  return React.createElement(
    AnimatedSlot,
    { key, comp: animatedSvgComponent(node.type), sv: slotSVs[node.slot], staticProps },
    children,
  );
}

export function createUlottie(mod: UlottieModule) {
  const { tree, meta, init } = mod;

  const slots: Array<{ slot: number; initial: Props }> = [];
  (function walk(n: UlottieNode) {
    if (n.slot !== undefined) {
      slots.push({
        slot: n.slot,
        initial: 'display' in n.staticProps ? { display: n.staticProps.display } : {},
      });
    }
    n.children?.forEach(walk);
  })(tree);

  const Root = svgComponent(tree.type);

  return forwardRef<UlottieRef, UlottieProps>(function Ulottie(props, ref) {
    const { progress, autoplay = true, loop = true, speed = 1, style } = props;
    const pinned = progress !== undefined;

    const slotSVs = useMemo(() => {
      const m: Record<number, SharedValue<Props>> = {};
      for (const s of slots) m[s.slot] = makeMutable<Props>(s.initial);
      return m;
    }, []);

    // UI-thread state. `handles` holds the compiled module's mount result: it
    // is created and only ever read on the UI thread, so the worklet functions
    // inside it never cross back to the JS runtime.
    const handles = useSharedValue<UlottieHandles | null>(null);
    const clock = useSharedValue(meta.ip);
    const playing = useSharedValue(autoplay);
    const lastApplied = useSharedValue(NaN); // NaN ≠ anything → first frame always applies

    // Mount-time warm-up: schedule init() onto the UI thread as soon as the
    // component commits, so it overlaps the Fabric commit instead of N
    // instances serializing their inits inside the first vsync tick. runOnUI
    // scheduling order against the first frame callback is not guaranteed, so
    // the frame callback below keeps its lazy fallback.
    useEffect(() => {
      runOnUI(() => {
        'worklet';
        if (handles.value === null) handles.value = init();
      })();
    }, []);

    // Always registered, even when paused or pinned: a paused frame costs one
    // number comparison, and it is what makes seek()/goToFrame()/progress
    // changes take effect without extra plumbing.
    useFrameCallback((info) => {
      'worklet';
      let h = handles.value;
      if (h === null) {
        // init() is a worklet and its handles are consumed here every frame,
        // so it must run on the UI thread; first invocation mounts lazily.
        h = init();
        handles.value = h;
      }
      const ip = h.ip;
      const op = h.op;
      let frame: number;
      if (pinned) {
        frame = ip + (progress as number) * (op - ip);
      } else {
        if (playing.value) {
          const dt = (info.timeSincePreviousFrame ?? 0) / 1000;
          let f = clock.value + dt * h.fr * speed;
          if (f >= op) {
            if (loop) {
              f = ip + ((f - ip) % (op - ip));
            } else {
              f = op;
              playing.value = false;
            }
          }
          clock.value = f;
        }
        frame = clock.value;
      }
      if (frame === lastApplied.value) return;
      lastApplied.value = frame;
      h.apply(frame);
      const dirty = h.dirty;
      for (let i = 0; i < dirty.length; i++) {
        const el = dirty[i];
        const p = el.p;
        const out: Props = {};
        for (const k in p) {
          if (k === 'transform') {
            // This write path skips rn-svg's JS prop extraction (which turns
            // `transform` into the native `matrix` prop), so emit the native
            // name directly — the compiled value is already the 6-tuple the
            // native side wants. A raw `transform` here is silently dropped.
            out.matrix = p[k];
          } else {
            // Includes `display`: the compiled '' (visible) must be written
            // as-is — native maps '' to nil (visible) via
            // RCTNSStringFromStringNilIfEmpty, while an `undefined` here is
            // dropped in serialization and a previous 'none' would stick.
            out[k] = p[k];
          }
        }
        slotSVs[el.i].value = out;
        el.d = 0;
      }
      dirty.length = 0;
    });

    useImperativeHandle(
      ref,
      () => ({
        play() {
          // Runs on the UI thread because it reads clock/handles, which the
          // frame callback owns there.
          runOnUI(() => {
            'worklet';
            const op = handles.value?.op ?? meta.op;
            if (clock.value >= op) clock.value = handles.value?.ip ?? meta.ip;
            playing.value = true;
          })();
        },
        pause() {
          playing.value = false;
        },
        seek(p: number) {
          clock.value = meta.ip + p * (meta.op - meta.ip);
        },
        goToFrame(frame: number) {
          clock.value = frame;
        },
      }),
      [],
    );

    // The subtree is fully static from React's point of view (per-frame updates
    // flow through shared values); only the root re-renders on style changes.
    // The root Svg stays a plain (non-animated) component: repainting the root
    // triggers a Fabric bug in rn-svg (#2962), and the compiler never slots it.
    const children = useMemo(
      () => tree.children?.map((c, i) => buildNode(c, slotSVs, String(i))),
      [slotSVs],
    );
    // The user's style goes on a wrapper View, not on the Svg: rn-svg's root
    // pushes its width/height props ('100%' here) as styles AFTER the style
    // prop, so a style-sized bare Svg collapses to 0 inside any auto-sized
    // parent. '100%' resolves against this wrapper instead.
    return React.createElement(
      View,
      { style },
      React.createElement(Root, tree.staticProps, children),
    );
  });
}
