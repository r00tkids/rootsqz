# Model config

rootsqz compresses with a set of models, each predicting the next bit in its own way, and a mixer that combines their predictions. A config says which models to use and how they are set up. The decoder is generated from the config, so it only contains the code of the models you use.

The presets are configs too: [4k](../src/compressor/default_config.json), [64k](../src/compressor/size_64k_config.json) and [64k-wasm](../src/compressor/size_64k_wasm_config.json). To make your own, copy one, edit it and pass it with `--config`. Or let [the search](#search) do it for you.

## Models
`model` is the mixer, and `model.models` lists the models it mixes:

- `NOrderByte`: predicts from the previous bytes that `byte_mask` picks, e.g. `"0b00000011"` for the last two. The presets have several of these.
- `Match` (`{"type":"Match","min_len":6}`): finds the last time the previous `min_len` bytes appeared, and predicts that what followed then follows again. `candidates`, `expected_byte_context` and `second` tune it.
- `JsContext` (`{"type":"JsContext","variant":0}`): predicts from the JavaScript around the current token. Variant 0 uses the bracket, object key and punctuation; 1 the token that came after the previous token last time; 2 the last byte and how deep in brackets it is; 3 the kinds of the last tokens.
- `CharClass` (`{"type":"CharClass","order":3}`): predicts from the kinds of the last characters (letters, digits, …).
- `Word`: predicts from the current word. Both presets have one.
- `TokenPosition` and `IndirectByte` (`{"type":"IndirectByte","context_bytes":2,"max_count":15}`): other models you can try. The presets don't use them.
- `MatchPredictor`: the older match model. `Match` replaced it in the 64k presets.

## The ContextMixer
The 64k presets mix with a `ContextMixer`, in the style of paq8. It keeps several sets of weights and picks one by a context, its `selectors`:

- `"None"`: one set of weights.
- `"Order1"`: a set per previous byte.
- `"Known"`: a set per number of models that have seen their context before.
- `{"State": i}`: a set per state of model `i` of `models`, such as how long a `Match` has been matching.
- `{"Context": i}`: a set per context of model `i`. Only a `JsContext` offers one.

`apms` then refine the mixed prediction by a context of their own: `"Order0"`, `"Order1"`, `"Order2"`, `{"State": i}` or `{"Context": i}`.

The `i` in selectors and APMs is the position of a model in `models`. When you add or remove a model, update these numbers too.

## Other settings
Under `static_model_params`:

- `hash_table_pow2_size`: the size of the table that the context models share, as a power of two. Each entry takes 8 bytes in the decoder, so 26 means 512 MB.
- `count_offset`: how fast the models learn. Lower learns faster.
- `context_input_scale`: how much the mixer listens to the context models. The ContextMixer works best with more than 1.
- `wasm.models`: models for WebAssembly files. Only the `64k-wasm` preset has them.
- `records.models`: models for tables of numbers in binary files. rootsqz tries them on each part of your files, and keeps them only where they save more than their decoder code costs.

## Search
`--search` (or `rootsqz search`) starts from a config and tries one small change at a time: a different context, count limit, match length or learning rate, a model added or removed, a mixer selector or APM added or removed, and so on. It keeps each change that makes the output smaller, counting the decoder code that the change adds or removes, until no change helps. On a 64k intro this takes about an hour, and the result doesn't depend on how many threads it uses.

It skips changes after which the decoder would need more than 1 GB of memory, by an estimate.

With wasm files, it also tries the WebAssembly models of the `64k-wasm` preset if the config has none.

`--search` during a build saves its result only when it's done, so stopping it loses the progress. `rootsqz search` searches without building, and can be stopped and resumed:

```sh
rootsqz search -i main.js,data.bin -o config.json
```

- `-i` takes the main JavaScript and its files, separated by commas. Repeat it to find one config for several intros.
- `-o` is where the best config so far is written, after each improvement. You can stop the search with Ctrl-C at any time and build with this file.
- `--state <FILE>` saves the progress. To resume a stopped search, run it again with the same `--state`, and `--config` set to the `-o` file.
- `--eval` only prints the size of each intro with `--config`, and the memory its decoder needs.
- `--max-memory-mb` changes the memory limit (default 1000).
