import child_process, { type StdioOptions } from "node:child_process";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

/**
 * Name of the global that the boot code of the output puts the bundled files in,
 * as in `rsqz.files["<file name>"]`.
 */
export const RUNTIME_GLOBAL = "rsqz";

/**
 * Packages that hold the executable, by `<process.platform>-<process.arch>`.
 */
const PLATFORM_PACKAGES: Record<string, string> = {
  "darwin-arm64": "@r00tkids/rootsqz-darwin-arm64",
  "darwin-x64": "@r00tkids/rootsqz-darwin-x64",
  "linux-arm64": "@r00tkids/rootsqz-linux-arm64",
  "linux-x64": "@r00tkids/rootsqz-linux-x64",
  "win32-x64": "@r00tkids/rootsqz-win32-x64",
};

/**
 * Path of the rootsqz executable for this platform.
 * The environment variable `ROOTSQZ_BINARY_PATH` takes precedence over the installed platform package.
 *
 * @throws If there is no prebuilt executable for this platform, or its package is not installed.
 */
export function binaryPath(): string {
  if (process.env.ROOTSQZ_BINARY_PATH) {
    return process.env.ROOTSQZ_BINARY_PATH;
  }

  const platform = `${process.platform}-${process.arch}`;
  const platformPackage = PLATFORM_PACKAGES[platform];
  if (!platformPackage) {
    throw new Error(
      `rootsqz has no prebuilt executable for ${platform}. Build one from https://github.com/r00tkids/rootsqz and set ROOTSQZ_BINARY_PATH to it.`,
    );
  }

  const executable = process.platform === "win32" ? "rootsqz.exe" : "rootsqz";
  try {
    return require.resolve(`${platformPackage}/bin/${executable}`);
  } catch {
    throw new Error(
      `The package ${platformPackage} with the rootsqz executable is not installed. It is an optional dependency of @r00tkids/rootsqz, so install without omitting optional dependencies, or set ROOTSQZ_BINARY_PATH to an executable.`,
    );
  }
}

/**
 * Environment for the executable. It points the executable to the uglify-js of this package,
 * unless `ROOTSQZ_UGLIFYJS` is set already.
 *
 * @param env Environment to extend. By default `process.env`.
 */
export function toolEnvironment(env: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  if (env.ROOTSQZ_UGLIFYJS) {
    return env;
  }

  return {
    ...env,
    ROOTSQZ_UGLIFYJS: require.resolve("uglify-js/bin/uglifyjs"),
    ROOTSQZ_NODE: process.execPath,
  };
}

/**
 * Options for running the executable.
 */
export type RunOptions = {
  /**
   * Path of the executable. By default the result of `binaryPath()`.
   */
  binaryPath?: string;

  /**
   * Working directory of the executable. Relative paths in the arguments resolve against it.
   */
  cwd?: string;

  /**
   * Standard streams of the executable. By default they are inherited.
   */
  stdio?: StdioOptions;
};

/**
 * Runs the rootsqz executable with the given command line arguments.
 * Rejects if it cannot be started or exits with a code other than 0.
 *
 * @param args Command line arguments of the executable.
 * @param options Executable, working directory and standard streams.
 */
export function run(args: string[], options: RunOptions = {}): Promise<void> {
  const executable = options.binaryPath ?? binaryPath();

  return new Promise<void>((resolve, reject) => {
    const child = child_process.spawn(executable, args, {
      cwd: options.cwd,
      env: toolEnvironment(),
      stdio: options.stdio ?? "inherit",
    });

    child.once("error", reject);
    child.once("close", (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`rootsqz process exited with ${code === null ? `signal ${signal}` : `code ${code}`}`));
      }
    });
  });
}

/**
 * A view of the compression report, for `reportDetail` and `rootsqz report --view`.
 */
export type ReportView =
  | "sections"
  | "lines"
  | "blocks"
  | "hotspots"
  | "tokens"
  | "token-totals"
  | "bytes"
  | "bits"
  | "repeats"
  | "models"
  | "suggestions";

/**
 * How the web output decodes: in one go on the main thread, in slices on the main thread,
 * or in a web worker. See `CompressOptions.decoder`.
 */
export type Decoder = "serial" | "sliced" | "worker";

/**
 * Loader shown while the web output decompresses.
 * Either `preset`, or your own `html` and `js`; the executable rejects `preset` combined with `html` or `js`.
 */
