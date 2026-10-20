//! AGENTS.md of a report directory: what is in this report and how to query it, for coding agents.

use std::fmt::Write;

use clap::{Args as _, Command};
use serde_json::Value;

use super::{query, View};

/// The guide for a report with this summary (report.json).
pub fn agents_md(summary: &Value) -> String {
    let mut s = String::new();
    let num = |v: &Value| v.as_f64().unwrap_or(0.);
    let totals = &summary["totals"];

    s += "# rootsqz compression report\n\n";
    let _ = writeln!(
        s,
        "This directory is a compression report written by rootsqz {} (`--report`, or `report: true` of the plugin). It tells what each byte of the compressed stream costs, to find what to change in the source to make the intro smaller. The next build with the report on overwrites it, so copy `report.json` before changing something, to compare after.\n",
        summary["rootsqz_version"].as_str().unwrap_or("?")
    );
    s += "The agent skill `rootsqz-report` describes the whole workflow: how to make the report, read it, and what changes pay off. Install it into a project with `npx rootsqz install-skill`.\n\n";

    s += "## This report\n\n";
    let _ = writeln!(
        s,
        "- {} input bytes compressed to a stream of {:.0} bytes; the output file has {} bytes.",
        num(&totals["input_bytes"]),
        num(&totals["cost_bytes"]),
        num(&totals["output_bytes"])
    );
    s += "- Sections of the decompressed stream, in the order they are compressed (`--section` takes the index):\n";
    for section in summary["sections"].as_array().into_iter().flatten() {
        let name = section["label"]
            .as_str()
            .filter(|l| !l.is_empty())
            .or(section["path"].as_str())
            .unwrap_or("?");
        let _ = writeln!(
            s,
            "  {}. `{name}` ({}, {}): bytes {}..{}, {:.0} bytes of cost, {:.2} bits per byte",
            section["index"],
            section["kind"].as_str().unwrap_or("?"),
            if section["text"].as_bool() == Some(true) {
                "text"
            } else {
                "binary"
            },
            section["start"],
            section["end"],
            num(&section["cost_bytes"]),
            num(&section["bits_per_byte"]),
        );
    }
    let section_name = |index: &Value| -> String {
        summary["sections"]
            .as_array()
            .and_then(|all| all.iter().find(|s| s["index"] == *index))
            .and_then(|s| s["label"].as_str().or(s["path"].as_str()))
            .unwrap_or("?")
            .to_owned()
    };
    match summary["source_map"].as_object() {
        Some(map) => {
            let _ = writeln!(
                s,
                "- Source positions: {} original source files, {} bytes mapped. The views `sources` and `source-lines`, the filter `--source` and the columns `source` and `source_line` work. From:",
                map["sources"], map["mapped_bytes"],
            );
            for m in map["maps"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    s,
                    "  - `{}` ({}) for section {} `{}`",
                    m["file"].as_str().unwrap_or("?"),
                    m["kind"].as_str().unwrap_or("?"),
                    m["section"],
                    section_name(&m["section"]),
                );
            }
        }
        None => s += "- No source positions: costs are not mapped to original source files. Compress with `--report-source-map` (the plugin does it by itself), and give wasm files their debug info (`--report-wasm-debug`).\n",
    }
    for wasm in summary["wasm"].as_array().into_iter().flatten() {
        let index = &wasm["section"];
        let debug = summary["wasm_debug"]
            .as_array()
            .and_then(|all| all.iter().find(|d| d["section"] == *index));
        let names = match debug {
            Some(d) => {
                let files: Vec<String> = d["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|f| {
                        let gave: Vec<&str> = f["gave"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|g| g.as_str())
                            .collect();
                        let gave = if gave.is_empty() {
                            "nothing".to_owned()
                        } else {
                            gave.join(", ")
                        };
                        format!("`{}` ({gave})", f["path"].as_str().unwrap_or("?"))
                    })
                    .collect();
                let unmatched = d["unmatched_functions"].as_array().map_or(0, |u| u.len());
                let mut text = format!("debug info from {}", files.join(", "));
                if unmatched > 0 {
                    text += &format!(", which does not apply to {unmatched} functions whose code differs");
                }
                text
            }
            None => "no debug info: functions have no names; give the unstripped file with `--report-wasm-debug`".to_owned(),
        };
        let _ = writeln!(
            s,
            "- Section {index} `{}` is a wasm file: the views `wasm-sections`, `wasm-functions`, `wasm-instructions` and `wasm-opcode-totals` describe it; {names}.",
            section_name(index)
        );
    }
    let models = summary["models"]["names"].as_array().map_or(0, |m| m.len());
    if models > 0 {
        let _ = writeln!(
            s,
            "- Submodel predictions recorded for {models} submodels: the view `models` and the filter `--models` work."
        );
    } else {
        s += "- No submodel predictions: compress with `--report-models` (`reportModels`) for the view `models` and `--models`.\n";
    }
    for file in summary["detail_files"].as_array().into_iter().flatten() {
        let _ = writeln!(
            s,
            "- `{}`: the view `{}` as TSV.",
            file["file"].as_str().unwrap_or("?"),
            file["view"].as_str().unwrap_or("?")
        );
    }

    s += "\n## Files\n\n";
    s += "- `report.json`: the summary. Start here; its `about` entry explains the fields. `suggestions` has candidates worth looking into, each with a query.\n";
    s += "- `report.html`: a viewer for people. It embeds all the data, so do not read it.\n";
    s += "- `data.bin`, `sources.json`: raw data for `rootsqz report`. Do not read them.\n";

    s += "\n## Querying\n\n";
    s += "```sh\nnpx rootsqz report <this directory> --view <view> [filters]\n```\n\n";
    s += "Use `rootsqz` without `npx` if it is installed. It prints tab separated rows with a header line. Offsets are byte offsets in the decompressed stream (the sections above, one after the other). Keep the output short with `--top`; use `bytes` and `bits` only with `--range`.\n\n";
    s += "Views:\n";
    for view in View::ALL {
        let description = match view {
            View::Summary => "report.json, as JSON (the default)",
            _ => view.description(),
        };
        let _ = writeln!(s, "- `{}`: {description}", view.name());
    }
    s += "\nFilters:\n";
    let command = query::Args::augment_args(Command::new("report"));
    for arg in command.get_arguments() {
        let (Some(long), Some(help)) = (arg.get_long(), arg.get_help()) else {
            continue;
        };
        if long == "view" {
            continue;
        }
        let value = match arg.get_value_names() {
            Some(names) if arg.get_action().takes_values() => format!(
                " <{}>",
                names
                    .iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            _ => String::new(),
        };
        let mut help = help.to_string().replace('\n', " ");
        let values: Vec<_> = arg
            .get_possible_values()
            .iter()
            .map(|v| v.get_name().to_owned())
            .collect();
        if !values.is_empty() {
            help += &format!(" ({})", values.join(", "));
        }
        let _ = writeln!(s, "- `--{long}{value}`: {help}");
    }

    s += "\n## Reading the costs\n\n";
    s += "- The cost of a byte is the number of bits the compressor spends on it; the costs add up to the size of the compressed stream.\n";
    s += "- A byte costs less when similar bytes came before it, also in earlier sections. A change also changes the cost of what comes after it.\n";
    s += "- Suggestions and estimates are heuristics: rebuild after a change and compare `totals.output_bytes` of `report.json` with the copy from before.\n";
    s
}
