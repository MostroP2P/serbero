# Serbero

Serbero helps the parties of a [Mostro](https://mostro.network) dispute resolve
it themselves, and brings in a human solver when that is needed. It never moves
funds and never decides a dispute.

> **Status:** early development. Nothing here is ready to run in production.

- Specification: [`docs/`](docs/README.md)
- Implementation plan: [`docs/plan.md`](docs/plan.md)
- Contributor and agent rules: [`AGENTS.md`](AGENTS.md)

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

[MIT](LICENSE)
