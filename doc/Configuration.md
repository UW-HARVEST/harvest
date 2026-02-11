# Configuration

## Configuration file

`harvest-translate` reads configuration values from a config file in TOML
format. The location is OS-dependent; to print the location, run:

```sh
cargo run -p harvest_translate -- --print-config-path
```

You can see an example of the syntax at `translate/default_config.toml`.

## Configuration flag

Additionally, configuration values can be specified on the command line using
`--config`. For example, to set the LLM server address for the
raw_source_to_cargo_llm tool, run:

```sh
cargo run -p harvest_translate --release -- --config tools.raw_source_to_cargo_llm.address=127.0.0.1
```

The `--config` flag overrides configuration from the configuration file.
