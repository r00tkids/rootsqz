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
  "darwin-arm64": "@rootkids/rootsqz-darwin-arm64",
  "darwin-x64": "@rootkids/rootsqz-darwin-x64",
  "linux-arm64": "@rootkids/rootsqz-linux-arm64",
  "linux-x64": "@rootkids/rootsqz-linux-x64",
  "win32-x64": "@rootkids/rootsqz-win32-x64",
};

/**
 * Path of the rootsqz executable for this platform.
 * The environment variable `ROOTSQZ_BINARY_PATH` takes precedence over the installed platform package.
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
      `The package ${platformPackage} with the rootsqz executable is not installed. It is an optional dependency of @rootkids/rootsqz, so install without omitting optional dependencies, or set ROOTSQZ_BINARY_PATH to an executable.`,
    );
  }
}

/**
 * Environment for the executable. It points the executable to the uglify-js of this package,
 * unless `ROOTSQZ_UGLIFYJS` is set already.
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
   * Files to pack with compression (`--files`).
   * The order matters, files of similar content should follow each other.
   */
  files?: string[];

  /**
   * Files to pack without compression, e.g. JPEG (`--pre-compressed-files`).
   */
  preCompressedFiles?: string[];

  /**
   * Target platform of the output (`--target`). The executable defaults to `web`.
   */
  target?: "web" | "node";

  /**
   * Embedded compression preset (`--size-profile`). The executable defaults to `4k`;
   * `64k` adds a bounded match predictor for larger JavaScript inputs.
   */
  sizeProfile?: "4k" | "64k";

  /**
   * Pack the output as a Brotli stream decoded with `DecompressionStream('brotli')` (`--brotli`).
   * Cannot be combined with `config`, `report` or the `64k` size profile.
   */
  brotli?: boolean;

  /**
   * Path to a complete JSON compression config (`--config`). Takes precedence over `sizeProfile`.
   */
  config?: string;

  /**
   * Write a detailed compression report to the output directory (`--report`).
   */
  report?: boolean;

  /**
   * Further arguments passed to the executable as they are.
   */
  extraArgs?: string[];
};

/**
 * Command line arguments of the executable for the given options.
 */
export function compressArgs(options: CompressOptions): string[] {
  if (options.brotli && (options.config || options.report || options.sizeProfile === "64k")) {
    throw new Error('The "brotli" option cannot be combined with "config", "report" or the "64k" size profile.');
  }

  const args: string[] = ["--js-main", options.jsMain];

  for (const file of options.files ?? []) {
    args.push("--files", file);
  }
  for (const preCompressedFile of options.preCompressedFiles ?? []) {
    args.push("--pre-compressed-files", preCompressedFile);
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
  args.push(...(options.extraArgs ?? []));

  return args;
}

/**
 * Packs and compresses a JavaScript file and further files into the output directory.
 */
export function compress(options: CompressOptions): Promise<void> {
  return run(compressArgs(options), options);
}
