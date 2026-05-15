import * as esbuild from "esbuild";
import * as path from "node:path";

const nodeShims: Record<string, string> = {
  assert: path.resolve("./src/compat/node/assert.cjs"),
  buffer: path.resolve("./src/compat/node/buffer.cjs"),
  crypto: path.resolve("./src/compat/node/crypto.cjs"),
  events: path.resolve("./src/compat/node/events.cjs"),
  fs: path.resolve("./src/compat/node/fs.cjs"),
  "fs/promises": path.resolve("./src/compat/node/fs-promises.cjs"),
  module: path.resolve("./src/compat/node/module.cjs"),
  os: path.resolve("./src/compat/node/os.cjs"),
  path: path.resolve("./src/compat/node/path.cjs"),
  stream: path.resolve("./src/compat/node/stream.cjs"),
  url: path.resolve("./src/compat/node/url.cjs"),
  util: path.resolve("./src/compat/node/util.cjs"),
  worker_threads: path.resolve("./src/compat/node/worker_threads.cjs"),
};

await esbuild.build({
  logLevel: "info",
  bundle: true,
  entryPoints: ["./src/index.ts"],
  format: "esm",
  outfile: "dist/index.js",
  minifyWhitespace: true,
  define: {
    global: "globalThis",
    "__filename": "\"\"",
    "process.version": "\"\"",
  },
  alias: {
    jiti: path.resolve("./src/compat/jiti.cjs"),
    "jiti/package.json": path.resolve("./src/compat/jiti-package.cjs"),
    ...nodeShims,
    ...Object.fromEntries(
      Object.entries(nodeShims).map(([k, v]) => [`node:${k}`, v]),
    ),
  },
});
