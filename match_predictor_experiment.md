# Bounded match predictor experiment

Baseline commit `8e4ab7b` produces a 9055-byte `testout/web/index.html` with `cargo test node_tests::web`. The measurement includes the HTML header, deflated decoder, encoded JavaScript, and the test's bundled `Cargo.toml`.

The optional `MatchPredictor` uses a 64 KiB byte ring and a fixed-size context lookup table. Its config accepts `context_bytes` (1–4), `confidence` (greater than 0.5 and less than 1), and `table_bits` (8–20; default 16). At 16 bits the history and lookup arrays use about 832 KiB, excluding object overhead. The decoder source is included only when the model appears in the config.

| Experiment | Best complete HTML size |
| --- | ---: |
| Baseline default mixer, no match predictor | **9055 bytes** |
| Match input added; context 2–4 bytes, confidence 0.7/0.8/0.9/0.97, table 14/16 bits | 9208 bytes |
| Remove each of the 12 original mixer inputs in turn while adding match input | 9208 bytes |
| Match input added; context 1–3 bytes, confidence 0.97/0.99/0.995/0.999 | 9208 bytes |
| Match input added; mixer learning rate 0.0003/0.001/0.003, context rate 0.01/0.044/0.1, context scale 0.2/0.5/1.0 | 9208 bytes |
| Confidence ramp after the first matched byte; context 2–4 bytes, later confidence 0.9/0.97/0.99/0.995 | 9209 bytes |

The best candidate adds `{"type":"MatchPredictor","context_bytes":2,"confidence":0.97,"table_bits":16}` to the default mixer's `models`. Its full HTML is 9208 bytes, 153 bytes above baseline. With this candidate active, `cargo test node_tests::round_trip` passes both the source and random-data round trips, and `cargo test node_tests::web` passes. The default config remains at baseline because no tested variant improves the complete HTML size. A focused Node round-trip test keeps Rust encoder and JS decoder parity checked for the optional model.