export type LoaderOptions = {
  /**
   * Built-in loader (`--loader`): `"spinner"`, with the decoder running in one go by default,
   * `"bar"`, a progress bar with the decoder running in slices by default,
   * or `"progress"`, the browser's own progress bar (a `<progress>` element), also with the decoder running in slices by default.
   */
  preset?: "spinner" | "bar" | "progress";

  /**
   * File with the markup and CSS of the loader (`--loader-html`). It is packed as it is, without minification.
   */
  html?: string;

  /**
   * File with the script of the loader (`--loader-js`). It runs after the markup is inserted and sets
   * `rsqz.progress = (done, total) => ...`, which is called with the number of decoded bytes.
   * The decoder then runs in slices by default, so the page repaints while decoding.
   */
  js?: string;

  /**
   * Keep the loader when the main JavaScript runs, instead of clearing the page (`--loader-keep`).
   * The demo can then continue the progress with `rsqz.progress` and removes the loader itself.
   */
  keep?: boolean;
};

/**
 * Options of `compress`. They correspond to the command line options of the executable.
 */
export type CompressOptions = RunOptions & {
  /**
   * JavaScript file that is evaluated after decompression (`--js-main`).
   */
  jsMain: string;

  /**
   * Output directory (`--output-directory`).
   */
  outputDirectory: string;

  /**
   * Files to pack with compression (`--file`).
   * The order matters, files of similar content should follow each other.
   */
  files?: string[];

  /**
   * Files to pack without compression, e.g. JPEG (`--pre-compressed-file`).
   */
  preCompressedFiles?: string[];

  /**
   * Target platform of the output (`--target`). The executable defaults to `web`.
   */
  target?: "web" | "node";

  /**
   * Embedded compression preset (`--size-profile`). The executable defaults to `64k`, whose
   * decoder needs up to about 1 GB of memory; `4k` is for small intros, `64k-wasm` also
   * models WebAssembly files. `64k-semi-fast` decodes about 1.7 times as fast as `64k` and packs
   * about 2% larger, `64k-fast` about twice as fast and about 6% larger.
   */
  sizeProfile?: "4k" | "64k" | "64k-wasm" | "64k-semi-fast" | "64k-fast";

  /**
   * Pack the output as a Brotli stream decoded with `DecompressionStream('brotli')` (`--brotli`).
   * Cannot be combined with `config`, `report`, `wasmDecoder` or a `64k` size profile.
   */
  brotli?: boolean;

  /**
   * Path to a complete JSON compression config (`--config`). Takes precedence over `sizeProfile`.
   */
  config?: string;

  /**
   * Write a compression report to `report/` in the output directory (`--report`):
   * `report.json` (a summary for analysis, also by an LLM), `report.html` and `data.bin`,
   * which `rootsqz report <dir>` queries.
   */
  report?: boolean;

  /**
   * Also record what every submodel predicted (`--report-models`). Implies `report`.
   */
  reportModels?: boolean;

  /**
   * Also write these views of the report as TSV files (`--report-detail`). Implies `report`.
   */
  reportDetail?: ReportView[];

  /**
   * Source map of `jsMain` (`--report-source-map`), to tell in the report which original
   * source files and lines its bytes come from. Implies `report`.
   */
  reportSourceMap?: string;

  /**
   * Names of files in the report, by their path as given in `jsMain` or `files`
   * (`--report-label <file>=<label>`), e.g. the module a bundler made a file from. Implies `report`.
   */
  reportLabels?: Record<string, string>;

  /**
   * Debug info of WebAssembly files in `files`, by their path as given there
   * (`--report-wasm-debug <file>=<debug>`): the unstripped `.wasm` for function names and
   * DWARF source lines, or a `.wasm.map` source map. Implies `report`. Without it, the report
   * looks for `x.debug.wasm` and `x.wasm.map` next to `x.wasm`.
   */
  reportWasmDebug?: Record<string, string | string[]>;

  /**
   * Loader shown while the web output decompresses (`--loader`, `--loader-html`, `--loader-js`, `--loader-keep`).
   */
  loader?: LoaderOptions;

  /**
   * How the web output decodes (`--decoder`): `"serial"`, in one go on the main thread, `"sliced"`,
   * on the main thread in slices so the page repaints, or `"worker"`, in a web worker so the page
   * stays responsive. By default `"sliced"` with a loader script, otherwise `"serial"`.
   */
  decoder?: Decoder;

  /**
   * Decode with a WebAssembly decoder generated for the model config (`--wasm-decoder`), with any
   * `decoder`. It decodes 5–6 times as fast as the JavaScript decoder, for about 1 KB more.
   * Cannot be combined with `brotli`.
   */
  wasmDecoder?: boolean;

  /**
   * Keep the browser from showing the packed bytes of the web output as text, and from parsing
   * them as HTML, while the page loads (`--hide-data`). Adds 19 bytes. By default on with the 64k
   * size profiles, and off with `4k` and `brotli`.
   */
  hideData?: boolean;

  /**
   * Title of the web page (`--title`), set before the web output decompresses.
   */
  title?: string;

  /**
   * File with JavaScript that runs first in the web output (`--boot-js`), before the loader and
   * decompression, e.g. to set up the page. It is minified with the boot code.
   */
  bootJs?: string;

  /**
   * Search for the model config that packs the input smallest (`--search`), starting from `config`
   * or the `sizeProfile` preset. Takes about an hour for a 64k intro; the config found is written
   * to `search_config.json` in the output directory.
   */
  search?: boolean;

  /**
   * Number of configs the search tries at the same time (`--search-threads`). By default the number of CPUs.
   */
  searchThreads?: number;

  /**
   * Directory where the search keeps the config it found, to reuse it for the same input (`--search-cache`).
   */
  searchCache?: string;

  /**
   * Further arguments passed to the executable as they are.
   */
  extraArgs?: string[];
};

