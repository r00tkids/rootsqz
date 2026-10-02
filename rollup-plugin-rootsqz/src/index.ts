import { PluginContext, rollup, type OutputAsset, type OutputChunk, type OutputOptions, type Plugin } from "rollup";
import querystring from "node:querystring";
import fs from "node:fs/promises";
import fsSync from "node:fs";
import path from "node:path";
import child_process from "node:child_process";
import { createFilter, FilterPattern } from "@rollup/pluginutils";

type WebSqzFile = {
  fileName: string;
  content: Buffer;
  isCompressed: boolean;
  fileExt: string;
  isText: boolean;
};

export type WebsqzFileTransformRes = {
  /**
   * The result of processing
   */
  content: Buffer;

  /**
   * Is already compressed and should not be compressed again by websqz
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

type WebSqzOptions = {
  websqzPath?: string;

  /**
   * File transform hooks to process files before they are imported in code or compressed by websqz
   */
  fileTransforms?: [
    {
      include?: FilterPattern,
      exclude?: FilterPattern,
      transform: (ctx: PluginContext, id: string, content: Buffer) => Promise<WebsqzFileTransformRes>;
    }
  ]
};

type WebSqzCliOptions = {
  jsMain: string;
  files: string[];
  preCompressedFiles: string[];
  output: string;
}

class WebSqzExe {
  websqzPath: string;
  constructor(websqzPath: string) {
    this.websqzPath = websqzPath;
  }

  async run(cliOptions: WebSqzCliOptions): Promise<void> {
    const args: string[] = [];

    args.push("--js-main", cliOptions.jsMain);

    for (const file of cliOptions.files) {
      args.push("--files", file);
    }
    for (const preCompressedFile of cliOptions.preCompressedFiles) {
      args.push("--pre-compressed-files", preCompressedFile);
    }

    args.push("--output-directory", cliOptions.output);

    const spawn = await child_process.spawn(this.websqzPath, args, {
      stdio: "inherit",
    });

    return new Promise<void>((resolve, reject) => {
      spawn.once("error", (err) => {
        reject(err);
      });

      spawn.once("close", (code) => {
        if (code !== 0) {
          reject(new Error(`websqz process exited with code ${code}`));
        } else {
          resolve();
        }
      });
    });
  }
}

function websqzExecutablePath(executablePath: string | undefined): string {
  if (!executablePath) {
    const path = import.meta.dirname + "/bin/websqz";
    const extension = process.platform == "win32" ? ".exe" : "";

    if (fsSync.existsSync(path + extension)) {
      return path + extension;
    }

    return "websqz" + extension;
  }

  return executablePath;
}

export default function (options: WebSqzOptions = {}): Plugin {
  const isBuild = process.env.NODE_ENV === "production";
  const websqzExePath = websqzExecutablePath(options.websqzPath);
  const websqzExe = new WebSqzExe(websqzExePath);

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

  const files = new Map<string, WebSqzFile>();
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

  const loadAndTransform = async function (id: string, hookRes: WebsqzFileTransformRes) {
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
          ? `export default new TextDecoder().decode(wsqz.files["${fileName}"]);` 
          : `export default wsqz.files["${fileName}"];`,
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
    name: "rollup-plugin-websqz",

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

        const isWebSqzTxt = parsed["websqz-txt"] != null;
        const isWebSqzBin = parsed["websqz-bin"] != null;

        if (isWebSqzTxt && isWebSqzBin) {
          throw new Error(
            `Cannot use both websqz-txt and websqz-bin on the same import: ${id}`,
          );
        }

        const isCompressed = parsed["compressed"] != null;

        if (isWebSqzTxt || isWebSqzBin) {
          const content = await loadFromDisk();
          
          let hookRes: WebsqzFileTransformRes = {
            content,
            isCompressed,
            isText: isWebSqzTxt,
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
            "websqz-tmp");
        if (await fs.stat(outDir).catch(() => false)) {
          await fs.rm(outDir, { recursive: true, force: true });
        }

        const filesToCompress = [];
        const preCompressedFiles = [];

        for (const [id, file] of files) {
          this.debug(`Copying '${id}' for websqz...`);
          const outPath = path.resolve(
            outputOptions.dir || "",
            "websqz-tmp",
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

        // Sort by file extension for better compression ratios in websqz
        filesToCompress.sort((a, b) => Math.sign(a.fileExt.localeCompare(b.fileExt)) + 2 * (a.isText === b.isText ? 0 : a.isText ? -1 : 1));

        this.info(`Using websqz executable at '${websqzExe.websqzPath}'`);
        await websqzExe.run({
          jsMain: path.resolve(
            outputOptions.dir || "",
            jsFileName,
          ),
          files: filesToCompress.map(f => f.path),
          preCompressedFiles: preCompressedFiles,
          output: path.resolve(
            outputOptions.dir || "",
            "websqz-output",
          ),
        });

        const relOutPath = path.relative(".", path.resolve(outputOptions.dir || "", "websqz-output"));
        this.info(`Websqz completed, output at '${relOutPath}'.\nRun 'python -m http.server -d ${relOutPath}' to serve the output.`);
      }
    },
  };
}
