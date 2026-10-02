const fs = require("fs");
const https = require("https");
const http = require("http");
const path = require("path");
const os = require("os");

const RELEASE_BASE_URL = "https://github.com/r00tkids/websqz/releases/download";
const VERSION = "0.4";
const TOOL_NAME = "websqz";
const BIN_DIR = path.resolve(__dirname, "../dist/bin");

function getRustTarget() {
  const platform = process.platform;
  const arch = process.arch;

  let triple;
  let ext = "";

  if (platform === "win32") {
    ext = ".exe";
    if (arch === "x64") triple = "x86_64-pc-windows-msvc";
    else if (arch === "arm64") triple = "aarch64-pc-windows-msvc";
    else triple = `${arch}-pc-windows-msvc`;
  } else if (platform === "darwin") {
    if (arch === "x64") triple = "x86_64-apple-darwin";
    else if (arch === "arm64") triple = "aarch64-apple-darwin";
    else triple = `${arch}-apple-darwin`;
  } else if (platform === "linux") {
    if (arch === "x64") triple = "x86_64-unknown-linux-gnu";
    else if (arch === "arm64") triple = "aarch64-unknown-linux-gnu";
    else triple = `${arch}-unknown-linux-gnu`;
  } else {
    throw new Error(`Unsupported platform: ${platform}`);
  }

  return { triple, ext };
}

function buildAssetName(triple, ext) {
  return `${TOOL_NAME}-${triple}${ext}`;
}

function getDownloadUrl(assetName) {
  return `${RELEASE_BASE_URL}/v${VERSION}/${assetName}`;
}

function httpRequest(url, opts, callback) {
  const client = url.startsWith("https://") ? https : http;
  return client.get(url, opts, callback);
}

function downloadToFile(url, destPath) {
  return new Promise((resolve, reject) => {
    const maxRedirects = 5;
    let redirects = 0;

    function get(urlToGet) {
      const req = httpRequest(urlToGet, {}, (res) => {
        // Follow redirects
        if (
          res.statusCode >= 300 &&
          res.statusCode < 400 &&
          res.headers.location
        ) {
          if (redirects++ >= maxRedirects) {
            reject(new Error("Too many redirects"));
            return;
          }
          const next = new URL(res.headers.location, urlToGet).toString();
          res.resume();
          get(next);
          return;
        }

        if (res.statusCode !== 200) {
          reject(
            new Error(
              `Download failed: ${res.statusCode} ${res.statusMessage}`,
            ),
          );
          res.resume();
          return;
        }

        const total = parseInt(res.headers["content-length"] || "0", 10);
        let downloaded = 0;

        const fileStream = fs.createWriteStream(destPath, { mode: 0o755 });
        res.on("data", (chunk) => {
          downloaded += chunk.length;
          if (total) {
            const pct = ((downloaded / total) * 100).toFixed(1);
            process.stdout.write(
              `\rDownloading ${path.basename(destPath)} ${pct}% (${(downloaded / 1024).toFixed(1)} KB)`,
            );
          }
        });

        res.pipe(fileStream);

        fileStream.on("finish", () => {
          fileStream.close(() => {
            process.stdout.write("\n");
            resolve();
          });
        });

        fileStream.on("error", (err) => {
          fs.unlink(destPath, () => reject(err));
        });
      });

      req.on("error", (err) => reject(err));
    }

    get(url);
  });
}

async function ensureBinDir() {
  await fs.promises.mkdir(BIN_DIR, { recursive: true });
}

async function makeExecutable(filePath) {
  if (process.platform === "win32") {
    // Windows uses .exe; no chmod needed generally
    return;
  }
  try {
    await fs.promises.chmod(filePath, 0o755);
  } catch (err) {
    // Best-effort, not fatal
    console.warn(
      `Warning: could not set executable permissions on ${filePath}: ${err.message}`,
    );
  }
}

async function main() {
  try {
    const { triple, ext } = getRustTarget();
    const assetName = buildAssetName(triple, ext);
    const url = getDownloadUrl(assetName);

    console.log(`Detected platform: ${process.platform} ${process.arch}`);
    console.log(`Downloading asset: ${assetName}`);
    console.log(`From: ${url}`);

    if (fs.existsSync(BIN_DIR + "/" + TOOL_NAME + ext)) {
      console.log(`WebSQZ binary already exists at ${BIN_DIR}/${TOOL_NAME}${ext}, skipping download.`);
      return;
    }

    await ensureBinDir();

    const destFileName = `${TOOL_NAME}${ext}`; // place binary in bin with consistent name
    const destPath = path.join(BIN_DIR, destFileName);

    // Remove existing file if present (optional)
    try {
      await fs.promises.unlink(destPath);
    } catch (err) {
      // ignore if not exists
    }

    await downloadToFile(url, destPath);

    await makeExecutable(destPath);

    console.log(`Downloaded and saved to: ${destPath}`);
    console.log("Done.");
  } catch (err) {
    console.error(`Error: ${err.message}`);
    process.exitCode = 1;
  }
}

if (require.main === module) {
  main();
}