function wantsReport(options: CompressOptions): boolean {
  return Boolean(
    options.report ||
      options.reportModels ||
      options.reportDetail?.length ||
      options.reportSourceMap ||
      Object.keys(options.reportLabels ?? {}).length ||
      Object.keys(options.reportWasmDebug ?? {}).length,
  );
}

/**
 * Command line arguments of the executable for the given options.
 *
 * @throws If `brotli` is combined with `config`, a report option, `wasmDecoder` or a `64k` size profile.
 */
export function compressArgs(options: CompressOptions): string[] {
  if (options.brotli && (options.config || wantsReport(options) || options.wasmDecoder || options.sizeProfile !== undefined && options.sizeProfile !== "4k")) {
    throw new Error('The "brotli" option cannot be combined with "config", "report", "wasmDecoder" or a "64k" size profile.');
  }

  const args: string[] = ["--js-main", options.jsMain];

  for (const file of options.files ?? []) {
    args.push("--file", file);
  }
  for (const preCompressedFile of options.preCompressedFiles ?? []) {
    args.push("--pre-compressed-file", preCompressedFile);
  }

  args.push("--output-directory", options.outputDirectory);

  if (options.target) {
    args.push("--target", options.target);
  }
  if (options.config) {
    args.push("--config", options.config);
  }
  if (options.sizeProfile) {
    args.push("--size-profile", options.sizeProfile);
  }
  if (options.brotli) {
    args.push("--brotli");
  }
  if (options.report) {
    args.push("--report");
  }
  if (options.loader?.preset) {
    args.push("--loader", options.loader.preset);
  }
  if (options.loader?.html) {
    args.push("--loader-html", options.loader.html);
  }
  if (options.loader?.js) {
    args.push("--loader-js", options.loader.js);
  }
  if (options.loader?.keep) {
    args.push("--loader-keep");
  }
  if (options.decoder) {
    args.push("--decoder", options.decoder);
  }
  if (options.wasmDecoder) {
    args.push("--wasm-decoder");
  }
  if (options.hideData !== undefined) {
    args.push(`--hide-data=${options.hideData}`);
  }
  if (options.title !== undefined) {
    args.push("--title", options.title);
  }
  if (options.bootJs) {
    args.push("--boot-js", options.bootJs);
  }
  if (options.search) {
    args.push("--search");
  }
  if (options.searchThreads) {
    args.push("--search-threads", String(options.searchThreads));
  }
  if (options.searchCache) {
    args.push("--search-cache", options.searchCache);
  }
  if (options.reportModels) {
    args.push("--report-models");
  }
  if (options.reportDetail?.length) {
    args.push("--report-detail", options.reportDetail.join(","));
  }
  if (options.reportSourceMap) {
    args.push("--report-source-map", options.reportSourceMap);
  }
  for (const [file, label] of Object.entries(options.reportLabels ?? {})) {
    args.push("--report-label", `${file}=${label}`);
  }
  for (const [file, debug] of Object.entries(options.reportWasmDebug ?? {})) {
    for (const d of Array.isArray(debug) ? debug : [debug]) {
      args.push("--report-wasm-debug", `${file}=${d}`);
    }
  }
  args.push(...(options.extraArgs ?? []));

  return args;
}

/**
 * Packs and compresses a JavaScript file and further files into the output directory.
 * Rejects if the executable cannot be started or exits with a code other than 0.
 *
 * @throws If `brotli` is combined with `config`, a report option, `wasmDecoder` or a `64k` size profile.
 */
export function compress(options: CompressOptions): Promise<void> {
  return run(compressArgs(options), options);
}
