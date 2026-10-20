import { PluginContext, rollup, type OutputAsset, type OutputChunk, type OutputOptions, type Plugin } from "rollup";
import querystring from "node:querystring";
import fs from "node:fs/promises";
import path from "node:path";
import { createFilter, FilterPattern } from "@rollup/pluginutils";
import { compress, RUNTIME_GLOBAL, type LoaderOptions, type ReportView } from "@r00tkids/rootsqz";

export type { LoaderOptions };

/**
 * A file that is passed to rootsqz next to the JavaScript bundle.
 */
type RootSqzFile = {
  /**
   * Short generated name that the file has in the output.
   */
  fileName: string;

  /**
   * Content to pack.
   */
  content: Buffer;

  /**
   * Is already compressed and is packed without compression.
   */
  isCompressed: boolean;

  /**
   * File extension including the dot, used to order the files.
   */
  fileExt: string;

  /**
   * Is it text? The import then gives a string instead of an `Uint8Array`.
   */
  isText: boolean;

  /**
   * Was made by one of the `fileTransforms`, so it differs from the file on disk.
   */
  transformed: boolean;
};

/**
 * Result of a file transform hook.
 */
export type RootsqzFileTransformRes = {
  /**
   * The result of processing
   */
  content: Buffer;

  /**
   * Is already compressed and should not be compressed again by rootsqz
   */
  isCompressed: boolean;

  /**
   * Is it text? The plugin orders files by type for better compression ratios
   */
  isText: boolean;

  /**
   * File extension including the dot, e.g. .txt.
   * This is used to order files for better compression ratios.
   * Same type of content should share same file extension.
   * 
   * By default it is extracted from the original file path.
   */
  fileExt?: string;
};

/**
 * Options of the plugin.
 */
export type RootsqzOptions = {
  /**
   * Path of the rootsqz executable. By default the one installed with `@r00tkids/rootsqz`,
   * or the one named by the environment variable `ROOTSQZ_BINARY_PATH`.
   */
  rootsqzPath?: string;

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
   * Write a compression report to `rootsqz-output/report/` (`--report`):
   * `report.json` (a summary for analysis, also by an LLM), `report.html` and `data.bin`,
   * which `rootsqz report <dir>` queries.
   * The report names the imported files by their module and maps the bundle back to its
   * original sources. For that, the plugin turns on `sourcemap: "hidden"` if no source map is set.
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
   * Debug info of imported WebAssembly files, by the path of the imported file (relative to the
   * working directory, or absolute): the unstripped `.wasm` for function names and DWARF source
   * lines, or a `.wasm.map` source map (`--report-wasm-debug`). Implies `report`.
   * Without it, the plugin uses `x.debug.wasm` and `x.wasm.map` next to an imported `x.wasm`.
   */
  reportWasmDebug?: Record<string, string>;

  /**
   * Loader shown while the output decompresses: a built-in `preset` ("spinner", "bar" or "progress"),
   * or your own `html` and `js` files. `keep` leaves it in place when the bundle runs.
   * Paths are relative to the working directory.
   */
  loader?: LoaderOptions;

  /**
   * Name of the global that the boot code of the executable puts the files in.
   * By default `rsqz`. Set it to `wsqz` when `rootsqzPath` names an executable up to 0.4.0.
   */
  runtimeGlobal?: string;

  /**
   * Further arguments passed to the executable as they are.
   */
  rootsqzArgs?: string[];

  /**
   * File transform hooks to process files before they are imported in code or compressed by rootsqz
   */
  fileTransforms?: [
    {
      /**
       * Module ids that the transform applies to. By default all.
       */
      include?: FilterPattern,

      /**
       * Module ids that the transform does not apply to.
       */
      exclude?: FilterPattern,

      /**
       * Returns the content to pack for the module `id`, given the content of its file.
       * If it returns nothing, the next matching transform is tried.
       *
       * @param ctx Rollup plugin context of the `load` hook.
       * @param id Module id, including its query parameters.
       * @param content Content of the file on disk.
       */
      transform: (ctx: PluginContext, id: string, content: Buffer) => Promise<RootsqzFileTransformRes>;
    }
  ]
};

/**
 * Rollup / Vite plugin that packs the bundle and the imported files into one HTML file with rootsqz.
 * It only compresses when `NODE_ENV` is `production`, otherwise it inlines the imported files.
 *
 * @param options Options of the plugin.
 * @returns The plugin, for the `plugins` array of the Rollup or Vite config.
 * @throws If `brotli` is combined with `config`, a report option or the `64k` size profile.
 */
