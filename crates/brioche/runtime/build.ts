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
    window: "globalThis",
    "process.versions.node": "\"0.0.0\"",
  },
  plugins: [
    alias({
      buffer: path.resolve("./src/compat/node/buffer.cjs"),
      events: path.resolve("./src/compat/node/events.cjs"),
      fs: path.resolve("./src/compat/node/fs.cjs"),
      os: path.resolve("./src/compat/node/os.cjs"),
      path: path.resolve("./src/compat/node/path.cjs"),
      stream: path.resolve("./src/compat/node/stream.cjs"),
      util: path.resolve("./src/compat/node/util.cjs"),
    }),
  ],
});
