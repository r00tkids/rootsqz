import { PluginContext, rollup, type OutputAsset, type OutputChunk, type OutputOptions, type Plugin } from "rollup";
import querystring from "node:querystring";
import fs from "node:fs/promises";
import path from "node:path";
import { createFilter, FilterPattern } from "@rollup/pluginutils";
import { compress, RUNTIME_GLOBAL } from "@r00tkids/rootsqz";

type RootSqzFile = {
  fileName: string;
  content: Buffer;
  isCompressed: boolean;
  fileExt: string;
  isText: boolean;
};

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

type RootSqzOptions = {
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
   * Write a detailed compression report next to the output (`--report`).
   */
  report?: boolean;

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
      include?: FilterPattern,
      exclude?: FilterPattern,
      transform: (ctx: PluginContext, id: string, content: Buffer) => Promise<RootsqzFileTransformRes>;
    }
  ]
};

export default function (options: RootSqzOptions = {}): Plugin {
  if (options.brotli && (options.config || options.report || options.sizeProfile === "64k")) {
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

  const loadAndTransform = async function (id: string, hookRes: RootsqzFileTransformRes) {
    if (isBuild) {
      const fileName = findNextAvailableFileName();
      files.set(id, {
        fileName,
        content: hookRes.content,
        isCompressed: hookRes.isCompressed,
        fileExt: hookRes.fileExt ?? path.extname(id),
        isText: hookRes.isText,
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
              return await loadAndTransform(id, hookRes);
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

        const isCompressed = parsed["compressed"] != null;

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
        .filter(([fileName, asset]) => fileName.endsWith('.js') && asset.type === 'chunk');

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

        for (const [id, file] of files) {
          this.debug(`Copying '${id}' for rootsqz...`);
          const outPath = path.resolve(
            outputOptions.dir || "",
            "rootsqz-tmp",
            file.fileName,
          );

          await fs.mkdir(path.dirname(outPath), { recursive: true });
          await fs.writeFile(outPath, file.content);

          if (file.isCompressed) {
            preCompressedFiles.push(path.relative(".", outPath));
          } else {
            filesToCompress.push({ isText: file.isText, fileExt: file.fileExt, path: path.relative(".", outPath) });
          }
        }

        // Sort by file extension for better compression ratios in rootsqz
        filesToCompress.sort((a, b) => Math.sign(a.fileExt.localeCompare(b.fileExt)) + 2 * (a.isText === b.isText ? 0 : a.isText ? -1 : 1));

        await compress({
          binaryPath: options.rootsqzPath,
          jsMain: path.resolve(
            outputOptions.dir || "",
            jsFileName,
          ),
          files: filesToCompress.map(f => f.path),
          preCompressedFiles: preCompressedFiles,
          outputDirectory: path.resolve(
            outputOptions.dir || "",
            "rootsqz-output",
          ),
          sizeProfile: options.sizeProfile,
          brotli: options.brotli,
          config: options.config,
          report: options.report,
          extraArgs: options.rootsqzArgs,
        });

        const relOutPath = path.relative(".", path.resolve(outputOptions.dir || "", "rootsqz-output"));
        this.info(`Rootsqz completed, output at '${relOutPath}'.\nRun 'python -m http.server -d ${relOutPath}' to serve the output.`);
      }
    },
  };
}
