//! Track playback: resolve each track's slot to the entity it animates, then
//! write sampled values back into the scene per frame.
//!
//! The compiler links this module too — its bbox pass drives the same
//! [`Player`] across every sampled frame to measure animated geometry, so
//! playback semantics cannot drift between compile-time measurement and
//! device-time rendering.

use crate::rtdl::{
    Animation, Channel, Clip, FxPass, Geom, Gradient, Node, PaintSource, Track,
};
use alloc::vec::Vec;

extern crate alloc;

/// What a track's slot resolved to in the decoded scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Node(u32),
    Gradient(u32),
    Stop { gradient: u32, stop: u32 },
    /// A node whose [`Clip::Path`] carries this slot.
    Clip(u32),
    /// One fx pass on a node; several slots (blur/offset/flood of a shadow)
    /// resolve here and the channel type picks the field.
    Fx { node: u32, stage: u32, pass: u32 },
}

pub struct Player {
    /// The live scene; [`Player::apply`] mutates it in place. Its `tracks`
    /// are moved out into [`Player::tracks`].
    pub anim: Animation,
    pub tracks: Vec<Track>,
    /// Parallel to `tracks`; `None` when the slot matched nothing (a track
    /// for an element the display list has no writable field on).
    pub targets: Vec<Option<Target>>,
    /// The frame-`ip` bake as decoded, restored on a backward seek so a
    /// channel whose first key lies after the seek target reads the bake,
    /// not a later frame's leftover.
    base_nodes: Vec<Node>,
    base_gradients: Vec<Gradient>,
    last: f32,
}

impl Player {
    pub fn new(mut anim: Animation) -> Self {
        let tracks = core::mem::take(&mut anim.tracks);
        let targets = tracks.iter().map(|t| resolve(&anim, t.slot)).collect();
        let base_nodes = anim.nodes.clone();
        let base_gradients = anim.gradients.clone();
        Player {
            anim,
            tracks,
            targets,
            base_nodes,
            base_gradients,
            last: f32::NEG_INFINITY,
        }
    }

    /// Sample every channel at frame `f` and write the results into the
    /// scene. A channel with no key at or before `f` writes nothing — the
    /// value already in the scene (the frame-`ip` bake, or the previous
    /// `apply`) stands, which is exactly the web runtime's "attribute not
    /// yet set" behavior. That is correct while `f` moves forward, but a
    /// backward seek (the player's loop wrap) would keep the previous
    /// cycle's leftovers in those channels — so a non-monotonic `f` first
    /// restores the frame-`ip` bake: one clone per loop cycle, not per
    /// frame.
    pub fn apply(&mut self, f: f32) {
        if f < self.last {
            self.anim.nodes.clone_from(&self.base_nodes);
            self.anim.gradients.clone_from(&self.base_gradients);
        }
        self.last = f;
        for (i, track) in self.tracks.iter().enumerate() {
            let Some(target) = self.targets[i] else {
                continue;
            };
            for ch in &track.channels {
                apply_channel(&mut self.anim, target, ch, f);
            }
        }
    }

    /// Whether the node holding `slot` can dip below full opacity at any
    /// frame (used by the compiler's layer detection).
    pub fn animates_opacity(&self, slot: u32) -> bool {
        self.tracks.iter().any(|t| {
            t.slot == slot
                && t.channels.iter().any(|c| match c {
                    Channel::Opacity(k) => k.values.iter().any(|&v| v < 1.0),
                    _ => false,
                })
        })
    }
}

fn resolve(anim: &Animation, slot: u32) -> Option<Target> {
    for (i, node) in anim.nodes.iter().enumerate() {
        let i = i as u32;
        match node {
            Node::Group(g) => {
                if g.slot == Some(slot) {
                    return Some(Target::Node(i));
                }
                if let Some(Clip::Path { slot: Some(s), .. }) = &g.clip
                    && *s == slot
                {
                    return Some(Target::Clip(i));
                }
                for (si, stage) in g.fx.iter().enumerate() {
                    for (pi, pass) in stage.passes.iter().enumerate() {
                        let hit = match pass {
                            FxPass::Blur { slot: s, .. } => *s == Some(slot),
                            FxPass::Shadow {
                                blur_slot,
                                offset_slot,
                                flood_slot,
                                ..
                            } => {
                                *blur_slot == Some(slot)
                                    || *offset_slot == Some(slot)
                                    || *flood_slot == Some(slot)
                            }
                            _ => false,
                        };
                        if hit {
                            return Some(Target::Fx {
                                node: i,
                                stage: si as u32,
                                pass: pi as u32,
                            });
                        }
                    }
                }
            }
            Node::Shape(s) => {
                if s.slot == Some(slot) {
                    return Some(Target::Node(i));
                }
            }
            Node::Image(_) => {}
        }
    }
    for (gi, g) in anim.gradients.iter().enumerate() {
        if g.slot == Some(slot) {
            return Some(Target::Gradient(gi as u32));
        }
        for (si, stop) in g.stops.iter().enumerate() {
            if stop.slot == Some(slot) {
                return Some(Target::Stop {
                    gradient: gi as u32,
                    stop: si as u32,
                });
            }
        }
    }
    None
}

