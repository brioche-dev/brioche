import * as esbuild from "esbuild";
import alias from "esbuild-plugin-alias";
import * as path from "node:path";

await esbuild.build({
  bundle: true,
  entryPoints: ["./src/index.ts"],
  format: "esm",
  outfile: "dist/index.js",
  minifyWhitespace: true,
  define: {
    global: "globalThis",
    "__filename": "\"\"",
  },
  plugins: [
    alias({
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
    }),
  ],
});
