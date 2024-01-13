import * as prettier from "prettier";
import prettierPluginTypescript from "prettier/plugins/typescript";
import prettierPluginEstree from "prettier/plugins/estree";

export async function format(source: string): Promise<string> {
  return await prettier.format(source, {
    parser: "typescript",
    experimentalTernaries: true,
    plugins: [
      prettierPluginTypescript,
      prettierPluginEstree,
    ],
  });
}