fn apply_channel(anim: &mut Animation, target: Target, ch: &Channel, f: f32) {
    match target {
        Target::Node(i) => apply_node(&mut anim.nodes[i as usize], ch, f),
        Target::Gradient(gi) => {
            if let Channel::Gradient(k) = ch
                && let Some(v) = k.at(f, false)
            {
                anim.gradients[gi as usize].coords = v;
            }
        }
        Target::Stop { gradient, stop } => {
            if let Channel::Stop(k) = ch
                && let Some([o, r, g, b]) = k.at(f, false)
            {
                let s = &mut anim.gradients[gradient as usize].stops[stop as usize];
                s.offset = o;
                s.color[0] = r;
                s.color[1] = g;
                s.color[2] = b;
            }
        }
        Target::Clip(i) => {
            if let Channel::Path(k) = ch
                && let Some(v) = k.at(f, false)
                && let Node::Group(g) = &mut anim.nodes[i as usize]
                && let Some(Clip::Path { path, .. }) = &mut g.clip
            {
                *path = v;
            }
        }
        Target::Fx { node, stage, pass } => {
            let Node::Group(g) = &mut anim.nodes[node as usize] else {
                return;
            };
            let p = &mut g.fx[stage as usize].passes[pass as usize];
            match (ch, p) {
                (Channel::BlurStd(k), FxPass::Blur { sx, sy, .. }) => {
                    if let Some([x, y]) = k.at(f, false) {
                        *sx = x;
                        *sy = y;
                    }
                }
                (Channel::ShadowStd(k), FxPass::Shadow { std_dev, .. }) => {
                    if let Some(v) = k.at(f, false) {
                        *std_dev = v;
                    }
                }
                (Channel::ShadowOffset(k), FxPass::Shadow { dx, dy, .. }) => {
                    if let Some([x, y]) = k.at(f, false) {
                        *dx = x;
                        *dy = y;
                    }
                }
                (Channel::FloodOpacity(k), FxPass::Shadow { flood_opacity, .. }) => {
                    if let Some(v) = k.at(f, false) {
                        *flood_opacity = v;
                    }
                }
                _ => {}
            }
        }
    }
}

