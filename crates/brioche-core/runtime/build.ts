import * as esbuild from "esbuild";
import { aliasPath } from "esbuild-plugin-alias-path";
import * as path from "node:path";

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
  plugins: [
    aliasPath({
      alias: {
        assert: path.resolve("./src/compat/node/assert.cjs"),
        buffer: path.resolve("./src/compat/node/buffer.cjs"),
        crypto: path.resolve("./src/compat/node/crypto.cjs"),
        events: path.resolve("./src/compat/node/events.cjs"),
        fs: path.resolve("./src/compat/node/fs.cjs"),
        module: path.resolve("./src/compat/node/module.cjs"),
        os: path.resolve("./src/compat/node/os.cjs"),
        path: path.resolve("./src/compat/node/path.cjs"),
        stream: path.resolve("./src/compat/node/stream.cjs"),
        url: path.resolve("./src/compat/node/url.cjs"),
        util: path.resolve("./src/compat/node/util.cjs"),
        "node:assert": path.resolve("./src/compat/node/assert.cjs"),
        "node:buffer": path.resolve("./src/compat/node/buffer.cjs"),
        "node:crypto": path.resolve("./src/compat/node/crypto.cjs"),
        "node:events": path.resolve("./src/compat/node/events.cjs"),
        "node:fs": path.resolve("./src/compat/node/fs.cjs"),
        "node:fs/promises": path.resolve("./src/compat/node/fs-promises.cjs"),
        "node:module": path.resolve("./src/compat/node/module.cjs"),
        "node:os": path.resolve("./src/compat/node/os.cjs"),
        "node:path": path.resolve("./src/compat/node/path.cjs"),
        "node:stream": path.resolve("./src/compat/node/stream.cjs"),
        "node:url": path.resolve("./src/compat/node/url.cjs"),
        "node:util": path.resolve("./src/compat/node/util.cjs"),
        "node:worker_threads": path.resolve("./src/compat/node/worker_threads.cjs"),
        "jiti": path.resolve("./src/compat/jiti.cjs"),
        "jiti/package.json": path.resolve("./src/compat/jiti-package.cjs"),
      },
    }),
  ],
});