export default function (options: RootsqzOptions = {}): Plugin {
  const wantsReport = Boolean(options.report || options.reportModels || options.reportDetail?.length ||
    Object.keys(options.reportWasmDebug ?? {}).length);
  const wasmDebug = new Map(Object.entries(options.reportWasmDebug ?? {}).map(([file, debug]) => [path.resolve(file), path.resolve(debug)]));
  if (options.brotli && (options.config || wantsReport || options.sizeProfile === "64k")) {
    throw new Error('The "brotli" option cannot be combined with "config", "report" or the "64k" size profile.');
  }

  const isBuild = process.env.NODE_ENV === "production";
  const runtimeGlobal = options.runtimeGlobal ?? RUNTIME_GLOBAL;

  const fileTransforms = options.fileTransforms?.map(transform => {
    const include = transform.include ? (Array.isArray(transform.include) ? transform.include : [transform.include]) 
      : undefined;
    const exclude = transform.exclude ? (Array.isArray(transform.exclude) ? transform.exclude : [transform.exclude]) 
      : undefined;

    const filter = createFilter(include, exclude);
    return {
      transform: transform.transform,
      filter,
    }
  });

  const files = new Map<string, RootSqzFile>();
  // Whether the plugin turned on the source map for the report, so it removes the file again
  let addedSourcemap = false;
  let fileNameIdx = 0;

  const findNextAvailableFileName = () => {
    const startChar = 97; // a
    const endChar = 122; // z
    const alphabetSize= endChar - startChar;

    let numChars = fileNameIdx === 0 ? 1 : Math.floor(Math.log(fileNameIdx) / Math.log(alphabetSize)) + 1;
    let candidateName = "";
    
    for (let i = 0; i < numChars; i++) {
      const charCode = startChar + ((Math.floor(fileNameIdx / Math.pow(alphabetSize, i))) % alphabetSize);
      candidateName = String.fromCharCode(charCode) + candidateName;
    }

    fileNameIdx++;

    return candidateName;
  }

  const loadAndTransform = async function (id: string, hookRes: RootsqzFileTransformRes, transformed = false) {
    if (isBuild) {
      const fileName = findNextAvailableFileName();
      files.set(id, {
        fileName,
        content: hookRes.content,
        isCompressed: hookRes.isCompressed,
        fileExt: hookRes.fileExt ?? path.extname(id),
        isText: hookRes.isText,
        transformed,
      });

      return {
        code: hookRes.isText 
          ? `export default new TextDecoder().decode(${runtimeGlobal}.files["${fileName}"]);` 
          : `export default ${runtimeGlobal}.files["${fileName}"];`,
        moduleSideEffects: false,
        moduleType: 'js',
      };
    } else {
      return {
        code: hookRes.isText 
          ? `export default ${JSON.stringify(hookRes.content.toString("utf-8"))};` 
          : `export default Uint8Array.fromBase64("${hookRes.content.toString("base64")}");`,
        moduleSideEffects: false,
        moduleType: 'js',
      };
    }
  };

  return {
    name: "rollup-plugin-rootsqz",

    outputOptions(outputOptions: OutputOptions) {
      if (isBuild && wantsReport && !outputOptions.sourcemap) {
        addedSourcemap = true;
        return { ...outputOptions, sourcemap: "hidden" };
      }
      return null;
    },

    load: {
      order: "pre",
      async handler(id: string) {
        const qIdx = id.indexOf("?");
        const beforeParams = id.slice(0, qIdx === -1 ? id.length : qIdx);
        const afterParams = id.slice(id.indexOf("?") + 1);
        const parsed = querystring.parse(afterParams);
        const cleanedUpId = beforeParams;

        let cachedFile: Buffer | null = null;
        const loadFromDisk = async () => {
          if (cachedFile != null) {
            return cachedFile;
          }
          cachedFile = await fs.readFile(cleanedUpId);
          return cachedFile;
        };

        if (fileTransforms) {
          for (const transform of fileTransforms) {
            if (transform.filter(id)) {
              let hookRes = await transform.transform(this, id, await loadFromDisk());
              if (hookRes == null) {
                continue;
              }
              return await loadAndTransform(id, hookRes, true);
            }
          }
        }

        const isRootSqzTxt = parsed["rootsqz-txt"] != null;
        const isRootSqzBin = parsed["rootsqz-bin"] != null;

        if (isRootSqzTxt && isRootSqzBin) {
          throw new Error(
            `Cannot use both rootsqz-txt and rootsqz-bin on the same import: ${id}`,
          );
        }

        const isCompressed = parsed["pre-compressed"] != null;

        if ((isRootSqzTxt || isRootSqzBin) && parsed["compressed"] != null) {
          throw new Error(
            `The "compressed" query parameter was renamed to "pre-compressed": ${id}`,
          );
        }

        if (isRootSqzTxt || isRootSqzBin) {
          const content = await loadFromDisk();
          
          let hookRes: RootsqzFileTransformRes = {
            content,
            isCompressed,
            isText: isRootSqzTxt,
            fileExt: path.extname(cleanedUpId),
          };
          return await loadAndTransform(id, hookRes);
        }
      }
    },

    writeBundle: async function (outputOptions: OutputOptions, bundle: { [fileName: string]: OutputAsset | OutputChunk }) {
      const jsFiles = Object.entries(bundle)
        .filter((entry): entry is [string, OutputChunk] => entry[0].endsWith('.js') && entry[1].type === 'chunk');

      if (jsFiles.length === 0) {
        throw new Error('No JavaScript file found in the bundle.');
      }
      if (jsFiles.length > 1) {
        throw new Error('Multiple JavaScript files found in the bundle. Make sure to bundle into a single file.\nTry setting "build.rollupOptions.output.inlineDynamicImports" to true in Vite config.');
      }

      const [jsFileName, jsChunk] = jsFiles[0];

      if (isBuild) {
        const outDir = path.resolve(
            outputOptions.dir || "",
            "rootsqz-tmp");
        if (await fs.stat(outDir).catch(() => false)) {
          await fs.rm(outDir, { recursive: true, force: true });
        }

        const filesToCompress = [];
        const preCompressedFiles = [];
        const reportLabels: Record<string, string> = {};
        const reportWasmDebug: Record<string, string[]> = {};

        // Files are registered when they're loaded, so leave out the ones whose import was tree-shaken away
        const renderedIds = new Set(
          Object.entries(jsChunk.modules)
            .filter(([, module]) => module.renderedLength > 0)
            .map(([id]) => id),
        );

        for (const [id, file] of files) {
          if (!renderedIds.has(id)) {
            this.debug(`Skipping '${id}', the bundle doesn't use it`);
            continue;
          }
          this.debug(`Copying '${id}' for rootsqz...`);
          const outPath = path.resolve(
            outputOptions.dir || "",
            "rootsqz-tmp",
            file.fileName,
          );

          await fs.mkdir(path.dirname(outPath), { recursive: true });
          await fs.writeFile(outPath, file.content);

          // The module id, with the query that tells how it was imported
          const qIdx = id.indexOf("?");
          const query = qIdx === -1 ? "" : id.slice(qIdx);
          reportLabels[path.relative(".", outPath)] =
            path.relative(".", qIdx === -1 ? id : id.slice(0, qIdx)) + query + (file.transformed ? " (transformed)" : "");

          // Debug info of a wasm file: given, or next to it. The report checks that it matches.
          const source = path.resolve(qIdx === -1 ? id : id.slice(0, qIdx));
          const isWasm = file.content.subarray(0, 4).equals(Buffer.from("\0asm"));
          if (wantsReport && isWasm && !file.isCompressed) {
            const given = wasmDebug.get(source);
            const candidates = given ? [given] : source.endsWith(".wasm")
              ? [source.slice(0, -5) + ".debug.wasm", source + ".map"]
              : [];
            const found = [];
            for (const c of candidates) {
              if (given || await fs.stat(c).then(st => st.isFile(), () => false)) found.push(path.relative(".", c));
            }
            if (found.length) reportWasmDebug[path.relative(".", outPath)] = found;
          }

          if (file.isCompressed) {
            preCompressedFiles.push(path.relative(".", outPath));
          } else {
            filesToCompress.push({ isText: file.isText, fileExt: file.fileExt, path: path.relative(".", outPath) });
          }
        }

        // Sort by file extension for better compression ratios in rootsqz
        filesToCompress.sort((a, b) => Math.sign(a.fileExt.localeCompare(b.fileExt)) + 2 * (a.isText === b.isText ? 0 : a.isText ? -1 : 1));

        const jsMain = path.resolve(outputOptions.dir || "", jsFileName);
        reportLabels[jsMain] = path.relative(".", jsMain);
        let reportSourceMap: string | undefined;
        if (wantsReport && jsChunk.map) {
          // Write the source map with absolute source paths, as they are relative to the bundle
          const sourceMap = { ...jsChunk.map, sources: jsChunk.map.sources.map(source =>
            source.startsWith("\0") || /^[a-z][a-z0-9+.-]*:/i.test(source)
              ? source
              : path.resolve(path.dirname(jsMain), source)) };
          reportSourceMap = path.resolve(outputOptions.dir || "", "rootsqz-tmp", "main.js.map");
          await fs.mkdir(path.dirname(reportSourceMap), { recursive: true });
          await fs.writeFile(reportSourceMap, JSON.stringify(sourceMap));
          if (addedSourcemap) {
            await fs.rm(`${jsMain}.map`, { force: true });
          }
        }

        await compress({
          binaryPath: options.rootsqzPath,
          jsMain,
          files: filesToCompress.map(f => f.path),
          preCompressedFiles: preCompressedFiles,
          outputDirectory: path.resolve(
            outputOptions.dir || "",
            "rootsqz-output",
          ),
          sizeProfile: options.sizeProfile,
          brotli: options.brotli,
          config: options.config,
          report: wantsReport,
          loader: options.loader,
          reportModels: options.reportModels,
          reportDetail: options.reportDetail,
          reportSourceMap,
          reportLabels: wantsReport ? reportLabels : undefined,
          reportWasmDebug: wantsReport ? reportWasmDebug : undefined,
          extraArgs: options.rootsqzArgs,
        });

        const relOutPath = path.relative(".", path.resolve(outputOptions.dir || "", "rootsqz-output"));
        this.info(`Rootsqz completed, output at '${relOutPath}'.\nRun 'python -m http.server -d ${relOutPath}' to serve the output.`);
      }
    },
  };
}