fn apply_node(node: &mut Node, ch: &Channel, f: f32) {
    // Fields shared by groups and shapes.
    {
        let (matrix, opacity, hidden) = match node {
            Node::Group(g) => (&mut g.matrix, &mut g.opacity, &mut g.hidden),
            Node::Shape(s) => (&mut s.matrix, &mut s.opacity, &mut s.hidden),
            Node::Image(_) => return,
        };
        match ch {
            Channel::Matrix(k) => {
                if let Some(v) = k.at(f, false) {
                    *matrix = Some(v);
                }
                return;
            }
            Channel::Opacity(k) => {
                if let Some(v) = k.at(f, false) {
                    *opacity = v;
                }
                return;
            }
            Channel::Hidden(k) => {
                if let Some(v) = k.at(f, true) {
                    *hidden = v != 0.0;
                }
                return;
            }
            _ => {}
        }
    }
    let Node::Shape(s) = node else { return };
    match ch {
        Channel::Path(k) => {
            if let Some(v) = k.at(f, false) {
                s.geom = Geom::Path(v);
            }
        }
        Channel::Rect(k) => {
            if let Some([x, y, w, h, rx, ry]) = k.at(f, false) {
                s.geom = Geom::Rect { x, y, w, h, rx, ry };
            }
        }
        Channel::Ellipse(k) => {
            if let Some([cx, cy, rx, ry]) = k.at(f, false) {
                s.geom = Geom::Ellipse { cx, cy, rx, ry };
            }
        }
        Channel::Fill(k) => {
            if let Some(v) = k.at(f, false) {
                s.paint.fill = Some(PaintSource::Color(v));
            }
        }
        Channel::FillOpacity(k) => {
            if let Some(v) = k.at(f, false) {
                s.paint.fill_opacity = v;
            }
        }
        Channel::Stroke(k) => {
            if let Some(v) = k.at(f, false) {
                s.paint.stroke = Some(PaintSource::Color(v));
            }
        }
        Channel::StrokeOpacity(k) => {
            if let Some(v) = k.at(f, false) {
                s.paint.stroke_opacity = v;
            }
        }
        Channel::StrokeWidth(k) => {
            if let Some(v) = k.at(f, false) {
                s.paint.stroke_width = v;
            }
        }
        Channel::Dash(k) => {
            if let Some(v) = k.at(f, false) {
                s.paint.dash = Some(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtdl::{Group, Keys, Paint, Shape};

    /// The one runnable check: slot resolution finds the shape, and applying
    /// two frames moves its geometry per the keys.
    #[test]
    fn resolve_and_apply() {
        let anim = Animation {
            width: 10.0,
            height: 10.0,
            fr: 30.0,
            ip: 0.0,
            op: 10.0,
            nodes: alloc::vec![
                Node::Group(Group {
                    children: alloc::vec![1],
                    opacity: 1.0,
                    ..Group::default()
                }),
                Node::Shape(Shape {
                    slot: Some(3),
                    matrix: None,
                    opacity: 1.0,
                    hidden: false,
                    geom: Geom::Ellipse {
                        cx: 0.0,
                        cy: 0.0,
                        rx: 1.0,
                        ry: 1.0,
                    },
                    even_odd: false,
                    paint: Paint::default(),
                }),
            ],
            gradients: Vec::new(),
            images: Vec::new(),
            tracks: alloc::vec![Track {
                slot: 3,
                channels: alloc::vec![Channel::Ellipse(Keys {
                    frames: alloc::vec![0.0, 10.0],
                    values: alloc::vec![[0.0, 0.0, 1.0, 1.0], [10.0, 0.0, 1.0, 1.0]],
                })],
            }],
        };
        let mut player = Player::new(anim);
        assert_eq!(player.targets, alloc::vec![Some(Target::Node(1))]);
        player.apply(5.0);
        let Node::Shape(s) = &player.anim.nodes[1] else {
            panic!("shape");
        };
        let Geom::Ellipse { cx, .. } = s.geom else {
            panic!("ellipse");
        };
        assert_eq!(cx, 5.0);
    }

    /// A channel whose first key lies after frame `ip` (a layer that enters
    /// late) writes nothing before that key — so a loop wrap back to frame 0
    /// must restore the frame-`ip` bake instead of keeping the exit pose.
    #[test]
    fn backward_seek_restores_baseline() {
        let anim = Animation {
            width: 10.0,
            height: 10.0,
            fr: 30.0,
            ip: 0.0,
            op: 10.0,
            nodes: alloc::vec![
                Node::Group(Group {
                    children: alloc::vec![1],
                    opacity: 1.0,
                    ..Group::default()
                }),
                Node::Shape(Shape {
                    slot: Some(3),
                    matrix: None,
                    opacity: 1.0,
                    hidden: false,
                    geom: Geom::Ellipse {
                        cx: 0.0,
                        cy: 0.0,
                        rx: 1.0,
                        ry: 1.0,
                    },
                    even_odd: false,
                    paint: Paint::default(),
                }),
            ],
            gradients: Vec::new(),
            images: Vec::new(),
            tracks: alloc::vec![Track {
                slot: 3,
                channels: alloc::vec![
                    Channel::Ellipse(Keys {
                        frames: alloc::vec![5.0, 10.0],
                        values: alloc::vec![[5.0, 0.0, 1.0, 1.0], [10.0, 0.0, 1.0, 1.0]],
                    }),
                    Channel::Hidden(Keys {
                        frames: alloc::vec![5.0],
                        values: alloc::vec![1.0],
                    }),
                ],
            }],
        };
        let mut looped = Player::new(anim.clone());
        looped.apply(10.0);
        looped.apply(0.0); // the JS player's loop wrap
        let mut fresh = Player::new(anim);
        fresh.apply(0.0);
        let state = |p: &Player| {
            let Node::Shape(s) = &p.anim.nodes[1] else {
                panic!("shape");
            };
            let Geom::Ellipse { cx, .. } = s.geom else {
                panic!("ellipse");
            };
            (cx, s.hidden)
        };
        assert_eq!(state(&looped), state(&fresh));
        assert_eq!(state(&looped), (0.0, false));
    }
}
