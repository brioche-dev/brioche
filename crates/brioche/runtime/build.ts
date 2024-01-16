import * as esbuild from "esbuild";

await esbuild.build({
  bundle: true,
  entryPoints: ["./src/index.ts"],
  format: "esm",
  outfile: "dist/index.js",
  minifyWhitespace: true,
  define: {
    window: "globalThis",
  },
});
