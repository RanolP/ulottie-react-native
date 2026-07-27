# μLottie

μLottie (or micro Lottie) is an ahead-of-time compiler for the [Lottie file format](https://lottie.github.io/).

It takes a Lottie JSON and emits a JavaScript module with a tiny runtime to evaluate it.

See the [EXPLAINER.md](EXPLAINER.md) to understand the original motivations.

> **Status: proof of concept.** Never released, no programatic API yet.
> AI is largely involved. Kinda vibe, still heavy steering by human.
> They keep working state on [HANDOFF.md](HANDOFF.md), for how it works and what is left.

## Try it

The entire compiler is fully available on the browser. You can see it from [the demo page](https://ulottie.cometkim.dev).

Or to try it locally, see [ulottie-dev-server](ulottie-dev-server/README.md) for instructions.

## Compiler Usage

```sh
cargo run -p ulottie-compiler -- --help
```

## Layout

- `ulottie-compiler/`: the compiler (serde -> IR -> payload -> scene → JS)
- `ulottie-compiler/runtime/`: the runtime, as JS modules
- `ulottie-dev-server/`: compile server, test suites, and the demo page
- `_fixtures/animations/`: Lottie fixtures to track coverage

## Tests

```sh
cargo nextest run --features eval       # 87 tests
yarn workspace ulottie-dev-server test  # 157 tests
```

The second spawns the compile server itself, drives a real browser, and pixel-diffs every fixture against lottie-web. Both must stay green.

## License

[MIT](LICENSE)
