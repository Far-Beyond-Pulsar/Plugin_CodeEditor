# Design notes: the lantern

A short design note showing the Markdown the code editor highlights.

## Goal

The lantern lights a **small radius** around the player and *flickers* when
enemies are near. See also [the lighting plan](lighting.md).

## Requirements

1. It must never hide the floor directly under the player.
2. It dims over time, `fuel` going from `1.0` to `0.0`.
3. Refuelling is instant.

## Tuning values

| Value | Default | Notes |
|---|---|---|
| `radius` | 6.0 | metres at full fuel |
| `flicker_hz` | 8 | only near enemies |
| `burn_rate` | 0.01 | fuel per second |

## Sketch

```rust
fn radius(fuel: f32) -> f32 {
    2.0 + 4.0 * fuel
}
```

> Open question: should the flicker be audible?

- [x] Pick a base radius
- [ ] Prototype the flicker
- [ ] Playtest the burn rate
